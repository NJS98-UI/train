//! `TraeUpstream.kt` 的 Rust 对应体：区域/hosts、SOLO 头、请求体改写、SSE 状态机、CN 聊天泵。
//! 权威依据 = TraeUpstream.kt + TraeRegion.kt 源码（冲突已按源码修正文档偏差：
//! `normalizeTools` 实际是把 `function.parameters` **对象字符串化**，非 required/properties 补齐）。

use super::{
    finish, json_quote, new_stream, spawn_pump, ChatRequest, StreamOutcome, StreamResult, Upstream,
};
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

// ---------------------------------------------------------------------------
// TraeRegion（对应 TraeRegion.kt）
// ---------------------------------------------------------------------------

/// 区域配置：id + 五个 host + clientId。`of()` 未知/空白一律回退 CN（`fromId` 语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraeRegion {
    pub id: &'static str,
    pub agent_host: &'static str,
    pub ug_host: &'static str,
    pub user_region_header: &'static str,
    pub oauth_host: &'static str,
    pub console_host: &'static str,
    pub client_id: &'static str,
}

pub const CN: TraeRegion = TraeRegion {
    id: crate::types::region::CN,
    agent_host: "https://trae-api-cn.mchost.guru",
    ug_host: "https://api.trae.cn",
    user_region_header: "CN",
    oauth_host: "https://api.trae.com.cn",
    console_host: "https://www.trae.cn",
    client_id: "en1oxy7wnw8j9n",
};

pub const SG: TraeRegion = TraeRegion {
    id: crate::types::region::SG,
    agent_host: "https://core-normal.trae.ai",
    ug_host: "https://ug-normal.trae.ai",
    user_region_header: "",
    oauth_host: "https://ug-normal.trae.ai",
    console_host: "https://www.trae.ai",
    client_id: "ono9krqynydwx5",
};

impl TraeRegion {
    /// `companion IDE_VERSION`。
    pub const IDE_VERSION: &'static str = "0.1.43";

    /// `fromId`：trim+lowercase 精确匹配，未知回退 CN。
    pub fn of(id: &str) -> &'static TraeRegion {
        match id.trim().to_lowercase().as_str() {
            crate::types::region::SG => &SG,
            _ => &CN,
        }
    }

    /// 桌面版没有可变全局区域；“当前区域”= `config.json` 的 `trae_region`（`load()` 等价）。
    pub fn from_config() -> &'static TraeRegion {
        Self::of(&crate::config::get().trae_region)
    }

    /// `canLogin`。
    pub fn can_login(&self) -> bool {
        !self.oauth_host.is_empty() && !self.console_host.is_empty() && !self.client_id.is_empty()
    }

    /// `isSupported`。
    pub fn is_supported(&self) -> bool {
        self.id == CN.id
    }
}

// ---------------------------------------------------------------------------
// 常量（TraeUpstream.kt）
// ---------------------------------------------------------------------------

pub const EP_CHAT: &str = "/api/agent/v3/llm_utils_chat";
pub const APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
pub const IDE_VERSION_CODE: &str = "20260716";
pub const DEVICE_BRAND: &str = "83DG";
pub const OS_VERSION: &str = "Windows 11 Pro";
pub const FUNCTION: &str = "solo_work_remote";
pub const DEFAULT_MODE: &str = "DeepSeek-V4-Flash";

/// 合法模型 id 形状（信任边界，见源码注释）。
pub fn model_id_valid(m: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9._-]{1,64}$").expect("regex"))
        .is_match(m)
}

/// 上游聊天 client：connect 20s（对应 OkHttp connectTimeout(20s)）。
/// ⚠️ reqwest 0.12 无独立 readTimeout；SSE 长连接也不能设总 timeout（会掐断流）。
/// TODO: 等价物是 per-chunk 空闲超时，reqwest 需 tokio::time::timeout 包裹 chunk() 读取，
/// Kotlin readTimeout=180s 的语义在流式读取循环里用 `tokio::time::timeout` 近似（见 run_cn/read_lines）。
pub(crate) fn chat_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest chat client")
    })
}

// ---------------------------------------------------------------------------
// prepareBody（源码 90-175 行逐语义移植；入参为**已 clone 的客户端 body**，原地改写）
// ---------------------------------------------------------------------------

/// OpenAI body → Trae body。返回改写后的值（`raw` 被就地修改，对齐 Kotlin 原地语义）。
pub fn prepare_body(raw: &mut Value) {
    let obj = raw.as_object_mut().expect("body 必须是 JSON object");
    obj.insert("stream".into(), Value::Bool(true));
    obj.insert("function".into(), Value::String(FUNCTION.into()));

    if let Some(msgs) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for m in msgs.iter_mut() {
            let Some(mo) = m.as_object_mut() else { continue };
            let role = mo.get("role").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if role == "assistant" && mo.contains_key("tool_calls") {
                let tcs = mo.remove("tool_calls").unwrap_or(Value::Null);
                let kept = rebuild_tool_calls(&tcs);
                if !kept.is_empty() {
                    mo.insert("tool_calls".into(), Value::Array(kept));
                }
            }
            let content_is_str = mo
                .get("content")
                .map(|c| matches!(c, Value::String(_)))
                .unwrap_or(false);
            if content_is_str {
                let text = mo.remove("content").and_then(|v| v.as_str().map(|s| s.to_string())).unwrap();
                mo.insert(
                    "content".into(),
                    Value::Array(vec![serde_json::json!({ "type": "text", "text": text })]),
                );
            }
        }
    }

    let mut model = obj.get("model").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if model.is_empty() {
        model = DEFAULT_MODE.into();
    }
    if !model_id_valid(&model) {
        model = DEFAULT_MODE.into();
    }
    obj.insert("config_name".into(), Value::String(model.clone()));
    obj.insert("model".into(), Value::String(model));

    normalize_tool_choice(obj);
    normalize_tools(obj);
}

