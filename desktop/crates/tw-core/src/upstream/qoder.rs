//! `QoderUpstream.kt` 的 Rust 对应体：COSY `/algo/` 签名链路 → OpenAI chat.completion.chunk。
//! 协议权威 = `QoderUpstream.kt` 源码（825 行版）；请求体需整体过 [`cosy::encode_body`] 编码，
//! SSE 帧是 `{statusCodeValue, body}` **信封**，真 chunk 在 `body` 字符串里。
//!
//! 与 Trae 的差异（源码注释原话）：
//!  - Trae 走 `solo_work_remote` + 裸 `Cloud-IDE-JWT`；Qoder **必须**带 COSY 签名头；
//!  - 输出格式与 Trae 完全一致（`data: {...}\n\n` + 末尾 `data: [DONE]`）。
//!
//! # 移植裁决/近似（与 Kotlin 源码不一致处的记录）
//!  - `queueWaitMs` 的抠数正则在 Kotlin 里写的是 `\*"key\*"`（字面星号），按字面移植会让
//!    **它自己的 selfCheck 用例 1 失败**（`{"code":"10605","retryAfterMs":1500}` 抠不到数、
//!    返回 2000）。意图显然是一个反斜杠的转义写掉了一层（想匹配 `\"key\":` 的**可选反斜杠**），
//!    故本移植采用 `\\*"key\\*"\s*:\s*(\d+)`（0+ 个反斜杠 + 引号），同时满足：
//!    明文 JSON、单层转义 `\"…\"`、双层转义 `\\\"…\\\"` 三种形状，且三个 selfCheck 用例全过。
//!  - transport 异常标签：Kotlin 用 `e.javaClass.simpleName`（SocketTimeoutException/IOException…），
//!    Rust `reqwest::Error` 无类名，按 `is_timeout/is_connect` 映射成同名标签（近似，注释标注）。
//!  - `prepare:`/`body:` 两个错误标签路径在 Rust 里不可达（`prepare_body` 是纯函数、不抛异常），
//!    保留字面量以便网关侧文案对齐。

use super::trae::{chat_client, emit_frame, LineBuffer};
use super::{finish, json_quote, ChatRequest, StreamOutcome, StreamResult, Upstream};
use super::{new_stream, opt_str, spawn_pump};
use crate::auth::api::qoder as qoder_api;
use crate::auth::model::{now_ms, now_sec};
use crate::auth::store::QoderAuthStore;
use crate::crypto::cosy;
use crate::crypto::machine;
use crate::models::catalog::{QModel, QoderModelCatalog, DEFAULT_KEY};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc::Sender;

/// 参考实现 `callUpstream` 的 maxAttempts。
pub const MAX_ATTEMPTS: u32 = 3;

/// 可重试错误最长等待：本地网关客户端在等首字，睡 30s = 全链路假死（源码注释）。
pub const MAX_RETRY_WAIT_MS: i64 = 2_000;

/// 客户端没给 max_tokens 时的默认值（与参考实现一致）。
pub const DEFAULT_MAX_TOKENS: i64 = 32_000;

pub const NATIVE_INFER_PATH: &str = cosy::NATIVE_INFER_PATH;
pub const NATIVE_INFER_QUERY: &str = cosy::NATIVE_INFER_QUERY;

// ---------------------------------------------------------------------------
// 请求体构造（OpenAI → Qoder remote chat ask，源码 69-236 行逐语义移植）
// ---------------------------------------------------------------------------

/// 把客户端 OpenAI 请求体改写成 Qoder 上游形状。要点全是源码注释里"踩过的坑"：
/// `business` 必带；system 双放（顶层 + messages[0]）；tool 角色不发 `contents`；
/// 带 tool_calls 的 content 必须是字符串（可空串）；必须补 ephemeral 缓存标记。
pub fn prepare_body(src: &Value) -> Value {
    let catalog = QoderModelCatalog::global();
    let raw_model = opt_str(src, "model").unwrap_or("");
    let resolved = catalog.resolve(raw_model, 0);

    let mut sys_parts: Vec<String> = Vec::new();
    let mut up_messages: Vec<Value> = Vec::new();
    if let Some(msgs) = src.get("messages").and_then(|v| v.as_array()) {
        for m in msgs {
            if !m.is_object() {
                continue;
            }
            let role = opt_str(m, "role").unwrap_or("").to_string();
            if role == "system" || role == "developer" {
                if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                    if !c.is_empty() {
                        sys_parts.push(c.to_string());
                    }
                }
                continue;
            }
            up_messages.push(to_up_message(m, &role));
        }
    }
    ensure_cache_marker(&mut up_messages);
    let system = sys_parts.join("\n\n");

    let request_id = uuid::Uuid::new_v4().to_string();
    let business = business_info(&up_messages);

    let mut body = Map::new();
    body.insert("business".into(), business);
    body.insert("request_id".into(), Value::String(request_id.clone()));
    body.insert("request_set_id".into(), Value::String(request_id.clone()));
    body.insert("chat_record_id".into(), Value::String(request_id));
    body.insert("session_id".into(), Value::String(catalog.session_for(resolved.key())));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("chat_task".into(), Value::String("FREE_INPUT".into()));
    body.insert("chat_context".into(), Value::Object(Map::new()));
    body.insert("is_reply".into(), Value::Bool(true));
    body.insert("is_retry".into(), Value::Bool(false));
    body.insert("source".into(), Value::from(1));
    body.insert("version".into(), Value::String("3".into()));
    body.insert("agent_id".into(), Value::String("agent_common".into()));
    body.insert("task_id".into(), Value::String("common".into()));
    body.insert("session_type".into(), Value::String("qodercli".into()));
    body.insert("aliyun_user_type".into(), Value::String(String::new()));
    body.insert("model_config".into(), model_config_json(&resolved.model));
    body.insert("custom_model".into(), Value::Null);
    body.insert("system".into(), Value::String(system));
    body.insert("messages".into(), Value::Array(up_messages));
    body.insert(
        "tools".into(),
        src.get("tools").and_then(|v| v.as_array()).cloned().map(Value::Array).unwrap_or_else(|| Value::Array(vec![])),
    );
    body.insert("parameters".into(), parameters(src));
    Value::Object(body)
}

