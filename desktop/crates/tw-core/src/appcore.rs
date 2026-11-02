//! 进程级装配（对应 Kotlin `object Gateway` 的持有者角色 + DataStore 初始化 +
//! `MainActivity.onCreate` 的启动编排）。start/stop 幂等。
//!
//! 接线面（analysis `gateway-server.md` §4 / `checkin-credits.md` §5）：
//! - `gateway_start` = `Gateway.start` → `Server::assemble_with(build_backends())`（账号级登记表）
//!   + `usageRecorder` 注入（`UsageStore.record` + `CreditMonitor.noteForwarded`）。
//! - `startup` = 启动编排：意图驱动的网关自启 → 签到调度 → 额度监控（单项失败只告警，不阻塞）。

use crate::storage::DataStore;
use std::path::Path;
use std::sync::Arc;

pub struct AppCore {
    pub data: Arc<DataStore>,
    /// 当前网关服务句柄（gateway::Server）。Mutex 仅作槽位保护，非串行锁。
    pub server: parking_lot::Mutex<Option<crate::gateway::Server>>,
}

static CORE: std::sync::OnceLock<Arc<AppCore>> = std::sync::OnceLock::new();

/// 初始化：注入数据目录（Tauri app_data_dir 或 bridge --data-dir）。
pub fn init(data_dir: impl AsRef<Path>) {
    crate::storage::init(&data_dir);
    let core = Arc::new(AppCore { data: crate::storage::store(), server: Default::default() });
    if CORE.set(core).is_ok() {
        // 启动自检（对齐 MainActivity.onCreate 的 selfCheck 集群：失败只记录，不阻塞）
        if let Err(e) = crate::fingerprint::self_check() {
            eprintln!("[自检警告（不影响使用）] {}", e);
        }
    }
}

pub fn core() -> Arc<AppCore> {
    CORE.get().expect("AppCore 未初始化（先调用 tw_core::appcore::init）").clone()
}

/// 幂等启动网关（`Gateway.start`：已在跑直接返回 Ok(false)，防 EADDRINUSE）。
/// 后端登记来自装配层 [`crate::gateway::build_backends`]（账号级 failover 候选序）。
pub async fn gateway_start() -> Result<bool, String> {
    let c = core();
    {
        if c.server.lock().as_ref().map(|s| s.is_running()).unwrap_or(false) {
            return Ok(false);
        }
    }
    let s = crate::gateway::Server::assemble_with(crate::gateway::build_backends()?)?;
    s.set_on_usage(usage_hook());
    s.clone().spawn().await?;
    c.server.lock().replace(s);
    let running = crate::appcore::gateway_running();
    if running {
        crate::emitter::emit(crate::emitter::topics::SERVICE_STATE, serde_json::json!({ "running": true }));
    }
    Ok(running)
}

/// 幂等停止（`Gateway.stop` + `CreditMonitor.stop` 语义）。签到调度独立于网关，不停。
pub async fn gateway_stop() {
    let c = core();
    let s = c.server.lock().take();
    if let Some(s) = s {
        s.shutdown().await;
    }
    crate::credits::monitor::stop();
    crate::emitter::emit(crate::emitter::topics::SERVICE_STATE, serde_json::json!({ "running": false }));
}

/// App 打开时的启动编排（等价 `onCreate` + `autoStartServiceIfWanted`）：
/// 用户没主动停过就拉起网关；随后挂调度、启监控。任何单项失败只告警，不阻塞其余步骤。
pub async fn startup() {
    if intent::should_autostart_on_open() {
        if let Err(e) = gateway_start().await {
            eprintln!("[启动] 网关自启失败: {}", e);
        }
    }
    if let Err(e) = crate::checkin::scheduler::start(Arc::new(crate::checkin::CheckinState::global())) {
        eprintln!("[启动] 签到调度挂载失败: {}", e);
    }
    crate::credits::monitor::start();
}

pub fn gateway_running() -> bool {
    CORE.get().map(|c| c.server.lock().as_ref().map(|s| s.is_running()).unwrap_or(false)).unwrap_or(false)
}

/// usageRecorder（§4）：每轮 settle 落一条用量 + 喂额度监控。
/// `urgent` 语义对齐 Kotlin `noteForwarded(forwardFailed)`：本轮失败必查；
/// 成功时仅在"凭证临期 / 额度缓存见底"时跳过节流（防对正常流量高频轮询）。
fn usage_hook() -> crate::gateway::UsageHook {
    Arc::new(|rec: crate::gateway::Record| {
        let digits = |k: &str| -> i64 {
            rec.usage.as_ref().and_then(|u| u.get(k)).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
        };
        crate::usage::record(&crate::types::UsageRecord {
            ts: crate::auth::model::now_ms(),
            model: rec.model.clone(),
            prompt: digits("prompt_tokens"),
            completion: digits("completion_tokens"),
            total: digits("total_tokens"),
            ms: rec.elapsed_ms,
            ok: rec.ok,
            err: rec.err_type.clone().unwrap_or_default(),
        });
        // cand.id 形态 "平台#uid"：网关把登记 id 同时填进 backend/uid（Record 契约注释）。
        let (platform, uid) = match rec.backend.split_once('#') {
            Some((p, u)) if !u.trim().is_empty() => (p, Some(u.to_string())),
            Some((p, _)) => (p, None),
            None => (rec.backend.as_str(), None),
        };
        let urgent = forward_urgent(platform, uid.as_deref(), !rec.ok);
        crate::credits::monitor::note_forwarded(platform, uid, urgent);
    })
}