/// Kotlin `prepareBody` 里 tool_calls 的重建：`function`→`function_call` 摊平 +
/// 空名 function_call 丢弃；返回空数组时调用方**移除** tool_calls。
fn rebuild_tool_calls(tcs: &Value) -> Vec<Value> {
    let arr: Vec<Value> = tcs.as_array().cloned().unwrap_or_default();
    let mut kept: Vec<Value> = Vec::new();
    for mut tc in arr {
        if let Some(tc_obj) = tc.as_object_mut() {
            if let Some(f) = tc_obj.get("function").filter(|v| v.is_object()).cloned() {
                tc_obj.remove("function");
                tc_obj.insert("function_call".into(), f);
            }
        }
        let fc_blank_name = tc
            .get("function_call")
            .filter(|v| v.is_object())
            .map(|fc| fc.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().is_empty())
            .unwrap_or(false);
        if fc_blank_name {
            continue;
        }
        kept.push(tc);
    }
    kept
}

fn normalize_tool_choice(obj: &mut Map<String, Value>) {
    // Kotlin：suppress = 同时移除 tools/functions
    let Some(tc) = obj.get("tool_choice").cloned() else { return };
    match &tc {
        Value::String(s) => {
            if s.trim().eq_ignore_ascii_case("none") {
                obj.remove("tool_choice");
                obj.remove("tools");
                obj.remove("functions");
            }
        }
        Value::Object(tco) => {
            let ty = tco.get("type").and_then(|v| v.as_str()).unwrap_or("").trim().to_lowercase();
            match ty.as_str() {
                "none" => {
                    obj.remove("tool_choice");
                    obj.remove("tools");
                    obj.remove("functions");
                }
                "auto" | "required" => {
                    obj.insert("tool_choice".into(), Value::String(ty));
                }
                "function" => {
                    let mut name = tco
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        name = tco.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    }
                    let name = name.trim();
                    obj.insert(
                        "tool_choice".into(),
                        Value::String(if name.is_empty() { "auto".into() } else { name.into() }),
                    );
                }
                _ => {
                    obj.remove("tool_choice");
                }
            }
        }
        _ => {
            obj.remove("tool_choice");
        }
    }
}

fn normalize_tools(obj: &mut Map<String, Value>) {
    if !obj.contains_key("tools") {
        return;
    }
    let Some(list) = obj.get("tools").and_then(|v| v.as_array()).cloned() else { return };
    if list.is_empty() {
        obj.remove("tools");
        return;
    }
    let mut out: Vec<Value> = Vec::new();
    for mut t in list {
        let Some(to) = t.as_object_mut() else { continue };
        if !to.get("function").map(|v| v.is_object()).unwrap_or(false) {
            continue; // optJSONObject("function") ?: continue
        }
        if let Some(params) = to
            .get("function")
            .and_then(|f| f.as_object())
            .and_then(|f| f.get("parameters"))
            .filter(|p| p.is_object())
            .cloned()
        {
            if let Some(fo) = to.get_mut("function").and_then(|f| f.as_object_mut()) {
                fo.insert("parameters".into(), Value::String(params.to_string()));
            }
        }
        out.push(t);
    }
    if out.is_empty() {
        obj.remove("tools");
    } else {
        obj.insert("tools".into(), Value::Array(out));
    }
}

// ---------------------------------------------------------------------------
// SOLO 头（183-204 行）
// ---------------------------------------------------------------------------

