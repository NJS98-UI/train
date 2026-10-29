// PORT-TODO(agent=qoder): 逐行移植 QoderModelCache.kt。
use crate::state::AppState;

pub struct ImportResult {
    pub plain: Option<String>,
    pub msg: String,
    // PORT-TODO: 字段对齐 Kotlin
}
pub fn cli_hint(_uid: &str) -> String { todo!("port QoderModelCache.kt") }
pub fn import_blob(_app: &AppState, _text: &str, _uid: &str) -> ImportResult {
    todo!("port QoderModelCache.kt")
}
pub fn self_check() {}
