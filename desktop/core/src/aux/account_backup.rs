// PORT-TODO(agent=aux): 逐行移植 AccountBackup.kt。
use crate::state::AppState;
use serde_json::Value;

pub fn export_all(_app: &AppState) -> Value { todo!("port AccountBackup.kt") }
pub fn import_all(_app: &AppState, _trae: &str, _qoder: &str) -> Value { todo!("port AccountBackup.kt") }
pub fn self_check() {}
