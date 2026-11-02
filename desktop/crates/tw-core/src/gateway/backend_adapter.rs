//! 装配层：把 upstream 层的 [`Upstream`] 实现接到网关冻结契约 [`ChatBackend`] 上。
//!
//! 对应 Kotlin `LocalFakeServer` 的 `credProvider + backendForModelProvider + runUpstream` 三件套：
//! - **按账号登记**：每个可用账号一个 `(id = "平台#uid", UpstreamBackend)`，网关的 failover
//!   即沿登记序换 id（对齐 §4 冻结契约的账号级拆分建议，也复刻 `pick_failover_candidate`
//!   "剔除失败号、优先受支持区域"的排序纪律——active 排最前、其余 CN 区在前按登记序）。
//! - **无凭证不对称**：某平台被选中却没有任何可用账号时，仍登记一个 uid 为空的占位后端，
//!   保持 Trae→`NoCredential`（200 fake）/ Qoder→`UpstreamUnavailable`（502）的既有语义（§3.1/§2.2）。
//! - **惰启动**：`chat_stream` 只做廉价的凭证形态校验；真正的 `Upstream::stream_chat`
//!   （会 spawn 网络 pump）推迟到返回流**首次被轮询**时才发生——网关换号预启动下一候选
//!   （§3.3 `tryFailover`）因此不会浪费一次真实请求。

use super::{ChatBackend, ChatStream, Frame, GatewayError, GatewayResult};
use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::models::catalog::QoderModelCatalog;
use crate::models::repo::ModelsRepo;
use crate::models::routing::models_for;
use crate::types::{BACKEND_AUTO, BACKEND_QODER, BACKEND_TRAE};
use crate::upstream::{qoder::QoderUpstream, trae::TraeUpstream, ChatRequest, StreamResult, Upstream};
use futures_util::stream::unfold;
use serde_json::Value;
use std::sync::Arc;

/// 后端所属平台（决定校验形态、fake/502 不对称与模型表来源）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    Trae,
    Qoder,
}

impl Platform {
    fn as_str(self) -> &'static str {
        match self {
            Platform::Trae => BACKEND_TRAE,
            Platform::Qoder => BACKEND_QODER,
        }
    }
}

/// 一个账号（或占位空号）到网关的转发后端。
pub struct UpstreamBackend {
    platform: Platform,
    /// 登记账号 uid；空串 = 占位后端（运行期按 store 的 active 账号兜底）。
    uid: String,
    upstream: Arc<dyn Upstream>,
    trae_store: Arc<TraeAuthStore>,
    qoder_store: Arc<QoderAuthStore>,
}

impl UpstreamBackend {
    fn new(platform: Platform, uid: String, upstream: Arc<dyn Upstream>, trae_store: Arc<TraeAuthStore>, qoder_store: Arc<QoderAuthStore>) -> Self {
        Self { platform, uid, upstream, trae_store, qoder_store }
    }

    /// 从登记 id（`"平台#uid"`）解析本次实际账号；缺省回退登记 uid。
    fn resolve_uid(&self, api_key_id: &str) -> String {
        match api_key_id.split_once('#') {
            Some((_p, u)) => u.trim().to_string(),
            None => self.uid.trim().to_string(),
        }
    }

    /// Trae 形态校验：无账号 / access_token 空白（`load_by_uid` 已过滤）/ authFailed 标记 → 无凭证。
    /// uid 空（占位登记）→ 按 active 兜底解析。
    fn trae_usable(&self, uid: &str) -> Option<String> {
        let store = &self.trae_store;
        let target = if uid.is_empty() { store.active_uid().unwrap_or_default() } else { uid.to_string() };
        if target.is_empty() || store.auth_failed_for(Some(&target)) || store.load_by_uid(&target).is_none() {
            return None;
        }
        Some(target)
    }

    /// Qoder 形态校验：无账号 / `chat_ready` 不过（uid+jobToken 齐）/ authFailed 标记 → 不可用。
    /// 临期 jobToken 不拦截——upstream 层自带续期（upstream 模块头「token 新鲜度」）。
    fn qoder_usable(&self, uid: &str) -> Option<String> {
        let store = &self.qoder_store;
        let target = if uid.is_empty() { store.active_uid().unwrap_or_default() } else { uid.to_string() };
        if target.is_empty() || store.auth_failed_for(Some(&target)) {
            return None;
        }
        if !store.load_by_uid(&target).map(|a| a.chat_ready()).unwrap_or(false) {
            return None;
        }
        Some(target)
    }
}

