// 逐行移植 LocalFakeServer.kt + Gateway.kt —— 本地 OpenAI 兼容网关。
//
// Android 版手写 HTTP（ServerSocket + 逐字节解析）；桌面版用 axum 0.8 承担 HTTP 框架层
//（请求行/头解析、Content-Length/chunked body 读取由 hyper 完成，等价于自检里钉住的那两个坑）。
// 业务层行为逐行对齐：路径归一化、OPTIONS 预检最先答、/models 双路径别名、
// Bearer 鉴权、streamWithFailover 控制流（同账号续期重试 → 换号 ≤2 次 → 回滚）、
// 非流式聚合、上游错误翻译、假回复、用量记录 + 转发后积分监测节流。
use crate::state::AppState;
use crate::types::{QoderCred, TraeCred};
use crate::{log_i, log_w};
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::Stream;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;

pub const PORT: u16 = 8787;

const TAG: &str = "LocalFakeServer";
const GATEWAY_TAG: &str = "Gateway";

/// 单请求体上限。**不是可调项，是安全边界**（对齐 Android MAX_BODY_BYTES）。
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

const SIXTY_MINUTES_SEC: i64 = 3600;
/// ⚠️ Qoder 的续期阈值单位是**毫秒**（validJobTokenFor(..., skewMillis)），
/// 和 Trae 的 skewSec 不是一个单位 —— Android 版在这里踩过 3.6 秒的坑。
const SIXTY_MINUTES_MS: i64 = 60 * 60 * 1000;

pub struct ServerHandle {
    pub shutdown: tokio::sync::watch::Sender<bool>,
}

// ------------------------------------------------------------------
// 本地 API Key（Gateway.apiKey）
// ------------------------------------------------------------------

/// 本地 API Key（与上游 access_token 是两套）。
///
/// Android 版由「APK 签名指纹 + Widevine 设备 ID」派生；桌面版等价物是**机器指纹**
/// （/etc/machine-id 等，见 AppState::machine_fingerprint）—— 同一台机器永远算出同一个 key，
/// 落库里不存 key，应用数据被清多少次都自愈。
pub fn api_key(app: &AppState) -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let fp = app.machine_fingerprint();
            let digest = Sha256::digest(fp.as_bytes());
            let hexs = hex::encode(digest);
            format!("sk-{}", &hexs[..32])
        })
        .clone()
}

// ------------------------------------------------------------------
// 网关启停（Gateway.start / stop / isRunning）
// ------------------------------------------------------------------

/// 幂等启动：已在跑就直接返回。重复 start 会去抢同一个端口 → `EADDRINUSE`，所以这道闸必须留着。
pub async fn start(app: Arc<AppState>) -> Result<(), String> {
    let (placeholder_tx, _) = tokio::sync::watch::channel(false);
    {
        let mut g = lock(&app.server);
        if g.is_some() {
            return Ok(());
        }
        // 占位：把「正在启动」先写进状态，避免并发 start 双重绑端口；绑定失败再回滚。
        *g = Some(ServerHandle { shutdown: placeholder_tx });
    }
    let r = start_inner(&app).await;
    if let Err(e) = r {
        if let Ok(mut g) = app.server.lock() {
            *g = None;
        }
        return Err(e);
    }
    Ok(())
}

async fn start_inner(app: &Arc<AppState>) -> Result<(), String> {
    let router = Router::new().fallback(dispatch).with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", PORT))
        .await
        .map_err(|e| format!("网关启动失败: {}", e))?;
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.changed().await;
            })
            .await;
    });
    if let Ok(mut g) = app.server.lock() {
        *g = Some(ServerHandle { shutdown: tx });
    }
    log_i!(GATEWAY_TAG, "网关已启动，端口 {}", PORT);
    Ok(())
}

/// 幂等停止：没启动时调用也安全（用户可能没开就退出）。
pub fn stop(app: &AppState) {
    crate::aux::credit_monitor::stop();
    if let Ok(mut g) = app.server.lock() {
        if let Some(h) = g.take() {
            let _ = h.shutdown.send(true);
        }
    }
    log_i!(GATEWAY_TAG, "网关已停止");
}

pub fn is_running(app: &AppState) -> bool {
    app.server.lock().map(|g| g.is_some()).unwrap_or(false)
}

// ------------------------------------------------------------------
// HTTP 分发（LocalFakeServer.handle）—— 全部进 fallback，逐分支对齐 Kotlin 的 when
// ------------------------------------------------------------------

