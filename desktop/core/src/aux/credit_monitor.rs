// PORT-TODO(agent=aux): 逐行移植 CreditMonitor.kt。
use crate::state::AppState;

/// 每次转发完成后调用（内部节流 60s；urgent=true 跳过节流，失败换号判定用）
pub fn note_forwarded(_app: &AppState, _backend: &str, _uid: &str, _urgent: bool) {}
pub fn is_auto_switch_enabled(_app: &AppState) -> bool { false }
pub fn set_auto_switch_enabled(_app: &AppState, _on: bool) {}
pub fn threshold(_app: &AppState) -> i64 { 0 }
pub fn set_threshold(_app: &AppState, _v: i64) {}
pub fn scan_interval(_app: &AppState) -> i64 { 0 }
pub fn set_scan_interval(_app: &AppState, _v: i64) {}
pub fn stop() {}
/// 用户手动把检测间隔调成 >0 时，由调度器周期调用（对齐 Kotlin onResume 起线程）
pub async fn scan_once(_app: &AppState) {}
pub fn self_check() {}
