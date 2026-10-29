// PORT-TODO(agent=qoder): 逐行移植 QoderUpstream.kt（COSY 签名聊天链路）。
use crate::state::AppState;
use crate::types::QoderCred;
use serde_json::Value;

/// 对齐 Kotlin QoderUpstream 的请求体加工（prepareBody）
pub fn prepare_body(_app: &AppState, _src: Value, _cred: &QoderCred) -> Value { todo!("port QoderUpstream.kt") }

/// COSY 聊天流：emit 逐帧推 SSE chunk；返回 (usage, err)。
/// refresh_job_token：401 时回调，续期成功返回新 jobToken（对齐 Kotlin refreshJobToken 参数）。
pub async fn chat_stream(
    _app: &AppState,
    _client_body: Value,
    _cred: QoderCred,
    _emit: &mut (dyn FnMut(String) + Send),
    _refresh_job_token: &mut (dyn FnMut() -> Option<String> + Send),
) -> (Option<Value>, Option<String>) {
    todo!("port QoderUpstream.kt")
}

pub async fn fetch_model_list_raw(_app: &AppState, _cred: &QoderCred, _jt: &str) -> Option<String> {
    todo!("port QoderUpstream.kt")
}
pub fn self_check() { todo!("port QoderUpstream.kt selfCheck") }
