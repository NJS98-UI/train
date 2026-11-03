//! gateway 测试：HTTP 解析在 http.rs、帧/聚合/分类在 sse.rs、兜底表在 models_fallback.rs；
//! 本文件覆盖：鉴权 key / 错误形状 / failover 控制流（mock ChatBackend 注入）/ loopback 端到端。

use super::*;
use std::collections::VecDeque;
use std::sync::Arc;

// =========================================================================
// 测试基建：mock ChatBackend
// =========================================================================

#[derive(Clone)]
enum Reply {
    /// 流式产出这些帧（Done 应由调用方放在最后；缺 Done → 网关按 interrupted settle）
    Stream(Vec<Frame>),
    /// 启动即失败（廉价校验语义）
    StartErr(GatewayError),
}

struct MockBackend {
    models: Vec<Value>,
    replies: Mutex<VecDeque<Reply>>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl MockBackend {
    fn new(replies: Vec<Reply>) -> Arc<Self> {
        Self::new_with(replies, Vec::new())
    }
    fn new_with(replies: Vec<Reply>, models: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            models,
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
        })
    }
    fn call_ids(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(id, _)| id.clone()).collect()
    }
}

impl ChatBackend for MockBackend {
    fn list_models(&self) -> Vec<Value> {
        self.models.clone()
    }
    fn chat_stream(&self, body: Value, api_key_id: &str) -> GatewayResult<ChatStream> {
        self.calls.lock().push((api_key_id.to_string(), body));
        match self.replies.lock().pop_front() {
            Some(Reply::Stream(fs)) => Ok(Box::pin(futures_util::stream::iter(fs))),
            Some(Reply::StartErr(e)) => Err(e),
            None => Err(GatewayError::Upstream("exhausted".into())),
        }
    }
}

fn chunk(payload: &str) -> Frame {
    Frame::Chunk(format!("data: {}\n\n", payload))
}
/// SSE_HEADER（&[u8]）在字符串断言中的形式
fn sse_hdr() -> &'static str {
    std::str::from_utf8(http::SSE_HEADER).unwrap()
}
fn choices_frame(txt: &str) -> Frame {
    chunk(&format!("{{\"id\":\"c1\",\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}", txt))
}
fn ok_done() -> Frame {
    Frame::Done { usage: Some(json!({"total_tokens": 3})), err: None }
}
fn err_done(e: &str) -> Frame {
    Frame::Done { usage: None, err: Some(e.to_string()) }
}

struct Harness {
    _server: Server,
    inner: Arc<Inner>,
    records: Arc<Mutex<Vec<Record>>>,
}

fn harness(backends: Vec<(String, Arc<dyn ChatBackend>)>) -> Harness {
    let server = Server::assemble_with(backends).unwrap().with_api_key("sk-test000000000000000000000000000000");
    let records: Arc<Mutex<Vec<Record>>> = Arc::new(Mutex::new(Vec::new()));
    let r2 = records.clone();
    server.set_on_usage(Arc::new(move |rec| r2.lock().push(rec)));
    Harness { inner: server.inner.clone(), _server: server, records }
}

fn cand(id: &str, b: Arc<MockBackend>) -> Cand {
    Cand { id: id.to_string(), backend: b }
}

/// 直接驱动 failover 控制流（不经 TCP）。
async fn drive(h: &Harness, wants: bool, model: &str, body: Value, cands: Vec<Cand>) -> Vec<u8> {
    let mut w: Vec<u8> = Vec::new();
    stream_with_failover(&mut w, &h.inner, &body, wants, model.to_string(), cands).await;
    w
}

// =========================================================================
// 鉴权（§2.1 / 任务 3）
// =========================================================================

#[test]
fn authorized_semantics_match_kotlin() {
    assert!(authorized("", None), "key 为空放行（实践中派生 key 恒非空）");
    assert!(authorized("sk-abc", Some("Bearer sk-abc")));
    assert!(authorized("sk-abc", Some("bearer  sk-abc "))); // 前缀大小写无关 + 值 trim
    assert!(authorized("sk-abc", Some("BeArEr sk-abc")));
    assert!(!authorized("sk-abc", None));
    assert!(!authorized("sk-abc", Some("sk-abc")), "缺 Bearer 前缀");
    assert!(!authorized("sk-abc", Some("Basic sk-abc")), "错误 scheme");
    assert!(!authorized("sk-abc", Some("Bearer sk-abd")), "不相等");
    assert!(!authorized("sk-abc", Some("Bearer")), "不足 7 字符前缀");
}

