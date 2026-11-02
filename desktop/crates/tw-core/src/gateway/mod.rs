//! 本地 OpenAI 兼容网关（`LocalFakeServer.kt` + `Gateway.kt` 的桌面移植）。
//! 唯一依据：`.monkeycode/analysis/gateway-server.md` + `app/.../LocalFakeServer.kt` 源码逐字节比对。
//!
//! 模块划分：
//! - [`http`]：手写最小 HTTP/1.1 解析与逐字节响应写出（tw-core 无 hyper 直接依赖，见 http 模块头注释）
//! - [`sse`]：SSE 帧工具 / 聚合 / 上游错误提取与人性化 / 换号-续期判定（纯函数）
//! - [`models_fallback`]：`/v1/models` 内置 21 条兜底快照
//!
//! 生命周期：`assemble`(装配) → `spawn`(绑 0.0.0.0:8787 起 accept) → `shutdown`(停听 + 有界等待活动连接收尾)。

pub mod http;
pub mod models_fallback;
pub mod sse;

mod backend_adapter;
pub use backend_adapter::build_backends;

use futures_util::StreamExt;
use http::{IO_TIMEOUT, MAX_BODY_BYTES};
use parking_lot::Mutex;
use serde_json::{json, Value};
use sse::{CHOICES_MARKER, DONE_FRAME};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::{timeout, Instant};

/// 监听端口（§7.1 `PORT=8787`；8788 太常见 / 84772 超上限 / 4772 特权端口，历史落到 8787）。
pub const PORT: u16 = 8787;

/// Qoder 后端无凭证的 502 默认文案（§2.2 逐字；**刻意不回假回复**——否则"根本没转发"伪装成转发成功）。
pub const DEFAULT_UPSTREAM_UNAVAILABLE_MSG: &str = "Qoder 账号不可用：jobToken 为空或已失效。\
请回 App 重新登录 Qoder，或在「聊天转发后端」里切到 Trae。";

// ======================================================================
// 帧流与后端抽象
// ======================================================================

/// 上游转发的一帧。后端实现只需产出这两类：`Chunk` = 交给客户端的**完整帧字符串**
/// （逐字节契约：`data: …\n\n` / `event: error\ndata: …\n\n` 等，网关原样 UTF-8 写出并 flush），
/// `Done` = 等价的 Kotlin `done(usage, err)` 收尾回调（**必须是流的最后一项**）。
#[derive(Debug, Clone)]
pub enum Frame {
    /// 一帧完整 SSE 输出（自带结尾 `\n\n`）。
    Chunk(String),
    /// 上游结束：`usage` = token 用量 JSON（可为 None），`err` = 错误种类字符串
    /// （约定形态：`http:<status>` / `queued:<modelKey>` / `<前缀>:<码|exception>` / `interrupted` 等，
    /// 供 [`sse::auth_related_error`] / [`sse::retryable_upstream_error`] 解析——语义表见 analysis §3.4）。
    Done { usage: Option<Value>, err: Option<String> },
}

/// 帧流。`Item = Frame`；`Done` 之后不再产出（网关在 `Done` 处停止轮询）。
/// 构建可用 `futures_util::stream::iter(vec![…])` 或 `tokio::sync::mpsc` 适配。
pub type ChatStream = futures_util::stream::BoxStream<'static, Frame>;

/// 网关错误（决定对客户端的 4 种错误形状 + 重试语义）。
#[derive(Debug, Clone)]
pub enum GatewayError {
    /// 该后端当前没有可用凭证 → Trae 语义：`200 fakeCompletion()`（§3.1，**不报错**）。
    NoCredential,
    /// 后端明确不可用 → `502 upstream_unavailable`（§2.2 Qoder 语义，msg=None 时用默认文案）。
    UpstreamUnavailable(Option<String>),
    /// 启动即失败（cheap 校验级），载荷是错误种类字符串 → 进入 finish 收尾（stream: 合成 error 帧；
    /// 非流式: 502 upstream_error + humanize）。**传输类失败应优先在流内以 emitError 帧表达**，
    /// 不要走这个变体（对应文档 §3.3 ⚠️ 修复要求：连接期异常转错误事件再 settle）。
    Upstream(String),
    /// 其它内部错误（等价 `Upstream`，载荷字符串仅入日志与 err_type）。
    Io(String),
}