impl ChatBackend for UpstreamBackend {
    /// 单平台模型表（对账 Kotlin `modelsFor` 的固定选择分支；同名模型的 Trae 优先由
    /// 网关"命中前置 + 登记序 trae 在前"解决，本层不做并集以免串台）。
    fn list_models(&self) -> Vec<Value> {
        let (repo, catalog) = (ModelsRepo::global(), QoderModelCatalog::global());
        match self.platform {
            Platform::Trae => models_for(BACKEND_TRAE, true, false, &repo, &catalog),
            Platform::Qoder => models_for(BACKEND_QODER, false, true, &repo, &catalog),
        }
    }

    fn chat_stream(&self, body: Value, api_key_id: &str) -> GatewayResult<ChatStream> {
        let uid = self.resolve_uid(api_key_id);
        let target = match self.platform {
            Platform::Trae => self.trae_usable(&uid).ok_or(GatewayError::NoCredential)?,
            Platform::Qoder => self.qoder_usable(&uid).ok_or(GatewayError::UpstreamUnavailable(None))?,
        };
        let upstream = self.upstream.clone();
        Ok(deferred(upstream, ChatRequest { body, uid: target, token: None }))
    }
}

// ======================================================================
// 惰启动流：首次 poll 才触网，FrameStream 帧 → 网关 Frame
// ======================================================================

enum LazyState {
    /// 尚未调用 `Upstream::stream_chat`（零网络副作用）。
    Pre(Arc<dyn Upstream>, ChatRequest),
    /// 泵已启动，逐帧透传。
    Run(StreamResult),
    /// 已产出 Done（网关契约：Done 后不再产出）。
    End,
}

enum Step {
    Chunk(String, StreamResult),
    Final(StreamResult),
}

/// 取下一帧；通道耗尽（pump 结束）后交由调用方读 outcome 定 Done。
async fn next_frame(mut sr: StreamResult) -> Step {
    match sr.frames.next().await {
        Some(f) => Step::Chunk(f, sr),
        None => Step::Final(sr),
    }
}

fn deferred(upstream: Arc<dyn Upstream>, req: ChatRequest) -> ChatStream {
    Box::pin(unfold(LazyState::Pre(upstream, req), |state| async move {
        let sr = match state {
            LazyState::Pre(u, r) => u.stream_chat(&r),
            LazyState::Run(sr) => sr,
            LazyState::End => return None,
        };
        match next_frame(sr).await {
            Step::Chunk(f, sr) => Some((Frame::Chunk(f), LazyState::Run(sr))),
            Step::Final(sr) => {
                // 兜底：err=None 且一帧未出 = pump 异常早退（连接建立期彻底失败的 Kotlin 形态）
                // → `interrupted`，网关据此才允许换号（§3.3 闸门纪律依赖非空 err）。
                let mut err = sr.outcome.err();
                if err.is_none() && !sr.outcome.saw_frame() {
                    err = Some("interrupted".to_string());
                }
                Some((Frame::Done { usage: sr.outcome.usage(), err }, LazyState::End))
            }
        }
    }))
}

// ======================================================================
// 装配入口（appcore 调用）
// ======================================================================

/// 按当前配置与账号存储构建网关登记表（failover 候选序）。
pub fn build_backends() -> Result<Vec<(String, Arc<dyn ChatBackend>)>, String> {
    let selection = crate::config::get().gateway_backend;
    Ok(build_backends_from(&selection, &TraeAuthStore::global(), &QoderAuthStore::global()))
}