/// `Builder.solo()` 的纯构造版：返回头键值对（测试逐键断言；发送时按此装配）。
/// `stream=true → text/event-stream`；uid 空白 → 不发 X-Uid（缺它上游挂死，见源码注释）。
pub fn solo_headers(token: &str, stream: bool, uid: &str) -> Vec<(&'static str, String)> {
    let mut h: Vec<(&'static str, String)> = Vec::new();
    h.push((
        "Accept",
        if stream { "text/event-stream".into() } else { "application/json".into() },
    ));
    h.push(("Content-Type", "application/json".into()));
    h.push(("User-Agent", format!("Trae/{}", TraeRegion::IDE_VERSION)));
    h.push(("Authorization", format!("Cloud-IDE-JWT {token}")));
    h.push(("X-Cloudide-Token", token.to_string()));
    h.push(("X-Ide-Token", token.to_string()));
    if !uid.trim().is_empty() {
        h.push(("X-Uid", uid.to_string()));
    }
    h.push(("X-App-Id", APP_ID.into()));
    h.push(("X-App-Version", "default".into()));
    h.push(("X-Ide-Version", TraeRegion::IDE_VERSION.into()));
    h.push(("X-Ide-Version-Code", IDE_VERSION_CODE.into()));
    h.push(("X-App-Version-Code", IDE_VERSION_CODE.into()));
    h.push(("X-Ide-Version-Type", "stable".into()));
    h.push(("X-Device-Type", "windows".into()));
    h.push(("X-OS-Version", OS_VERSION.into()));
    h.push(("X-Device-Brand", DEVICE_BRAND.into()));
    h.push(("Request-Traffic-Type", "prod".into()));
    h
}

/// CN 聊天 URL。
pub fn chat_url(region: &TraeRegion) -> String {
    format!("{}{}", region.agent_host, EP_CHAT)
}

// ---------------------------------------------------------------------------
// 设备身份派生（250-302 行；供签到/用量链路复用，同 Kotlin internal 语义导出）
// ---------------------------------------------------------------------------

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// `seededStream`：反复 SHA-256("<salt>:<seed>" || i 大端4字节) 直到够 nbytes。
pub fn seeded_stream(seed: &str, salt: &str, nbytes: usize) -> Vec<u8> {
    let data = format!("{salt}:{seed}");
    let mut out: Vec<u8> = Vec::new();
    let mut i: u32 = 0;
    while out.len() < nbytes {
        let mut buf = data.as_bytes().to_vec();
        buf.extend_from_slice(&i.to_be_bytes());
        out.extend_from_slice(&sha256(&buf));
        i += 1;
    }
    out.truncate(nbytes);
    out
}

/// `deviceIdFor`：15 位纯数字（%10 逐字节）。
pub fn device_id_for(uid: &str) -> String {
    seeded_stream(uid, "devid", 16)
        .iter()
        .take(15)
        .map(|b| (((*b as u16) % 10) as u8 + b'0') as char)
        .collect()
}

/// `sessionIdFor`：64 位小写 hex。
pub fn session_id_for(uid: &str) -> String {
    seeded_stream(uid, "sess", 32)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// `marketUserIdFor`：确定性 UUID v4（修正 version/variant 位）。
pub fn market_user_id_for(uid: &str) -> String {
    let mut bs = seeded_stream(uid, "market", 16);
    bs[6] = (bs[6] & 0x0F) | 0x40;
    bs[8] = (bs[8] & 0x3F) | 0x80;
    let hex = crate::crypto::cosy::hex_lower(&bs);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `uuidNoDash`：随机 v4 去连字符（32 hex）。
pub fn uuid_no_dash() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

// ---------------------------------------------------------------------------
// SSE 行状态机（894-939 行）
// ---------------------------------------------------------------------------

/// `SseResult`：单帧扫完后的产物。
#[derive(Clone, Debug, Default)]
pub struct SseResult {
    pub event: String,
    pub response: String,
    pub reasoning: String,
    pub tool_calls_raw: Option<Value>,
    pub usage: Option<Value>,
    pub finish_reason: String,
    pub error_code: i64,
    pub error_message: String,
}

/// `SseState`：行→事件闭合状态机（Kotlin：仅空行成帧；data 无换行拼接）。
#[derive(Default)]
pub struct SseState {
    event: String,
    data: String,
}

impl SseState {
    pub fn scan(&mut self, line: &str) -> Option<SseResult> {
        if line.is_empty() {
            if self.event.is_empty() {
                self.reset();
                return None;
            }
            let r = parse_event(&self.event.clone(), &self.data.clone());
            self.reset();
            return Some(r);
        } else if let Some(rest) = line.strip_prefix("event:") {
            self.event = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.data.push_str(rest);
        }
        // `:` 注释行/其他 → 忽略
        None
    }

    fn reset(&mut self) {
        self.event = String::new();
        self.data = String::new();
    }
}

fn opt_long(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        Some(Value::Bool(b)) => *b as i64,
        _ => 0,
    }
}

fn parse_event(name: &str, data: &str) -> SseResult {
    let mut r = SseResult { event: name.trim().to_string(), ..Default::default() };
    if data.is_empty() {
        return r;
    }
    let Ok(raw) = serde_json::from_str::<Value>(data) else { return r };
    let s = |k: &str| raw.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match r.event.as_str() {
        "output" => {
            r.response = s("response");
            // ⚠️ 必须是"真字符串"才取（防 JSON null → "null" 字面量，源码注释）
            r.reasoning = raw.get("reasoning_content").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if let Some(tc) = raw.get("tool_calls").filter(|v| !v.is_null()) {
                r.tool_calls_raw = Some(tc.clone());
            }
        }
        "token_usage" => r.usage = Some(raw),
        "done" => r.finish_reason = s("finish_reason"),
        "error" => {
            r.error_code = opt_long(raw.get("code"));
            r.error_message = s("message");
        }
        _ => {}
    }
    r
}

// ---------------------------------------------------------------------------
// tool_calls 清洗（851-875 行）
// ---------------------------------------------------------------------------

pub fn strip_solo_tools(raw: &Value) -> Option<Value> {
    let arr: Vec<Value> = if let Some(a) = raw.as_array() {
        a.clone()
    } else if raw.is_object() {
        vec![raw.clone()]
    } else {
        return None;
    };
    let mut out = Vec::new();
    for call in arr {
        let Some(mut call) = call.as_object().cloned() else { continue };
        if let Some(fc) = call.get("function_call").filter(|v| !v.is_null()).cloned() {
            call.remove("function_call");
            call.insert("function".into(), fc);
        }
        if let Some(fnv) = call.get_mut("function") {
            if let Some(fo) = fnv.as_object_mut() {
                fo.remove("namespace");
                fo.remove("partial_arguments");
            }
        }
        let name_empty = call
            .get("function")
            .and_then(|f| f.as_object())
            .map(|fo| fo.get("name").and_then(|v| v.as_str()).map(str::is_empty).unwrap_or(false))
            .unwrap_or(false);
        if call.get("id").and_then(|v| v.as_str()).map(|s| s.is_empty()).unwrap_or(true) {
            call.remove("id");
        }
        if call.get("type").and_then(|v| v.as_str()).map(|s| s.is_empty()).unwrap_or(true) {
            call.remove("type");
        }
        if let (true, Some(fo)) = (name_empty, call.get_mut("function")) {
            if let Some(fo) = fo.as_object_mut() {
                fo.remove("name");
            }
        }
        out.push(Value::Object(call));
    }
    Some(Value::Array(out))
}

// ---------------------------------------------------------------------------
// CN 流转换（791-849 行）——纯状态机 + 帧构造，泵与测试共用
// ---------------------------------------------------------------------------

/// `writeChunk`：id/created/model 固定形状的一个 chunk 帧文本。
pub fn write_chunk(id: &str, delta: &Map<String, Value>, finish: Option<&str>) -> String {
    let mut choice = Map::new();
    choice.insert("index".into(), Value::from(0));
    if let Some(f) = finish {
        choice.insert("finish_reason".into(), Value::String(f.to_string()));
    }
    choice.insert("delta".into(), Value::Object(delta.clone()));
    let mut chunk = Map::new();
    chunk.insert("id".into(), Value::String(id.to_string()));
    chunk.insert("object".into(), Value::String("chat.completion.chunk".into()));
    chunk.insert("created".into(), Value::from(crate::auth::model::now_sec()));
    chunk.insert("model".into(), Value::String(String::new()));
    chunk.insert("choices".into(), Value::Array(vec![Value::Object(choice)]));
    format!("data: {}\n\n", Value::Object(chunk))
}

/// CN 流的累积状态（streamSse 局部变量组）。
pub struct CnStream {
    pub id: String,
    pub pending_usage: Option<Value>,
    pub last_usage: Option<Value>,
    pub saw_done: bool,
    pub saw_tool_call: bool,
    sse: SseState,
}

impl CnStream {
    pub fn new(now_ms: i64) -> Self {
        Self {
            id: format!("chatcmpl-{now_ms}"),
            pending_usage: None,
            last_usage: None,
            saw_done: false,
            saw_tool_call: false,
            sse: SseState::default(),
        }
    }

    /// 喂一条 SSE 行；产出要写回客户端的帧（`emit`）。返回终态 settle 值：Some((usage,err))。
    pub fn push_line(&mut self, line: &str, emit: &mut dyn FnMut(String)) -> Option<(Option<Value>, Option<String>)> {
        let Some(v) = self.sse.scan(line) else { return None };
        match v.event.as_str() {
            "output" => {
                let mut delta = Map::new();
                if !v.response.is_empty() {
                    delta.insert("content".into(), Value::String(v.response));
                }
                if !v.reasoning.is_empty() {
                    delta.insert("reasoning_content".into(), Value::String(v.reasoning));
                }
                if let Some(tc) = &v.tool_calls_raw {
                    if let Some(tc) = strip_solo_tools(tc) {
                        delta.insert("tool_calls".into(), tc);
                        self.saw_tool_call = true;
                    }
                }
                if !delta.is_empty() {
                    let mut frame = write_chunk(&self.id, &delta, None);
                    // pendingUsage 的挂载时机：Kotlin 是 writeChunk 尾部检查 pendingUsage——
                    // 它属于"下一次 writeChunk 携带 usage"，见 push_line_done/usage 分支处理。
                    if let Some(u) = self.pending_usage.take() {
                        let mut val: Value =
                            serde_json::from_str(&frame[6..frame.len() - 2]).expect("chunk 帧必可解");
                        val.as_object_mut().unwrap().insert("usage".into(), u);
                        frame = format!("data: {}\n\n", serde_json::to_string(&rekey_chunk(&val)).expect("ser"));
                    }
                    emit(frame);
                }
            }
            "token_usage" => {
                self.pending_usage = v.usage.clone();
                self.last_usage = v.usage;
            }
            "done" => {
                let fin = if self.saw_tool_call { "tool_calls" } else { v.finish_reason.as_str() };
                let delta = Map::new();
                let mut frame = write_chunk(&self.id, &delta, Some(fin));
                if let Some(u) = self.pending_usage.take() {
                    let mut val: Value =
                        serde_json::from_str(&frame[6..frame.len() - 2]).expect("chunk 帧必可解");
                    val.as_object_mut().unwrap().insert("usage".into(), u);
                    frame = format!("data: {}\n\n", serde_json::to_string(&rekey_chunk(&val)).expect("ser"));
                }
                emit(frame);
                emit("data: [DONE]\n\n".into());
                self.saw_done = true;
                return Some((self.last_usage.clone(), None));
            }
            "error" => {
                emit(format!(
                    "event: error\ndata: {}\n\n",
                    json_quote(&format!("solo error code={} msg={}", v.error_code, v.error_message))
                ));
                emit("data: [DONE]\n\n".into());
                self.saw_done = true;
                return Some((None, Some(format!("solo:{}", v.error_code))));
            }
            _ => {}
        }
        None
    }

    /// EOF 未 done 的兜底（Kotlin 848 行）：补 DONE + err=interrupted。
    pub fn finalize_eof(&mut self, emit: &mut dyn FnMut(String)) -> Option<(Option<Value>, Option<String>)> {
        if self.saw_done {
            return None;
        }
        emit("data: [DONE]\n\n".into());
        Some((None, Some("interrupted".into())))
    }
}

/// Kotlin JSONObject 序列化时 usage 落在 chunk 末尾；serde BTreeMap 会按字母序排。
/// 这里只保证**语义一致**（键/值全等），不保证键序——JSON 键序对消费方无意义（契约注释已声明）。
/// rekey 仅作显式占位：直接透传。
fn rekey_chunk(v: &Value) -> &Value {
    v
}

// ---------------------------------------------------------------------------
// 行缓冲（readLine 语义）
// ---------------------------------------------------------------------------

pub(crate) struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }
    /// 按 `\n` 切出完整行（剥尾部 `\r`），等价 BufferedReader.readLine 的批量版。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..pos).collect();
            self.buf.drain(..1); // \n
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }
    /// EOF 残留半行（readLine 会把最后一段无换行内容作为一行返回）。
    pub fn flush_tail(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }
}

