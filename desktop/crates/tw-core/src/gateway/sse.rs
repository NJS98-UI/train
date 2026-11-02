//! SSE 帧工具 + 上游错误分类/聚合/人性化（LocalFakeServer companion 纯函数的移植）。
//! 逐字节语义依据 `.monkeycode/analysis/gateway-server.md` §3.4 / §3.5 / §3.6 / §3.7。

use serde_json::{json, Value};

/// 流式成功闸门特征串：帧字符串包含 `"choices"`（含引号）即认定上游成功开始输出。
pub const CHOICES_MARKER: &str = "\"choices\"";

/// 终止哨兵帧（逐字节：`data: [DONE]\n\n`）。
pub const DONE_FRAME: &str = "data: [DONE]\n\n";

/// `data: <payload>\n\n`（前缀 `data:` 后固定一个空格；SSE 行分隔符是 `\n` 不是 `\r\n`）。
pub fn data_frame(payload: &str) -> String {
    format!("data: {}\n\n", payload)
}

/// `event: error\ndata: "<JSON 字符串字面量>"\n\n`（data 为 quote 后的字符串，对齐 JSONObject.quote）。
pub fn error_event_frame(msg: &str) -> String {
    let quoted =
        serde_json::to_string(&Value::String(msg.to_string())).unwrap_or_else(|_| "\"\"".into());
    format!("event: error\ndata: {}\n\n", quoted)
}

/// 合成「连接期/启动异常」的收尾帧序列（修复 Kotlin 已知缺陷：头已发却零字节 → 转错误事件再 settle）。
/// 返回 (error 帧, [DONE] 帧)。
pub fn synth_error_frames(msg: &str) -> (String, &'static str) {
    (error_event_frame(msg), DONE_FRAME)
}

/// §3.4 `authRelatedError`：仅 401/403 触发同账号续期；`queued:` 前缀恒 false。
pub fn auth_related_error(err: &str) -> bool {
    if err.starts_with("queued:") {
        return false;
    }
    match err.rsplit(':').next().map(str::trim).and_then(|s| s.parse::<i64>().ok()) {
        Some(code) => code == 401 || code == 403,
        None => false,
    }
}

/// §3.4 `retryableUpstreamError`：queued → true；401/403/429/>=500 → true；
/// 解析不出（网络异常/非 http 类）→ 保守 true；400/404/422 → false。
pub fn retryable_upstream_error(err: &str) -> bool {
    if err.starts_with("queued:") {
        return true;
    }
    match err.rsplit(':').next().map(str::trim).and_then(|s| s.parse::<i64>().ok()) {
        Some(code) => code == 401 || code == 403 || code == 429 || code >= 500,
        None => true,
    }
}

