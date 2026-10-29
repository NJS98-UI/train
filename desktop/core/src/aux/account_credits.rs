// PORT-TODO(agent=aux): 逐行移植 AccountCredits.kt。
// Qoder 侧查询若需要 Qoder 模块的材料，用 qoder::auth_api / qoder::cosy_crypto 的 pub 项。
use crate::state::AppState;
use serde_json::Value;

pub const TRAE: &str = "trae";
pub const QODER: &str = "qoder";

#[derive(Clone, Debug, Default)]
pub struct CreditItem {
    pub text: String,
    pub error: String,
    // PORT-TODO: 字段对齐 Kotlin
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot; // PORT-TODO: 对齐 Kotlin 快照结构

pub fn load_cache(_app: &AppState) {}
pub fn current() -> Snapshot { todo!("port AccountCredits.kt") }
pub async fn refresh_all(_app: &AppState, _force: bool) -> Snapshot { todo!("port AccountCredits.kt") }
pub fn trae_of(_snap: &Snapshot, _uid: &str) -> Option<CreditItem> { todo!("port AccountCredits.kt") }
pub fn qoder_of(_snap: &Snapshot, _uid: &str) -> Option<CreditItem> { todo!("port AccountCredits.kt") }
pub fn label(_item: Option<&CreditItem>) -> String { todo!("port AccountCredits.kt") }
pub fn total_label(_snap: &Snapshot, _platform: &str) -> String { todo!("port AccountCredits.kt") }
pub fn forget(_app: &AppState, _platform: &str, _uid: &str) {}
pub fn self_check() {}