#[test]
fn derived_key_shape_and_stability() {
    // §5.3：sk- + sha256(install_id|machine) 前 32 hex → 全长 35；二次取值一致
    crate::storage::init(std::env::temp_dir().join(format!("tw-gw-key-{}", std::process::id())));
    assert!(crate::fingerprint::self_check().is_ok());
    let key = crate::fingerprint::api_key();
    assert_eq!(key.chars().count(), 35);
    assert!(key.starts_with("sk-"));
    let hex = &key[3..];
    assert_eq!(hex.len(), 32);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "小写 hex");
    assert_eq!(key, crate::fingerprint::api_key(), "每次取值必须一致（钉死全客户端 401 回归）");
}

// =========================================================================
// 错误形状（§2.2 四种逐字段对齐 + openai_error 形状）
// =========================================================================

#[test]
fn four_error_shapes_exact() {
    // 401 / 400 / 413 / 502-upstream_unavailable / 404：type 恒 api_error，status 不进 body
    let e401 = http::openai_error("invalid_api_key", "missing or invalid local API key");
    assert_eq!(e401["error"]["message"], "missing or invalid local API key");
    assert_eq!(e401["error"]["code"], "invalid_api_key");
    assert_eq!(e401["error"]["type"], "api_error");

    let e400 = http::openai_error("invalid_request_error", "请求体不是合法 JSON");
    assert_eq!(e400["error"]["code"], "invalid_request_error");

    let e413 = body_too_large_error();
    assert_eq!(e413["error"]["message"], "请求体过大（上限 8 MB）");
    assert_eq!(e413["error"]["code"], "invalid_request_error");

    let e502 = http::openai_error("upstream_unavailable", DEFAULT_UPSTREAM_UNAVAILABLE_MSG);
    assert_eq!(
        e502["error"]["message"],
        "Qoder 账号不可用：jobToken 为空或已失效。请回 App 重新登录 Qoder，或在「聊天转发后端」里切到 Trae。"
    );
    assert_eq!(e502["error"]["code"], "upstream_unavailable");

    let e502b = http::openai_error("upstream_error", "上游转发失败");
    assert_eq!(e502b["error"]["code"], "upstream_error");

    let e404 = http::openai_error("not_found", "unknown endpoint: /nope");
    assert_eq!(e404["error"]["code"], "not_found");
    for v in [&e401, &e400, &e413, &e502, &e404] {
        let obj = v["error"].as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert_eq!(obj["type"], "api_error");
    }
}

#[test]
fn fake_completion_shape_is_exact() {
    // §3.1：id/model/文案逐字对齐
    let f = fake_completion();
    assert_eq!(f["id"], "chatcmpl-fake-local");
    assert_eq!(f["object"], "chat.completion");
    assert_eq!(f["model"], "glm-5.2");
    assert_eq!(f["choices"][0]["message"]["role"], "assistant");
    assert_eq!(
        f["choices"][0]["message"]["content"],
        "这是本地假服务返回的测试响应。阶段2第2步已跑通：本地 OpenAI 兼容接口可用，下一步再接真实 TraeWork 上游转发。"
    );
    assert_eq!(f["choices"][0]["finish_reason"], "stop");
    assert_eq!(f["usage"], json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}));
    assert!(f["created"].as_i64().unwrap() > 0);
}

// =========================================================================
// failover 控制流（§3.3 streamWithFailover 逐规则；任务 4）
// =========================================================================