/// §3.6 `aggregateChunks`：SSE 帧列表 → 非流式 `chat.completion` JSON。
/// 规则（逐条对齐 Kotlin）：
/// - 只处理 trim 后以 `data:` 开头的行；空 payload / `[DONE]` / 非法 JSON 跳过
/// - `id` 取第一个非空，缺省 `chatcmpl-local`；`created` 取第一个 >0，缺省当前秒
/// - `delta.content` / `delta.reasoning_content` 逐帧拼接（reasoning 为空则不写键）
/// - `delta.tool_calls` 按 index（缺省数组下标）累积：id/name 首个非空、arguments 逐帧拼接、type 恒 function
/// - `finish_reason` 最后一个非空且 ≠ 字面 `"null"`，初值 `"stop"`；有 tool_calls 且仍为 stop → `"tool_calls"`
/// - `usage` 取最后一个含 usage 对象的帧
pub fn aggregate_chunks(frames: &[String], model: &str) -> Value {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish = "stop".to_string();
    let mut usage: Option<Value> = None;
    let mut id: Option<String> = None;
    let mut created = 0i64;
    // LinkedHashMap 语义：保持插入序（Vec<(i64, Value)> 模拟，index → 累积条目）
    let mut tool_calls: Vec<(i64, Value)> = Vec::new();

    for frame in frames {
        for line in frame.split('\n') {
            let t = line.trim();
            if !t.starts_with("data:") {
                continue;
            }
            let payload = t[5..].trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let obj: Value = match serde_json::from_str::<Value>(payload) {
                Ok(v) if v.is_object() => v,
                _ => continue,
            };
            if id.is_none() {
                if let Some(s) = obj.get("id").and_then(|x| x.as_str()) {
                    if !s.trim().is_empty() {
                        id = Some(s.to_string());
                    }
                }
            }
            if created == 0 {
                created = obj.get("created").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            if let Some(u) = obj.get("usage") {
                if u.is_object() {
                    usage = Some(u.clone());
                }
            }
            let choices = match obj.get("choices").and_then(|x| x.as_array()) {
                Some(a) => a.clone(),
                None => continue,
            };
            for c in choices {
                // 中途帧的 finish_reason 是空串/字面 "null"，只在有真值时覆盖。
                if let Some(fr) = c.get("finish_reason").and_then(|x| x.as_str()) {
                    if !fr.trim().is_empty() && fr != "null" {
                        finish = fr.to_string();
                    }
                }
                let d = match c.get("delta") {
                    Some(v) if v.is_object() => v,
                    _ => continue,
                };
                if let Some(s) = d.get("content").and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        content.push_str(s);
                    }
                }
                if let Some(s) = d.get("reasoning_content").and_then(|x| x.as_str()) {
                    if !s.is_empty() {
                        reasoning.push_str(s);
                    }
                }
                let tcs = match d.get("tool_calls").and_then(|x| x.as_array()) {
                    Some(a) => a.clone(),
                    None => continue,
                };
                for (j, tc) in tcs.iter().enumerate() {
                    let idx = tc.get("index").and_then(|x| x.as_i64()).unwrap_or(j as i64);
                    let pos = match tool_calls.iter().position(|(k, _)| *k == idx) {
                        Some(p) => p,
                        None => {
                            tool_calls.push((
                                idx,
                                json!({"index": idx, "type": "function",
                                        "function": {"name": "", "arguments": ""}}),
                            ));
                            tool_calls.len() - 1
                        }
                    };
                    let acc = &mut tool_calls[pos].1;
                    if let Some(i) = tc.get("id").and_then(|x| x.as_str()) {
                        if !i.trim().is_empty() {
                            acc["id"] = json!(i);
                        }
                    }
                    let fnv = match tc.get("function") {
                        Some(v) if v.is_object() => v.clone(),
                        _ => continue,
                    };
                    let acc_name = acc["function"]["name"].as_str().unwrap_or("").to_string();
                    if acc_name.is_empty() {
                        if let Some(n) = fnv.get("name").and_then(|x| x.as_str()) {
                            if !n.trim().is_empty() {
                                acc["function"]["name"] = json!(n);
                            }
                        }
                    }
                    if let Some(a) = fnv.get("arguments").and_then(|x| x.as_str()) {
                        if !a.is_empty() {
                            let cur = acc["function"]["arguments"].as_str().unwrap_or("").to_string();
                            acc["function"]["arguments"] = json!(cur + a);
                        }
                    }
                }
            }
        }
    }

    let mut message = json!({"role": "assistant", "content": content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        // 有工具调用时 content 必须是字符串（可为空），OpenAI 客户端据此走工具分支。
        message["tool_calls"] = Value::Array(tool_calls.into_iter().map(|(_, v)| v).collect());
        if finish == "stop" {
            finish = "tool_calls".to_string();
        }
    }
    let mut out = json!({
        "id": id.unwrap_or_else(|| "chatcmpl-local".to_string()),
        "object": "chat.completion",
        "created": if created > 0 { created } else { chrono::Utc::now().timestamp() },
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
    });
    if let Some(u) = usage {
        out["usage"] = u;
    }
    out
}