async fn dispatch(State(app): State<Arc<AppState>>, mut req: Request) -> Response {
    // 路径归一化：客户端拼 baseURL 的方式五花八门 ——
    //   `http://host/v1/` + `/models`  → `/v1//models`（双斜杠）
    //   `http://host/v1` + `/models/`  → `/v1/models/`（尾斜杠）
    // 归一化只做「合并重复斜杠 + 去掉尾斜杠」，不碰大小写也不做模糊匹配。
    let raw_path = req.uri().path().to_string();
    let norm = normalize_path(&raw_path);
    if norm != raw_path {
        *req.uri_mut() = rebuild_uri(&norm, req.uri().query());
    }
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let content_length = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let chunked = req
        .headers()
        .get(header::TRANSFER_ENCODING)
        .map(|v| v.to_str().unwrap_or("").to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    // 访问日志：⚠️ 只记**去掉查询串**的路径 —— 部分客户端把 API Key 放在 `?key=` 里，
    // 打整条 path 等于把凭据写进日志。
    log_i!(
        TAG,
        "→ {} {} auth={} len={} chunked={}",
        method,
        path,
        auth_header.is_some(),
        content_length,
        chunked
    );

    let mut res = if method == "OPTIONS" {
        // CORS 预检**必须最先答、且任何路径都答**。204 是预检的标准应答。
        raw_response(204, "text/plain", "")
    } else {
        match (method.as_str(), path.as_str()) {
            ("GET", "/healthz") => raw_response(200, "text/plain", "ok"),
            // 路径别名：**部分客户端请求不带 `/v1` 的 `/models`**（Chatbox 就是），两个都收。
            // 模型清单无敏感数据，免鉴权便于各类客户端拉取；聊天接口仍必须鉴权。
            (_, "/v1/models") | (_, "/models") => handle_models(&app, &path),
            ("POST", "/v1/chat/completions") | ("POST", "/chat/completions") => {
                handle_chat(&app, req).await
            }
            _ => {
                // 未知路径必须留痕：这条日志直接指出是路径没对上。同样不记查询串（可能含 key）。
                log_w!(TAG, "← 404 未知端点: {} {}", method, path);
                json_response(404, openai_error("not_found", &format!("unknown endpoint: {}", path)))
            }
        }
    };
    // CORS：**每一个响应都要带**。实测 Chatbox 拉模型前会先发 `OPTIONS /v1/models` 预检，
    // 缺这几个头预检就失败（现象正是"获取模型失败"，且它会每 500ms 重试）。
    add_cors(&mut res);
    res
}

fn handle_models(app: &Arc<AppState>, path: &str) -> Response {
    let data = crate::gateway::backend::models_for(app);
    let n = data.as_array().map(|a| a.len()).unwrap_or(0);
    log_i!(TAG, "← 200 {} （{} 个模型）", path, n);
    json_response(200, json!({ "object": "list", "data": data }))
}

async fn handle_chat(app: &Arc<AppState>, req: Request) -> Response {
    let api_key = api_key(app);
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    if !authorized(&api_key, auth.as_deref()) {
        return json_response(401, openai_error("invalid_api_key", "missing or invalid local API key"));
    }
    // 两种传体方式都要认：多数客户端发 Content-Length，但 **Chatbox 发 chunked**。
    // hyper 已替我们按字节精确读完（含中文多字节），等价于 Kotlin 的 readBody/readChunkedBody。
    let (_, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => {
            // 体过大/非法：必须**明确回 413**，不能让它冒泡成"连接被重置"。
            log_w!(TAG, "请求体被拒: 请求体过大: 上限 {} 字节", MAX_BODY_BYTES);
            return json_response(
                413,
                openai_error(
                    "invalid_request_error",
                    &format!("请求体过大（上限 {} MB）", MAX_BODY_BYTES / 1024 / 1024),
                ),
            );
        }
    };
    let body_str = String::from_utf8_lossy(&bytes).into_owned();

    // `auto` 后端按**模型**路由，所以这里得先把 model 抠出来。
    // 解析失败不致命：路由回退到"当前后端"，聊天链路的解析会自己报错。
    let model_name = serde_json::from_str::<Value>(&body_str)
        .ok()
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let backend = crate::gateway::backend::route_for(app, &model_name);

    if backend == crate::gateway::backend::QODER {
        // ---- Qoder 后端（COSY 签名链路）----
        let qcred = qoder_cred_provider(app).await;
        let Some(qcred) = qcred.filter(|c| c.chat_ready()) else {
            // ⚠️ 绝不能回假回复：那会让"根本没转发"看起来像"转发成功但答案是假的"。
            log_w!(TAG, "Qoder 后端但没有可用凭证（uid/ jobToken 为空）");
            return json_response(
                502,
                openai_error(
                    "upstream_unavailable",
                    "Qoder 账号不可用：jobToken 为空或已失效。请回 App 重新登录 Qoder，或在「聊天转发后端」里切到 Trae。",
                ),
            );
        };
        // 请求体是信任边界：空体/非法 JSON 必须回 400，绝不能让解析错误冒泡。
        let Ok(src) = serde_json::from_str::<Value>(&body_str) else {
            return json_response(400, openai_error("invalid_request_error", "请求体不是合法 JSON"));
        };
        let wants_stream = src.get("stream").and_then(Value::as_bool).unwrap_or(false);
        return forward_qoder(app, src, wants_stream, qcred).await;
    }

    // ---- Trae 后端（原有链路）----
    // 每次请求都问一次「当下有效」的凭证：服务长驻，token 会过期，
    // 靠启动时快照的那一个迟早变 401。返回 null = 当前没有可用账号 → 走"无 token"分支。
    let Some(fresh) = trae_cred_provider(app).await else {
        return json_response(200, fake_completion());
    };
    if fresh.access_token.trim().is_empty() {
        return json_response(200, fake_completion());
    }
    let Ok(src) = serde_json::from_str::<Value>(&body_str) else {
        return json_response(400, openai_error("invalid_request_error", "请求体不是合法 JSON"));
    };
    let wants_stream = src.get("stream").and_then(Value::as_bool).unwrap_or(false);
    forward_trae(app, src, wants_stream, fresh).await
}

// ------------------------------------------------------------------
// 凭证提供者（Gateway.start 里注入的那几个 provider）
// ------------------------------------------------------------------

/// 每次转发前取「当下有效」的 Trae 凭证（必要时续期）。
async fn trae_cred_provider(app: &AppState) -> Option<TraeCred> {
    let _g = app.trae_lock.lock().await;
    let t = crate::trae::auth_api::restart_with_valid_token(app, SIXTY_MINUTES_SEC).await?;
    let mut c = crate::trae::auth_store::load(app)?;
    c.access_token = t;
    Some(c)
}

/// 每次转发前取「当下有效」的 jobToken（必要时续期）。
async fn qoder_cred_provider(app: &AppState) -> Option<QoderCred> {
    let _g = app.qoder_lock.lock().await;
    let cred = crate::qoder::auth_store::load(app)?;
    let jt = crate::qoder::auth_api::valid_job_token_for(app, &cred, SIXTY_MINUTES_MS)
        .await
        .unwrap_or_else(|| cred.job_token.clone());
    if jt.is_empty() {
        return None;
    }
    let mut c = cred;
    c.job_token = jt;
    Some(c)
}

// ------------------------------------------------------------------
// 转发（streamingChat / streamingQoderChat / streamWithFailover）
// ------------------------------------------------------------------

type EmitBox = Box<dyn FnMut(String) + Send>;
type UpResult = (Option<Value>, Option<String>);
type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type RunUpstreamFn = Box<dyn FnMut(EmitBox) -> BoxFut<UpResult> + Send>;
type FailoverFn = Box<dyn FnMut(String) -> BoxFut<bool> + Send>;
type RollbackFn = Box<dyn Fn() -> BoxFut<()> + Send>;
type RefreshFn = Box<dyn FnMut(String) -> BoxFut<bool> + Send>;
type UsedUidFn = Box<dyn Fn() -> String + Send>;

/// 非流式请求的最终产物（流式时帧已全部走 tx，无需返回值）。
enum FinalBody {
    Streamed,
    Json(Value),
    Err(String),
}

#[derive(Default, Clone)]
struct EmitState {
    pending: Vec<String>,
    streaming: bool,
}

fn write_chunk(
    wants_stream: bool,
    tx: &Option<mpsc::UnboundedSender<String>>,
    collected: &Mutex<Vec<String>>,
    s: String,
) {
    if !wants_stream {
        lock(collected).push(s);
        return;
    }
    if let Some(tx) = tx {
        // 客户端可能提前断开（Broken pipe），属正常边界：忽略写失败，流转仍继续并记录用量
        tx.send(s).ok();
    }
}

/// 走真实 Trae 转发（原有链路）。
///
/// ⚠️ `wants_stream` 必须在 `prepareBody` **之前**读：prepareBody 会把 `stream` 原地改成 true
///（上游只认流式），改完就看不到客户端原本要的是不是非流式了。（调用方已先读。）
async fn forward_trae(app: &Arc<AppState>, src: Value, wants_stream: bool, cred: TraeCred) -> Response {
    let prepared = crate::trae::upstream::prepare_body(src);
    let model = prepared
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cur = Arc::new(Mutex::new(cred));
    let original_uid = lock(&cur).uid.clone();

    let run_upstream: RunUpstreamFn = {
        let app = app.clone();
        let prepared = prepared.clone();
        let cur = cur.clone();
        Box::new(move |mut emit: EmitBox| {
            let app = app.clone();
            let prepared = prepared.clone();
            let cur = cur.clone();
            Box::pin(async move {
                let c = lock(&cur).clone();
                crate::trae::upstream::chat_stream(&app, prepared, c.access_token, c.uid, &mut *emit).await
            })
        })
    };

    // 故障转移：上游**还没吐正文**就失败（token 死了的 401 / 限流 429 / 5xx）时换另一个账号重试。
    // 换号会**真的切过去并持久化** —— 否则下一个请求还拿同一个死账号去撞墙。
    let try_failover: FailoverFn = {
        let app = app.clone();
        let cur = cur.clone();
        Box::new(move |err: String| {
            let app = app.clone();
            let cur = cur.clone();
            Box::pin(async move {
                let failed_uid = lock(&cur).uid.clone();
                let metas = crate::trae::auth_store::list(&app);
                let Some(next) = crate::trae::auth_store::pick_failover_candidate(&metas, &failed_uid)
                else {
                    return false;
                };
                log_i!(
                    GATEWAY_TAG,
                    "转发失败({})，自动换到账号 {}({})",
                    err,
                    next.uid,
                    next.region_id
                );
                let _g = app.trae_lock.lock().await;
                crate::trae::auth_store::set_active(&app, &next.uid);
                // 必须同步区域：否则会拿新账号的 token 打旧区域的 host
                crate::trae::auth_store::sync_region_to_active_account(&app);
                let Some(t) = crate::trae::auth_api::restart_with_valid_token(&app, SIXTY_MINUTES_SEC).await
                else {
                    return false;
                };
                let Some(mut c) = crate::trae::auth_store::load(&app) else {
                    return false;
                };
                c.access_token = t;
                *lock(&cur) = c;
                true
            })
        })
    };

    // 换号也没救活 → 切回用户原来选的账号（否则账号会在请求间来回跳）
    let on_rollback: RollbackFn = {
        let app = app.clone();
        Box::new(move || {
            let app = app.clone();
            let original_uid = original_uid.clone();
            Box::pin(async move {
                log_i!(GATEWAY_TAG, "换号后仍失败，切回原账号 {}", original_uid);
                let _g = app.trae_lock.lock().await;
                crate::trae::auth_store::set_active(&app, &original_uid);
                crate::trae::auth_store::sync_region_to_active_account(&app);
            })
        })
    };

    // 401 先自己救：强制续一次 token，同账号重试。续不动才轮到换号。
    let refresh_same_account: Option<RefreshFn> = Some({
        let app = app.clone();
        let cur = cur.clone();
        Box::new(move |_err: String| {
            let app = app.clone();
            let cur = cur.clone();
            Box::pin(async move {
                let c = lock(&cur).clone();
                let _g = app.trae_lock.lock().await;
                let Some(t) = crate::trae::auth_api::force_refresh_token_for(&app, &c).await else {
                    return false;
                };
                lock(&cur).access_token = t;
                true
            })
        })
    });

    let used_uid: UsedUidFn = {
        let cur = cur.clone();
        Box::new(move || lock(&cur).uid.clone())
    };

    finish_forward(
        app.clone(),
        wants_stream,
        model,
        crate::gateway::backend::TRAE.to_string(),
        used_uid,
        run_upstream,
        try_failover,
        on_rollback,
        refresh_same_account,
    )
    .await
}

/// 走 Qoder COSY 签名转发（QoderUpstream）。
///
/// 与 Trae 的差别只有"上游怎么打"，控制流共用 streamWithFailover。
/// 401 时由 QoderAuthApi.forceRefreshJobToken 强制续一次再重试（由上游内部回调触发）。
async fn forward_qoder(app: &Arc<AppState>, src: Value, wants_stream: bool, cred: QoderCred) -> Response {
    let model = src
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cur = Arc::new(Mutex::new(cred));
    let original_uid = lock(&cur).uid.clone();

    let run_upstream: RunUpstreamFn = {
        let app = app.clone();
        let src = src.clone();
        let cur = cur.clone();
        Box::new(move |mut emit: EmitBox| {
            let app = app.clone();
            let src = src.clone();
            let cur = cur.clone();
            Box::pin(async move {
                let c = lock(&cur).clone();
                // 转发中 jobToken 失效时由上游同步回调这里强续一次。
                //（force_refresh_job_token 是 async，这里 block_in_place 桥接；Tauri 运行时是多线程 tokio。）
                let app2 = app.clone();
                let cur2 = cur.clone();
                let mut refresh: Box<dyn FnMut() -> Option<String> + Send> = Box::new(move || {
                    let c = lock(&cur2).clone();
                    let app3 = app2.clone();
                    let t = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async move {
                            crate::qoder::auth_api::force_refresh_job_token(&app3, &c).await
                        })
                    });
                    if let Some(t) = &t {
                        lock(&cur2).job_token = t.clone();
                    }
                    t
                });
                crate::qoder::upstream::chat_stream(&app, src, c, &mut *emit, &mut refresh).await
            })
        })
    };

    let try_failover: FailoverFn = {
        let app = app.clone();
        let cur = cur.clone();
        Box::new(move |err: String| {
            let app = app.clone();
            let cur = cur.clone();
            Box::pin(async move {
                let failed_uid = lock(&cur).uid.clone();
                let Some(next) = crate::qoder::auth_store::list(&app)
                    .into_iter()
                    .find(|m| m.uid != failed_uid)
                else {
                    return false;
                };
                log_i!(GATEWAY_TAG, "Qoder 转发失败({})，自动换到账号 {}", err, next.uid);
                let _g = app.qoder_lock.lock().await;
                crate::qoder::auth_store::set_active(&app, &next.uid);
                let Some(c) = crate::qoder::auth_store::load(&app) else {
                    return false;
                };
                let jt = crate::qoder::auth_api::valid_job_token_for(&app, &c, SIXTY_MINUTES_MS)
                    .await
                    .unwrap_or_else(|| c.job_token.clone());
                if jt.is_empty() {
                    return false;
                }
                let mut c = c;
                c.job_token = jt;
                *lock(&cur) = c;
                true
            })
        })
    };

    let on_rollback: RollbackFn = {
        let app = app.clone();
        Box::new(move || {
            let app = app.clone();
            let original_uid = original_uid.clone();
            Box::pin(async move {
                log_i!(GATEWAY_TAG, "Qoder 换号后仍失败，切回原账号 {}", original_uid);
                let _g = app.qoder_lock.lock().await;
                crate::qoder::auth_store::set_active(&app, &original_uid);
            })
        })
    };

    let used_uid: UsedUidFn = {
        let cur = cur.clone();
        Box::new(move || lock(&cur).uid.clone())
    };

    finish_forward(
        app.clone(),
        wants_stream,
        model,
        crate::gateway::backend::QODER.to_string(),
        used_uid,
        run_upstream,
        try_failover,
        on_rollback,
        None,
    )
    .await
}