/// 纯函数版装配（store 注入，单测免依赖全局存储）。
/// 顺序纪律：**trae 在前、qoder 在后**（同名模型 Trae 优先，对齐 `auto_union` 并集序）；
/// 平台内 active 最前、其余 CN-受支持区在前按登记序；authFailed/空 token 形态剔除；
/// 选中平台无任何可用账号时登记占位裸 id（保持 fake/502 不对称语义）。
pub(crate) fn build_backends_from(
    selection: &str,
    trae: &Arc<TraeAuthStore>,
    qoder: &Arc<QoderAuthStore>,
) -> Vec<(String, Arc<dyn ChatBackend>)> {
    let sel = if selection == BACKEND_TRAE || selection == BACKEND_QODER { selection } else { BACKEND_AUTO };
    let mut out: Vec<(String, Arc<dyn ChatBackend>)> = Vec::new();
    if sel != BACKEND_QODER {
        push_platform(&mut out, Platform::Trae, trae_ordered(trae), Arc::new(TraeUpstream), trae, qoder);
    }
    if sel != BACKEND_TRAE {
        push_platform(&mut out, Platform::Qoder, qoder_ordered(qoder), Arc::new(QoderUpstream), trae, qoder);
    }
    out
}

fn push_platform(
    out: &mut Vec<(String, Arc<dyn ChatBackend>)>,
    platform: Platform,
    uids: Vec<String>,
    upstream: Arc<dyn Upstream>,
    trae: &Arc<TraeAuthStore>,
    qoder: &Arc<QoderAuthStore>,
) {
    if uids.is_empty() {
        let backend = UpstreamBackend::new(platform, String::new(), upstream, trae.clone(), qoder.clone());
        out.push((platform.as_str().to_string(), Arc::new(backend)));
        return;
    }
    for uid in uids {
        let backend = UpstreamBackend::new(platform, uid.clone(), upstream.clone(), trae.clone(), qoder.clone());
        out.push((format!("{}#{}", platform.as_str(), uid), Arc::new(backend)));
    }
}

/// Trae 候选序：active 最前 → 其余受支持区域（CN）在前 → 其他，稳定保持登记序；
/// 剔除 authFailed 与 `load_by_uid` 不通过（空 access_token）的账号。
fn trae_ordered(store: &TraeAuthStore) -> Vec<String> {
    let active = store.active_uid();
    let mut usable: Vec<(u8, String)> = store
        .list()
        .iter()
        .filter(|m| !store.auth_failed_for(Some(&m.uid)) && store.load_by_uid(&m.uid).is_some())
        .map(|m| {
            let rank = if active.as_deref() == Some(m.uid.as_str()) {
                0
            } else if m.is_supported() {
                1
            } else {
                2
            };
            (rank, m.uid.clone())
        })
        .collect();
    usable.sort_by_key(|(rank, _)| *rank);
    usable.into_iter().map(|(_, uid)| uid).collect()
}