impl GatewayError {
    /// 错误种类字符串（Record.err_type / 换号判定输入）。
    pub fn kind_str(&self) -> String {
        match self {
            GatewayError::NoCredential => "no_credential".into(),
            GatewayError::UpstreamUnavailable(m) => m.clone().unwrap_or_else(|| "upstream_unavailable".into()),
            GatewayError::Upstream(e) => e.clone(),
            GatewayError::Io(e) => e.clone(),
        }
    }
}

pub type GatewayResult<T> = Result<T, GatewayError>;

/// **FROZEN（冻结契约，上游/auth/models 层并行开发的对接面）** —— 本地网关的转发后端。
///
/// 语义映射（analysis §4 / §3，Kotlin `credProvider`+`backendForModelProvider`+`runUpstream` 的合体）：
/// - `list_models`：等价 `modelsProvider` 的单后端贡献（OpenAI `{"id","object","created",…}` 对象数组）。
///   返回空 = 该后端无产出（Trae 未刷新时的合法空态，**不是**错误）。
/// - `chat_stream`：等价「每次转发前取当下有效凭证 + runUpstream」。**必须是廉价的同步校验**
///   （凭证形态级：缺凭证返回 [`GatewayError::NoCredential`]、账号明确不可用返回
///   [`GatewayError::UpstreamUnavailable`]）；真正的上游网络请求必须推迟到流**首次被轮询**时才发生。
///   若返回 `Ok(stream)`，流内以 `Frame::Done{usage, err}` 收尾表达全部失败形态
///   （含 §3.5 的 emitError 帧），网关据 err+got_choices 决定换号。
/// - `api_key_id`：装配时登记的 (backend id, Arc<dyn ChatBackend>) 的 id 字符串。集成方若按账号拆分登记
///   （同一实现多账号 → 多个 id，如 `"trae#uid1"`），网关的 failover 就是沿登记序换 id；
///   否则 id 即后端名，账号级换号由后端内部完成（err 非鉴权类时网关仍会按规则尝试下一登记项）。
///
/// **换号重试安全标记（got_choices_frame 语义，形态自定=网关侧闸门，契约如下）**：
/// 网关不信任后端自报；每轮从流首帧起把帧暂存 pending，直到某帧字符串包含 `"choices"`
/// （含引号，§7.1 成功闸门特征串）才认定"已开始输出正文"并实时转发。`Done{err}` 出现在
/// 闸门打开**之前**才允许换下一候选（对齐 §3.3 streamWithFailover 逐条规则，总换号 ≤2 次 +
/// 同账号续期 1 次）；闸门打开后的任何失败都**不**换号（半途回答无法撤回）。
pub trait ChatBackend: Send + Sync + 'static {
    /// 本后端贡献的 `/v1/models` data 数组（空 = 无产出）。
    fn list_models(&self) -> Vec<Value>;

    /// 发起一次转发。见 trait 顶部冻结注释。
    fn chat_stream(&self, body: Value, api_key_id: &str) -> GatewayResult<ChatStream>;
}

/// 一次转发的用量记录（等价 Kotlin `usageRecorder` 的 7 个位置参数；
/// **usage 层由并行开发，本类型是 gateway 侧的对外钩子数据**，集成方负责映射到自己的存储）。
#[derive(Debug, Clone)]
pub struct Record {
    /// 请求的模型名（客户端原文，缺省空串）。
    pub model: String,
    /// 上游 settle 的 usage JSON（`prompt_tokens/completion_tokens/total_tokens` 等，可无）。
    pub usage: Option<Value>,
    /// 本轮上游尝试耗时（毫秒，等价 `now - t0` 的 Kotlin 语义：最后一轮）。
    pub elapsed_ms: i64,
    /// 本轮是否成功（`err == null`）。
    pub ok: bool,
    /// 错误种类字符串（成功时 None）。
    pub err_type: Option<String>,
    /// 命中的后端 id（等价 Kotlin backend 参数 + usedUid；按账号登记拆分时二者可区分）。
    pub backend: String,
    /// 本次实际使用的 api_key_id（等价 `usedUid()`，缺省空串语义由集成方处理）。
    pub uid: String,
}

/// `usageRecorder` 回调类型（§4）：每次转发循环收尾调用（成功/彻底失败都调，含换号后的最终结果）。
pub type UsageHook = Arc<dyn Fn(Record) + Send + Sync>;

// ======================================================================
// Server
// ======================================================================

#[derive(Clone)]
struct Cand {
    id: String,
    backend: Arc<dyn ChatBackend>,
}

