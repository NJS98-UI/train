//! `TraeIntlUpstream.kt` 的 Rust 对应体：国际服（SG）session 式聊天协议。
//! 协议权威 = 本类源码（浏览器抓包实证）；`/workspace/app` 文档中已过时的部分（query 对象形状等）一律不采。
//!
//! 三段式：① POST /api/remote/v1/chat_sessions → ② POST .../{sid}/commit →
//! ③ GET .../{sid}/events?reply_to_message_id={mid}（SSE，plan_item 累积快照→diff 成增量）。
//! 多轮上下文只能靠复用 `chat_session_id`（实测 `initial_message.content` 非空会让上游**完全不生成**，
//! 且回填 sid 走"只建不续"仍拿新会话——该限制原样保留，见 [session_key] 注释）。

use super::trae::{chat_client, emit_frame, LineBuffer};
use super::{finish, json_quote, StreamOutcome};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::Sender;

pub const HOST: &str = "https://core-normal.trae.ai";
pub const EP_SESSIONS: &str = "/api/remote/v1/chat_sessions";

/// 与账号区域无关；国际服 web 端固定 UA。
pub const UA_WEB: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

const NO_SESSION: &str = "\u{0}NO_SESSION";

/// 国际服 web 通用头（不含 Accept 差异）。
pub fn web_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Cloud-IDE-JWT {token}")),
        ("X-Trae-Client-Type", "web".into()),
        ("x-user-region", "US".into()),
        ("Origin", "https://work.trae.ai".into()),
        ("Referer", "https://work.trae.ai/".into()),
        ("User-Agent", UA_WEB.into()),
    ]
}

/// POST 请求（json body）附加头。
pub fn post_headers() -> Vec<(&'static str, String)> {
    vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "application/json, text/plain, */*".into()),
    ]
}

/// GET 事件流附加头。
pub fn stream_get_headers() -> Vec<(&'static str, String)> {
    vec![("Accept".into(), "text/event-stream".into())]
}

// ---------------------------------------------------------------------------
// 纯函数：prompt / sessionKey / toolSetting / createBody（单测逐字节断言）
// ---------------------------------------------------------------------------

fn flatten_content(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut sb = String::new();
            for p in parts {
                if p.get("type").and_then(|v| v.as_str()).unwrap_or("") == "text" {
                    sb.push_str(p.get("text").and_then(|v| v.as_str()).unwrap_or(""));
                }
            }
            sb
        }
        _ => String::new(), // tool 结果只带 tool_call_id 而无 content → 空串（已知上限）
    }
}

/// `promptOf`：最后一条 user 消息文本。
pub fn prompt_of(body: &Value) -> String {
    let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) else { return String::new() };
    for m in msgs.iter().rev() {
        if !m.is_object() {
            continue;
        }
        if m.get("role").and_then(|v| v.as_str()).unwrap_or("") == "user" {
            return flatten_content(m);
        }
    }
    String::new()
}

/// `sessionKey`：系统提示（每条 system content 累加、每步 <512 才继续加）+ `\0` + 首条 user，
/// 总长截前 512 字符。无 user → 空串（chat_stream 直接报错）。
/// ⚠️ 实测上游 `POST /chat_sessions` **只建不续**（回填 sid 也拿新会话）——缓存先留着，
/// 抓到真实"发第二条消息"的形状再改 [create_body] 即可生效（同 Kotlin 注释）。
pub fn session_key(body: &Value) -> String {
    let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) else { return String::new() };
    let mut sys = String::new();
    let mut first_user = String::new();
    for m in msgs {
        if !m.is_object() {
            continue;
        }
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "system" && sys.chars().count() < 512 {
            sys.push_str(&flatten_content(m));
        }
        if role == "user" && first_user.is_empty() {
            first_user = flatten_content(m);
            break;
        }
    }
    if first_user.trim().is_empty() {
        return String::new();
    }
    let key = format!("{sys}\u{0}{first_user}");
    key.chars().take(512).collect()
}

/// `toolSetting`：OpenAI tools → `tool_plus_setting`（双重编码 JSON 字符串）。
/// 形状未实测（`tool_call_info.name` 恒空），失败不影响聊天：建会话被拒会摘掉重试一次。
pub fn tool_setting(prepared: &Value) -> Option<String> {
    let arr = prepared.get("tools").and_then(|v| v.as_array())?;
    let mut list: Vec<Value> = Vec::new();
    for t in arr {
        // Kotlin：`optJSONObject(i)?.optJSONObject("function") ?: continue`（无 function 跳过本条）
        let Some(f) = t.get("function").filter(|v| v.is_object()) else { continue };
        let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if name.is_empty() {
            continue;
        }
        let description = f.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let parameters = f
            .get("parameters")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        list.push(serde_json::json!({ "name": name, "description": description, "parameters": parameters }));
    }
    if list.is_empty() {
        return None;
    }
    Some(Value::Object({
        let mut o = Map::new();
        o.insert("tools".into(), Value::Array(list));
        o
    })
    .to_string())
}