/// §3.7 `extractUpstreamError`：从攒下的帧抠上游错误文案（`data:` 行 payload 为 JSON 字符串
/// 字面量，或对象取 `error`（String 直接用；JSONObject 取 message，退 type））。首个非空命中。
pub fn extract_upstream_error(frames: &[String]) -> Option<String> {
    for frame in frames {
        for line in frame.split('\n') {
            let t = line.trim();
            if !t.starts_with("data:") {
                continue;
            }
            let payload = t[5..].trim();
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            if payload.starts_with('"') {
                let parsed: Result<Value, _> = serde_json::from_str(payload);
                if let Ok(Value::String(s)) = parsed {
                    if !s.trim().is_empty() {
                        return Some(humanize_upstream_error(&s));
                    }
                }
                continue;
            }
            let obj: Value = match serde_json::from_str::<Value>(payload) {
                Ok(v) if v.is_object() => v,
                _ => continue,
            };
            let e = match obj.get("error") {
                Some(v) => v.clone(),
                None => continue,
            };
            let msg = match &e {
                Value::String(s) => s.clone(),
                Value::Object(o) => o
                    .get("message")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| o.get("type").and_then(|x| x.as_str()).map(|s| s.to_string()))
                    .unwrap_or_default(),
                other => other.to_string(),
            };
            if !msg.trim().is_empty() {
                return Some(humanize_upstream_error(&msg));
            }
        }
    }
    None
}