/// Qoder 候选序：active 最前，其余按登记序；剔除 authFailed 与非 `chat_ready`。
fn qoder_ordered(store: &QoderAuthStore) -> Vec<String> {
    let active = store.active_uid();
    let mut usable: Vec<(u8, String)> = store
        .list()
        .iter()
        .filter(|m| !store.auth_failed_for(Some(&m.uid)) && store.load_by_uid(&m.uid).map(|a| a.chat_ready()).unwrap_or(false))
        .map(|m| {
            let rank = if active.as_deref() == Some(m.uid.as_str()) { 0 } else { 1 };
            (rank, m.uid.clone())
        })
        .collect();
    usable.sort_by_key(|(rank, _)| *rank);
    usable.into_iter().map(|(_, uid)| uid).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::model::{QoderAccount, TraeAccount};
    use crate::upstream::{finish, new_stream};
    use futures_util::StreamExt;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 记录调用次数的假上游：try_send 预置帧 + finish，poll 立即收敛，全程零网络。
    struct MockUpstream {
        calls: Arc<AtomicUsize>,
        frames: Vec<String>,
        err: Option<String>,
    }

    impl MockUpstream {
        fn new(frames: Vec<&str>, err: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                calls: Arc::new(AtomicUsize::new(0)),
                frames: frames.into_iter().map(str::to_string).collect(),
                err: err.map(str::to_string),
            })
        }
    }

    impl Upstream for MockUpstream {
        fn stream_chat(&self, _req: &ChatRequest) -> StreamResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (tx, res) = new_stream();
            for f in &self.frames {
                let _ = tx.try_send(f.clone());
            }
            drop(tx);
            finish(&res.outcome, None, self.err.clone());
            res
        }
    }

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_store(tag: &str) -> Arc<crate::storage::DataStore> {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("tw-adapter-{}-{}-{}", tag, n, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // 跨进程残留清干净再注入
        Arc::new(crate::storage::DataStore::new(dir))
    }

    fn trae_account(uid: &str, region: &str) -> TraeAccount {
        TraeAccount {
            uid: uid.into(),
            nickname: uid.into(),
            region_id: region.into(),
            expires_at: 0,
            access_token: format!("tok-{}", uid),
            refresh_token: "r".into(),
        }
    }

    fn qoder_account(uid: &str, job: &str) -> QoderAccount {
        QoderAccount { uid: uid.into(), name: uid.into(), job_token: job.into(), pat: format!("pat-{}", uid), ..Default::default() }
    }

    fn backend(trae: Arc<TraeAuthStore>, qoder: Arc<QoderAuthStore>, platform: Platform, uid: &str, mock: Arc<MockUpstream>) -> UpstreamBackend {
        UpstreamBackend::new(platform, uid.into(), mock, trae, qoder)
    }

    fn empty_stores() -> (Arc<TraeAuthStore>, Arc<QoderAuthStore>) {
        (Arc::new(TraeAuthStore::with_store(temp_store("trae"))), Arc::new(QoderAuthStore::with_store(temp_store("qoder"))))
    }

    async fn collect_frames(s: &mut ChatStream) -> Vec<Frame> {
        let mut out = Vec::new();
        while let Some(f) = s.next().await {
            out.push(f);
        }
        out
    }

    #[tokio::test]
    async fn chat_stream_is_lazy() {
        let (ts, qs) = empty_stores();
        ts.save(&trae_account("u1", "cn"));
        let mock = MockUpstream::new(vec!["data: {\"choices\":[]}\n\n", "data: [DONE]\n\n"], None);
        let b = backend(ts, qs, Platform::Trae, "u1", mock.clone());

        // 廉价校验阶段绝不能触网（网关 tryFailover 会预启动下一候选的 chat_stream）。
        let mut stream = b.chat_stream(json!({"model": "m"}), "trae#u1").expect("可用账号应通过校验");
        assert_eq!(mock.calls.load(Ordering::SeqCst), 0, "chat_stream 返回时不得调用 upstream");

        let got = collect_frames(&mut stream).await;
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1, "首次轮询才启动 upstream");
        assert_eq!(got.len(), 3, "两帧 + Done 收尾");
        assert!(matches!(&got[0], Frame::Chunk(s) if s.contains("choices")));
        assert!(matches!(&got[1], Frame::Chunk(s) if s.contains("[DONE]")));
        match &got[2] {
            Frame::Done { usage, err } => {
                assert!(usage.is_none());
                assert_eq!(err, &None);
            }
            other => panic!("末项应为 Done: {:?}", other),
        }
    }

    #[tokio::test]
    async fn err_maps_to_done_and_interrupted_fallback() {
        let (ts, qs) = empty_stores();
        ts.save(&trae_account("u1", "cn"));

        // 上游错误：http:429 → Done.err 原样透出。
        let mock = MockUpstream::new(vec![], Some("http:429"));
        let b = backend(ts.clone(), qs.clone(), Platform::Trae, "u1", mock.clone());
        let mut s = b.chat_stream(json!({}), "trae#u1").unwrap();
        match s.next().await {
            Some(Frame::Done { err, .. }) => assert_eq!(err.as_deref(), Some("http:429")),
            other => panic!("期望 Done: {:?}", other),
        }
        assert!(s.next().await.is_none());

        // 无帧且无错误 = pump 异常早退 → interrupted 兜底（网关闸门纪律要求非空 err 才换号）。
        let mock2 = MockUpstream::new(vec![], None);
        let b2 = backend(ts, qs, Platform::Trae, "u1", mock2);
        let mut s2 = b2.chat_stream(json!({}), "trae#u1").unwrap();
        match s2.next().await {
            Some(Frame::Done { err, .. }) => assert_eq!(err.as_deref(), Some("interrupted")),
            other => panic!("期望 Done(interrupted): {:?}", other),
        }
    }

    #[test]
    fn cheap_validation_shapes() {
        let (ts, qs) = empty_stores();

        // Trae 无账号 → NoCredential（200 fake 语义）。
        let b = backend(ts.clone(), qs.clone(), Platform::Trae, "", MockUpstream::new(vec![], None));
        assert!(matches!(b.chat_stream(json!({}), "trae"), Err(GatewayError::NoCredential)));

        // Qoder 无账号 → UpstreamUnavailable(None)（502 语义）。
        let b = backend(ts.clone(), qs.clone(), Platform::Qoder, "", MockUpstream::new(vec![], None));
        assert!(matches!(b.chat_stream(json!({}), "qoder"), Err(GatewayError::UpstreamUnavailable(m)) if m.is_none()));

        // authFailed 标记：装配后运行期新标记也要拦住。
        ts.save(&trae_account("u2", "cn"));
        ts.mark_auth_failed("u2");
        let b = backend(ts.clone(), qs.clone(), Platform::Trae, "u2", MockUpstream::new(vec![], None));
        assert!(matches!(b.chat_stream(json!({}), "trae#u2"), Err(GatewayError::NoCredential)));

        qs.save(&qoder_account("q2", "job"));
        qs.mark_auth_failed("q2");
        let b = backend(ts, qs, Platform::Qoder, "q2", MockUpstream::new(vec![], None));
        assert!(matches!(b.chat_stream(json!({}), "qoder#q2"), Err(GatewayError::UpstreamUnavailable(_))));
    }

    #[test]
    fn candidate_order_and_registration() {
        let ts_arc = Arc::new(TraeAuthStore::with_store(temp_store("ord-trae")));
        let ts = &*ts_arc;
        ts.save(&trae_account("cn1", "cn"));
        ts.save(&trae_account("sg1", "sg"));
        ts.save(&trae_account("cn2", "cn"));
        ts.save(&TraeAccount { uid: "no_tok".into(), access_token: " ".into(), ..Default::default() });
        ts.save(&trae_account("cn3", "cn"));
        ts.set_active("cn2");
        ts.mark_auth_failed("cn3");
        assert_eq!(
            trae_ordered(ts),
            vec!["cn2".to_string(), "cn1".to_string(), "sg1".to_string()],
            "active→CN→SG（cn3 authFailed、no_tok 空 token 剔除）"
        );

        let qs_arc = Arc::new(QoderAuthStore::with_store(temp_store("ord-qoder")));
        let qs = &*qs_arc;
        qs.save(&qoder_account("q1", "job1"));
        qs.save(&qoder_account("q2", ""));
        qs.save(&qoder_account("q3", "job3"));
        qs.set_active("q3");
        assert_eq!(qoder_ordered(qs), vec!["q3".to_string(), "q1".to_string()], "qoder：active 最前，剔除非 chat_ready 的 q2");

        // 显式选择：只登记该平台；无可用账号 → 占位裸 id。
        let (empty_ts, empty_qs) = empty_stores();
        assert_eq!(
            build_backends_from(BACKEND_TRAE, &empty_ts, &empty_qs).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["trae"]
        );
        assert_eq!(
            build_backends_from(BACKEND_QODER, &empty_ts, &empty_qs).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["qoder"]
        );

        // auto：trae 在前 qoder 后（同名模型 Trae 优先的登记序纪律）。
        let ids: Vec<String> = build_backends_from(BACKEND_AUTO, &ts_arc, &qs_arc).iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(ids, vec!["trae#cn2", "trae#cn1", "trae#sg1", "qoder#q3", "qoder#q1"], "登记序：trae 前 qoder 后");
    }

    #[tokio::test]
    async fn placeholder_backend_falls_back_to_active() {
        // 占位后端（uid 空）运行期出现可用账号时：按 active 兜底通过校验并发起请求。
        let (ts, qs) = empty_stores();
        let b = backend(ts.clone(), qs.clone(), Platform::Trae, "", MockUpstream::new(vec![], None));
        assert!(matches!(b.chat_stream(json!({}), "trae"), Err(GatewayError::NoCredential)));
        ts.save(&trae_account("late", "cn"));
        let mut s = b.chat_stream(json!({}), "trae").expect("active 兜底应通过");
        match s.next().await {
            Some(Frame::Done { err, .. }) => assert_eq!(err.as_deref(), Some("interrupted")),
            other => panic!("期望 Done: {:?}", other),
        }
    }
}