// ---------------------------------------------------------------------------
// 泵 + Upstream 实现
// ---------------------------------------------------------------------------

pub struct TraeUpstream;

impl Upstream for TraeUpstream {
    /// 契约见 mod.rs。CN/SG 在**取凭证之后**按账号区域一次性分派（区域快照纪律，源码 756-767 行）。
    fn stream_chat(&self, req: &ChatRequest) -> StreamResult {
        let (tx, res) = new_stream();
        let outcome = res.outcome.clone();
        let owned = req.clone();
        spawn_pump(async move { run(owned, tx, outcome).await });
        res
    }
}

/// 统一入口：加载账号 → 刷新 token → 区域分派。
pub(crate) async fn run(req: ChatRequest, tx: Sender<String>, outcome: Arc<StreamOutcome>) {
    use crate::auth::api::trae as trae_api;
    let store = crate::auth::store::TraeAuthStore::global();
    let account = if req.uid.trim().is_empty() {
        store.load()
    } else {
        store.load_by_uid(&req.uid).or_else(|| store.load())
    };
    let Some(account) = account else {
        emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote("没有可用的 Trae 账号"))).await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("no_account".into()));
        return;
    };
    let uid = account.uid.clone();
    let region = TraeRegion::of(&account.region_id);
    let token = match req.token.clone() {
        // 网关已刷新过 → 直接用（仍走 ensureFresh 判定，防止拿旧快照撞墙）
        Some(t) if !t.trim().is_empty() => trae_api::restart_with_valid_token_for(&store, &account, trae_api::REFRESH_SKEW_SEC)
            .await
            .unwrap_or(t),
        _ => trae_api::restart_with_valid_token_for(&store, &account, trae_api::REFRESH_SKEW_SEC)
            .await
            .or_else(|| nonblank(&account.access_token).map(str::to_string))
            .unwrap_or_default(),
    };
    let token = if token.trim().is_empty() { None } else { Some(token) };
    let Some(token) = token else {
        emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote("没有可用的 Trae token"))).await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("no_token".into()));
        return;
    };
    let mut body = req.body.clone();
    if body.is_null() {
        body = Value::Object(Map::new());
    }
    prepare_body(&mut body);
    if region.id != crate::types::region::CN {
        super::trae_intl::run(body, token, uid, tx, outcome).await;
        return;
    }
    run_cn(body, &token, &uid, region, tx, &outcome).await;
}