/// `createBody`：字段逐字对齐源码 295-333 行。⚠️ `initial_message.content` **必须空数组**。
pub fn create_body(user_text: &str, model: &str, uid: &str, session_id: &str) -> Value {
    // query 是 **JSON 字符串**（不是对象）
    let query = Value::Array(vec![serde_json::json!({
        "type": "text", "data": { "content": user_text }
    })])
    .to_string();
    let common = Value::Object({
        let mut o = Map::new();
        o.insert("language".into(), Value::String("zh-cn".into()));
        o.insert("app_language".into(), Value::String("en".into()));
        o.insert("quality".into(), Value::String("stable".into()));
        o.insert("app_version".into(), Value::String("1.0.0.63".into()));
        o.insert("biz_user_id".into(), Value::String(uid.to_string()));
        o.insert("user_unique_id".into(), Value::String(uid.to_string()));
        o.insert("scope".into(), Value::String("marscode-us".into()));
        o.insert("tenant".into(), Value::String("marscode".into()));
        o.insert("solo_chat_mode".into(), Value::String("work".into()));
        o.insert("user_identity".into(), Value::String("Free".into()));
        o.insert("is_freshman".into(), Value::String("0".into()));
        o.insert("is_privacy_mode".into(), Value::from(0));
        o.insert("privacy_mode".into(), Value::String("off".into()));
        o
    })
    .to_string();
    let mut im = Map::new();
    im.insert("chat_session_id".into(), Value::String(session_id.to_string()));
    im.insert("content".into(), Value::Array(vec![]));
    im.insert("query".into(), Value::String(query));
    im.insert("model_name".into(), Value::String(model.to_string()));
    im.insert("agent_id".into(), Value::String("solo_work_remote".into()));
    im.insert("agent_type".into(), Value::String("solo_work_remote".into()));
    im.insert("model_selection_strategy".into(), Value::String("auto".into()));
    im.insert("common_params".into(), Value::String(common));
    let mut body = Map::new();
    body.insert("mode".into(), Value::String("work".into()));
    body.insert("environment_id".into(), Value::String("default".into()));
    body.insert("initial_message".into(), Value::Object(im));
    body.insert("env".into(), Value::String("remote".into()));
    body.insert("auto_create_project".into(), Value::Bool(false));
    body.insert("origin".into(), Value::String("web".into()));
    Value::Object(body)
}

// ---------------------------------------------------------------------------
// 会话缓存（容量 32，插入序 FIFO）
// ---------------------------------------------------------------------------

const SESSIONS_MAX: usize = 32;

fn sessions() -> &'static Mutex<std::collections::VecDeque<(String, String)>> {
    static S: std::sync::OnceLock<Mutex<std::collections::VecDeque<(String, String)>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::VecDeque::new()))
}

fn get_session(key: &str) -> Option<String> {
    let g = sessions().lock().expect("sessions 锁中毒");
    g.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn remember_session(key: &str, sid: &str) {
    if key.is_empty() {
        return;
    }
    let mut g = sessions().lock().expect("sessions 锁中毒");
    if let Some(item) = g.iter_mut().find(|(k, _)| k == key) {
        item.1 = sid.to_string();
        return;
    }
    if g.len() >= SESSIONS_MAX {
        g.pop_front();
    }
    g.push_back((key.to_string(), sid.to_string()));
}

// ---------------------------------------------------------------------------
// 事件流状态机（339-423 行）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Output,
    Usage,
    Done,
    Error,
    Ignore,
}

/// `Parsed`。
#[derive(Clone, Debug)]
pub struct Parsed {
    pub kind: Kind,
    pub content: String,
    pub reasoning: String,
    pub usage: Option<Value>,
    pub error_text: String,
    pub tool_name: String,
    pub tool_args: String,
}

/// `deltaOf`（累积快照→增量；selfCheck 钉住）。
pub fn delta_of(prev: &str, now: &str) -> String {
    if now.is_empty() || now == prev {
        return String::new();
    }
    if now.starts_with(prev) {
        let n = prev.chars().count();
        return now.chars().skip(n).collect();
    }
    now.to_string() // 非累积增长（重写/回退/换一轮）：整段下发，宁可多也不丢
}

