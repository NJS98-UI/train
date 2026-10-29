// PORT-TODO(agent=aux): 逐行移植 BatchCheckin.kt。
// 先 grep MainActivity.kt 的 `BatchCheckin.` 调用点，全部移植。
use crate::state::AppState;
use serde_json::Value;

/// 一键签到全部账号（逐个签，不只当前那个）
pub async fn checkin_all(_app: &AppState) -> Value { todo!("port BatchCheckin.kt") }