struct Inner {
    /// `None` = 用 `fingerprint::api_key()` 派生（§5.3：`sk-` + sha256(install_id|机器hash) 前 32 hex）。
    /// 惰性读取：装配期不强制 storage 初始化。
    api_key_override: Mutex<Option<String>>,
    listen_ip: String,
    listen_port: u16,
    backends: Vec<(String, Arc<dyn ChatBackend>)>,
    on_usage: Mutex<Option<UsageHook>>,
    running: AtomicBool,
    bound_port: AtomicU16,
    shutdown_notify: Notify,
    active_conns: AtomicUsize,
    conns_idle: Notify,
}

impl Inner {
    /// 当前生效的本地 API Key（override 优先；否则派生 key，进程内 OnceLock 缓存）。
    fn api_key(&self) -> String {
        if let Some(k) = self.api_key_override.lock().as_ref() {
            return k.clone();
        }
        crate::fingerprint::api_key()
    }
}

/// 本地网关服务句柄（Clone 共享同一实例；`Gateway.server` 单例语义由调用方持有）。
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

impl Server {
    /// appcore 兼容零参入口 = `assemble_with(vec![])`（无后端：models 走内置兜底、chat 走 fakeCompletion）。
    pub fn assemble() -> Result<Self, String> {
        Self::assemble_with(Vec::new())
    }

    /// 装配（§5.4 Gateway.start 接线清单）：登记 (backend id, 实现) 列表。
    /// 列表顺序 = failover 的兜底候选序（模型名命中的后端会被前置）。
    pub fn assemble_with(backends: Vec<(String, Arc<dyn ChatBackend>)>) -> Result<Self, String> {
        Ok(Self {
            inner: Arc::new(Inner {
                api_key_override: Mutex::new(None),
                listen_ip: "0.0.0.0".into(),
                listen_port: PORT,
                backends,
                on_usage: Mutex::new(None),
                running: AtomicBool::new(false),
                bound_port: AtomicU16::new(0),
                shutdown_notify: Notify::new(),
                active_conns: AtomicUsize::new(0),
                conns_idle: Notify::new(),
            }),
        })
    }

    /// 覆写本地 API Key（测试/特殊部署；生产默认恒为 `fingerprint::api_key()`）。
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        *Arc::get_mut(&mut self.inner).expect("独占装配期").api_key_override.lock() = Some(key.into());
        self
    }

    /// 覆写监听地址/端口（port=0 由内核分配，仅测试用；生产用默认 0.0.0.0:8787）。
    pub fn with_listen(mut self, ip: impl Into<String>, port: u16) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("独占装配期");
        inner.listen_ip = ip.into();
        inner.listen_port = port;
        self
    }

    /// 安装用量回调钩子（等价 Kotlin `LocalFakeServer.usageRecorder` 注入）。
    pub fn set_on_usage(&self, hook: UsageHook) {
        *self.inner.on_usage.lock() = Some(hook);
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// 实际绑定端口（spawn 成功后可读；port=0 场景取回内核分配值）。
    pub fn bound_port(&self) -> u16 {
        self.inner.bound_port.load(Ordering::SeqCst)
    }

    /// 启动（幂等：已在跑直接 return——重复 start 会 EADDRINUSE 崩掉，§5.4）。
    /// 绑 `listen_ip:listen_port`，backlog 由内核控制（与 §1.1 的 50 存在偏差，见模块报告）。
    pub async fn spawn(self) -> Result<(), String> {
        let inner = self.inner.clone();
        if inner.running.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Ok(()); // 已在运行
        }
        let addr = format!("{}:{}", inner.listen_ip, inner.listen_port)
            .parse::<SocketAddr>()
            .map_err(|e| format!("监听地址非法: {}", e))?;
        let listener = match TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    inner.running.store(false, Ordering::SeqCst);
                    return Err(format!("端口 {} 已被占用（EADDRINUSE）", addr));
                }
                inner.running.store(false, Ordering::SeqCst);
                return Err(format!("绑定 {} 失败: {}", addr, e));
            }
        };
        if let Ok(la) = listener.local_addr() {
            inner.bound_port.store(la.port(), Ordering::SeqCst);
        }
        println!("网关已启动，端口 {}", inner.bound_port.load(Ordering::SeqCst));
        tokio::spawn(async move { accept_loop(inner, listener).await });
        Ok(())
    }

    /// 停止（§1.1 stop：置 false + 关 listener，不无限 join；本实现在 soTimeout 同值 15s 内
    /// 有界等待活动连接收尾，等价"存量连接自然结束"且不阻塞调用方到超时之外）。幂等。
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        if !inner.running.swap(false, Ordering::SeqCst) {
            return; // 已是停止态（幂等）
        }
        inner.shutdown_notify.notify_waiters();
        // 有界等待活动连接排空
        loop {
            let wait = inner.conns_idle.notified();
            if inner.active_conns.load(Ordering::SeqCst) == 0 {
                break;
            }
            if timeout(IO_TIMEOUT, wait).await.is_err() {
                break; // Kotlin 不 join；有界等待后放行
            }
        }
    }
}

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    loop {
        let stop = inner.shutdown_notify.notified(); // 预登记，唤醒不丢
        tokio::select! {
            _ = stop => break,                       // running=false：drop listener → 停止监听
            acc = listener.accept() => match acc {
                Ok((sock, _addr)) => {
                    // tcpNoDelay：SSE 小包必须关 Nagle（§1.2，Rust 移植必须保留，回环无副作用）
                    let _ = sock.set_nodelay(true);
                    inner.active_conns.fetch_add(1, Ordering::SeqCst);
                    let inner2 = inner.clone();
                    tokio::spawn(async move {
                        handle_conn(inner2.clone(), sock).await;
                        if inner2.active_conns.fetch_sub(1, Ordering::SeqCst) == 1 {
                            inner2.conns_idle.notify_waiters();
                        }
                    });
                }
                Err(e) => {
                    if !inner.running.load(Ordering::SeqCst) {
                        break;
                    }
                    eprintln!("LocalFakeServer: accept 失败 {}", e); // 原实现：连接级异常只记日志不影响进程
                }
            }
        }
    }
}