/// `handleEvent`。
pub fn handle_event(name: &str, data: &str) -> Option<Parsed> {
    if data.trim().is_empty() {
        return None;
    }
    let raw: Value = serde_json::from_str(data).ok()?;
    let mut p = Parsed {
        kind: Kind::Ignore,
        content: String::new(),
        reasoning: String::new(),
        usage: None,
        error_text: String::new(),
        tool_name: String::new(),
        tool_args: String::new(),
    };
    match name {
        "plan_item" => {
            p.kind = Kind::Output;
            p.content = raw.get("thought").and_then(|v| v.as_str()).unwrap_or("").to_string();
            p.reasoning =
                raw.get("reasoning_content").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let info = raw.get("tool_call_info").filter(|v| v.is_object());
            if let Some(info) = info {
                p.tool_name =
                    info.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                match info.get("params") {
                    Some(Value::String(s)) => p.tool_args = s.clone(),
                    Some(v) if !v.is_null() => p.tool_args = v.to_string(),
                    _ => p.tool_args = "{}".into(),
                }
            }
            if p.content.is_empty() && p.reasoning.is_empty() && p.tool_name.is_empty() {
                p.kind = Kind::Ignore;
            }
        }
        "token_usage" => {
            p.kind = Kind::Usage;
            p.usage = Some(raw.clone());
        }
        "done" => {
            p.kind = Kind::Done;
            p.usage = raw.get("usage").filter(|v| v.is_object()).cloned();
        }
        "error" => {
            p.kind = Kind::Error;
            p.error_text = raw
                .get("message")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| raw.get("error").and_then(|v| v.as_str()).map(str::to_string))
                .unwrap_or_else(|| raw.to_string().chars().take(200).collect());
        }
        _ => {
            p.kind = Kind::Ignore;
        }
    }
    Some(p)
}

/// SG 流累积状态（streamEvents 局部变量组），纯转换可测。
pub struct IntlStream {
    pub id: String,
    last_thought: String,
    last_reasoning: String,
    saw_tool_call: bool,
    saw_done: bool,
    last_usage: Option<Value>,
    event_name: String,
    data_buf: String,
}

impl IntlStream {
    pub fn new(now_ms: i64) -> Self {
        Self {
            id: format!("chatcmpl-{now_ms}"),
            last_thought: String::new(),
            last_reasoning: String::new(),
            saw_tool_call: false,
            saw_done: false,
            last_usage: None,
            event_name: String::new(),
            data_buf: String::new(),
        }
    }

    fn chunk_text(&self, delta: &Map<String, Value>, finish: Option<&str>) -> String {
        let mut choice = Map::new();
        choice.insert("index".into(), Value::from(0));
        choice.insert("delta".into(), Value::Object(delta.clone()));
        if let Some(f) = finish {
            choice.insert("finish_reason".into(), Value::String(f.to_string()));
        }
        let mut c = Map::new();
        c.insert("id".into(), Value::String(self.id.clone()));
        c.insert("object".into(), Value::String("chat.completion.chunk".into()));
        c.insert("created".into(), Value::from(crate::auth::model::now_sec()));
        c.insert("choices".into(), Value::Array(vec![Value::Object(choice)]));
        format!("data: {}\n\n", Value::Object(c))
    }

    /// 喂一条 SSE 行；终态返回 Some((usage, err))。
    pub fn push_line(
        &mut self,
        line: &str,
        emit: &mut dyn FnMut(String),
    ) -> Option<(Option<Value>, Option<String>)> {
        if line.is_empty() {
            // 事件边界
            let (ev, data) = (std::mem::take(&mut self.event_name), std::mem::take(&mut self.data_buf));
            if ev.is_empty() {
                return None;
            }
            let Some(r) = handle_event(&ev, &data) else { return None };
            match r.kind {
                Kind::Output => {
                    let dc = delta_of(&self.last_thought, &r.content);
                    let dr = delta_of(&self.last_reasoning, &r.reasoning);
                    self.last_thought = r.content;
                    self.last_reasoning = r.reasoning;
                    let mut d = Map::new();
                    if !dc.is_empty() {
                        d.insert("content".into(), Value::String(dc));
                    }
                    if !dr.is_empty() {
                        d.insert("reasoning_content".into(), Value::String(dr));
                    }
                    if !r.tool_name.is_empty() && !self.saw_tool_call {
                        self.saw_tool_call = true;
                        d.insert("tool_calls".into(), Value::Array(vec![tool_call_json(&r.tool_name, &r.tool_args, crate::auth::model::now_ms())]));
                        // OpenAI 要求带 tool_calls 的帧 content 为空串（覆盖上面的 dc）
                        d.insert("content".into(), Value::String(String::new()));
                    }
                    if !d.is_empty() {
                        emit(self.chunk_text(&d, None));
                    }
                }
                Kind::Usage => {
                    self.last_usage = r.usage;
                }
                Kind::Done => {
                    emit(self.chunk_text(&Map::new(), Some(if self.saw_tool_call { "tool_calls" } else { "stop" })));
                    emit("data: [DONE]\n\n".into());
                    self.saw_done = true;
                    return Some((self.last_usage.clone().or(r.usage), None));
                }
                Kind::Error => {
                    emit(format!("event: error\ndata: {}\n\n", json_quote(&r.error_text)));
                    emit("data: [DONE]\n\n".into());
                    self.saw_done = true;
                    let tag: String = r.error_text.chars().take(60).collect();
                    return Some((None, Some(format!("intl:{tag}"))));
                }
                Kind::Ignore => {}
            }
            return None;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.event_name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.data_buf.push_str(rest);
        }
        // 其余行（id:/注释/重试）忽略
        None
    }

