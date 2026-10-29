// PORT-TODO(agent=aux): 逐行移植 UsageStore.kt（按模型统计用量）。
use crate::state::AppState;
use serde_json::Value;

pub fn record(_app: &AppState, _model: &str, _prompt: i64, _completion: i64, _total: i64, _ms: i64, _ok: bool, _err: &str) {}
pub fn summary(_app: &AppState) -> Value { todo!("port UsageStore.kt") }
