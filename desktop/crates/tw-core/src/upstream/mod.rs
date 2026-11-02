//! upstream 层：TraeUpstream / TraeIntlUpstream / QoderUpstream 的 Rust 对应体。
//! 权威依据：/workspace/app 的 TraeUpstream.kt、TraeIntlUpstream.kt、QoderUpstream.kt（源码优先于文档）。
//!
//! # 转发契约（Kotlin `out`/`onResult` 回调的等价物）
//!
//! 每个上游把一次聊天请求的结果转成 **OpenAI SSE 帧序列**：
//! - 每帧是**完整可写回客户端的文本**，形如 `data: {...}\n\n` 或 `event: error\ndata: ...\n\n`；
//! - 正常结束与错误结束都会发出终结帧 `data: [DONE]\n\n`（除非连接建立阶段就失败，此时无任何帧）；
//! - `onResult(usage, err)` 拆进 [`StreamOutcome`]：`usage()` 为最终用量（可 null），
//!   `err()` 为错误标签（null=成功），与 Kotlin 的 settle 第二个参数逐值一致：
//!   `http:{code}` / `solo:{code}` / `interrupted`（CN）
//!   `intl:{msg≤60}` / `intl_create:{...}` / `intl_commit:{...}` / `intl_events:{...}`（SG）
//!   `body:{Err}` / `transport:{Err}` / `cosy:{msg}` / `queued:{modelKey}` / `http:{code}` / `prepare:{Err}`（Qoder）。
//!
//! # 网关侧换号重试判定
//!
//! `StreamOutcome::saw_frame()`：是否已写出过任何帧。
//! **err 非 null 且 saw_frame 为 false** = 客户端还没看到任何内容，换账号重试是安全的；
//! 一旦 saw_frame=true，流已污染，必须直接透传给客户端（与 LocalFakeServer 的判定纪律一致）。
//!
//! # token 新鲜度
//!
//! `Upstream::stream_chat` 在**发起网络请求前**自行调用 `crate::auth::api` 保证 token 新鲜
//! （Trae：`ensure_fresh_token`，skew=3600s；Qoder：临期续 jobToken + 401 强制续一次），
//! 网关无需预刷新；`ChatRequest.token/uid` 为空时按 store 当前账号兜底。
//!
//! # 依赖缺失说明（TODO）
//!
//! Cargo.toml 无 `tokio-stream` / `async-stream`，且本 wave 禁止新增依赖：
//! 帧流用手写 `FrameStream`（内部 `tokio::sync::mpsc::Receiver` + `next()`）替代
//! `tokio_stream::wrappers::ReceiverStream`。上层若需 `futures::Stream` 语义可按此方法适配。

pub mod qoder;
pub mod trae;
pub mod trae_intl;

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 一次聊天转发的请求。`body` 是客户端 OpenAI 请求体**原样 JSON**（model/messages/tools/
/// temperature/tool_choice/...），由各上游的 `prepare_body` 改写为上游形状（对齐 Kotlin
/// `TraeUpstream.prepareBody(raw)` / `QoderUpstream.prepareBody(src)` 的原地改写语义）。
#[derive(Clone, Debug)]
pub struct ChatRequest {
    /// 客户端 OpenAI 请求体（必须为 JSON object）。
    pub body: Value,
    /// 账号 uid。空串 → 上游按各 store 的当前（active）账号兜底。
    pub uid: String,
    /// 可选的预取 token（网关若已刷新可传入）；上游校验新鲜度后可能覆盖它。
    pub token: Option<String>,
}

impl Default for ChatRequest {
    fn default() -> Self {
        Self { body: Value::Object(Default::default()), uid: String::new(), token: None }
    }
}

/// 转发结果的共享终态（消费帧的同时可读，流转结束后为最终值）。
#[derive(Default)]
pub struct StreamOutcome {
    saw_frame: AtomicBool,
    err: Mutex<Option<String>>,
    usage: Mutex<Option<Value>>,
}

impl StreamOutcome {
    pub fn saw_frame(&self) -> bool {
        self.saw_frame.load(Ordering::Acquire)
    }
    pub fn err(&self) -> Option<String> {
        self.err.lock().expect("outcome 锁中毒").clone()
    }
    pub fn usage(&self) -> Option<Value> {
        self.usage.lock().expect("outcome 锁中毒").clone()
    }
}

/// 帧流：`next().await` 逐帧取，返回 `None` 表示流终止（DONE 后通道被 pump 关闭）。
/// TODO: 依赖缺 tokio-stream，此处手写 Receiver 包装（见模块头说明）。
pub struct FrameStream {
    rx: tokio::sync::mpsc::Receiver<String>,
    outcome: Arc<StreamOutcome>,
}

impl FrameStream {
    /// 取下一帧。写出即标记 `saw_frame`（网关无需自己记）。
    pub async fn next(&mut self) -> Option<String> {
        let f = self.rx.recv().await;
        if f.is_some() {
            self.outcome.saw_frame.store(true, Ordering::Release);
        }
        f
    }
}

/// `stream_chat` 的返回：帧流 + 终态共享句柄。
pub struct StreamResult {
    pub frames: FrameStream,
    pub outcome: Arc<StreamOutcome>,
}

/// 上游实现契约（对象安全：同步签名，网络 pump 在 tokio 后台任务里跑）。
pub trait Upstream: Send + Sync {
    /// 必须在 tokio runtime 内调用（内部 `tokio::spawn` pump）。
    /// 返回立即不阻塞；错误（含 HTTP 非 2xx）以 `event: error` 帧 + `[DONE]` 呈现，
    /// 仅连接建立阶段彻底失败时表现为"无帧 + err 标错"。
    fn stream_chat(&self, req: &ChatRequest) -> StreamResult;
}

pub(crate) fn new_stream() -> (tokio::sync::mpsc::Sender<String>, StreamResult) {
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(32);
    let outcome = Arc::new(StreamOutcome::default());
    (tx, StreamResult { frames: FrameStream { rx, outcome: outcome.clone() }, outcome })
}

/// pump 结束统一落账（对齐 Kotlin `settle`：每个 pump 只调用一次）。
pub(crate) fn finish(outcome: &StreamOutcome, usage: Option<Value>, err: Option<String>) {
    *outcome.err.lock().expect("outcome 锁中毒") = err;
    *outcome.usage.lock().expect("outcome 锁中毒") = usage;
}

pub(crate) fn json_quote(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// org.json `optString(k, default)` 等价：仅**真字符串**时取值，null/缺失/其他类型→default。
/// （TraeUpstream/QoderUpstream 大量用类型安全取法防 "null" 字面量，见源码注释。）
pub(crate) fn opt_str<'a>(o: &'a Value, k: &str) -> Option<&'a str> {
    o.get(k).and_then(|v| v.as_str())
}

#[allow(dead_code)] // 契约基础设施：供各泵/后续网关层使用
pub(crate) fn opt_str_or<'a>(o: &'a Value, k: &str, d: &'a str) -> String {
    opt_str(o, k).unwrap_or(d).to_string()
}

/// 网络 pump 在后台任务里跑：spawn 统一入口（保证 trait 的对象安全）。
pub(crate) fn spawn_pump<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(fut);
}
