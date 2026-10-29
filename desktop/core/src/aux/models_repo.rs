// PORT-TODO(agent=aux): 逐行移植 ModelsRepo.kt。
use crate::state::AppState;
use serde_json::Value;

pub fn init(_app: &AppState) {}
/// 内存中的 Trae 模型表（未刷新时为空数组）
pub fn current() -> Value { serde_json::json!([]) }
pub fn save(_app: &AppState, _arr: &Value) {}
/// 从上游拉真实模型清单并本地持久化
pub async fn refresh(_app: &AppState, _access_token: &str) -> Result<usize, String> {
    todo!("port ModelsRepo.kt")
}
pub fn self_check() {}