/// 单连接：一个连接只处理一个请求（keep-alive 全部断开，§1.6）。
async fn handle_conn(inner: Arc<Inner>, sock: TcpStream) {
    let (r, mut w) = sock.into_split();
    let mut rd = BufReader::new(r);
    // handle 外层兜底：任何未捕获异常只记 WARN 并关 socket（§2.3）
    let head = match http::read_request_head(&mut rd).await {
        Ok(Some(h)) => h,
        Ok(None) => return, // 静默关闭（请求行 EOF / 不足 2 段）
        Err(e) => {
            eprintln!("LocalFakeServer: 连接处理失败: {}", e);
            return;
        }
    };
    process(inner, &mut rd, &mut w, head).await;
}

async fn process<R: tokio::io::AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    inner: Arc<Inner>,
    rd: &mut BufReader<R>,
    w: &mut W,
    head: http::RequestHead,
) {
    // 路径归一化（§1.4）
    let query = http::normalize_path(&head.path);
    // 访问日志（§1.5：只记去查询串的路径，禁止记录查询串——可能含 key）
    println!(
        "→ {} {} auth={} len={} chunked={}",
        head.method,
        query,
        head.auth.is_some(),
        head.content_length,
        head.chunked
    );

    // 路由表（§2：匹配顺序 = Kotlin when 分支顺序，第一个命中生效）
    if head.method == "OPTIONS" {
        // CORS 预检最先答、任何路径都答、不看鉴权（§1.6/§2 分支1）
        let _ = http::write_raw(w, 204, "text/plain", b"").await;
        return;
    }
    if query == "/healthz" && head.method == "GET" {
        // 分支2：200 text/plain 裸 `ok`（4 字节，无鉴权）
        let _ = http::write_raw(w, 200, "text/plain", b"ok").await;
        return;
    }
    if query == "/v1/models" || query == "/models" {
        // 分支3：不限制方法；免鉴权（无敏感数据）
        let data = models_json(&inner);
        println!("← 200 {} （{} 个模型）", query, data.as_array().map(|a| a.len()).unwrap_or(0));
        let _ = http::write_json(w, 200, &json!({"object": "list", "data": data})).await;
        return;
    }
    if (query == "/v1/chat/completions" || query == "/chat/completions") && head.method == "POST" {
        // 分支4：鉴权在 chat handler 内（§2 表）；先鉴权后读体（Kotlin 顺序）
        if !authorized(&inner.api_key(), head.auth.as_deref()) {
            let _ = http::write_json(
                w,
                401,
                &http::openai_error("invalid_api_key", "missing or invalid local API key"),
            )
            .await;
            return;
        }
        // 两种传体都要认：Content-Length 或 chunked（Chatbox 实测发 chunked，§1.8）
        let body = if head.chunked {
            match http::read_chunked_body(rd).await {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
                    let _ = http::write_json(w, 413, &body_too_large_error()).await;
                    return;
                }
                Err(_) => return, // 读超时/断连：静默关闭（对齐 Kotlin 冒泡关 socket）
            }
        } else if head.content_length > (MAX_BODY_BYTES as i64) {
            let _ = http::write_json(w, 413, &body_too_large_error()).await;
            return;
        } else {
            match http::read_body_exact(rd, head.content_length).await {
                Ok(b) => b,
                Err(_) => {
                    // 超限或读失败：明确回 413，不能冒泡成"连接被重置"（§1.8）
                    let _ = http::write_json(w, 413, &body_too_large_error()).await;
                    return;
                }
            }
        };
        serve_chat(w, &inner, head.auth.as_deref(), &body).await;
        return;
    }
    // 分支5：未知路径 404 + WARN（§2；不记查询串——可能含 key）
    eprintln!("LocalFakeServer: ← 404 未知端点: {} {}", head.method, query);
    let _ = http::write_json(
        w,
        404,
        &http::openai_error("not_found", &format!("unknown endpoint: {}", query)),
    )
    .await;
}