/// 流式转发的**公共骨架**：攒帧、判定"是否已开始输出"、换号、收尾聚合。
///
/// Trae 与 Qoder 两条上游的协议完全不同（请求体、签名、SSE 帧格式），但
/// "什么时候允许换号""非流式怎么聚合""用量怎么记"这套控制流**必须一致** ——
/// 换号判定错一次就会把真错误藏起来、白烧另一个账号。
#[allow(clippy::too_many_arguments)]
async fn finish_forward(
    app: Arc<AppState>,
    wants_stream: bool,
    model: String,
    backend: String,
    used_uid: UsedUidFn,
    run_upstream: RunUpstreamFn,
    try_failover: FailoverFn,
    on_rollback: RollbackFn,
    refresh_same_account: Option<RefreshFn>,
) -> Response {
    if wants_stream {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        // 流式：控制流跑在后台任务里，响应头（200 + event-stream）立刻返回给客户端 ——
        // 对齐 Android 版"先写响应头再转发"的时序，否则长回答会让客户端等到超时。
        tokio::spawn(run_loop(
            app, wants_stream, model, backend, used_uid, run_upstream,
            try_failover, on_rollback, refresh_same_account, Some(tx),
        ));
        // 流式响应头：CORS 头由全局 add_cors 统一补，这里只给 event-stream 必需的两个。
        Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(sse_stream(rx)))
            .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
    } else {
        match run_loop(
            app, wants_stream, model, backend, used_uid, run_upstream,
            try_failover, on_rollback, refresh_same_account, None,
        )
        .await
        {
            FinalBody::Streamed => unreachable!(),
            FinalBody::Json(v) => json_response(200, v),
            FinalBody::Err(msg) => json_response(502, openai_error("upstream_error", &msg)),
        }
    }
}

