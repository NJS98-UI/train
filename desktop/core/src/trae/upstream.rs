// PORT-TODO(agent=trae): 逐行移植 TraeUpstream.kt（含按区域分派国服/国际的逻辑）。
use crate::state::AppState;
use serde_json::Value;

/// 对齐 Kotlin TraeUpstream.prepareBody（stream 强制 true 等加工）
pub fn prepare_body(_src: Value) -> Value { todo!("port TraeUpstream.kt") }

/// 打上游聊天流：emit 逐帧推送 SSE chunk 字符串；返回 (usage, err)。
/// 对齐 Kotlin chatStream(prepared, token, uid, emit, done)。
pub async fn chat_stream(
    _app: &AppState,
    _prepared: Value,
    _access_token: String,
    _uid: String,
    _emit: &mut (dyn FnMut(String) + Send),
) -> (Option<Value>, Option<String>) {
    todo!("port TraeUpstream.kt")
}

pub fn self_check() { todo!("port TraeUpstream.kt selfCheck") }

// PORT-TODO: CheckinStatus 等其余 pub 项（grep MainActivity/BatchCheckin 的调用点后补齐）