fn body_too_large_error() -> Value {
    http::openai_error(
        "invalid_request_error",
        &format!("请求体过大（上限 {} MB）", MAX_BODY_BYTES / 1024 / 1024),
    )
}

/// `/v1/models` 数据源（§2 分支3 + 任务冻结规则）：
/// - 无登记后端 → 内置 21 条兜底快照（[`models_fallback`]）
/// - 有登记后端 → 按登记序合并各 `list_models()`，按 `id` 去重先到先得；
///   全部为空时返回空数组（Trae 未刷新的合法空态，对齐 `ModelsRepo.current()`，**不**触发兜底）
fn models_json(inner: &Inner) -> Value {
    if inner.backends.is_empty() {
        return Value::Array(models_fallback::fallback_models());
    }
    let mut out: Vec<Value> = Vec::new();
    for (_id, b) in &inner.backends {
        for m in b.list_models() {
            let mid = m.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if mid.is_empty() || !out.iter().any(|o| o.get("id").and_then(|x| x.as_str()) == Some(mid.as_str())) {
                out.push(m);
            }
        }
    }
    Value::Array(out)
}

/// 本地鉴权（§2.1 逐条：key 空→放行；`Bearer ` 前缀大小写无关；余下 trim 精确相等）。
pub fn authorized(api_key: &str, auth: Option<&str>) -> bool {
    if api_key.is_empty() {
        return true; // 实践中派生 key 恒非空
    }
    match auth {
        None => false,
        Some(a) => {
            let bytes = a.as_bytes();
            bytes.len() >= 7
                && a[..7].eq_ignore_ascii_case("Bearer ")
                && a[7..].trim() == api_key
        }
    }
}