    /// EOF 未 done：强制 stop+DONE（与 Kotlin 422 行一致，err=null）。
    pub fn finalize_eof(&mut self, emit: &mut dyn FnMut(String)) -> Option<(Option<Value>, Option<String>)> {
        if self.saw_done {
            return None;
        }
        emit(self.chunk_text(&Map::new(), Some("stop")));
        emit("data: [DONE]\n\n".into());
        Some((self.last_usage.clone(), None))
    }
}

/// `toolCall`：完整下发一次的工具调用分片（上游不给参数增量）。
pub fn tool_call_json(name: &str, args: &str, now_ms: i64) -> Value {
    serde_json::json!({
        "index": 0,
        "id": format!("call_{now_ms}"),
        "type": "function",
        "function": { "name": name, "arguments": args },
    })
}

// ---------------------------------------------------------------------------
// 泵
// ---------------------------------------------------------------------------

/// `tryCreate` 成功：`(sid, mid)`；sid 可为空串（上游没给），由调用方判 `no_session`。
pub(crate) async fn run(body: Value, token: String, uid: String, tx: Sender<String>, outcome: Arc<StreamOutcome>) {
    let user_text = prompt_of(&body);
    if user_text.trim().is_empty() {
        emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote("请求里没有 user 消息内容"))).await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("intl:empty_user_message".into()));
        return;
    }
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let key = session_key(&body);
    let reuse = get_session(&key);

    // ---------- ① 建会话（带 tools 失败就摘掉重试一次：工具字段是尽力而为） ----------
    let created_body = create_body(&user_text, &model, &uid, reuse.as_deref().unwrap_or(""));
    let ts = tool_setting(&body);
    let created = try_create(created_body.clone(), ts.clone(), &token).await;
    let created = match created {
        // 哨兵在第二元：带 tools 被拒 → 摘掉 tool_plus_setting 重试一次
        Ok((_, b)) if b == NO_SESSION && ts.is_some() => try_create(created_body, None, &token).await,
        other => other,
    };
    let (a, b) = match created {
        Err(msg) => {
            emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote(&msg))).await;
            emit_frame(&tx, "data: [DONE]\n\n").await;
            finish(&outcome, None, Some("intl_create:exception".into()));
            return;
        }
        Ok(v) => v,
    };
    if b == NO_SESSION {
        emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote(&format!("国际服建会话失败 http {a}")))).await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some(format!("intl_create:{a}")));
        return;
    }
    let (sid, mid) = (a, b);
    if sid.trim().is_empty() {
        emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote("国际服建会话未返回 chat_session_id"))).await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some("intl_create:no_session".into()));
        return;
    }
    remember_session(&key, &sid);

    // ---------- ② commit（少了它模型不会生成） ----------
    let commit_url = format!("{HOST}{EP_SESSIONS}/{sid}/commit");
    let title: String = user_text.chars().take(80).collect();
    let commit_body = serde_json::json!({ "title": title });
    let commit = post_send(commit_url, &token, &commit_body, " commit").await;
    match commit {
        Err(msg) => {
            emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote(&msg))).await;
            emit_frame(&tx, "data: [DONE]\n\n").await;
            finish(&outcome, None, Some("intl_commit:exception".into()));
            return;
        }
        Ok((code, text)) => {
            if code != 200 {
                let capped: String = text.chars().take(200).collect();
                emit_frame(
                    &tx,
                    &format!("event: error\ndata: {}\n\n", json_quote(&format!("国际服 commit 失败 http {code}: {capped}"))),
                )
                .await;
                emit_frame(&tx, "data: [DONE]\n\n").await;
                finish(&outcome, None, Some(format!("intl_commit:{code}")));
                return;
            }
        }
    }

    // ---------- ③ 订阅事件流 ----------
    let ev_url = format!("{HOST}{EP_SESSIONS}/{sid}/events?reply_to_message_id={mid}");
    let req = build_get(ev_url, &token);
    let resp = match req.send().await {
        Err(e) => {
            emit_frame(&tx, &format!("event: error\ndata: {}\n\n", json_quote(&format!("国际服事件流异常: {e}")))).await;
            emit_frame(&tx, "data: [DONE]\n\n").await;
            finish(&outcome, None, Some("intl_events:exception".into()));
            return;
        }
        Ok(r) => r,
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let capped: String = text.chars().take(200).collect();
        emit_frame(
            &tx,
            &format!(
                "event: error\ndata: {}\n\n",
                json_quote(&format!("国际服事件流失败 http {}: {capped}", status.as_u16()))
            ),
        )
        .await;
        emit_frame(&tx, "data: [DONE]\n\n").await;
        finish(&outcome, None, Some(format!("intl_events:{}", status.as_u16())));
        return;
    }
    stream_events(resp, &tx, &outcome).await;
}