/// 控制流主体（参数全部 owned；流式时整个 future 跑在后台任务里）。
#[allow(clippy::too_many_arguments)]
async fn run_loop(
    app: Arc<AppState>,
    wants_stream: bool,
    model: String,
    backend: String,
    used_uid: UsedUidFn,
    mut run_upstream: RunUpstreamFn,
    mut try_failover: FailoverFn,
    on_rollback: RollbackFn,
    mut refresh_same_account: Option<RefreshFn>,
    tx: Option<mpsc::UnboundedSender<String>>,
) -> FinalBody {
    // 非流式时先攒着，最后一次性聚合成 JSON 再写（见 aggregate_chunks）。
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));

    let mut attempt: i32 = 0;
    let mut did_refresh = false; // 已在同一账号上强制续期重试过
    loop {
            let t0 = Instant::now();
            let es = Arc::new(Mutex::new(EmitState::default()));
            let (usage, err) = {
                let emit: EmitBox = Box::new({
                    let es = es.clone();
                    let tx = tx.clone();
                    let collected = collected.clone();
                    move |chunk: String| {
                        let mut st = lock(&es);
                        if st.streaming {
                            drop(st);
                            write_chunk(wants_stream, &tx, &collected, chunk);
                        } else {
                            st.pending.push(chunk.clone());
                            // 已确认这次上游成功（出现过 choices 帧）
                            if chunk.contains("\"choices\"") {
                                st.streaming = true;
                                let pending = std::mem::take(&mut st.pending);
                                drop(st);
                                for p in pending {
                                    write_chunk(wants_stream, &tx, &collected, p);
                                }
                            }
                        }
                    }
                });
                run_upstream(emit).await
            };
            let st = lock(&es).clone();
            let pending = st.pending;
            let streaming = st.streaming;

            // 先在同一账号上强制续期重试：token 未到期却被判废时，换号是浪费
            //（换号会真的切走并持久化，用户下次对话就换了账号）。
            let mut refreshed = false;
            if !streaming {
                if let Some(e) = &err {
                    if !did_refresh && auth_related_error(e) {
                        if let Some(r) = refresh_same_account.as_mut() {
                            if r(e.clone()).await {
                                refreshed = true;
                            }
                        }
                    }
                }
            }
            if refreshed {
                did_refresh = true;
                log_i!(TAG, "同账号续期后重试（原错误: {}）", err.as_deref().unwrap_or(""));
            }

            // 换号条件：这次没成功 + 续期没救活 + 错误值得换 + 还没换过 + 确实有别的账号。
            // 允许在**前两次**尝试里各换一次（最多换 2 个号），换到能用为止。
            let switched = if !refreshed && !streaming {
                match &err {
                    Some(e) if attempt <= 1 && retryable_upstream_error(e) => try_failover(e.clone()).await,
                    _ => false,
                }
            } else {
                false
            };

            if !switched && !refreshed {
                // 收尾：把攒下的（正常情况下是错误事件）交给客户端
                for p in pending {
                    write_chunk(wants_stream, &tx, &collected, p);
                }
                // 聊天用量回调：每次转发结束调用。
                let uid = used_uid();
                let ms = t0.elapsed().as_millis() as i64;
                let (p_tok, c_tok, t_tok) = usage
                    .as_ref()
                    .map(|u| {
                        (
                            u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
                            u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
                            u.get("total_tokens").and_then(Value::as_i64).unwrap_or(0),
                        )
                    })
                    .unwrap_or((0, 0, 0));
                let ok = err.is_none();
                crate::aux::usage_store::record(&app, &model, p_tok, c_tok, t_tok, ms, ok, err.as_deref().unwrap_or(""));
                // 「只在转发的时候加载正在使用的」：每次转发后顺手刷新当前账号额度（内部节流 60s）。
                // ⚠️ 失败（ok=false）时传 urgent=true：**跳过节流立刻判定换号**。
                crate::aux::credit_monitor::note_forwarded(&app, &backend, &uid, !ok);

                // 换了号却没救活 → 还原成用户原来的账号，免得每次请求都换一次、来回跳
                if attempt > 0 && err.is_some() {
                    on_rollback().await;
                }
                // 非流式：到这里所有帧都攒完了。
                // ⚠️ 必须先判错：上游发的是 `event: error` + `data: {"error":…}` 这种帧，
                // aggregateChunks 只认 choices，会把它聚合成**空回复** ——
                // 客户端于是看到 200 + 空内容，实际是额度/鉴权问题。
                if !wants_stream {
                    let frames = lock(&collected).clone();
                    let up_err = extract_upstream_error(&frames);
                    if err.is_some() || up_err.is_some() {
                        let msg = match up_err {
                            Some(m) => m,
                            None => humanize_upstream_error(err.as_deref().unwrap()),
                        };
                        log_w!(TAG, "非流式上游失败: {}", msg);
                        return FinalBody::Err(msg);
                    }
                    return FinalBody::Json(aggregate_chunks(&frames, &model));
                }
                return FinalBody::Streamed;
            }
            log_i!(TAG, "换号后重试（原错误: {}）", err.as_deref().unwrap_or(""));
            attempt += 1;
        }
    }
}