/// 固定假回复（§3.1 `fakeCompletion()` 逐字节移植：结构真实、内容假）。
pub fn fake_completion() -> Value {
    json!({
        "id": "chatcmpl-fake-local",
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant",
                "content": "这是本地假服务返回的测试响应。阶段2第2步已跑通：本地 OpenAI 兼容接口可用，下一步再接真实 TraeWork 上游转发。"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

/// 按模型选候选（§3.1/§3.2 适配裁决：auth/models 层并行中，网关侧以登记序 + 模型表命中为路由）：
/// `list_models` 声明了该模型名（id/name/key/aliases，忽略大小写）的后端置前，其余按登记序；
/// 空模型名 → 全按登记序。显式后端选择（`config.gateway_backend` 三档）由装配方通过
/// 登记顺序/单后端登记表达（集成钩子，见模块报告）。
fn pick_candidates(inner: &Arc<Inner>, model: &str) -> Vec<Cand> {
    let m = model.trim().to_lowercase();
    let cand_list: Vec<Cand> = inner
        .backends
        .iter()
        .map(|(id, b)| Cand { id: id.clone(), backend: b.clone() })
        .collect();
    if m.is_empty() {
        return cand_list;
    }
    let hit = |c: &Cand| -> bool {
        c.backend.list_models().iter().any(|o| {
            let eq = |k: &str| o.get(k).and_then(|x| x.as_str()).map(|s| s.to_lowercase() == m).unwrap_or(false);
            eq("id") || eq("name") || eq("key")
                || o.get("aliases")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().any(|x| x.as_str().map(|s| s.to_lowercase() == m).unwrap_or(false)))
                    .unwrap_or(false)
        })
    };
    let mut hit_side: Vec<Cand> = Vec::new();
    let mut miss_side: Vec<Cand> = Vec::new();
    for c in cand_list {
        if hit(&c) {
            hit_side.push(c);
        } else {
            miss_side.push(c);
        }
    }
    hit_side.extend(miss_side);
    hit_side
}

/// chat 主干（§3.1）：鉴权→宽松取 model→选候选→严格 parse（非法 JSON→400）→stream/non-stream→失败回滚等。
/// 注意顺序对齐 Kotlin：**路由阶段 body 非法 JSON 不致命**（model 按 ""），400 只在进入转发后出现；
/// 且 Kotlin 的凭证预检先于 400——冻结契约下 chat_stream 需要 Value，无法在无凭证候选上做预检，
/// 本实现将 400 提前（客户端错误优先级更高，见模块报告「不一致裁决」）。
async fn serve_chat<W: AsyncWrite + Unpin>(w: &mut W, inner: &Arc<Inner>, auth: Option<&str>, body: &str) {
    // 鉴权在 chat 入口最先（§2 表分支4：authorized 失败 → 401，先于读体/路由）
    if !authorized(&inner.api_key(), auth) {
        let _ = http::write_json(
            w,
            401,
            &http::openai_error("invalid_api_key", "missing or invalid local API key"),
        )
        .await;
        return;
    }
    // 严格 parse 先行（parseBodyOr400，§2.2）：空体/非法 JSON 是信任边界，必须回 400，绝不冒泡（§2.3）。
    // 顺序对齐 Kotlin：读体→解析→400，再路由选号/无凭证 fake。
    let strict: Value = match serde_json::from_str::<Value>(body) {
        Ok(v) if v.is_object() => v,
        _ => {
            let _ = http::write_json(w, 400, &http::openai_error("invalid_request_error", "请求体不是合法 JSON")).await;
            return;
        }
    };
    // 路由用 model（缺失按空串，不致命，§2.3）
    let model_lenient = strict.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let cands = pick_candidates(inner, &model_lenient);
    if cands.is_empty() {
        // 无登记后端 = 无任何账号 → Trae 无凭证语义：200 fakeCompletion（§3.1 fresh==null 分支）
        let _ = http::write_json(w, 200, &fake_completion()).await;
        return;
    }
    // wants_stream 必须先行读取（后端 prepareBody 会原地改 stream——§3.9 顺序契约在此体现）
    let wants_stream = strict.get("stream").and_then(|x| x.as_bool()).unwrap_or(false);
    // 聚合/响应里的模型名用客户端原文（Kotlin trae 分支的默认名回落在 upstream 层，网关不掺假）。
    let model = strict.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string();
    stream_with_failover(w, inner, &strict, wants_stream, model, cands).await;
}

/// **§3.3 `streamWithFailover` 的逐规则移植**（控制流全在这里，后端只出帧）：
/// 1. wants_stream → 先写流式响应头（仅一轮之前写一次），非流式什么都不写、`collected` 攒帧
/// 2. 成功闸门：帧先入 pending，出现含 `"choices"` 的帧才实时转发/入 collected
/// 3. 同账号续期重试（仅一次 `did_refresh`）：`!streaming && err && auth_related_error` → 同候选重开一轮
/// 4. 换号重试：`!refreshed && !streaming && err && attempt<=1 && retryable` → 沿候选序换下一个
///    （**总换号 ≤2 次**，attempt 只在换号时 +1；续期不消耗 attempt）
///    next 候选启动即 Err = Kotlin `takeIf` 过滤不通过 → 以当轮错误收尾
/// 5. 不重试 → 收尾：flush pending → `on_usage` 回调（成功/彻底失败都记）→
///    非流式：`extract_upstream_error` 优先，否则 humanize(err) → 502；无错 → 200 aggregate
/// 6. 修复项（§3.3 ⚠️ / §8.3 #4，对外形状保持）：流式但闸门未开且无任何 pending 帧时，
///    合成 `event: error` + `[DONE]` 帧（替代 Kotlin 的零字节垃圾行为）
///
/// Kotlin `failoverRollback`（乐观切号失败回滚）不在网关：本架构换号不改"当前激活账号"持久态
/// （由 auth 层裁决），回滚语义由后端 id 选型承载，见模块报告。
async fn stream_with_failover<W: AsyncWrite + Unpin>(
    w: &mut W,
    inner: &Arc<Inner>,
    strict: &Value,
    wants_stream: bool,
    model: String,
    cands: Vec<Cand>,
) {
    let mut ci: usize = 0;
    let mut attempt: usize = 0; // 换号次数（Kotlin attempt：只在换号时++）
    let mut did_refresh = false;
    let mut header_written = false;
    let mut dead = false; // 客户端提前断开：吞写失败继续（§2.3 Broken pipe 语义）
    let mut prefetched: Option<ChatStream> = None;

    loop {
        // —— 启动本轮 ——
        let mut stream = match prefetched.take() {
            Some(s) => s,
            None => match cands[ci].backend.chat_stream(strict.clone(), &cands[ci].id) {
                Ok(s) => s,
                Err(e) => {
                    match e {
                        GatewayError::NoCredential => {
                            let _ = http::write_json(w, 200, &fake_completion()).await;
                            return;
                        }
                        GatewayError::UpstreamUnavailable(m) => {
                            let msg = m.unwrap_or_else(|| DEFAULT_UPSTREAM_UNAVAILABLE_MSG.to_string());
                            let _ = http::write_json(w, 502, &http::openai_error("upstream_unavailable", &msg)).await;
                            return;
                        }
                        other => {
                            // 启动即失败（§3.3 ⚠️ 修复路径）：流式先补头，再以零帧虚拟轮 finalize →
                            // 合成错误帧（流式）或 502 + humanize（非流式），并记录 usage。
                            if wants_stream && !header_written {
                                let _ = http::write_sse_header(w).await;
                            }
                            finalize(
                                w, inner, &mut dead, &model, &cands[ci],
                                false, Vec::new(), (!wants_stream).then(Vec::new),
                                None, Some(other.kind_str()), Instant::now(),
                            )
                            .await;
                            return;
                        }
                    }
                }
            },
        };

        if wants_stream && !header_written {
            // §3.3 步骤1：流式头在任何聚合/错误 JSON 之前写出
            let _ = http::write_sse_header(w).await;
            header_written = true;
        }

        // —— 跑一轮上游 ——
        let t0 = Instant::now();
        let mut pending: Vec<String> = Vec::new();
        let mut streaming = false; // 已出现含 choices 的帧 = 成功闸门打开
        let mut usage: Option<Value> = None;
        let mut err: Option<String> = None;
        let mut got_done = false;
        let mut collected: Option<Vec<String>> = (!wants_stream).then(Vec::new);

        while let Some(frame) = stream.next().await {
            match frame {
                Frame::Chunk(s) => {
                    if streaming {
                        emit_chunk(w, &mut dead, &mut collected, s).await;
                    } else {
                        pending.push(s.clone());
                        if s.contains(CHOICES_MARKER) {
                            streaming = true;
                            for p in pending.drain(..) {
                                emit_chunk(w, &mut dead, &mut collected, p).await;
                            }
                        }
                    }
                }
                Frame::Done { usage: u, err: e } => {
                    usage = u;
                    if e.is_some() {
                        err = e;
                    }
                    got_done = true;
                    break; // Done 为最后一项，其余忽略
                }
            }
        }
        if !got_done && err.is_none() {
            // 流被 drop 且未 settle → 上游提前 EOF（Trae 语义 `interrupted`，§3.5）
            err = Some("interrupted".to_string());
        }

        // —— 同账号续期重试（仅一次）——
        let refreshed = !streaming && err.is_some() && !did_refresh && sse::auth_related_error(err.as_deref().unwrap_or(""));
        if refreshed {
            did_refresh = true;
            println!("同账号续期后重试（原错误: {:?}）", err);
            continue; // 同 ci 重开一轮（prefetched=None → 再次 chat_stream；凭证续期由后端完成）
        }

        // —— 换号重试（attempt<=1 ⇒ 最多 2 次换号）——
        let want_switch = !refreshed
            && !streaming
            && err.is_some()
            && attempt <= 1
            && ci + 1 < cands.len()
            && sse::retryable_upstream_error(err.as_deref().unwrap_or(""));
        if want_switch {
            // 预启动下一候选：Kotlin tryFailover + takeIf 过滤（不可用则"没换成"，以当前错误收尾）
            let next = cands[ci + 1].backend.chat_stream(strict.clone(), &cands[ci + 1].id);
            match next {
                Ok(s) => {
                    println!("换号后重试（原错误: {:?}）", err);
                    ci += 1;
                    attempt += 1;
                    prefetched = Some(s);
                    continue; // 本轮 pending（错误帧）废弃——新轮覆盖，对齐 Kotlin
                }
                Err(_) => {
                    // 无可换账号：落入下方 finalize，用当前错误收尾
                }
            }
        }

        // —— 收尾 ——
        finalize(w, inner, &mut dead, &model, &cands[ci], streaming, pending, collected, usage, err, t0).await;
        return;
    }

    async fn emit_chunk<W: AsyncWrite + Unpin>(
        w: &mut W,
        dead: &mut bool,
        collected: &mut Option<Vec<String>>,
        s: String,
    ) {
        match collected {
            Some(c) => c.push(s), // 非流式：只攒帧，不写
            None => {
                if *dead {
                    return;
                }
                // §3.3 步骤2：流式逐帧写出并 flush；IOException 吞掉（客户端提前断开仍继续读上游并记录用量）
                let r = timeout(IO_TIMEOUT, async {
                    w.write_all(s.as_bytes()).await?;
                    w.flush().await
                })
                .await;
                if r.is_err() || r.unwrap().is_err() {
                    *dead = true;
                }
            }
        }
    }

    /// finish（§3.3 步骤3 末段）：flush 剩余帧 → usage 回调 → 非流式聚合/502；流式修复补合成错误帧。
    async fn finalize<W: AsyncWrite + Unpin>(
        w: &mut W,
        inner: &Arc<Inner>,
        dead: &mut bool,
        model: &str,
        cand: &Cand,
        streaming: bool,
        mut pending: Vec<String>,
        mut collected: Option<Vec<String>>,
        usage: Option<Value>,
        err: Option<String>,
        t0: Instant,
    ) {
        let had_pending_frames = !pending.is_empty();
        for p in pending.drain(..) {
            emit_chunk(w, dead, &mut collected, p).await;
        }
        // usageRecorder：成功/彻底失败都调（含换号/续期后的最终结果），elapsed=本轮 t0（Kotlin 语义）
        let rec = Record {
            model: model.to_string(),
            usage: usage.clone(),
            elapsed_ms: t0.elapsed().as_millis() as i64,
            ok: err.is_none(),
            err_type: err.clone(),
            backend: cand.id.clone(),
            uid: cand.id.clone(),
        };
        if let Some(hook) = inner.on_usage.lock().as_ref().cloned() {
            hook(rec);
        }
        match collected {
            Some(coll) => {
                // 非流式（§3.3/§3.7）：**先判错**再聚合——否则 error 帧会被聚合成"200+空回复"
                let up_err = sse::extract_upstream_error(&coll);
                if err.is_some() || up_err.is_some() {
                    // §3.7：message = humanize( extract 优先，否则错误种类 )
                    let raw = up_err.unwrap_or_else(|| err.clone().unwrap_or_default());
                    let msg = sse::humanize_upstream_error(&raw);
                    eprintln!("LocalFakeServer: 非流式上游失败: {}", msg);
                    let _ = http::write_json(w, 502, &http::openai_error("upstream_error", &msg)).await;
                } else {
                    let mut agg = sse::aggregate_chunks(&coll, model);
                    // 帧内未带 usage 时补挂 Done 的 usage（网关契约：usage 以 Done 为准）
                    if agg.get("usage").is_none() {
                        if let Some(u) = usage {
                            agg["usage"] = u;
                        }
                    }
                    let _ = http::write_json(w, 200, &agg).await;
                }
            }
            None => {
                // 流式：闸门未开且本轮没有任何帧 → 修复语义（§3.3 ⚠️）：合成错误帧 + [DONE]，
                // 客户端不会收到"200 头 + 零字节"。（有帧时 error 帧已由后端产出并 flush，见上。）
                if !streaming && err.is_some() && !had_pending_frames {
                    let msg = sse::humanize_upstream_error(err.as_deref().unwrap_or(""));
                    let frame = sse::error_event_frame(&msg);
                    emit_chunk(w, dead, &mut None, frame).await;
                    emit_chunk(w, dead, &mut None, DONE_FRAME.to_string()).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