async fn stream_events(mut resp: reqwest::Response, tx: &Sender<String>, outcome: &StreamOutcome) {
    let mut st = IntlStream::new(crate::auth::model::now_ms());
    let mut lb = LineBuffer::new();
    let mut settled: Option<(Option<Value>, Option<String>)> = None;
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(180), resp.chunk()).await;
        let bytes = match next {
            Err(_) | Ok(Err(_)) => break,
            Ok(Ok(None)) => break,
            Ok(Ok(Some(b))) => b,
        };
        for line in lb.feed(&bytes) {
            let mut frames: Vec<String> = Vec::new();
            let done = st.push_line(&line, &mut |f| frames.push(f));
            for f in frames {
                emit_frame(tx, &f).await;
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
    if settled.is_none() {
        let mut frames: Vec<String> = Vec::new();
        if let Some(v) = st.finalize_eof(&mut |f| frames.push(f)) {
            settled = Some(v);
        }
        for f in frames {
            emit_frame(tx, &f).await;
        }
    }
    let (usage, err) = settled.unwrap_or((None, None));
    // 与 Kotlin 一致：事件流本身不再报异常标签（异常路径在上面已 return）
    finish(outcome, usage, err);
}

/// `createSession/tryCreate`：返回 `(sid_or_httpCodeString, mid_or_NO_SESSION)`；
/// Err(String)= 异常路径的人话消息（类名占位与 Kotlin 对齐策略见模块注释）。
async fn try_create(body: Value, tool_setting: Option<String>, token: &str) -> Result<(String, String), String> {
    let payload = match &tool_setting {
        Some(ts) => {
            let mut o = body.as_object().cloned().unwrap_or_default();
            o.insert("tool_plus_setting".into(), Value::String(ts.clone()));
            Value::Object(o)
        }
        None => body,
    };
    let url = format!("{HOST}{EP_SESSIONS}");
    let (code, text) = post_send(url, token, &payload, "建会话").await?;
    if code != 200 {
        return Ok((code.to_string(), NO_SESSION.to_string()));
    }
    let root: Value = serde_json::from_str(&text).map_err(|e| format!("国际服建会话异常: json: {e}"))?;
    let data = root.get("data").filter(|v| v.is_object());
    let s = |k: &str| -> String {
        data.and_then(|d| d.get(k)).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    Ok((s("chat_session_id"), s("message_id")))
}

/// POST JSON（web 头 + JSON 收）。返回 (status as u16, text)；网络/构建异常 → Err(人话消息)。
/// `label` 只影响异常人话文案（建会话/commit 两处共用发送器）。
async fn post_send(url: String, token: &str, body: &Value, label: &str) -> Result<(u16, String), String> {
    let mut rb = chat_client().post(&url).body(body.to_string());
    for (k, v) in web_headers(token) {
        rb = rb.header(k, v);
    }
    for (k, v) in post_headers() {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(|e| format!("国际服{label}异常: reqwest: {e}"))?;
    let code = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((code, text))
}

fn build_get(url: String, token: &str) -> reqwest::RequestBuilder {
    let mut rb = chat_client().get(&url);
    for (k, v) in web_headers(token) {
        rb = rb.header(k, v);
    }
    for (k, v) in stream_get_headers() {
        rb = rb.header(k, v);
    }
    rb
}

// ---------------------------------------------------------------------------
// 测试（纯逻辑，无网络）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn user_body() -> Value {
        serde_json::json!({
            "model": "Doubao-Seed-Code",
            "messages": [
                {"role": "system", "content": "你是助手"},
                {"role": "user", "content": "你好，世界"}
            ]
        })
    }

    #[test]
    fn prompt_of_string_and_part_array() {
        assert_eq!(prompt_of(&user_body()), "你好，世界");
        let b = serde_json::json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "image_url", "image_url": {}}, {"type": "text", "text": "b"}]},
            {"role": "user", "content": [{"type": "text", "text": "最后一条"}]}
        ]});
        assert_eq!(prompt_of(&b), "最后一条");
        // tool 结果既无 content 也不是文本 part → 空串（已知上限原样保留）
        let t = serde_json::json!({"messages": [{"role": "tool", "tool_call_id": "x"}]});
        assert_eq!(prompt_of(&t), "");
    }

    #[test]
    fn session_key_shape() {
        let b = user_body();
        let k = session_key(&b);
        assert_eq!(k, "你是助手\u{0}你好，世界");
        // 无 user → 空串（run 直接报 empty_user_message 的路径前提）
        assert_eq!(session_key(&serde_json::json!({"messages": [{"role": "system", "content": "s"}]})), "");
        // 截断 512：system 200 + user 400 → 600+1 → 取前 512
        let long = serde_json::json!({"messages": [
            {"role": "system", "content": "s".repeat(200)},
            {"role": "user", "content": "u".repeat(400)}
        ]});
        let kl = session_key(&long);
        assert_eq!(kl.chars().count(), 512);
        assert!(kl.starts_with(&"s".repeat(200)));
    }

    #[test]
    fn tool_setting_shape() {
        assert!(tool_setting(&user_body()).is_none());
        let b = serde_json::json!({"tools": [
            {"type": "function", "function": {"name": "f1", "description": "d", "parameters": {"type": "object"}}},
            {"type": "function", "function": {"name": "  "}},
            {"no_function": 1}
        ]});
        let ts = tool_setting(&b).unwrap();
        let v: Value = serde_json::from_str(&ts).unwrap();
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1); // 空名与无 function 丢弃
        assert_eq!(tools[0]["name"], "f1");
        assert_eq!(tools[0]["description"], "d");
        assert_eq!(tools[0]["parameters"]["type"], "object");
        // parameters 缺失 → {}
        let b2 = serde_json::json!({"tools": [{"function": {"name": "g"}}]});
        let v2: Value = serde_json::from_str(&tool_setting(&b2).unwrap()).unwrap();
        assert_eq!(v2["tools"][0]["parameters"], serde_json::json!({}));
    }

    #[test]
    fn create_body_exact_fields() {
        let b = create_body("你好", "M1", "uid-9", "sid-7");
        assert_eq!(b["mode"], "work");
        assert_eq!(b["environment_id"], "default");
        assert_eq!(b["env"], "remote");
        assert_eq!(b["auto_create_project"], false);
        assert_eq!(b["origin"], "web");
        let im = &b["initial_message"];
        assert_eq!(im["chat_session_id"], "sid-7");
        assert!(im["content"].as_array().unwrap().is_empty()); // ⚠️ 必须空数组
        assert_eq!(im["model_name"], "M1");
        assert_eq!(im["agent_id"], "solo_work_remote");
        assert_eq!(im["agent_type"], "solo_work_remote");
        assert_eq!(im["model_selection_strategy"], "auto");
        // query 与 common_params 是**字符串**
        let q: Value = serde_json::from_str(im["query"].as_str().unwrap()).unwrap();
        assert_eq!(q[0]["type"], "text");
        assert_eq!(q[0]["data"]["content"], "你好");
        let c: Value = serde_json::from_str(im["common_params"].as_str().unwrap()).unwrap();
        assert_eq!(c["biz_user_id"], "uid-9");
        assert_eq!(c["user_unique_id"], "uid-9");
        assert_eq!(c["scope"], "marscode-us");
        assert_eq!(c["tenant"], "marscode");
        assert_eq!(c["solo_chat_mode"], "work");
        assert_eq!(c["app_version"], "1.0.0.63");
        assert_eq!(c["user_identity"], "Free");
        assert_eq!(c["is_freshman"], "0");
        assert_eq!(c["is_privacy_mode"], 0);
        assert_eq!(c["privacy_mode"], "off");
        assert_eq!(c["language"], "zh-cn");
        assert_eq!(c["app_language"], "en");
        assert_eq!(c["quality"], "stable");
    }

    #[test]
    fn delta_of_cases() {
        assert_eq!(delta_of("ab", "abc"), "c");
        assert_eq!(delta_of("ab", "ab"), "");
        assert_eq!(delta_of("ab", ""), "");
        assert_eq!(delta_of("", "x"), "x");
        // 非累积（重写）：整段下发
        assert_eq!(delta_of("你好吗", "你"), "你");
        assert_eq!(delta_of("abc", "abd"), "abd");
        // 按字符不按字节：中文前缀也能正确切
        assert_eq!(delta_of("你好", "你好世界"), "世界");
    }

    fn plan_item(data: Value) -> String {
        data.to_string()
    }

    #[test]
    fn handle_event_kinds() {
        assert!(handle_event("plan_item", "  ").is_none());
        let p = handle_event(
            "plan_item",
            &plan_item(serde_json::json!({"thought": "t", "reasoning_content": "r", "tool_call_info": {"name": "f", "params": "{\"a\":1}"}})),
        )
        .unwrap();
        assert_eq!(p.kind, Kind::Output);
        assert_eq!(p.content, "t");
        assert_eq!(p.reasoning, "r");
        assert_eq!(p.tool_name, "f");
        assert_eq!(p.tool_args, r#"{"a":1}"#);
        // params 对象 → to_string；params 缺失 → "{}"
        let p2 = handle_event("plan_item", &plan_item(serde_json::json!({"tool_call_info": {"name": " f ", "params": {"k": 2}}}))).unwrap();
        assert_eq!(p2.tool_name, "f");
        assert_eq!(p2.tool_args, r#"{"k":2}"#);
        let p3 = handle_event("plan_item", &plan_item(serde_json::json!({"tool_call_info": {"name": "g"}}))).unwrap();
        assert_eq!(p3.tool_args, "{}");
        // 全空 plan_item → Ignore
        let p4 = handle_event("plan_item", &plan_item(serde_json::json!({"thought": ""}))).unwrap();
        assert_eq!(p4.kind, Kind::Ignore);
        // usage / done / error / 未知
        let u = handle_event("token_usage", &plan_item(serde_json::json!({"prompt_tokens": 1}))).unwrap();
        assert_eq!(u.kind, Kind::Usage);
        assert_eq!(u.usage.unwrap()["prompt_tokens"], 1);
        let d = handle_event("done", &plan_item(serde_json::json!({"usage": {"total": 3}}))).unwrap();
        assert_eq!(d.kind, Kind::Done);
        assert_eq!(d.usage.unwrap()["total"], 3);
        let e = handle_event("error", &plan_item(serde_json::json!({"message": "boom"}))).unwrap();
        assert_eq!(e.kind, Kind::Error);
        assert_eq!(e.error_text, "boom");
        let i = handle_event("whatever", "{}").unwrap();
        assert_eq!(i.kind, Kind::Ignore);
    }

    fn feed(st: &mut IntlStream, ev: &str, data: &str, out: &mut Vec<String>) -> Option<(Option<Value>, Option<String>)> {
        st.push_line(&format!("event: {ev}"), &mut |f| out.push(f));
        st.push_line(&format!("data: {data}"), &mut |f| out.push(f));
        st.push_line("", &mut |f| out.push(f))
    }

    #[test]
    fn intl_stream_accumulate_then_done() {
        let mut st = IntlStream::new(111);
        assert_eq!(st.id, "chatcmpl-111");
        let mut out: Vec<String> = Vec::new();
        let r = feed(&mut st, "plan_item", &plan_item(serde_json::json!({"thought": "he"})), &mut out);
        assert!(r.is_none());
        assert_eq!(out.len(), 1);
        let v: Value = serde_json::from_str(&out[0]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(v["id"], "chatcmpl-111");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"][0]["delta"]["content"], "he");
        assert!(v.get("model").is_none()); // 帧形状：无 model 键
        let r = feed(&mut st, "plan_item", &plan_item(serde_json::json!({"thought": "hello"})), &mut out);
        assert!(r.is_none());
        let v2: Value = serde_json::from_str(&out[1]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(v2["choices"][0]["delta"]["content"], "llo"); // 累积快照 → diff
        let r = feed(&mut st, "done", &plan_item(serde_json::json!({"usage": {"u": 1}})), &mut out).unwrap();
        assert_eq!(r.1, None);
        assert_eq!(r.0.unwrap()["u"], 1);
        let stop: Value = serde_json::from_str(&out[2]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(stop["choices"][0]["finish_reason"], "stop");
        assert_eq!(out[3], "data: [DONE]\n\n");
    }

    #[test]
    fn intl_stream_tool_call_overrides_content_and_finish() {
        let mut st = IntlStream::new(1);
        let mut out: Vec<String> = Vec::new();
        feed(
            &mut st,
            "plan_item",
            &plan_item(serde_json::json!({"thought": "用工具", "tool_call_info": {"name": "f", "params": "{}"}})),
            &mut out,
        );
        let v: Value = serde_json::from_str(&out[0]["data: ".len()..].trim_end()).unwrap();
        let delta = &v["choices"][0]["delta"];
        assert_eq!(delta["content"], ""); // tool_calls 帧 content 强制空串
        assert_eq!(delta["reasoning_content"].is_null(), true); // 无 reasoning 增量不带键
        assert_eq!(delta["tool_calls"][0]["function"]["name"], "f");
        assert!(delta["tool_calls"][0]["id"].as_str().unwrap().starts_with("call_"));
        feed(&mut st, "plan_item", &plan_item(serde_json::json!({"tool_call_info": {"name": "f", "params": "{}"}})), &mut out);
        assert_eq!(out.len(), 1); // 第二个工具帧被 saw_tool_call 抑制
        feed(&mut st, "done", "{}", &mut out);
        let stop: Value = serde_json::from_str(&out[1]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(stop["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn intl_stream_error_frame_caps_tag() {
        let mut st = IntlStream::new(2);
        let mut out: Vec<String> = Vec::new();
        let long_msg = "错".repeat(100);
        let (usage, err) = feed(&mut st, "error", &plan_item(serde_json::json!({"message": long_msg})), &mut out).unwrap();
        assert!(usage.is_none());
        let tag = err.unwrap();
        assert!(tag.starts_with("intl:"));
        assert_eq!(tag.chars().count() - "intl:".len(), 60); // 截 60 字符
        assert_eq!(out[0], format!("event: error\ndata: {}\n\n", json_quote(&long_msg)));
        assert_eq!(out[1], "data: [DONE]\n\n");
    }

    #[test]
    fn intl_finalize_eof_and_done_suppression() {
        let mut st = IntlStream::new(3);
        let mut out: Vec<String> = Vec::new();
        feed(&mut st, "token_usage", &plan_item(serde_json::json!({"t": 5})), &mut out);
        assert!(out.is_empty()); // usage 不上帧
        let (usage, err) = st.finalize_eof(&mut |f| out.push(f)).unwrap();
        assert_eq!(usage.unwrap()["t"], 5);
        assert!(err.is_none());
        let stop: Value = serde_json::from_str(&out[0]["data: ".len()..].trim_end()).unwrap();
        assert_eq!(stop["choices"][0]["finish_reason"], "stop");
        assert_eq!(out[1], "data: [DONE]\n\n");
        // 已 done 后 EOF 不再补帧
        let mut st2 = IntlStream::new(3);
        let mut out2: Vec<String> = Vec::new();
        feed(&mut st2, "done", "{}", &mut out2);
        assert!(st2.finalize_eof(&mut |f| out2.push(f)).is_none());
        assert_eq!(out2.len(), 2);
    }

    #[test]
    fn header_sets() {
        let w = web_headers("T");
        assert!(w.contains(&("Authorization", "Cloud-IDE-JWT T".to_string())));
        assert!(w.contains(&("x-user-region", "US".to_string())));
        assert!(w.contains(&("X-Trae-Client-Type", "web".to_string())));
        assert!(w.contains(&("Origin", "https://work.trae.ai".to_string())));
        assert_eq!(w.len(), 6);
        let hs = post_headers();
        let p: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert!(p.contains(&("Content-Type", "application/json")));
        assert!(p.contains(&("Accept", "application/json, text/plain, */*")));
        // GET events 仅 Accept（源码 239-242：无 Cache-Control）
        assert_eq!(stream_get_headers().len(), 1);
    }

    #[test]
    fn tool_call_json_shape() {
        let v = tool_call_json("f", "{\"a\":1}", 123);
        assert_eq!(v["index"], 0);
        assert_eq!(v["id"], "call_123");
        assert_eq!(v["type"], "function");
        assert_eq!(v["function"]["name"], "f");
        assert_eq!(v["function"]["arguments"], "{\"a\":1}");
    }

    #[test]
    fn sessions_cache_fifo_and_update() {
        remember_session("k1", "s1");
        remember_session("k2", "s2");
        remember_session("k1", "s1b");
        assert_eq!(get_session("k1").as_deref(), Some("s1b")); // 更新而非追加
        assert_eq!(get_session("k2").as_deref(), Some("s2"));
        assert_eq!(get_session(""), None); // 空 key 不缓存
        for i in 0..40 {
            remember_session(&format!("x{i}"), "v");
        }
        let g = sessions().lock().unwrap();
        assert_eq!(g.len(), SESSIONS_MAX); // FIFO 封顶
        // 插入满 32 才逐条挤出队首：k1/k2 + x0..x7 共 10 条被挤出 → 队首 x8
        assert_eq!(g.front().unwrap().0, "x8");
    }
}