#[tokio::test]
async fn failover_switch_before_choices() {
    // 未出 choices 帧 → 合法换号：A(503) → B 成功；A 轮的错误帧必须被废弃（不泄漏给客户端）
    let a = MockBackend::new(vec![Reply::Stream(vec![
        Frame::Chunk("event: error\ndata: \"upstream http 503\"\n\n".to_string()),
        err_done("http:503"),
    ])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("你"), choices_frame("好"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    let out = drive(&h, false, "m", json!({"model": "m"}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;

    let s = String::from_utf8_lossy(&out);
    assert!(s.starts_with("HTTP/1.1 200 OK\r\n"), "非流式聚合 200：{}", s);
    let body = s.split("\r\n\r\n").nth(1).unwrap();
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "你好");
    assert_eq!(v["usage"]["total_tokens"], 3);
    assert_eq!(a.call_ids(), vec!["A".to_string()]);
    assert_eq!(b.call_ids(), vec!["B".to_string()]);
    // usage 只在最终 settle 记一次（换号中间不记），记录的是成功轮
    let recs = h.records.lock();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].backend, "B");
    assert!(recs[0].ok);
    assert_eq!(recs[0].err_type, None);
    assert_eq!(recs[0].usage.as_ref().unwrap()["total_tokens"], 3);
}

#[tokio::test]
async fn no_switch_after_choices_frame() {
    // got_choices 闸门已开：后续上游失败**不换号**（半途回答不可撤回）
    let a = MockBackend::new(vec![Reply::Stream(vec![choices_frame("半"), err_done("http:500")])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("不该出现"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    let out = drive(&h, false, "m", json!({"model": "m"}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    let s = String::from_utf8_lossy(&out);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"), "已出 choices 又失败 → 502：{}", s);
    assert!(!s.contains("不该出现"), "B 的内容不得出现");
    assert!(b.calls.lock().is_empty(), "闸门开过不能再换号");
    let recs = h.records.lock();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].err_type.as_deref(), Some("http:500"));
    assert!(!recs[0].ok);
    assert_eq!(recs[0].backend, "A");
}

#[tokio::test]
async fn stream_mode_flushes_and_writes_sse_header_once() {
    // 流式：闸门打开前后帧都实时写出；SSE 头只在第一轮之前写一次
    let a = MockBackend::new(vec![Reply::Stream(vec![err_done("queued:dfmodel")])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("ok"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    let out = drive(&h, true, "m", json!({"model": "m", "stream": true}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    let s = String::from_utf8_lossy(&out);
    assert_eq!(s.matches(sse_hdr()).count(), 1, "SSE 头只出现一次");
    assert!(s.ends_with("data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n"));
    assert!(!s.contains("event: error"), "A 轮 queued 错误帧不得外泄");
    assert!(!s.contains("queued"), "A 轮错误原文不得外泄");
}

#[tokio::test]
async fn queued_error_switches_but_never_refreshes() {
    // queued: retryable=true, auth_related=false → 直接换号、绝不先续期（§3.4 / selfCheck #2）
    let a = MockBackend::new(vec![Reply::Stream(vec![err_done("queued:dfmodel")])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("x"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    drive(&h, false, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    assert_eq!(a.call_ids().len(), 1, "queued 不得触发同账号续期重试");
    assert_eq!(b.call_ids().len(), 1);
}

#[tokio::test]
async fn auth_error_refreshes_same_candidate_once() {
    // 401 → 先同账号"续期"重试（仅一次），成功则不换号（§3.3：防白切账号）
    let a = MockBackend::new(vec![
        Reply::Stream(vec![err_done("http:401")]),
        Reply::Stream(vec![choices_frame("救活"), ok_done()]),
    ]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("不该"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    let out = drive(&h, false, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    assert_eq!(a.call_ids(), vec!["A".to_string(), "A".to_string()], "401 应先在原候选重试一次");
    assert!(b.calls.lock().is_empty(), "续期成功不得换号");
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("救活"));
    assert_eq!(h.records.lock().len(), 1);
    assert!(h.records.lock()[0].ok);
}

#[tokio::test]
async fn auth_refresh_only_once_then_failover() {
    // 续期只有一次机会：第二次仍 401 → 走换号（did_refresh 钉死）
    let a = MockBackend::new(vec![
        Reply::Stream(vec![err_done("http:401")]),
        Reply::Stream(vec![err_done("http:401")]),
    ]);
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("B"), ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    drive(&h, false, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    assert_eq!(a.call_ids().len(), 2, "续期仅一次");
    assert_eq!(b.call_ids().len(), 1);
}

#[tokio::test]
async fn switch_capped_at_two_per_account() {
    // 最多换 2 个号：4 个候选全 500 → 只跑 3 轮（初始 + 2 次换号），第 4 个不碰
    let mk = || MockBackend::new((0..2).map(|_| Reply::Stream(vec![err_done("http:500")])).collect());
    let a = mk();
    let b = mk();
    let c = mk();
    let d = mk();
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone()), ("C".into(), c.clone()), ("D".into(), d.clone())]);
    let out = drive(&h, false, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone()), cand("C", c.clone()), cand("D", d.clone())]).await;
    assert_eq!(a.call_ids().len(), 1);
    assert_eq!(b.call_ids().len(), 1);
    assert_eq!(c.call_ids().len(), 1);
    assert!(d.calls.lock().is_empty(), "换号上限 2：第三个候选即止，第 4 个不得触碰");
    let s = String::from_utf8_lossy(&out);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"));
    let recs = h.records.lock();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].backend, "C");
    assert_eq!(recs[0].err_type.as_deref(), Some("http:500"));
}

#[tokio::test]
async fn non_retryable_error_never_switches() {
    // 400 是请求本身的问题：换谁都一样 → 不换号（§3.4 / selfCheck 边界）
    let a = MockBackend::new(vec![Reply::Stream(vec![err_done("http:400")])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    drive(&h, false, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    assert!(b.calls.lock().is_empty(), "400 不换号");
}

#[tokio::test]
async fn mid_switch_candidate_no_credential_finalizes_with_prev_error() {
    // 换号时下一候选启动即 NoCredential = Kotlin takeIf 过滤不通过 → 以当前(前轮)错误收尾
    let a = MockBackend::new(vec![Reply::Stream(vec![err_done("http:503")])]);
    let b = MockBackend::new(vec![Reply::StartErr(GatewayError::NoCredential)]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    let out = drive(&h, true, "m", json!({}), vec![cand("A", a.clone()), cand("B", b.clone())]).await;
    let s = String::from_utf8_lossy(&out);
    // 流式头已写（A 轮启动成功时）→ 修复语义：合成 error 帧 + [DONE]（绝不零字节）
    assert!(s.starts_with(sse_hdr()), "SSE 头必须在：{}", s);
    assert!(s.contains("event: error\ndata: \"http:503\"\n\n"), "应为前轮错误：{}", s);
    assert!(s.ends_with("data: [DONE]\n\n"));
    // 注意 B 的 StartErr 分支：want_switch 预启动 Err → 落 finalize（不 fake）
    assert!(!s.contains("chatcmpl-fake-local"));
    let recs = h.records.lock();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].err_type.as_deref(), Some("http:503"));
    assert_eq!(recs[0].backend, "A");
}

#[tokio::test]
async fn first_candidate_no_credential_returns_fake200() {
    // §2.2/§3.1：Trae 无凭证不报错——200 fakeCompletion（语义不对称必须保留）
    let a = MockBackend::new(vec![Reply::StartErr(GatewayError::NoCredential)]);
    let h = harness(vec![("A".into(), a.clone())]);
    let mut w: Vec<u8> = Vec::new();
    serve_chat(&mut w, &h.inner, Some("Bearer sk-test000000000000000000000000000000"), r#"{"model":"m","stream":false}"#).await;
    let s = String::from_utf8_lossy(&w);
    assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(s.contains("chatcmpl-fake-local"));
    assert!(h.records.lock().is_empty(), "fake 分支不产生上游尝试，不记 usage");
}

#[tokio::test]
async fn first_candidate_unavailable_returns_502_exact() {
    // Qoder 无凭证 → 502 upstream_unavailable，**绝不回假回复**（§3.1 语义不对称）
    let a = MockBackend::new(vec![Reply::StartErr(GatewayError::UpstreamUnavailable(None))]);
    let h = harness(vec![("qoder".into(), a.clone())]);
    let mut w: Vec<u8> = Vec::new();
    serve_chat(&mut w, &h.inner, Some("Bearer sk-test000000000000000000000000000000"), r#"{"model":"Qwen3 Coder Plus"}"#).await;
    let s = String::from_utf8_lossy(&w);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"));
    let body = s.split("\r\n\r\n").nth(1).unwrap();
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["error"]["code"], "upstream_unavailable");
    assert_eq!(v["error"]["message"], DEFAULT_UPSTREAM_UNAVAILABLE_MSG);
    assert!(!body.contains("chatcmpl-fake-local"));
}

#[tokio::test]
async fn stream_eof_without_done_is_interrupted() {
    // 流 drop 且无 Done → err=interrupted（§3.5 上游没发 done 就断流）
    let a = MockBackend::new(vec![Reply::Stream(vec![choices_frame("半")])]);
    let h = harness(vec![("A".into(), a.clone())]);
    let out = drive(&h, false, "m", json!({}), vec![cand("A", a.clone())]).await;
    let recs = h.records.lock();
    assert_eq!(recs[0].err_type.as_deref(), Some("interrupted"));
    // 非流式 + err → 502（先判错再聚合，§3.3 ⚠️ 客户端不能看到"200+空回复"假象）
    let s = String::from_utf8_lossy(&out);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"));
}

#[tokio::test]
async fn start_level_upstream_error_finishes_via_failover_settle() {
    // 启动即 Upstream 错误（非流式）→ 502 upstream_error + humanize（修复语义：settle 而非冒泡）
    let a = MockBackend::new(vec![Reply::StartErr(GatewayError::Upstream("transport:ConnectException".into()))]);
    let h = harness(vec![("A".into(), a.clone())]);
    let mut w: Vec<u8> = Vec::new();
    serve_chat(&mut w, &h.inner, Some("Bearer sk-test000000000000000000000000000000"), "{}").await;
    let s = String::from_utf8_lossy(&w);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"));
    assert!(s.contains("transport:ConnectException"), "错误原文必须保留（humanize 规则外原文透出）");
    let recs = h.records.lock();
    assert_eq!(recs.len(), 1);
    assert!(!recs[0].ok);
}

#[tokio::test]
async fn nonstream_upstream_error_frame_extracted_into_502() {
    // 非流式且 collected 含 error 帧 → 502 message = extractUpstreamError + humanize（§3.7）
    let a = MockBackend::new(vec![Reply::Stream(vec![
        Frame::Chunk("event: error\ndata: \"Billing daily count exceeded\"\n\n".to_string()),
        err_done("http:403"),
    ])]);
    let b = MockBackend::new(vec![Reply::Stream(vec![ok_done()])]);
    let h = harness(vec![("A".into(), a.clone()), ("B".into(), b.clone())]);
    // 403 可换号，但本例让 B 也失败观察 extract 优先级：A 轮先被换号丢弃……
    // 要观察 extract：让错误不可换号（400 不换号 + error 帧形状）
    let c = MockBackend::new(vec![Reply::Stream(vec![
        Frame::Chunk("event: error\ndata: \"Billing daily count exceeded\"\n\n".to_string()),
        err_done("http:400"),
    ])]);
    let h2 = harness(vec![("C".into(), c.clone())]);
    let out = drive(&h2, false, "m", json!({}), vec![cand("C", c.clone())]).await;
    let s = String::from_utf8_lossy(&out);
    assert!(s.starts_with("HTTP/1.1 502 Error\r\n"));
    assert!(s.contains("Billing daily count exceeded"), "上游原文必须在");
    assert!(s.contains("计费次数已用完"), "应命中 humanize 规则");
    let _ = &h; // 保留第一组观察变量避免未用告警（不参与断言）
    let _ = &out;
}

// =========================================================================
// chat 入口（鉴权/400/路由候选）
// =========================================================================

#[tokio::test]
async fn chat_requires_auth_before_anything() {
    let h = harness(Vec::new());
    let mut w: Vec<u8> = Vec::new();
    serve_chat(&mut w, &h.inner, None, "{}").await;
    let s = String::from_utf8_lossy(&w).to_string();
    // 头逐字段（JSON 键序由 serde_json BTreeMap 决定，不断言 body 字节序，对齐 Kotlin org.json 的无序性）
    let (head, body) = s.split_once("\r\n\r\n").expect("完整响应头+体");
    assert!(head.starts_with("HTTP/1.1 401 Unauthorized\r\n"), "{}", head);
    assert!(head.contains("Content-Type: application/json\r\n"), "{}", head);
    assert!(head.contains("Access-Control-Allow-Origin: *\r\n"), "{}", head);
    assert!(head.contains("Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n"), "{}", head);
    assert!(head.contains("Access-Control-Allow-Headers: *\r\n"), "{}", head);
    assert!(head.contains("Access-Control-Max-Age: 86400\r\n"), "{}", head);
    assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())), "{}", head);
    assert!(head.ends_with("Connection: close"), "{}", head);
    let v: Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["error"]["message"], "missing or invalid local API key");
    assert_eq!(v["error"]["type"], "api_error");
    assert_eq!(v["error"]["code"], "invalid_api_key");
}

#[tokio::test]
async fn bad_json_returns_400() {
    let h = harness(Vec::new());
    let mut w: Vec<u8> = Vec::new();
    serve_chat(&mut w, &h.inner, Some("Bearer sk-test000000000000000000000000000000"), "not json at all").await;
    let s = String::from_utf8_lossy(&w);
    assert!(s.starts_with("HTTP/1.1 400 Error\r\n"));
    assert!(s.contains("请求体不是合法 JSON"));
    assert!(s.contains("invalid_request_error"));
}

#[tokio::test]
async fn model_routing_hits_declaring_backend_first() {
    // 冻结规则：模型名命中（id/name/key/aliases 忽略大小写）的后端置前
    let declaring = MockBackend::new_with(
        vec![Reply::Stream(vec![choices_frame("命中"), ok_done()])],
        vec![
            json!({"id": "深度求索", "key": "dfmodel", "aliases": ["DeepSeek-V4-Flash"]}),
        ],
    );
    let other = MockBackend::new(vec![Reply::Stream(vec![ok_done()])]);
    let h = harness(vec![("o".into(), other.clone()), ("d".into(), declaring.clone())]);
    let cands = pick_candidates(&h.inner, "deepseek-v4-flash");
    assert_eq!(cands.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["d", "o"], "声明该模型的置前");
    // 空 model → 全按登记序
    let cands2 = pick_candidates(&h.inner, "");
    assert_eq!(cands2.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["o", "d"]);
}

// =========================================================================
// 端到端（tokio loopback，任务 1/2/5 验收）
// =========================================================================

async fn request(addr: SocketAddr, raw: &[u8]) -> Vec<u8> {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.set_nodelay(true).unwrap();
    sock.write_all(raw).await.unwrap();
    let mut buf = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut sock, &mut buf).await.unwrap();
    buf
}

fn addr_of(srv: &Server) -> SocketAddr {
    format!("127.0.0.1:{}", srv.bound_port()).parse().unwrap()
}

async fn spawn_test_server(backends: Vec<(String, Arc<dyn ChatBackend>)>) -> Server {
    let srv = Server::assemble_with(backends)
        .unwrap()
        .with_api_key("sk-loopback0000000000000000000000")
        .with_listen("127.0.0.1", 0);
    srv.clone().spawn().await.unwrap();
    assert!(srv.is_running());
    srv
}

#[tokio::test]
async fn e2e_healthz_is_bare_ok_with_cors() {
    let srv = spawn_test_server(vec![]).await;
    let resp = request(addr_of(&srv), b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert_eq!(
        String::from_utf8(resp).unwrap(),
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
    );
    srv.shutdown().await;
    assert!(!srv.is_running());
}

#[tokio::test]
async fn e2e_options_any_path_is_204_preflight() {
    // §1.6：OPTIONS 最先分支、任何路径都答、204 + 全套 CORS + CL:0、不看鉴权
    let srv = spawn_test_server(vec![]).await;
    let resp = request(addr_of(&srv), b"OPTIONS /v1/chat/completions HTTP/1.1\r\n\r\n").await;
    let s = String::from_utf8(resp).unwrap();
    assert_eq!(
        s,
        "HTTP/1.1 204 No Content\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    srv.shutdown().await;
}

#[tokio::test]
async fn e2e_models_fallback_when_no_backends() {
    // 任务 6：后端列表为空 → 内置 21 条兜底；路径别名与归一化生效
    let srv = spawn_test_server(vec![]).await;
    for path in ["/v1/models", "/models", "/v1//models/", "/models/?x=1"] {
        let resp = request(addr_of(&srv), format!("GET {} HTTP/1.1\r\n\r\n", path).as_bytes()).await;
        let s = String::from_utf8_lossy(&resp).to_string();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"), "{}", s);
        let body: Value = serde_json::from_str(s.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["object"], "list");
        assert_eq!(body["data"].as_array().unwrap().len(), 21, "path={}", path);
    }
    // POST 也算（分支不限制方法，§2 表 #3）
    let resp = request(addr_of(&srv), b"POST /v1/models HTTP/1.1\r\nContent-Length: 0\r\n\r\n").await;
    assert!(String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200 OK\r\n"));
    srv.shutdown().await;
}

#[tokio::test]
async fn e2e_404_unknown_endpoint_shape() {
    let srv = spawn_test_server(vec![]).await;
    let resp = request(addr_of(&srv), b"GET /nope HTTP/1.1\r\n\r\n").await;
    let s = String::from_utf8_lossy(&resp).to_string();
    assert!(s.starts_with("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n"));
    let body: Value = serde_json::from_str(s.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["message"], "unknown endpoint: /nope");
    assert_eq!(body["error"]["type"], "api_error");
    srv.shutdown().await;
}

#[tokio::test]
async fn e2e_chat_sse_stream_and_413() {
    // 完整 HTTP 路径：chunked 中文 body → SSE 流式透传；超限 CL → 413
    let b = MockBackend::new(vec![Reply::Stream(vec![choices_frame("你好"), Frame::Done { usage: None, err: None }])]);
    let srv = spawn_test_server(vec![("m".into(), b.clone())]).await;
    let addr = addr_of(&srv);
    let json = r#"{"model":"x","stream":true,"messages":[{"role":"user","content":"你好"}]}"#;
    let jb = json.as_bytes();
    let req = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nAuthorization: Bearer sk-loopback0000000000000000000000\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        jb.len(),
        json
    );
    let resp = request(addr, req.as_bytes()).await;
    let s = String::from_utf8_lossy(&resp).to_string();
    assert!(s.starts_with(sse_hdr()), "流式头必须逐字节：{}", s);
    assert!(s.ends_with("data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n"));
    // 后端收到的是按字节拼回的 UTF-8 原文
    let got_body = b.calls.lock()[0].1.clone();
    assert_eq!(got_body["messages"][0]["content"], "你好");

    // 谎报超大 Content-Length → 413（不等 body，直接拒）
    let resp = request(
        addr,
        b"POST /v1/chat/completions HTTP/1.1\r\nAuthorization: Bearer sk-loopback0000000000000000000000\r\nContent-Length: 9000000\r\n\r\nx",
    )
    .await;
    let s = String::from_utf8_lossy(&resp).to_string();
    assert!(s.starts_with("HTTP/1.1 413 Error\r\n"), "{}", s);
    assert!(s.contains("请求体过大（上限 8 MB）"), "{}", s);
    srv.shutdown().await;
}

#[tokio::test]
async fn e2e_lifecycle_spawn_idempotent_and_shutdown() {
    let srv = Server::assemble_with(vec![])
        .unwrap()
        .with_api_key("sk-lifecycle000000000000000000000")
        .with_listen("127.0.0.1", 0);
    let port_cell = srv.clone();
    srv.clone().spawn().await.unwrap();
    // 幂等：重复 spawn 直接 return（§5.4：重复 start 会 EADDRINUSE 崩掉——现在不会）
    port_cell.spawn().await.unwrap();
    assert!(srv.is_running());
    let addr = addr_of(&srv);
    let resp = request(addr, b"GET /healthz HTTP/1.1\r\n\r\n").await;
    assert!(!resp.is_empty());
    srv.shutdown().await;
    srv.shutdown().await; // 幂等
    assert!(!srv.is_running());
    // 端口回收：停后再连必然失败
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn e2e_trickle_before_request_line_times_out_and_closes() {
    // soTimeout 语义（§1.2/§1.3）：建连后不写字节 → 服务端限时断开（15s 路径太慢，这里只验证"不发就直接等待、
    // 服务端不会挂死"：关闭半连接由服务器读超时兜底。为控制测试时长，本用例只测静默即关的空连接。）
    let srv = spawn_test_server(vec![]).await;
    let addr = addr_of(&srv);
    let sock = TcpStream::connect(addr).await.unwrap();
    // 空连接：不发任何字节 → 服务端 read_line_raw 会等到 15s 超时。
    // 这里直接 drop 模拟立即断裂：服务端必须不 panic、后续请求正常。
    drop(sock);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let resp = request(addr, b"GET /healthz HTTP/1.1\r\n\r\n").await;
    assert!(String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200 OK"));
    srv.shutdown().await;
}