fn nonblank(s: &str) -> Option<&str> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

pub(crate) async fn emit_frame(tx: &Sender<String>, frame: &str) {
    let _ = tx.send(frame.to_string()).await;
}

async fn run_cn(
    body: Value,
    token: &str,
    uid: &str,
    region: &'static TraeRegion,
    tx: Sender<String>,
    outcome: &StreamOutcome,
) {
    let url = chat_url(region);
    let mut rb = chat_client().post(&url).body(body.to_string());
    for (k, v) in solo_headers(token, true, uid) {
        rb = rb.header(k, v);
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote(&format!("上游连接失败: {e}")))).await;
            emit_frame(&tx, "data: [DONE]\n\n").await;
            finish(outcome, None, Some(format!("transport:{}", e)));
            return;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let capped: String = text.chars().take(200).collect();
        emit_frame(
            &tx,
            &format!("event: error\ndata: {}\n\n", json_quote(&format!("upstream http {}: {}", status.as_u16(), capped))),
        )
        .await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(outcome, None, Some(format!("http:{}", status.as_u16())));
        return;
    }
    let mut st = CnStream::new(crate::auth::model::now_ms());
    let mut lb = LineBuffer::new();
    let mut resp = resp;
    let mut settled: Option<(Option<Value>, Option<String>)> = None;
    // readTimeout(180s) 近似：每次 poll 下一个 chunk 至多等 180s
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(180), resp.chunk()).await;
        match next {
            Err(_) | Ok(Err(_)) => break,
            Ok(Ok(None)) => break,
            Ok(Ok(Some(bytes))) => {
                for line in lb.feed(&bytes) {
                    let mut frames: Vec<String> = Vec::new();
                    let done = st.push_line(&line, &mut |f| frames.push(f));
                    for f in frames {
                        emit_frame(&tx, &f).await;
                    }
                    if let Some(v) = done {
                        settled = Some(v);
                        break;
                    }
                }
                if settled.is_some() {
                    break;
                }
            }
        }
    }
    if settled.is_none() {
        for line in lb.flush_tail().into_iter() {
            let mut frames: Vec<String> = Vec::new();
            let done = st.push_line(&line, &mut |f| frames.push(f));
            for f in frames {
                emit_frame(&tx, &f).await;
            }
            if let Some(v) = done {
                settled = Some(v);
                break;
            }
        }
    }
    if settled.is_none() {
        let mut frames: Vec<String> = Vec::new();
        if let Some(v) = st.finalize_eof(&mut |f| frames.push(f)) {
            settled = Some(v);
        }
        for f in frames {
            emit_frame(&tx, &f).await;
        }
    }
    let (usage, err) = settled.unwrap_or((None, None));
    finish(outcome, usage, err);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_body() -> Value {
        serde_json::from_str(r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#).unwrap()
    }

    // ---- model_id_valid：^[A-Za-z0-9._-]{1,64}$ ----

    #[test]
    fn model_id_valid_shape() {
        assert!(model_id_valid("DeepSeek-V4.Flash_1"));
        assert!(!model_id_valid(""));
        assert!(!model_id_valid("a b"));
        assert!(!model_id_valid("模型"));
        assert!(model_id_valid(&"x".repeat(64)));
        assert!(!model_id_valid(&"x".repeat(65)));
    }

    // ---- TraeRegion ----

    #[test]
    fn region_of_falls_back_to_cn() {
        assert_eq!(TraeRegion::of(" SG "), &SG);
        assert_eq!(TraeRegion::of("sg"), &SG);
        assert_eq!(TraeRegion::of(""), &CN);
        assert_eq!(TraeRegion::of("us"), &CN);
        assert!(CN.can_login());
        assert!(SG.can_login());
        // 国际服冻结为未开发：仅 CN 受支持（failover 加权，对齐 region_is_supported）
        assert!(CN.is_supported());
        assert!(!SG.is_supported());
    }

    #[test]
    fn chat_url_uses_agent_host() {
        assert_eq!(chat_url(&CN), "https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat");
        assert_eq!(chat_url(&SG), "https://core-normal.trae.ai/api/agent/v3/llm_utils_chat");
    }

    // ---- prepare_body ----

    #[test]
    fn prepare_body_basic_fields_and_model() {
        let mut b = fresh_body();
        prepare_body(&mut b);
        assert_eq!(b["stream"], true);
        assert_eq!(b["function"], FUNCTION);
        assert_eq!(b["model"], "gpt-4o");
        assert_eq!(b["config_name"], "gpt-4o");
        // content 字符串 → 单元素 text 数组
        assert_eq!(
            b["messages"][0]["content"],
            serde_json::json!([{"type": "text", "text": "hi"}])
        );

        let mut b2 = serde_json::json!({"model": "  ", "messages": []});
        prepare_body(&mut b2);
        assert_eq!(b2["model"], DEFAULT_MODE);
        assert_eq!(b2["config_name"], DEFAULT_MODE);

        let mut b3 = serde_json::json!({"model": "坏 model!", "messages": []});
        prepare_body(&mut b3);
        assert_eq!(b3["model"], DEFAULT_MODE);
    }

    #[test]
    fn prepare_body_rebuilds_assistant_tool_calls() {
        let mut b = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "c1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{}"}
                }]
            }]
        });
        prepare_body(&mut b);
        let msg = &b["messages"][0];
        assert_eq!(
            msg["tool_calls"][0]["function_call"],
            serde_json::json!({"name": "get_weather", "arguments": "{}"})
        );
        assert!(msg["tool_calls"][0].get("function").is_none());
        assert_eq!(msg["content"], serde_json::json!([{"type": "text", "text": ""}]));

        // 空 name 的 function_call 全部丢弃 → tool_calls 键整体移除
        let mut b2 = serde_json::json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "tool_calls": [{"id": "c1", "function": {"name": "  ", "arguments": ""}}]
            }]
        });
        prepare_body(&mut b2);
        assert!(b2["messages"][0].get("tool_calls").is_none());
    }

    #[test]
    fn prepare_body_tool_choice_variants() {
        // 字符串 none（大小写/空白不敏感）→ 三个键全删
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":" None ","tools":[],"functions":[]});
        prepare_body(&mut b);
        assert!(b.get("tool_choice").is_none());
        assert!(b.get("tools").is_none());
        assert!(b.get("functions").is_none());

        // 对象 auto/required → 摊平成小写字符串
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":{"type":"Required"}});
        prepare_body(&mut b);
        assert_eq!(b["tool_choice"], "required");

        // function 型：function.name 优先，取不到回落顶层 name；再空 → auto
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":{"type":"function","function":{"name":" get "}}});
        prepare_body(&mut b);
        assert_eq!(b["tool_choice"], "get");
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":{"type":"function","name":"top"}});
        prepare_body(&mut b);
        assert_eq!(b["tool_choice"], "top");
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":{"type":"function"}});
        prepare_body(&mut b);
        assert_eq!(b["tool_choice"], "auto");

        // 未知 type / 非法形状 → 删除
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":{"type":"weird"}});
        prepare_body(&mut b);
        assert!(b.get("tool_choice").is_none());
        let mut b = serde_json::json!({"model":"m","messages":[],"tool_choice":7});
        prepare_body(&mut b);
        assert!(b.get("tool_choice").is_none());
    }

    #[test]
    fn normalize_tools_stringifies_parameters() {
        // ⚠️ 文档曾写"补齐 required/properties"，源码实际是 object→字符串化，以源码为准
        let mut b = serde_json::json!({
            "model":"m","messages":[],
            "tools":[
                {"type":"function","function":{"name":"a","parameters":{"type":"object","x":1}}},
                {"type":"function"},
                {"type":"function","function":"not-obj"}
            ]
        });
        prepare_body(&mut b);
        assert_eq!(b["tools"].as_array().unwrap().len(), 1);
        let p = &b["tools"][0]["function"]["parameters"];
        assert!(p.is_string());
        assert_eq!(serde_json::from_str::<Value>(p.as_str().unwrap()).unwrap()["x"], 1);
        // 无 name 的条目不参与 tool_choice 兜底，这里保持原样
        assert_eq!(b["tools"][0]["function"]["name"], "a");

        let mut b2 = serde_json::json!({"model":"m","messages":[],"tools":[]});
        prepare_body(&mut b2);
        assert!(b2.get("tools").is_none());

        let mut b3 = serde_json::json!({"model":"m","messages":[]});
        prepare_body(&mut b3);
        assert!(b3.get("tools").is_none());
    }

    // ---- solo_headers ----

    #[test]
    fn solo_headers_verbatim_shape() {
        let h = solo_headers("tok", true, "uid1");
        let expect: Vec<(&str, &str)> = vec![
            ("Accept", "text/event-stream"),
            ("Content-Type", "application/json"),
            ("User-Agent", "Trae/0.1.43"),
            ("Authorization", "Cloud-IDE-JWT tok"),
            ("X-Cloudide-Token", "tok"),
            ("X-Ide-Token", "tok"),
            ("X-Uid", "uid1"),
            ("X-App-Id", "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8"),
            ("X-App-Version", "default"),
            ("X-Ide-Version", "0.1.43"),
            ("X-Ide-Version-Code", "20260716"),
            ("X-App-Version-Code", "20260716"),
            ("X-Ide-Version-Type", "stable"),
            ("X-Device-Type", "windows"),
            ("X-OS-Version", "Windows 11 Pro"),
            ("X-Device-Brand", "83DG"),
            ("Request-Traffic-Type", "prod"),
        ];
        assert_eq!(h.len(), expect.len());
        for (i, (k, v)) in expect.iter().enumerate() {
            assert_eq!(h[i].0, *k);
            assert_eq!(h[i].1, *v);
        }

        let h2 = solo_headers("t", false, " ");
        assert_eq!(h2[0].1, "application/json");
        assert!(h2.iter().all(|(k, _)| *k != "X-Uid"));
        assert_eq!(h2.len(), 16);
    }

    // ---- 设备身份派生 ----

    #[test]
    fn seeded_ids_are_deterministic_and_shapes() {
        assert_eq!(device_id_for("u"), device_id_for("u"));
        assert_ne!(device_id_for("u"), device_id_for("U"));
        let d = device_id_for("uid-1");
        assert_eq!(d.len(), 15);
        assert!(d.bytes().all(|b| b.is_ascii_digit()));

        let s = session_id_for("uid-1");
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let m = market_user_id_for("uid-1");
        let segs: Vec<&str> = m.split('-').collect();
        assert_eq!(&segs.iter().map(|s| s.len()).collect::<Vec<_>>(), &[8, 4, 4, 4, 12]);
        assert_eq!(segs[2].as_bytes()[0] as char, '4'); // version 位
        assert!("89ab".contains(segs[3].as_bytes()[0] as char)); // variant 位
        assert_eq!(market_user_id_for("uid-1"), m);
    }

    #[test]
    fn uuid_no_dash_shape_and_uniqueness() {
        let a = uuid_no_dash();
        let b = uuid_no_dash();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(&a[12..13], "4");
        assert!("89ab".contains(&a[16..17]));
    }

    // ---- SseState / parse_event ----

    #[test]
    fn sse_state_frames_on_blank_line_only() {
        let mut st = SseState::default();
        assert!(st.scan(": comment").is_none());
        assert!(st.scan("event: output").is_none());
        // data 行拼接（去 "data:" 前缀，含前导空格原样）
        assert!(st.scan("data: {\"response\":\"a").is_none());
        assert!(st.scan("data: b\"}").is_none()); // 仍需空行才成帧
        let r = st.scan("").unwrap();
        assert_eq!(r.event, "output");
        assert_eq!(r.response, "a b");
        // 成帧后复位：只有 data 没有 event → 空行不成帧
        assert!(st.scan("data: x").is_none());
        assert!(st.scan("").is_none());
    }

    #[test]
    fn sse_parse_event_kinds() {
        fn frame(st: &mut SseState, ev: &str, dt: &str) -> SseResult {
            st.scan(ev);
            st.scan(dt);
            st.scan("").unwrap()
        }
        let mut st = SseState::default();
        let r = frame(&mut st, "event: output", r#"data: {"response":"r","reasoning_content":null,"tool_calls":[1]}"#);
        assert_eq!(r.response, "r");
        // JSON null 必须回落空串（防 "null" 字面量，源码注释）
        assert_eq!(r.reasoning, "");
        assert_eq!(r.tool_calls_raw, Some(serde_json::json!([1])));

        let r = frame(&mut st, "event: token_usage", r#"data: {"input":3,"output":5}"#);
        assert_eq!(r.usage, Some(serde_json::json!({"input":3,"output":5})));

        let r = frame(&mut st, "event: done", r#"data: {"finish_reason":"stop"}"#);
        assert_eq!(r.finish_reason, "stop");

        // error：code 字符串/数字/布尔都走 opt_long
        let r = frame(&mut st, "event: error", r#"data: {"code":"429","message":"busy"}"#);
        assert_eq!(r.error_code, 429);
        assert_eq!(r.error_message, "busy");
        let r = frame(&mut st, "event: error", r#"data: {"code":true}"#);
        assert_eq!(r.error_code, 1);

        // 非法 JSON / 空数据 → 只带 event 名
        let r = frame(&mut st, "event: output", "data: not-json");
        assert_eq!(r.event, "output");
        assert_eq!(r.response, "");
    }

    // ---- strip_solo_tools ----

    #[test]
    fn strip_solo_tools_cleans_frames() {
        assert_eq!(strip_solo_tools(&serde_json::json!("s")), None);
        let out = strip_solo_tools(&serde_json::json!([
            {
                "id": "", "type": "function",
                "function_call": {"name": "f", "namespace": "n", "partial_arguments": "p", "arguments": "{}"},
                "extra": 1
            },
            "not-obj",
            {"id": "c2", "function": {"name": "", "arguments": "{}"}}
        ]))
        .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2); // 非对象条目跳过
        let a0 = &arr[0];
        assert!(a0.get("function_call").is_none());
        assert_eq!(a0["function"], serde_json::json!({"name":"f","arguments":"{}"})); // ns/partial 均已删除
        assert!(a0.get("id").is_none()); // 空 id 删除
        assert_eq!(a0["type"], "function");
        assert_eq!(a0["extra"], 1);
        let a1 = &arr[1];
        assert_eq!(a1["id"], "c2");
        assert!(a1["function"].get("name").is_none()); // 空 name 删除键
        // 单对象入参 → 包成数组
        let solo = strip_solo_tools(&serde_json::json!({"id": "x"})).unwrap();
        assert_eq!(solo.as_array().unwrap().len(), 1);
    }

    // ---- write_chunk / CnStream ----

    fn collected(st: &mut CnStream, lines: &[&str]) -> (Vec<String>, Option<(Option<Value>, Option<String>)>) {
        let mut frames = Vec::new();
        let mut settle = None;
        for l in lines {
            if let Some(s) = st.push_line(l, &mut |f| frames.push(f)) {
                settle = Some(s);
                break;
            }
        }
        (frames, settle)
    }

    #[test]
    fn write_chunk_frame_shape() {
        let mut delta = Map::new();
        delta.insert("content".into(), Value::String("hi".into()));
        let f = write_chunk("chatcmpl-1", &delta, None);
        assert!(f.starts_with("data: ") && f.ends_with("\n\n"));
        let v: Value = serde_json::from_str(&f[6..]).unwrap();
        assert_eq!(v["id"], "chatcmpl-1");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["model"], "");
        assert!(v["created"].as_i64().unwrap() > 0);
        assert_eq!(v["choices"][0]["index"], 0);
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
        assert!(v["choices"][0].get("finish_reason").is_none());
    }

    #[test]
    fn cn_stream_full_sequence_and_usage_mount() {
        let mut st = CnStream::new(1000);
        assert_eq!(st.id, "chatcmpl-1000");

        // 纯换行/注释不成帧
        let (frames, settle) = collected(&mut st, &["", ": ping"]);
        assert!(frames.is_empty() && settle.is_none());

        // output 帧（空 delta 不发帧）
        let (frames, _) = collected(
            &mut st,
            &["event: output", "data: {\"response\":\"\"}", ""],
        );
        assert!(frames.is_empty());

        // usage 先于任何 output → pendingUsage 挂到下一帧
        let (frames, _) = collected(&mut st, &["event: token_usage", "data: {\"input\":1,\"output\":2}", ""]);
        assert!(frames.is_empty());
        assert_eq!(st.last_usage, Some(serde_json::json!({"input":1,"output":2})));

        let (frames, _) = collected(&mut st, &["event: output", "data: {\"response\":\"hey\",\"reasoning_content\":\"rm\"}", ""]);
        assert_eq!(frames.len(), 1);
        let v: Value = serde_json::from_str(&frames[0][6..]).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "hey");
        assert_eq!(v["choices"][0]["delta"]["reasoning_content"], "rm");
        assert_eq!(v["usage"], serde_json::json!({"input":1,"output":2}));
        assert!(st.pending_usage.is_none());

        // done → finish_reason stop + usage 已耗尽 → [DONE] + settle
        let (frames, settle) = collected(&mut st, &["event: done", "data: {\"finish_reason\":\"stop\"}", ""]);
        assert_eq!(frames.len(), 2);
        let v: Value = serde_json::from_str(&frames[0][6..]).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v.get("usage").is_none());
        assert_eq!(frames[1], "data: [DONE]\n\n");
        let (usage, err) = settle.unwrap();
        assert_eq!(usage, Some(serde_json::json!({"input":1,"output":2})));
        assert_eq!(err, None);
        // 已 done → EOF 兜底不再发帧
        let (frames, settle) = {
            let mut f = Vec::new();
            let s = st.finalize_eof(&mut |x| f.push(x));
            (f, s)
        };
        assert!(frames.is_empty() && settle.is_none());
    }

    #[test]
    fn cn_stream_tool_call_overrides_finish_reason() {
        let mut st = CnStream::new(7);
        collected(
            &mut st,
            &["event: output", "data: {\"tool_calls\":[{\"id\":\"c\",\"function_call\":\"ignored\",\"function\":{\"name\":\"f\"}}]}", ""],
        );
        // function_call 非对象分支：仍会被搬到 function 键
        let mut _frames = Vec::new();
        let _s = st.push_line("event: done", &mut |f| _frames.push(f));
        assert!(_s.is_none());
        let (frames2, settle) = {
            let mut f = Vec::new();
            let s = st.push_line("", &mut |x| f.push(x));
            (f, s)
        };
        assert_eq!(frames2.len(), 2);
        // pending 未挂 usage 的空 delta 完成帧
        assert_eq!(
            serde_json::from_str::<Value>(&frames2[0][6..]).unwrap()["choices"][0]["delta"],
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&frames2[0][6..]).unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
        assert!(settle.unwrap().1.is_none());
    }

    #[test]
    fn cn_stream_error_and_eof_interrupted() {
        let mut st = CnStream::new(1);
        let (frames, settle) = collected(
            &mut st,
            &["event: error", "data: {\"code\":503,\"message\":\"no capacity\"}", ""],
        );
        assert_eq!(frames.len(), 2);
        assert!(frames[0].starts_with("event: error\ndata: "));
        let quoted: Value = serde_json::from_str(frames[0].split("data: ").nth(1).unwrap().trim()).unwrap();
        assert_eq!(quoted, "solo error code=503 msg=no capacity");
        assert_eq!(frames[1], "data: [DONE]\n\n");
        let (usage, err) = settle.unwrap();
        assert_eq!(usage, None);
        assert_eq!(err, Some("solo:503".into()));

        let mut st2 = CnStream::new(2);
        let mut f2 = Vec::new();
        let s = st2
            .finalize_eof(&mut |x| f2.push(x));
        // 全新流未 done → EOF 补 DONE + interrupted（usage 取 last_usage=None）
        assert_eq!(f2, vec!["data: [DONE]\n\n".to_string()]);
        let (usage, err) = s.unwrap();
        assert_eq!(usage, None);
        assert_eq!(err, Some("interrupted".into()));
    }

    // ---- LineBuffer ----

    #[test]
    fn line_buffer_handles_splits_cr_and_multibyte() {
        let mut lb = LineBuffer::new();
        assert_eq!(lb.feed(b"event: output\r\ndata: \xC3"), vec!["event: output"]); // \r\n 剥尾
        // 多字节一半挂起，下一片补全（A9 是 é 的延续字节）
        let lines = lb.feed(b"\xA99\nx");
        assert_eq!(lines, vec!["data: é9"]);
        // 残留 "x" + 下一个 \n → 成行
        assert_eq!(lb.feed(b"\n"), vec!["x".to_string()]);
        assert_eq!(lb.feed(b""), Vec::<String>::new());
        assert_eq!(lb.flush_tail(), None);
        lb.feed(b"tail-no-newline");
        assert_eq!(lb.flush_tail(), Some("tail-no-newline".into()));
        assert_eq!(lb.flush_tail(), None);
    }
}