/// §3.7 `humanizeUpstreamError`：保留原文，命中规则时在其后追加 `\n\n` + 中文提示。
pub fn humanize_upstream_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let hint = if lower.contains("billing daily count exceeded") {
        "Qoder 账号今天的计费次数已用完（Billing daily count exceeded）。明天会重置；急着用就换一个 Qoder 账号，或把后端切到 Trae。"
    } else if lower.contains("billing") && lower.contains("exceed") {
        "账号额度/计费次数已用尽，明天重置或换账号。"
    } else if lower.contains("quota") || lower.contains("insufficient") {
        "账号额度不足，换账号或等重置。"
    } else if lower.contains("unauthorized") || lower.contains("401") {
        "鉴权失败：Qoder 的 jobToken 可能已失效，回 App 重新登录一次。"
    } else {
        return raw.to_string();
    };
    format!("{}\n\n{}", raw, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    // —— §5.5 selfCheck：错误分类 12 条边界（换错号会白耗额度、藏真错误）——
    #[test]
    fn err_classification_boundaries() {
        assert!(retryable_upstream_error("http:401"), "401 应换号");
        assert!(retryable_upstream_error("intl_create:429"), "429 应换号");
        assert!(retryable_upstream_error("http:503"), "5xx 应换号");
        assert!(retryable_upstream_error("queued:dfmodel"), "排队应允许换号");
        assert!(!auth_related_error("queued:dfmodel"), "排队不该触发续期");
        assert!(!auth_related_error("http:429"), "429 不该触发续期");
        assert!(!auth_related_error("http:503"), "5xx 不该触发续期");
        assert!(auth_related_error("http:401"), "401 应触发续期");
        assert!(retryable_upstream_error("intl_events:exception"), "非 http 类应保守换号");
        assert!(!retryable_upstream_error("http:400"), "400 不该换号");
        assert!(!retryable_upstream_error("intl_commit:404"), "404 不该换号");
        assert!(!retryable_upstream_error("http:422"), "422 不该换号");
        assert!(retryable_upstream_error("http:500"), "500 应换号");
        // 解析不出状态码 → 保守 true（网络异常类）
        assert!(retryable_upstream_error("interrupted"));
        assert!(!auth_related_error("interrupted"));
    }

    // —— selfCheck 移植：逐帧拼接 / reasoning / finish / usage / id ——
    #[test]
    fn aggregate_text_frames_selfcheck() {
        let frames = vec![
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":100,\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"想\"}}]}\n\n".to_string(),
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你\"}}]}\n\n".to_string(),
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"}}]}\n\n".to_string(),
            "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"delta\":{}}],\"usage\":{\"total_tokens\":7}}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ];
        let agg = aggregate_chunks(&frames, "m1");
        let msg = &agg["choices"][0]["message"];
        assert_eq!(msg["content"].as_str().unwrap(), "你好", "应逐帧拼接 content");
        assert_eq!(msg["reasoning_content"].as_str().unwrap(), "想", "reasoning_content 应聚合");
        assert_eq!(agg["choices"][0]["finish_reason"].as_str().unwrap(), "stop");
        assert_eq!(agg["usage"]["total_tokens"].as_i64().unwrap(), 7, "usage 应带出");
        assert_eq!(agg["id"].as_str().unwrap(), "c1");
        assert_eq!(agg["object"].as_str().unwrap(), "chat.completion");
        assert_eq!(agg["model"].as_str().unwrap(), "m1");
        assert_eq!(agg["created"].as_i64().unwrap(), 100);
        // reasoning 为空则不写键
        assert!(aggregate_chunks(&["data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n".into()], "m")
            ["choices"]
            .is_array());
        assert!(aggregate_chunks(&["data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n".into()], "m")
            ["choices"]
            .get(0)
            .is_some());
    }

    #[test]
    fn aggregate_no_reasoning_key_when_empty() {
        let agg = aggregate_chunks(&["data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n".into()], "m");
        assert!(agg["choices"][0]["message"].get("reasoning_content").is_none());
    }

    // —— selfCheck 移植：工具调用聚合 ——
    #[test]
    fn aggregate_tool_calls_selfcheck() {
        let frames = vec![
            "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n".to_string(),
            "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n".to_string(),
            "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"北京\\\"}\"}}]}}]}\n\n".to_string(),
            "data: {\"id\":\"c2\",\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\",\"delta\":{}}]}\n\n".to_string(),
        ];
        let agg = aggregate_chunks(&frames, "m2");
        let msg = &agg["choices"][0]["message"];
        let tcs = msg["tool_calls"].as_array().expect("tool_calls 应为数组");
        assert_eq!(tcs.len(), 1, "工具调用应合并成 1 条");
        assert_eq!(tcs[0]["function"]["name"].as_str().unwrap(), "get_weather");
        assert_eq!(tcs[0]["function"]["arguments"].as_str().unwrap(), "{\"city\":\"北京\"}");
        assert!(msg.get("content").is_some(), "有工具调用时 content 键必须存在");
        assert_eq!(agg["choices"][0]["finish_reason"].as_str().unwrap(), "tool_calls");
    }

    #[test]
    fn aggregate_tool_stop_promotes_to_tool_calls() {
        let frames = vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"t\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]}}]}\n\n".to_string(),
        ];
        let agg = aggregate_chunks(&frames, "m");
        assert_eq!(agg["choices"][0]["finish_reason"].as_str().unwrap(), "tool_calls");
    }

    #[test]
    fn extract_and_humanize() {
        // JSON 字符串字面量形状
        let got = extract_upstream_error(&["event: error\ndata: \"Billing daily count exceeded\"\n\n".to_string()]);
        let s = got.unwrap();
        assert!(s.starts_with("Billing daily count exceeded"), "{s}");
        assert!(s.contains("计费次数已用完"));
        // error 对象形状
        let got = extract_upstream_error(&[
            "data: {\"error\":{\"message\":\"quota exhausted\",\"type\":\"x\"}}\n\n".to_string(),
        ]);
        let s = got.unwrap();
        assert!(s.starts_with("quota exhausted"));
        assert!(s.contains("额度不足"));
        // 无错误 → None（choices 帧不算）
        assert!(extract_upstream_error(&["data: {\"choices\":[{\"delta\":{}}]}\n\n".to_string()]).is_none());
        // 原文保留：非命中规则不加提示
        assert_eq!(humanize_upstream_error("上游 HTTP 503: busy"), "上游 HTTP 503: busy");
    }

    #[test]
    fn frame_format_bytes() {
        assert_eq!(data_frame("[DONE]"), "data: [DONE]\n\n");
        assert_eq!(DONE_FRAME, "data: [DONE]\n\n");
        // 逐字节：`event: error\ndata: "…"\n\n`（data 为 JSON 字符串字面量）
        assert_eq!(error_event_frame("boom"), "event: error\ndata: \"boom\"\n\n");
        let (a, b) = synth_error_frames("上游连接失败");
        assert!(a.starts_with("event: error\ndata: \""));
        assert!(a.ends_with("\"\n\n"));
        assert_eq!(b, "data: [DONE]\n\n");
        assert_eq!(CHOICES_MARKER, "\"choices\"");
    }
}