/// 跳节流判据：转发失败（urgent 主路径）∨ 凭证临期（expires_at=0 未知不判，防过度轮询）
/// ∨ 额度缓存见底（只读 `credits::account::snapshot()` 缓存，绝不在钩子里发网络请求）。
fn forward_urgent(platform: &str, uid: Option<&str>, failed: bool) -> bool {
    if failed {
        return true;
    }
    use crate::credits::monitor::TOKEN_SKEW_SEC;
    if platform == crate::types::BACKEND_QODER {
        let store = crate::auth::store::QoderAuthStore::global();
        let acc = match uid {
            Some(u) => store.load_by_uid(u),
            None => store.load(),
        };
        if let Some(a) = acc {
            if a.expires_at > 0 && crate::auth::store::QoderAuthStore::qoder_expired_within(a.expires_at, TOKEN_SKEW_SEC * 1000) {
                return true;
            }
        }
    } else {
        let store = crate::auth::store::TraeAuthStore::global();
        let acc = match uid {
            Some(u) => store.load_by_uid(u),
            None => store.load(),
        };
        if let Some(a) = acc {
            if a.expires_at > 0 && crate::auth::store::TraeAuthStore::expired_within(a.expires_at, TOKEN_SKEW_SEC) {
                return true;
            }
        }
    }
    let uid_resolved = match uid.map(str::to_string).or_else(|| {
        if platform == crate::types::BACKEND_QODER {
            crate::auth::store::QoderAuthStore::global().active_uid()
        } else {
            crate::auth::store::TraeAuthStore::global().active_uid()
        }
    }) {
        Some(u) if !u.trim().is_empty() => u,
        _ => return false,
    };
    let threshold = crate::config::get().auto_switch_threshold.max(crate::credits::monitor::MIN_THRESHOLD) as f64;
    crate::credits::account::snapshot()
        .get(platform, &uid_resolved)
        .map(|item| item.exceeded || item.remaining.map(|r| r <= threshold).unwrap_or(false))
        .unwrap_or(false)
}

/// should_run 三态意图（对应 prefs `traework2api_service`：从未表达/主动关/主动开）。
pub mod intent {
    use crate::storage::store;
    const FILE: &str = "service.json";

    pub fn has_user_intent() -> bool {
        store().load_json_opt(FILE).and_then(|v| v.get("hasUserIntent").and_then(|x| x.as_bool())).unwrap_or(false)
    }

    pub fn should_run() -> bool {
        store().load_json(FILE, serde_json::json!({})).get("shouldRun").and_then(|x| x.as_bool()).unwrap_or(false)
    }

    /// `autoStartServiceIfWanted`：已跑→跳过；用户主动停过→尊重；否则视为要跑。
    pub fn should_autostart_on_open() -> bool {
        if has_user_intent() && !should_run() {
            return false;
        }
        true
    }

    fn save(should: bool, intent: bool) {
        store().save_json(FILE, &serde_json::json!({ "shouldRun": should, "hasUserIntent": intent })).ok();
    }

    /// onStartCommand/ACTION_WATCHDOG 路径：markShouldRun(true)
    pub fn mark_should_run(v: bool) {
        save(v, true);
    }

    /// stopNow()：置 false 且落意图（"用户主动关了"）
    pub fn stop_now() {
        save(false, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("tw-appcore-{}-{}", tag, n))
    }

    /// 启动序列幂等：用户主动停过 → 网关不自启；重复 startup 不重复绑端口、不 panic。
    /// （scheduler::start 第二次走"已在运行"分支；monitor::start 无状态。）
    #[tokio::test]
    async fn startup_is_idempotent_when_user_stopped() {
        init(temp_dir("startup-idempotent"));
        intent::stop_now();
        startup().await;
        assert!(!gateway_running(), "用户主动停过：startup 不得拉起网关（更不得占 8787）");
        startup().await;
        assert!(!gateway_running());
    }

    /// usage_hook 的 id 拆分与 urgent 判定（纯判据，不发网络）：
    /// 失败轮恒 urgent；成功轮按"临期/见底"跳节流。
    #[test]
    fn forward_urgent_rules() {
        init(temp_dir("urgent"));
        // 失败 → 恒 true
        assert!(forward_urgent(crate::types::BACKEND_TRAE, None, true));
        // 成功 + expires_at=0（未知）+ 无额度缓存 → false（防过度轮询）
        assert!(!forward_urgent(crate::types::BACKEND_TRAE, Some("nobody"), false));
    }

    #[test]
    fn hook_splits_platform_and_uid() {
        // Record.backend 为登记 id："trae#u1" → 平台 trae / uid u1；裸 "trae" → uid None（active 兜底）。
        // 只断言拆分逻辑不 panic 且落库/监控链路可跑通（note_forwarded 的节流在无运行时下静默跳过）。
        init(temp_dir("hook"));
        let rec = crate::gateway::Record {
            model: "m".into(),
            usage: Some(serde_json::json!({"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3})),
            elapsed_ms: 9,
            ok: true,
            err_type: None,
            backend: "trae#u1".into(),
            uid: "trae#u1".into(),
        };
        let (platform, uid) = rec.backend.split_once('#').map(|(p, u)| (p, Some(u.to_string()))).unwrap_or((rec.backend.as_str(), None));
        assert_eq!((platform, uid.as_deref()), ("trae", Some("u1")));
        let _ = usage_hook()(rec); // 不 panic（本测试进程已 init 过全局存储）
    }
}