/// `businessInfo`：`name` 取最后一条 user 文本前 10 字符（源码 `take(10)`）。
pub fn business_info(msgs: &[Value]) -> Value {
    let name: String = last_user_text(msgs).chars().take(10).collect();
    serde_json::json!({
        "product": "cli",
        "version": cosy::COSY_VERSION,
        "type": "agent",
        "id": uuid::Uuid::new_v4().to_string(),
        "name": name,
        "begin_at": now_ms(),
        "stage": "start",
    })
}

/// `lastUserText`：倒序找第一条非空 user 文本（字符串 content 或其 `contents` 结构化副本）。
pub fn last_user_text(msgs: &[Value]) -> String {
    for m in msgs.iter().rev() {
        if opt_str(m, "role") != Some("user") {
            continue;
        }
        if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
            if !c.is_empty() {
                return c.to_string();
            }
        }
        if let Some(parts) = m.get("contents").and_then(|v| v.as_array()) {
            for p in parts {
                if opt_str(p, "type") == Some("text") {
                    let t = opt_str(p, "text").unwrap_or("");
                    if !t.is_empty() {
                        return t.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

/// `modelConfigJson`：price_factor 用官方真实倍率，未知按 1.0 发（源码注释）。
pub fn model_config_json(m: &QModel) -> Value {
    serde_json::json!({
        "key": m.key,
        "format": "openai",
        "source": "system",
        "enable": true,
        "display_name": m.name,
        "is_vl": true,
        "is_reasoning": m.reasoning,
        "price_factor": m.price_factor.unwrap_or(1.0),
    })
}

/// `parameters`：缺省 max_tokens=32000（不是无限）。
pub fn parameters(src: &Value) -> Value {
    let mut p = Map::new();
    let mc = int_or_zero(src, "max_completion_tokens");
    let mt = int_or_zero(src, "max_tokens");
    let max_tok = if mc > 0 { mc } else if mt > 0 { mt } else { DEFAULT_MAX_TOKENS };
    p.insert("max_tokens".into(), Value::from(max_tok));
    if let Some(e) = normalize_effort(opt_str(src, "reasoning_effort").unwrap_or("")) {
        p.insert("reasoning_effort".into(), Value::String(e.into()));
    }
    if let Some(v) = num_or_null(src, "temperature") {
        p.insert("temperature".into(), v);
    }
    if let Some(v) = num_or_null(src, "top_p") {
        p.insert("top_p".into(), v);
    }
    if let Some(arr) = src.get("stop").and_then(stop_from) {
        p.insert("stop".into(), arr);
    }
    if let Some(tc) = src.get("tool_choice").filter(|v| !v.is_null()) {
        p.insert("tool_choice".into(), tc.clone());
    }
    Value::Object(p)
}

/// `numOrNull`：org.json `getDouble` 会把可解析字符串转数字，`as_f64` 只认数字，
/// 这里再补一层字符串解析对齐语义。
fn num_or_null(o: &Value, key: &str) -> Option<Value> {
    let v = o.get(key).filter(|v| !v.is_null())?;
    if let Some(f) = v.as_f64() {
        return Some(Value::from(f));
    }
    let parsed = v.as_str()?.parse::<f64>().ok()?;
    Some(Value::from(parsed))
}

fn int_or_zero(o: &Value, key: &str) -> i64 {
    match o.get(key) {
        Some(v) if v.is_i64() => v.as_i64().unwrap_or(0),
        Some(v) if v.is_f64() => v.as_f64().unwrap_or(0.0) as i64,
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `normalizeEffort`：`minimal`→`low`；白名单外丢弃（发过去会被上游拒）。
pub fn normalize_effort(e: &str) -> Option<&'static str> {
    match e {
        "minimal" => Some("low"),
        "none" | "low" | "medium" | "high" | "xhigh" | "max" => Some(
            match e {
                "none" => "none",
                "low" => "low",
                "medium" => "medium",
                "high" => "high",
                "xhigh" => "xhigh",
                _ => "max",
            },
        ),
        _ => None,
    }
}

/// `stopFrom`：字符串或数组统一成数组（上游只认数组）；空串/空数组 → None。
pub fn stop_from(raw: &Value) -> Option<Value> {
    match raw {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(Value::Array(vec![Value::String(s.clone())]))
            }
        }
        Value::Array(a) => {
            if a.is_empty() {
                None
            } else {
                Some(Value::Array(a.clone()))
            }
        }
        _ => None,
    }
}

/// `toUpMessage`。
pub fn to_up_message(m: &Value, role: &str) -> Value {
    let mut o = Map::new();
    o.insert("role".into(), Value::String(role.to_string()));
    if let Some(n) = opt_str(m, "name") {
        if !n.is_empty() {
            o.insert("name".into(), Value::String(n.to_string()));
        }
    }
    if let Some(t) = opt_str(m, "tool_call_id") {
        if !t.is_empty() {
            o.insert("tool_call_id".into(), Value::String(t.to_string()));
        }
    }
    let content_raw = m.get("content").filter(|v| !v.is_null());
    // contents 是官方客户端并行发送的结构化副本；tool 角色不发（上游会当成重复内容）
    if let Some(Value::String(c)) = &content_raw {
        if !c.is_empty() && role != "tool" {
            o.insert(
                "contents".into(),
                Value::Array(vec![serde_json::json!({ "type": "text", "text": c })]),
            );
        }
    }
    let tool_calls = m.get("tool_calls").and_then(|v| v.as_array());
    if let Some(tcs) = tool_calls {
        if !tcs.is_empty() {
            o.insert("tool_calls".into(), Value::Array(tcs.clone()));
            // 带工具调用时 content 必须是字符串（可为空），null 让上游解析失败
            o.insert(
                "content".into(),
                match content_raw {
                    Some(Value::String(s)) => Value::String(s.clone()),
                    Some(v) => v.clone(),
                    None => Value::String(String::new()),
                },
            );
            return Value::Object(o);
        }
    }
    if let Some(v) = content_raw {
        o.insert("content".into(), v.clone());
    }
    Value::Object(o)
}

/// `ensureCacheMarker`：整段 prompt 里**恰好一个** ephemeral 标记；客户端已带就不动。
pub fn ensure_cache_marker(msgs: &mut [Value]) {
    for m in msgs.iter() {
        if let Some(parts) = m.get("contents").and_then(|v| v.as_array()) {
            if parts.iter().any(|p| p.get("cache_control").is_some_and(|c| !c.is_null())) {
                return;
            }
        }
    }
    for m in msgs.iter_mut().rev() {
        if opt_str(m, "role") == Some("tool") {
            continue;
        }
        let Some(parts) = m.get_mut("contents").and_then(|v| v.as_array_mut()) else { continue };
        for p in parts.iter_mut().rev() {
            let t = opt_str(p, "type").unwrap_or("").to_string();
            if t == "text" || t == "image_url" {
                if let Some(po) = p.as_object_mut() {
                    po.insert("cache_control".into(), serde_json::json!({ "type": "ephemeral" }));
                }
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 运行时字段缓存（源码 242-264 行：key=`uid|jobToken`，>8 全清）
// ---------------------------------------------------------------------------

fn runtime_cache() -> &'static Mutex<HashMap<String, cosy::RuntimeFields>> {
    static C: OnceLock<Mutex<HashMap<String, cosy::RuntimeFields>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `runtimeFieldsFor`：带上 jobToken 作 key——AES 明文含 security_oauth_token，token 一换旧材料作废。
pub fn runtime_fields_for(uid: &str, job_token: &str, org_id: &str, org_tags: &[String], policy_agreed: bool) -> cosy::RuntimeFields {
    let key = format!("{uid}|{job_token}");
    let mut g = runtime_cache().lock().expect("runtime 缓存锁中毒");
    if let Some(hit) = g.get(&key) {
        return hit.clone();
    }
    if g.len() > 8 {
        g.clear();
    }
    let rf = cosy::generate_runtime_fields(uid, job_token, org_id, org_tags, policy_agreed);
    g.insert(key, rf.clone());
    rf
}

// ---------------------------------------------------------------------------
// COSY 请求构造（buildRequest 502-519 行；刻意**不带 User-Agent**——与签名身份不一致会被拒）
// ---------------------------------------------------------------------------

/// 聊天 POST 全头（Authorization 传已签好串；org 两项按非空才发）。
#[allow(clippy::too_many_arguments)]
pub fn cosy_headers(
    authorization: &str,
    cosy_key: &str,
    unix_seconds: &str,
    uid: &str,
    model_key: &str,
    policy_agreed: bool,
    org_id: &str,
    org_tags: &[String],
) -> Vec<(&'static str, String)> {
    let mut h: Vec<(&'static str, String)> = vec![
        ("Accept", "text/event-stream".into()),
        ("Authorization", authorization.to_string()),
        ("Cache-Control", "no-cache".into()),
        ("Content-Type", "application/json".into()),
        ("Cosy-Business-Product", cosy::SCENE_BUSINESS_PRODUCT.into()),
        ("Cosy-Business-Type", cosy::SCENE_BUSINESS_TYPE.into()),
        ("Cosy-ClientType", cosy::SCENE_CLIENT_TYPE.into()),
        ("Cosy-Data-Policy", if policy_agreed { "agree" } else { "disagree" }.into()),
        ("Cosy-Date", unix_seconds.to_string()),
        ("Cosy-Key", cosy_key.to_string()),
        ("Cosy-MachineId", machine::machine_uuid()),
        ("Cosy-MachineOS", machine::MACHINE_OS.into()),
        ("Cosy-Scene", cosy::SCENE_SCENE.into()),
        ("Cosy-User", uid.to_string()),
        ("Cosy-Version", cosy::COSY_VERSION.into()),
        ("Login-Version", "v2".into()),
        ("X-Model-Key", model_key.to_string()),
        ("X-Model-Source", "system".into()),
    ];
    if !org_id.is_empty() {
        h.push(("Cosy-Organization-Id", org_id.to_string()));
    }
    if !org_tags.is_empty() {
        h.push(("Cosy-Organization-Tags", org_tags.join(",")));
    }
    h
}

/// `buildRequest` 的 URL：inferBase + NATIVE_INFER_PATH + NATIVE_INFER_QUERY。
pub fn infer_chat_url(mode: &str) -> String {
    format!("{}{}{}", qoder_api::infer_base(mode), NATIVE_INFER_PATH, NATIVE_INFER_QUERY)
}

// ---------------------------------------------------------------------------
// 错误输出 / 排队与重试辅助（源码 666-726 行）
// ---------------------------------------------------------------------------

/// `emitError`：与 Trae 不同，这里是**两帧**（`event: error` + 结构化 error 帧），
/// `[DONE]` 由调用方补。
pub fn emit_error_frames(message: &str) -> Vec<String> {
    let err = serde_json::json!({ "error": { "message": message, "type": "api_error" } });
    vec![
        format!("event: error\ndata: {}\n\n", json_quote(message)),
        format!("data: {}\n\n", err),
    ]
}

/// `queueWaitMs`：排队/资源受限的等待毫秒（含多层转义文本扫描，见模块头裁决）。
pub fn queue_wait_ms(raw: &str) -> Option<i64> {
    fn num(raw: &str, key: &str) -> Option<i64> {
        static POOL: OnceLock<Mutex<HashMap<String, regex::Regex>>> = OnceLock::new();
        let cache = POOL.get_or_init(|| Default::default());
        let mut g = cache.lock().expect("regex 缓存锁中毒");
        let re = g.entry(key.to_string()).or_insert_with(|| {
            regex::Regex::new(&format!("\\\\*\"{key}\\\\*\"\\s*:\\s*(\\d+)")).expect("regex 构造（仅字面量拼接）")
        });
        let caps = re.captures(raw)?;
        caps.get(1)?.as_str().parse::<i64>().ok()
    }
    // ⚠️ "isQueued" 标记必须是**带反斜杠**形态：明文 queue 对象走下方规整解析分支
    // （selfCheck 用例2：{"queue":{"isQueued":true,"waitTime":3000}} → 3000，不乘 1000）。
    let looks_queued = raw.contains(r#"\"isQueued\":true"#)
        || raw.contains("RESOURCE_LIMIT")
        || raw.contains(r#""code":"10605""#)
        || raw.contains("10605");
    if !looks_queued {
        // 仍兼容"规规矩矩给了 queue 对象"的形状
        let o: Value = serde_json::from_str(raw).ok()?;
        let q = o.get("queue").filter(|v| v.is_object())?;
        if q.get("isQueued").and_then(|v| v.as_bool()) == Some(true) {
            let ms = q.get("waitTime").and_then(|v| v.as_i64()).unwrap_or(0);
            return Some(if ms > 0 { ms } else { 2_000 });
        }
        return None;
    }
    // 从转义文本里抠秒数：retryAfterSeconds / waitTime / retryAfterMs 都认
    if let Some(s) = num(raw, "retryAfterSeconds") {
        return Some((s * 1000).max(1_000));
    }
    if let Some(s) = num(raw, "waitTime") {
        return Some((s * 1000).max(1_000));
    }
    if let Some(s) = num(raw, "retryAfterMs") {
        return Some(s.max(1_000));
    }
    Some(2_000)
}

/// `retryAfterMs`：`Retry-After: 5` → 5000（只认秒）。
pub fn retry_after_ms(value: Option<&str>) -> Option<i64> {
    let secs: f64 = value?.trim().parse().ok()?;
    Some((secs * 1000.0) as i64)
}

/// Kotlin 异常类简单名的近似映射（见模块头）。
fn transport_label(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "SocketTimeoutException"
    } else if e.is_connect() {
        "ConnectException"
    } else {
        "IOException"
    }
}

// ---------------------------------------------------------------------------
// SSE 信封解析（readEnvelopeSse 555-630 行，抽成可测状态机）
// ---------------------------------------------------------------------------

/// `ToolIndexer`：上游给并行工具调用都发 `index:0` 只靠新 id 区分；按 Kotlin 逻辑重排。
#[derive(Default)]
pub struct ToolIndexer {
    cur: i64,
    seen: bool,
}

impl ToolIndexer {
    /// @return 是否改写了 chunk（没改可原样透传省一次序列化）。
    pub fn remap(&mut self, chunk: &mut Value) -> bool {
        let Some(choices) = chunk.get_mut("choices").and_then(|v| v.as_array_mut()) else { return false };
        let mut changed = false;
        for ci in choices {
            let Some(delta) = ci.get_mut("delta") else { continue };
            let Some(tcs) = delta.get_mut("tool_calls").and_then(|v| v.as_array_mut()) else { continue };
            for tc in tcs.iter_mut() {
                let Some(tc) = tc.as_object_mut() else { continue };
                if !tc.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    if self.seen {
                        self.cur += 1;
                    } else {
                        self.seen = true;
                    }
                }
                let idx = tc.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
                if idx != self.cur {
                    changed = true;
                }
                tc.insert("index".into(), Value::from(self.cur));
            }
        }
        changed
    }
}

/// 信封流状态机。`push_line` 逐行喂；`finish()` EOF 收尾。
/// 错误消息进 [`Self::error`]（readEnvelopeSse 的返回值）；usage 只保留**最后一次**。
pub struct EnvelopeSse {
    event: String,
    data: String,
    error: Option<String>,
    usage: Option<Value>,
    first_chunk: bool,
    stopped: bool,
    indexer: ToolIndexer,
    client_model: String,
}

impl EnvelopeSse {
    pub fn new(client_model: &str) -> Self {
        Self {
            event: String::new(),
            data: String::new(),
            error: None,
            usage: None,
            first_chunk: true,
            stopped: false,
            indexer: ToolIndexer::default(),
            client_model: client_model.to_string(),
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn usage(&self) -> Option<Value> {
        self.usage.clone()
    }

    /// 逐行推进；返回 false 表示流应停止（`finish` 事件或错误帧）。
    pub fn push_line(&mut self, line: &str, emit: &mut dyn FnMut(String)) -> bool {
        if self.stopped {
            return false;
        }
        let trim = line.strip_suffix('\r').unwrap_or(line);
        if trim.is_empty() {
            let cont = self.flush(emit);
            if !cont {
                self.stopped = true;
            }
            return cont;
        }
        if let Some(rest) = trim.strip_prefix("event:") {
            if !self.event.is_empty() {
                self.event.push(' ');
            }
            self.event.push_str(rest.trim());
        } else if let Some(rest) = trim.strip_prefix("data:") {
            let mut d = rest;
            if d.starts_with(' ') {
                d = &d[1..]; // 只剥**一个**前导空格
            }
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(d);
        }
        // 其余行（id:/注释/retry:）忽略
        true
    }

    /// EOF 且未 stopped 时补一次 flush（对齐 `if (!stopped) flush()`）。
    pub fn finish(&mut self, emit: &mut dyn FnMut(String)) {
        if !self.stopped {
            self.flush(emit);
        }
    }

    fn flush(&mut self, emit: &mut dyn FnMut(String)) -> bool {
        if self.data.is_empty() && self.event.is_empty() {
            return true;
        }
        let ev = std::mem::take(&mut self.event);
        let payload = std::mem::take(&mut self.data).trim().to_string();
        if ev == "finish" {
            return false;
        }
        if payload.is_empty() {
            return true;
        }
        let Ok(env) = serde_json::from_str::<Value>(&payload) else { return true };
        let status = env.get("statusCodeValue").and_then(|v| v.as_i64()).unwrap_or(0);
        let env_body = env.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if status != 0 && status != 200 {
            let msg = serde_json::from_str::<Value>(&env_body)
                .ok()
                .and_then(|b| Some(opt_str(&b, "message").unwrap_or("").to_string()))
                .unwrap_or_default();
            self.error = Some(if !msg.is_empty() { msg } else { format!("upstream error status {status}") });
            return false;
        }
        if env_body.is_empty() {
            return true;
        }
        let Ok(mut chunk) = serde_json::from_str::<Value>(&env_body) else { return true };
        if let Some(err_obj) = chunk.get("error").filter(|v| v.is_object()) {
            let msg = opt_str(err_obj, "message").unwrap_or("").to_string();
            if !msg.is_empty() {
                self.error = Some(msg);
                return false;
            }
        }
        if let Some(u) = chunk.get("usage").filter(|v| v.is_object()) {
            self.usage = Some(u.clone());
        }
        let remapped = self.indexer.remap(&mut chunk);
        self.first_chunk = false;
        // 快路径：不改写时原样透传，只在最前注入 `model`（Qoder 不返 model，客户端需要）
        let trimmed = env_body.trim();
        let has_model = chunk.get("model").is_some_and(|v| !v.is_null());
                let wire = if !remapped && !has_model && trimmed.starts_with("{\"") {
            format!("{{\"model\":{},{}", json_quote(&self.client_model), &trimmed[1..])
        } else {
            if let Some(co) = chunk.as_object_mut() {
                co.insert("model".into(), Value::String(self.client_model.clone()));
            }
            chunk.to_string()
        };
        emit(format!("data: {wire}\n\n"));
        true
    }
}

// ---------------------------------------------------------------------------
// 转发主流程（chatStream 276-400 行的 async pump 形态）
// ---------------------------------------------------------------------------

pub struct QoderUpstream;

impl Upstream for QoderUpstream {
    fn stream_chat(&self, req: &ChatRequest) -> StreamResult {
        let (tx, res) = new_stream();
        let outcome = res.outcome.clone();
        let owned = req.clone();
        spawn_pump(async move { run(owned, tx, outcome).await });
        res
    }
}

async fn run(req: ChatRequest, tx: Sender<String>, outcome: Arc<StreamOutcome>) {
    let store = QoderAuthStore::global();
    let account = if req.uid.trim().is_empty() {
        store.load()
    } else {
        store.load_by_uid(&req.uid).or_else(|| store.load())
    };
    let Some(account) = account else {
        for f in emit_error_frames("没有可用的 Qoder 账号") {
            emit_frame(&tx, &f).await;
        }
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("no_account".into()));
        return;
    };

    // 请求前保证 jobToken 新鲜（含续期；网关也可预注入 req.token 覆盖）
    let job_initial = match &req.token {
        Some(t) if !t.trim().is_empty() => Some(t.clone()),
        _ => qoder_api::valid_job_token_for(&store, &account, qoder_api::JOB_SKEW_MS).await,
    };
    let mut job_token = job_initial.unwrap_or_else(|| account.job_token.clone());
    if job_token.trim().is_empty() {
        for f in emit_error_frames("没有可用的 Qoder jobToken") {
            emit_frame(&tx, &f).await;
        }
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("no_token".into()));
        return;
    }

    let client_model = opt_str(&req.body, "model").unwrap_or("").to_string();
    let upstream_body = prepare_body(&req.body);
    let model_key = upstream_body
        .get("model_config")
        .and_then(|mc| opt_str(mc, "key"))
        .unwrap_or(DEFAULT_KEY)
        .to_string();

    let bytes = upstream_body.to_string().into_bytes();
    let encoded = cosy::encode_body(&bytes);
    let encoded_str = String::from_utf8_lossy(&encoded).to_string();

    for attempt in 1..=MAX_ATTEMPTS {
        // 每次尝试都要新 requestId/时间戳/签名：上游按 (requestId, date) 防重放。
        let unix_seconds = now_sec().to_string();
        let runtime = runtime_fields_for(
            &account.uid,
            &job_token,
            &account.organization_id,
            &account.organization_tags,
            account.data_policy_agreed,
        );
        let payload_b64 =
            cosy::authorization_payload_default_version(&QoderModelCatalog::new_request_id(), &runtime.encrypt_user_info);
        let authorization = cosy::authorization_chat(&payload_b64, &runtime.key, &unix_seconds, &encoded_str);

        let mut rb = chat_client().post(infer_chat_url(&account.mode)).body(encoded.clone());
        for (k, v) in cosy_headers(
            &authorization,
            &runtime.key,
            &unix_seconds,
            &account.uid,
            &model_key,
            account.data_policy_agreed,
            &account.organization_id,
            &account.organization_tags,
        ) {
            rb = rb.header(k, v);
        }

        let resp = match rb.send().await {
            Err(e) => {
                let name = transport_label(&e);
                if attempt < MAX_ATTEMPTS {
                    // 传输异常几乎都是立即失败；100ms 缓一下瞬时抖动，真正退避交给上层换号
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
                for f in emit_error_frames(&format!("上游连接失败: {name}")) {
                    emit_frame(&tx, &f).await;
                }
                emit_frame(&tx, "data: [DONE]\n\n").await;
                finish(&outcome, None, Some(format!("transport:{name}")));
                return;
            }
            Ok(r) => r,
        };

        if resp.status().is_success() {
            let mut en = EnvelopeSse::new(&client_model);
            let mut lb = LineBuffer::new();
            let mut resp = resp;
            loop {
                let next = tokio::time::timeout(std::time::Duration::from_secs(180), resp.chunk()).await;
                match next {
                    Err(_) | Ok(Err(_)) => break,
                    Ok(Ok(None)) => break,
                    Ok(Ok(Some(b))) => {
                        let mut dead = false;
                        for line in lb.feed(&b) {
                            let mut frames: Vec<String> = Vec::new();
                            if !en.push_line(&line, &mut |f| frames.push(f)) {
                                dead = true;
                            }
                            for f in frames {
                                emit_frame(&tx, &f).await;
                            }
                            if dead {
                                break;
                            }
                        }
                        if dead {
                            break;
                        }
                    }
                }
            }
            let mut tail_frames: Vec<String> = Vec::new();
            en.finish(&mut |f| tail_frames.push(f));
            for f in tail_frames {
                emit_frame(&tx, &f).await;
            }
            let usage = en.usage();
            if let Some(msg) = en.error().map(str::to_string) {
                for f in emit_error_frames(&msg) {
                    emit_frame(&tx, &f).await;
                }
                emit_frame(&tx, "data: [DONE]\n\n").await;
                finish(&outcome, usage, Some(format!("cosy:{msg}")));
            } else {
                emit_frame(&tx, "data: [DONE]\n\n").await;
                finish(&outcome, usage, None);
            }
            return;
        }

        let code = resp.status().as_u16();
        let retry_after = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).map(str::to_string);
        let raw = resp.text().await.unwrap_or_default();

        // 401：只在第一次尝试续 token 重来（续完还 401 是账号问题，别再打）
        if code == 401 && attempt == 1 {
            let renewed = qoder_api::force_refresh_job_token(&store, &account).await;
            if let Some(rt) = renewed.filter(|t| !t.trim().is_empty()) {
                job_token = rt;
                continue;
            }
        }

        // 排队/资源受限：**立即报错，不重试、不降级、不换号**（源码注释三点理由原样成立）
        if let Some(queued) = queue_wait_ms(&raw) {
            let secs = (queued / 1000).max(1);
            let msg = format!("模型「{model_key}」当前不可用（上游资源受限，{secs} 秒后可重试）。请换其它模型，或在账号页切换 Qoder 账号。");
            for f in emit_error_frames(&msg) {
                emit_frame(&tx, &f).await;
            }
            emit_frame(&tx, "data: [DONE]\n\n").await;
            finish(&outcome, None, Some(format!("queued:{model_key}")));
            return;
        }

        let retryable = code == 408 || code == 429 || code >= 500;
        if retryable && attempt < MAX_ATTEMPTS {
            let backoff = (1i64 << (attempt - 1)) * 1000;
            let wait = retry_after_ms(retry_after.as_deref()).unwrap_or(backoff).min(MAX_RETRY_WAIT_MS);
            tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
            continue;
        }

        let capped: String = raw.chars().take(200).collect();
        for f in emit_error_frames(&format!("上游 HTTP {code}: {capped}")) {
            emit_frame(&tx, &f).await;
        }
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some(format!("http:{code}")));
        return;
    }
    finish(&outcome, None, Some("exhausted".into()));
}

// ---------------------------------------------------------------------------
// 测试（纯逻辑，无网络；selfCheck 的等价断言）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// catalog/config 会经 `storage::store()` 取值；单测按 gateway/tests.rs 同款惯例初始化
    /// 进程私有临时目录（OnceLock 先到先得，纯测试用途无冲突）。
    fn ensure_store() {
        crate::storage::init(std::env::temp_dir().join(format!("tw-qoder-test-{}", std::process::id())));
    }

    fn body_src() -> Value {
        serde_json::from_str(
            r#"{"model":"Ultimate","stream":false,
             "messages":[
               {"role":"system","content":"你是助手"},
               {"role":"developer","content":"要简洁"},
               {"role":"user","content":"你好"},
               {"role":"assistant","content":null,"tool_calls":[
                  {"id":"t1","type":"function","function":{"name":"f","arguments":"{}"}}]},
               {"role":"tool","tool_call_id":"t1","content":"结果"}
             ],
             "max_tokens":123,"temperature":0.3,"top_p":0.9,"stop":["\n"],
             "tool_choice":"auto","reasoning_effort":"minimal"}"#,
        )
        .unwrap()
    }

    #[test]
    fn prepare_body_has_required_top_level_fields() {
        ensure_store();
        // selfCheck 550-558 行等价：顶层必带 + stream/chat_task/session_type/version 钉死
        let b = prepare_body(&body_src());
        for k in [
            "business",
            "request_id",
            "request_set_id",
            "chat_record_id",
            "session_id",
            "chat_task",
            "model_config",
            "messages",
            "tools",
            "parameters",
            "system",
            "version",
        ] {
            assert!(b.get(k).is_some(), "缺少 {k}");
        }
        assert_eq!(b["stream"], Value::Bool(true));
        // stream=false 客户端请求也强制 true（顶层字段表原样断言）
        assert_eq!(b["chat_task"], "FREE_INPUT");
        assert_eq!(b["session_type"], "qodercli");
        assert_eq!(b["version"], "3");
        assert_eq!(b["agent_id"], "agent_common");
        assert_eq!(b["task_id"], "common");
        assert_eq!(b["source"], 1);
        assert!(b["custom_model"].is_null());
        let rid = b["request_id"].as_str().unwrap();
        assert_eq!(rid, b["request_set_id"].as_str().unwrap());
        assert_eq!(rid, b["chat_record_id"].as_str().unwrap());
        assert!(!b["chat_context"].as_object().unwrap().contains_key("_"));
    }

    #[test]
    fn prepare_body_resolves_ultimate() {
        ensure_store();
        let b = prepare_body(&body_src());
        let mc = &b["model_config"];
        assert_eq!(mc["key"], "ultimate");
        assert_eq!(mc["format"], "openai");
        assert_eq!(mc["source"], "system");
        assert_eq!(mc["enable"], true);
        assert_eq!(mc["display_name"], "极致");
        assert_eq!(mc["is_vl"], true);
        assert_eq!(mc["is_reasoning"], true);
        assert_eq!(mc["price_factor"], 0.8); // 官方真实倍率（builtin 快照 ultimate=0.8）
        assert_eq!(b["session_id"].as_str().unwrap().starts_with("q2a-"), true);
    }

    #[test]
    fn prepare_body_system_join_and_message_transform() {
        ensure_store();
        let b = prepare_body(&body_src());
        assert_eq!(b["system"], "你是助手\n\n要简洁");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3); // system/developer 被抽走

        // 带 tool_calls：content 必须是空串，tool_calls 保留
        let assistant = &msgs[1];
        assert!(assistant.get("tool_calls").is_some());
        assert_eq!(assistant["content"], "");

        // tool 角色不发 contents，tool_call_id 保留
        let tool = &msgs[2];
        assert_eq!(tool["role"], "tool");
        assert!(tool.get("contents").is_none());
        assert_eq!(tool["tool_call_id"], "t1");

        // 缓存标记恰好一个，且落在最后一条非 tool 的可缓存消息（这里是 user=msgs[0]）
        let parts = msgs[0]["contents"].as_array().unwrap();
        assert_eq!(parts[0]["cache_control"]["type"], "ephemeral");
        assert!(msgs[1].get("contents").is_none());
    }

    #[test]
    fn prepare_body_parameters() {
        ensure_store();
        let b = prepare_body(&body_src());
        let p = &b["parameters"];
        assert_eq!(p["max_tokens"], 123);
        assert_eq!(p["reasoning_effort"], "low");
        assert!((p["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-9);
        assert_eq!(p["top_p"], 0.9);
        assert_eq!(p["stop"].as_array().unwrap()[0], "\n");
        assert_eq!(p["tool_choice"], "auto");
        // 缺省 max_tokens=32000
        let empty = prepare_body(&serde_json::json!({"model":"auto","messages":[]}));
        assert_eq!(empty["parameters"]["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn normalize_effort_and_stop() {
        assert_eq!(normalize_effort("minimal"), Some("low"));
        assert_eq!(normalize_effort("high"), Some("high"));
        assert_eq!(normalize_effort("weird"), None);
        assert_eq!(normalize_effort("AUTO"), None);
        let s = stop_from(&Value::String("x".into())).unwrap();
        assert_eq!(s.as_array().unwrap().len(), 1);
        assert!(stop_from(&Value::String(String::new())).is_none());
        assert!(stop_from(&Value::Array(vec![])).is_none());
        assert!(stop_from(&serde_json::json!({"a":1})).is_none());
    }

    #[test]
    fn max_completion_tokens_preferred() {
        ensure_store();
        let b = prepare_body(&serde_json::json!({"model":"auto","messages":[],
            "max_completion_tokens":7,"max_tokens":99}));
        assert_eq!(b["parameters"]["max_tokens"], 7);
    }

    #[test]
    fn cache_marker_not_added_if_present() {
        let mut msgs = vec![serde_json::json!({"role":"user","contents":[
            {"type":"text","text":"a","cache_control":{"type":"ephemeral"}}]})];
        let before = serde_json::to_string(&msgs).unwrap();
        ensure_cache_marker(&mut msgs);
        assert_eq!(serde_json::to_string(&msgs).unwrap(), before);
    }

    #[test]
    fn queue_wait_three_cases_like_selfcheck() {
        assert_eq!(queue_wait_ms(r#"{"code":"10605","retryAfterMs":1500}"#), Some(1500));
        assert_eq!(queue_wait_ms(r#"{"queue":{"isQueued":true,"waitTime":3000}}"#), Some(3000));
        assert_eq!(queue_wait_ms(r#"{"code":"ok"}"#), None);
        // 文档实录的多层转义 403：retryAfterSeconds=30 → 30000ms
        let nested = r#"{"code":"403","message":"{\"code\":\"10605\",\"message\":\"{\\\"isQueued\\\":true,\\\"modelKey\\\":\\\"auto\\\",\\\"retryAfterSeconds\\\":30,\\\"subCode\\\":\\\"RESOURCE_LIMIT\\\"}\"}"}"#;
        assert_eq!(queue_wait_ms(nested), Some(30_000));
        // looks_queued 但抠不到数 → 默认 2000
        assert_eq!(queue_wait_ms("RESOURCE_LIMIT"), Some(2000));
    }

    #[test]
    fn retry_after_parses_seconds() {
        assert_eq!(retry_after_ms(Some("5")), Some(5000));
        assert_eq!(retry_after_ms(Some(" 0.5")), Some(500));
        assert_eq!(retry_after_ms(None), None);
        assert_eq!(retry_after_ms(Some("abc")), None);
    }

    #[test]
    fn tool_indexer_remaps_parallel_calls() {
        // selfCheck 813-823 行等价：两个新 id → 0、1
        let mut chunk: Value = serde_json::from_str(
            r#"{"choices":[{"delta":{"tool_calls":[
                 {"id":"a","index":0,"function":{"name":"f1","arguments":"{}"}},
                 {"id":"b","index":0,"function":{"name":"f2","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();
        let mut ix = ToolIndexer::default();
        ix.remap(&mut chunk);
        let tcs = chunk["choices"][0]["delta"]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs[0]["index"], 0);
        assert_eq!(tcs[1]["index"], 1);
    }

    #[test]
    fn tool_indexer_continuation_keeps_cur() {
        let mut ix = ToolIndexer::default();
        let mut c1: Value = serde_json::from_str(r#"{"choices":[{"delta":{"tool_calls":[{"id":"a","index":0}]}}]}"#).unwrap();
        assert!(!ix.remap(&mut c1)); // index 本来=0，未改写 → 可透传
        let mut c2: Value = serde_json::from_str(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{"}}]}}]}"#).unwrap();
        assert!(!ix.remap(&mut c2)); // 无新 id：index 保持 0
        let mut c3: Value = serde_json::from_str(r#"{"choices":[{"delta":{"tool_calls":[{"id":"b","index":0}]}}]}"#).unwrap();
        assert!(ix.remap(&mut c3)); // 新 id → index 改 1 → changed
        assert_eq!(c3["choices"][0]["delta"]["tool_calls"][0]["index"], 1);
    }

    #[test]
    fn envelope_fast_path_injects_model_only() {
        let inner = r#"{"choices":[{"index":0,"delta":{"content":"hi"}}]}"#;
        let mut en = EnvelopeSse::new("Ultimate");
        let mut out: Vec<String> = Vec::new();
        let data = format!("data: {{\"statusCodeValue\":200,\"body\":{}}}", json_quote(inner));
        assert!(en.push_line(&data, &mut |f| out.push(f)));
        assert!(en.push_line("", &mut |f| out.push(f)));
        assert_eq!(out.len(), 1);
        assert!(out[0].starts_with("data: {\"model\":\"Ultimate\","));
        assert!(out[0].contains(r#""content":"hi""#));
        assert!(out[0].ends_with("\n\n"));
    }

    #[test]
    fn envelope_finish_and_error_and_usage() {
        // usage 信封帧 + finish 事件 + 错误状态：三种语义各钉一条
        let mut en = EnvelopeSse::new("m");
        let mut out: Vec<String> = Vec::new();
        let usage_env = serde_json::json!({"statusCodeValue":200,"body":r#"{"usage":{"prompt_tokens":3},"choices":[]}"#});
        let chunk_env = serde_json::json!({"statusCodeValue":500,"body":"{\"message\":\"引擎炸了\"}"});
        assert!(en.push_line(&format!("data: {usage_env}"), &mut |f| out.push(f)));
        assert!(en.push_line("", &mut |f| out.push(f)));
        assert_eq!(en.usage().unwrap()["prompt_tokens"], 3);
        assert!(en.push_line("event: finish", &mut |f| out.push(f)));
        assert!(!en.push_line("", &mut |f| out.push(f))); // finish → 停
        assert!(en.push_line("data: whatever", &mut |f| out.push(f)) == false);
        let mut en2 = EnvelopeSse::new("m");
        assert!(en2.push_line(&format!("data: {chunk_env}"), &mut |_| {})); // 攒行不成帧
        assert!(!en2.push_line("", &mut |_| {})); // 空行 flush → 停止
        assert_eq!(en2.error(), Some("引擎炸了"));
    }

    #[test]
    fn envelope_multiline_data_joined_and_event_spaces() {
        let mut en = EnvelopeSse::new("m");
        let mut out: Vec<String> = Vec::new();
        assert!(en.push_line("event: message", &mut |f| out.push(f)));
        assert!(en.push_line("event: delta", &mut |f| out.push(f)));
        // 两行 data 合成一个 JSON 才解析（data 逐段 \n 拼接）
        assert!(en.push_line("data: {\"statusCodeValue\":200,", &mut |f| out.push(f)));
        assert!(en.push_line(r#"data:   "body":"{}"}"#, &mut |f| out.push(f)));
        assert!(en.push_line("", &mut |f| out.push(f)));
        // 多行 data 拼成一个 JSON 才解析；envBody="{}" 不以 `{"` 开头 → 走普通路径（插 model 重序列化）
        assert_eq!(out.len(), 1);
        let v: Value = serde_json::from_str(out[0]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(v["model"], "m");
    }

    #[test]
    fn cosy_headers_fixed_values() {
        ensure_store();
        let h = cosy_headers("Bearer COSY.X.S", "KEY", "1700000000", "u1", "ultimate", false, "org1", &["a".into(), "b".into()]);
        let get = |k: &str| h.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        assert_eq!(get("Accept").unwrap(), "text/event-stream");
        assert_eq!(get("Cache-Control").unwrap(), "no-cache");
        assert_eq!(get("Content-Type").unwrap(), "application/json");
        assert_eq!(get("Cosy-Business-Product").unwrap(), "cli");
        assert_eq!(get("Cosy-Business-Type").unwrap(), "agent");
        assert_eq!(get("Cosy-ClientType").unwrap(), "5");
        assert_eq!(get("Cosy-Data-Policy").unwrap(), "disagree");
        assert_eq!(get("Cosy-Date").unwrap(), "1700000000");
        assert_eq!(get("Cosy-Key").unwrap(), "KEY");
        assert_eq!(get("Cosy-MachineOS").unwrap(), machine::MACHINE_OS);
        assert_eq!(get("Cosy-Scene").unwrap(), "assistant");
        assert_eq!(get("Cosy-User").unwrap(), "u1");
        assert_eq!(get("Cosy-Version").unwrap(), "1.1.74");
        assert_eq!(get("Login-Version").unwrap(), "v2");
        assert_eq!(get("X-Model-Key").unwrap(), "ultimate");
        assert_eq!(get("X-Model-Source").unwrap(), "system");
        assert_eq!(get("Cosy-Organization-Id").unwrap(), "org1");
        assert_eq!(get("Cosy-Organization-Tags").unwrap(), "a,b");
        assert!(h.iter().all(|(k, _)| *k != "User-Agent"));
        // org 空 → 不发这两头
        let h2 = cosy_headers("a", "k", "1", "u", "m", true, "", &[]);
        assert!(h2.iter().all(|(k, _)| *k != "Cosy-Organization-Id" && *k != "Cosy-Organization-Tags"));
    }

    #[test]
    fn infer_chat_url_shape() {
        let u = infer_chat_url(qoder_api::MODE_GLOBAL);
        assert_eq!(u, format!("https://api2.qoder.sh{NATIVE_INFER_PATH}{NATIVE_INFER_QUERY}"));
        assert!(infer_chat_url(qoder_api::MODE_CN).starts_with("https://gateway.qoder.com.cn"));
    }

    #[test]
    fn emit_error_frames_shape() {
        let f = emit_error_frames("boom");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], "event: error\ndata: \"boom\"\n\n");
        let v: Value = serde_json::from_str(f[1]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(v["error"]["message"], "boom");
        assert_eq!(v["error"]["type"], "api_error");
    }

    #[test]
    fn runtime_fields_cache_hit_and_clear() {
        let uid = format!("uu-{}", now_ms());
        let r1 = runtime_fields_for(&uid, "jt1", "", &[], true);
        let r2 = runtime_fields_for(&uid, "jt1", "", &[], true);
        assert_eq!(r1.encrypt_user_info, r2.encrypt_user_info); // 命中缓存字节一致
        let r3 = runtime_fields_for(&uid, "jt2", "", &[], true);
        assert_ne!(r1.encrypt_user_info, r3.encrypt_user_info); // key 含 jobToken
    }
}
