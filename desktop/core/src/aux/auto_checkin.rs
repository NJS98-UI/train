// PORT-TODO(agent=aux): 逐行移植 AutoCheckin.kt。
use crate::state::AppState;
use serde_json::Value;

pub fn is_enabled(_app: &AppState) -> bool { false }
pub fn set_enabled(_app: &AppState, _on: bool) {}
pub fn time_text(_app: &AppState) -> String { todo!("port AutoCheckin.kt") }
/// 到点执行（含「没跑成则下次打开 App 自动补签」逻辑，对齐 Kotlin）
pub async fn run_due(_app: &AppState) -> Value { todo!("port AutoCheckin.kt") }
pub fn self_check() {}
