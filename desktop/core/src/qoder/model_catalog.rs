// PORT-TODO(agent=qoder): 逐行移植 QoderModelCatalog.kt。
use crate::state::AppState;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct QModel {
    pub key: String,
    pub name: String,
    pub multiplier: f64,
    pub aliases: Vec<String>,
    // PORT-TODO: 字段对齐 Kotlin QModel（key/name/倍率/别名/免费标记等）
}

pub const DEFAULT_KEY: &str = ""; // PORT-TODO: 对齐 Kotlin 默认模型 key

pub fn load(_app: &AppState) {}
pub fn save(_app: &AppState, _raw: &str) {}
pub fn current() -> Vec<QModel> { todo!("port QoderModelCatalog.kt") }
pub fn openai_models() -> Value { todo!("port QoderModelCatalog.kt") }
pub fn resolve(_name: &str) -> QModel { todo!("port QoderModelCatalog.kt") }
pub fn session_for(_key: &str) -> String { todo!("port QoderModelCatalog.kt") }
pub fn new_request_id() -> String { todo!("port QoderModelCatalog.kt") }
pub fn free_count() -> usize { 0 }
pub fn has_dynamic() -> bool { false }
pub fn set_dynamic(_v: Vec<QModel>) {}
pub fn parse_catalog_json(_raw: &str) -> Vec<QModel> { todo!("port QoderModelCatalog.kt") }
pub fn self_check() {}