/// 把内部 channel 的帧变成 SSE 响应体流。
fn sse_stream(
    mut rx: mpsc::UnboundedReceiver<String>,
) -> impl Stream<Item = Result<Bytes, std::convert::Infallible>> + Send + 'static {
    futures_util::stream::poll_fn(move |cx| {
        use std::task::Poll;
        match rx.poll_recv(cx) {
            Poll::Ready(Some(s)) => Poll::Ready(Some(Ok(Bytes::from(s)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    })
}

// ------------------------------------------------------------------
// 错误分类 / 错误翻译（companion 里的纯函数）
// ------------------------------------------------------------------

/// 只有**鉴权类**错误才值得"先在同一账号上续期再重试"。
/// 其他可重试错误（429 限流、5xx、排队）续期没有任何帮助。
pub(crate) fn auth_related_error(err: &str) -> bool {
    if err.starts_with("queued:") {
        return false;
    }
    let code = err.rsplit(':').next().unwrap_or("").trim().parse::<i32>().ok();
    matches!(code, Some(401) | Some(403))
}

/// 该错误是否值得换个账号重试。
/// 认证/限流/服务端问题 → 换号有意义；请求本身的问题（400/404/422）→ 换谁都一样。
/// 解析不出状态码的（如网络异常）→ 保守换一次。
pub(crate) fn retryable_upstream_error(err: &str) -> bool {
    // 排队/资源受限（Qoder 403 + RESOURCE_LIMIT）**要换号**：实测排队是**按账号**判的，
    // 同一模型 A 排队、B 正常，所以换号是真有效的手段。QoderUpstream 遇到排队会立即返回，
    // 所以换号是秒级的，不会像以前那样先白等 30 秒 × 3 次。
    if err.starts_with("queued:") {
        return true;
    }
    match err.rsplit(':').next().unwrap_or("").trim().parse::<i32>() {
        Ok(c) => c == 401 || c == 403 || c == 429 || c >= 500,
        Err(_) => true,
    }
}

/// 从攒下的 SSE 帧里抠出上游的错误文案。
/// 上游两种形状都要认：`data: "Billing daily count exceeded"`（纯字符串）
/// 和 `data: {"error":{"message":"…"}}`（对象）。抠不到返回 null。
pub(crate) fn extract_upstream_error(chunks: &[String]) -> Option<String> {
    for frame in chunks {
        for line in frame.split('\n') {
            let t = line.trim();
            let Some(payload) = t.strip_prefix("data:") else { continue };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if payload.starts_with('"') {
                // 纯 JSON 字符串字面量
                if let Ok(Value::String(s)) = serde_json::from_str::<Value>(payload) {
                    if !s.trim().is_empty() {
                        return Some(humanize_upstream_error(&s));
                    }
                }
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(payload) else { continue };
            let Some(e) = obj.get("error").filter(|v| !v.is_null()) else { continue };
            let msg = match e {
                Value::String(s) => s.clone(),
                Value::Object(o) => o
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| o.get("type").and_then(Value::as_str))
                    .unwrap_or("")
                    .to_string(),
                other => other.to_string(),
            };
            if !msg.trim().is_empty() {
                return Some(humanize_upstream_error(&msg));
            }
        }
    }
    None
}

/// 把上游错误翻译成用户能照做的中文。**保留原文**，只在其后追加提示 ——
/// 猜错原因比不猜更糟，用户拿原文去搜/去问都有用。
pub(crate) fn humanize_upstream_error(raw: &str) -> String {
    let low = raw.to_lowercase();
    let hint = if low.contains("billing daily count exceeded") {
        "Qoder 账号今天的计费次数已用完（Billing daily count exceeded）。明天会重置；急着用就换一个 Qoder 账号，或把后端切到 Trae。".to_string()
    } else if low.contains("billing") && low.contains("exceed") {
        "账号额度/计费次数已用尽，明天重置或换账号。".to_string()
    } else if low.contains("quota") || low.contains("insufficient") {
        "账号额度不足，换账号或等重置。".to_string()
    } else if low.contains("unauthorized") || low.contains("401") {
        "鉴权失败：Qoder 的 jobToken 可能已失效，回 App 重新登录一次。".to_string()
    } else {
        String::new()
    };
    if hint.is_empty() {
        raw.to_string()
    } else {
        format!("{}\n\n{}", raw, hint)
    }
}

// ------------------------------------------------------------------
// 非流式聚合（aggregateChunks）
// ------------------------------------------------------------------

/// 把内部产生的 SSE 帧聚合成一个**非流式** `chat.completion` 响应。
///
/// 要拼的字段（对齐 OpenAI 流式 → 非流式的语义）：
///  - `delta.content` 逐帧**拼接**成 `message.content`（只看最后一帧会丢字）
///  - `delta.reasoning_content` 拼接成 `message.reasoning_content`（推理模型的思考）
///  - `delta.tool_calls` 按 `index` 累积：`function.name` 取首个非空，`arguments` 逐帧拼接
///  - `finish_reason` 取**最后一个非空**值（中途帧是空的）
///  - `usage` 取最后一帧带的那个
pub(crate) fn aggregate_chunks(chunks: &[String], model: &str) -> Value {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish = "stop".to_string();
    let mut usage: Option<Value> = None;
    let mut id: Option<String> = None;
    let mut created: i64 = 0;
    // index → 累积中的 tool_call（name 取首个非空，arguments 逐帧拼）
    let mut tool_calls: std::collections::BTreeMap<i64, Value> = std::collections::BTreeMap::new();

    for frame in chunks {
        // 一帧可能含多行（`data: {...}` 后跟空行）；只认 data 行。
        for line in frame.split('\n') {
            let t = line.trim();
            let Some(payload) = t.strip_prefix("data:") else { continue };
            let payload = payload.trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(payload) else { continue };
            let Value::Object(obj) = obj else { continue };
            if id.is_none() {
                if let Some(s) = obj.get("id").and_then(Value::as_str) {
                    if !s.trim().is_empty() {
                        id = Some(s.to_string());
                    }
                }
            }
            if created == 0 {
                created = obj.get("created").and_then(Value::as_i64).unwrap_or(0);
            }
            if let Some(u) = obj.get("usage").filter(|v| v.is_object()) {
                usage = Some(u.clone());
            }
            let Some(choices) = obj.get("choices").and_then(Value::as_array) else { continue };
            for c in choices {
                let Value::Object(c) = c else { continue };
                // 中途帧的 finish_reason 是空串/字面 "null"，只在有真值时覆盖。
                let fr = c.get("finish_reason").and_then(Value::as_str).unwrap_or("");
                if !fr.trim().is_empty() && fr != "null" {
                    finish = fr.to_string();
                }
                let Some(d) = c.get("delta").and_then(Value::as_object) else { continue };
                if let Some(s) = d.get("content").and_then(Value::as_str) {
                    if !s.is_empty() {
                        content.push_str(s);
                    }
                }
                if let Some(s) = d.get("reasoning_content").and_then(Value::as_str) {
                    if !s.is_empty() {
                        reasoning.push_str(s);
                    }
                }
                let Some(tcs) = d.get("tool_calls").and_then(Value::as_array) else { continue };
                for (j, tc) in tcs.iter().enumerate() {
                    let Value::Object(tc) = tc else { continue };
                    let idx = tc.get("index").and_then(Value::as_i64).unwrap_or(j as i64);
                    let acc = tool_calls.entry(idx).or_insert_with(|| {
                        json!({ "index": idx, "type": "function",
                                "function": { "name": "", "arguments": "" } })
                    });
                    if let Some(s) = tc.get("id").and_then(Value::as_str) {
                        if !s.trim().is_empty() {
                            acc["id"] = json!(s);
                        }
                    }
                    let Some(fnc) = tc.get("function").and_then(Value::as_object) else { continue };
                    let acc_fn = acc.get_mut("function").unwrap().as_object_mut().unwrap();
                    if acc_fn.get("name").and_then(Value::as_str).unwrap_or("").is_empty() {
                        if let Some(s) = fnc.get("name").and_then(Value::as_str) {
                            if !s.trim().is_empty() {
                                acc_fn.insert("name".into(), json!(s));
                            }
                        }
                    }
                    if let Some(s) = fnc.get("arguments").and_then(Value::as_str) {
                        if !s.is_empty() {
                            let prev = acc_fn.get("arguments").and_then(Value::as_str).unwrap_or("").to_string();
                            acc_fn.insert("arguments".into(), json!(format!("{}{}", prev, s)));
                        }
                    }
                }
            }
        }
    }

    let mut message = json!({ "role": "assistant", "content": content });
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        let arr: Vec<Value> = tool_calls.into_values().collect();
        message["tool_calls"] = Value::Array(arr);
        // 有工具调用时 content 必须是字符串（可为空），OpenAI 客户端据此走工具分支。
        if finish == "stop" {
            finish = "tool_calls".to_string();
        }
    }
    let choice = json!({ "index": 0, "message": message, "finish_reason": finish });
    let mut out = json!({
        "id": id.unwrap_or_else(|| "chatcmpl-local".to_string()),
        "object": "chat.completion",
        "created": if created > 0 { created } else { chrono::Utc::now().timestamp() },
        "model": model,
        "choices": [choice],
    });
    if let Some(u) = usage {
        out["usage"] = u;
    }
    out
}

// ------------------------------------------------------------------
// 固定回复 / 错误体 / 小工具
// ------------------------------------------------------------------

/// 固定的测试回复：与真实 Trae 结构一致，但内容是假的。
fn fake_completion() -> Value {
    let content = "这是本地假服务返回的测试响应。阶段2第2步已跑通：本地 OpenAI 兼容接口可用，下一步再接真实 TraeWork 上游转发。";
    json!({
        "id": "chatcmpl-fake-local",
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": "glm-5.2",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
    })
}

fn openai_error(code: &str, msg: &str) -> Value {
    json!({ "error": { "message": msg, "type": "api_error", "code": code } })
}

fn authorized(api_key: &str, auth: Option<&str>) -> bool {
    if api_key.is_empty() {
        return true;
    }
    let Some(a) = auth else { return false };
    let Some(rest) = strip_prefix_ci(a, "Bearer ") else {
        return false;
    };
    rest.trim() == api_key
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn json_response(status: u16, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn raw_response(status: u16, content_type: &str, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn add_cors(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("*"));
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
}

/// 合并重复斜杠 + 去掉尾斜杠（对齐 Kotlin 的路径归一化）。
fn normalize_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut prev_slash = false;
    for c in p.chars() {
        if c == '/' {
            if !prev_slash {
                out.push(c);
            }
            prev_slash = true;
        } else {
            out.push(c);
            prev_slash = false;
        }
    }
    if out.len() > 1 {
        while out.ends_with('/') {
            out.pop();
        }
        if out.is_empty() {
            out.push('/');
        }
    }
    out
}

fn rebuild_uri(path: &str, query: Option<&str>) -> Uri {
    let pq = match query {
        Some(q) => format!("{}?{}", path, q),
        None => path.to_string(),
    };
    Uri::builder().path_and_query(pq).build().unwrap_or_else(|_| Uri::from_static("/"))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ------------------------------------------------------------------
// 自检（LocalFakeServer.selfCheck + Gateway.selfCheck）
// ------------------------------------------------------------------

/// 自检：错误分类、非流式聚合（含工具调用分片合并）、本地 API Key 稳定性。
/// （body 按字节读满 / chunked 解析这两项在桌面版由 hyper 承担，不再适用。）
pub fn self_check(app: &AppState) -> Result<(), String> {
    // ---- Gateway.selfCheck：本地 API Key 必须**多次取值一致且非空** ----
    let a = api_key(app);
    if a.is_empty() || !a.starts_with("sk-") {
        return Err("自检失败：本地 API Key 格式不对".into());
    }
    // 派生 key 是确定性的：两次取值必须一致（钉住"每次取值都变"会让所有客户端 401 的老 bug）
    if a != api_key(app) {
        return Err("自检失败：本地 API Key 不稳定（每次取值都变会让所有客户端 401）".into());
    }
    if a.len() != "sk-".len() + 32 {
        return Err("自检失败：派生 Key 长度不对（应为 sk- + 32 hex）".into());
    }

    // ---- 换号判定的边界：换错了会白耗另一个账号的额度、并把真错误藏起来。----
    if !retryable_upstream_error("http:401") {
        return Err("自检失败：401 应换号".into());
    }
    if !retryable_upstream_error("intl_create:429") {
        return Err("自检失败：429 应换号".into());
    }
    if !retryable_upstream_error("http:503") {
        return Err("自检失败：5xx 应换号".into());
    }
    // 排队要换号（按账号判的），但不该先续期、不该等重试
    if !retryable_upstream_error("queued:dfmodel") {
        return Err("自检失败：排队应允许换号".into());
    }
    if auth_related_error("queued:dfmodel") {
        return Err("自检失败：排队不该触发续期".into());
    }
    if auth_related_error("http:429") {
        return Err("自检失败：429 不该触发续期".into());
    }
    if auth_related_error("http:503") {
        return Err("自检失败：5xx 不该触发续期".into());
    }
    if !auth_related_error("http:401") {
        return Err("自检失败：401 应触发续期".into());
    }
    if !retryable_upstream_error("intl_events:exception") {
        return Err("自检失败：非 http 类错误应保守换号".into());
    }
    if retryable_upstream_error("http:400") {
        return Err("自检失败：400 是请求本身的问题，换谁都一样，不该换号".into());
    }
    if retryable_upstream_error("intl_commit:404") {
        return Err("自检失败：404 不该换号".into());
    }
    if retryable_upstream_error("http:422") {
        return Err("自检失败：422 不该换号".into());
    }
    if !retryable_upstream_error("http:500") {
        return Err("自检失败：500 应换号".into());
    }

    // ---- 非流式聚合：客户端发 stream:false 时要拿到一个完整 JSON。----
    let frames = vec![
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":100,\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"想\"}}]}\n\n".to_string(),
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你\"}}]}\n\n".to_string(),
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"}}]}\n\n".to_string(),
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"total_tokens\":7}}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ];
    let agg = aggregate_chunks(&frames, "m1");
    let msg = &agg["choices"][0]["message"];
    if msg["content"].as_str() != Some("你好") {
        return Err(format!(
            "自检失败：非流式聚合应逐帧拼接 content，得到 [{}]",
            msg["content"].as_str().unwrap_or("")
        ));
    }
    if msg["reasoning_content"].as_str() != Some("想") {
        return Err("自检失败：reasoning_content 未被聚合".into());
    }
    if agg["choices"][0]["finish_reason"].as_str() != Some("stop") {
        return Err("自检失败：finish_reason 应取最后一个非空值".into());
    }
    if agg["usage"]["total_tokens"].as_i64() != Some(7) {
        return Err("自检失败：usage 未被带出".into());
    }
    if agg["id"].as_str() != Some("c1") {
        return Err("自检失败：聚合后的 id 不对".into());
    }

    // ---- 工具调用：name 在首帧、arguments 分片在后帧，必须合并成一条而不是互相覆盖。----
    let tool_frames = vec![
        "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n".to_string(),
        "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n".to_string(),
        "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"北京\\\"}\"}}]}}]}\n\n".to_string(),
        "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n".to_string(),
    ];
    let tool_agg = aggregate_chunks(&tool_frames, "m2");
    let t_msg = &tool_agg["choices"][0]["message"];
    let tcs = t_msg.get("tool_calls").and_then(Value::as_array);
    match tcs {
        Some(arr) if arr.len() == 1 => {}
        Some(arr) => return Err(format!("自检失败：工具调用应合并成 1 条，得到 {}", arr.len())),
        None => return Err("自检失败：工具调用应合并成 1 条，得到 0".into()),
    }
    let tfn = &tcs.unwrap()[0]["function"];
    if tfn["name"].as_str() != Some("get_weather") {
        return Err("自检失败：工具名丢了".into());
    }
    if tfn["arguments"].as_str() != Some("{\"city\":\"北京\"}") {
        return Err(format!(
            "自检失败：工具参数分片未拼全，得到 [{}]",
            tfn["arguments"].as_str().unwrap_or("")
        ));
    }
    if t_msg.get("content").is_none() {
        return Err("自检失败：有工具调用时 content 仍需存在（可为空串）".into());
    }
    Ok(())
}
