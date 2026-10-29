// PORT-TODO(agent=qoder): 逐行移植 QoderCheckin.kt（每日领取 campaigns）。
// 先 grep MainActivity.kt 的 `QoderCheckin.` 调用点，全部移植为 pub fn。
use crate::state::AppState;
use serde_json::Value;

pub fn is_enabled(_app: &AppState) -> bool { false }
pub fn enable(_app: &AppState, _time_text: &str) {}
pub fn disable(_app: &AppState) {}
pub fn time_text(_app: &AppState) -> String { todo!("port QoderCheckin.kt") }
pub fn set_time(_app: &AppState, _t: &str) {}
pub fn last_results(_app: &AppState) -> Value { serde_json::json!([]) }
/// 定时到点执行一次实际领取（调度器用；对齐 Kotlin 的领取流程）
pub async fn run_once(_app: &AppState) -> Value { todo!("port QoderCheckin.kt") }
pub fn self_check() {}
