//! 事件发射器：桌面侧由 tw-app（Tauri event）或 tw-bridge（SSE）注入实现。
//! core 内部只依赖 trait，保持与 Tauri 解耦（design.md §Architecture 要点 3）。

use serde_json::Value;
use std::sync::{Arc, OnceLock};

pub trait Emitter: Send + Sync {
    fn emit(&self, topic: &str, payload: Value);
}

impl<F: Fn(&str, Value) + Send + Sync> Emitter for F {
    fn emit(&self, topic: &str, payload: Value) {
        self(topic, payload)
    }
}

static EMITTER: OnceLock<Arc<dyn Emitter>> = OnceLock::new();

pub fn set_emitter(e: Arc<dyn Emitter>) {
    let _ = EMITTER.set(e);
}

/// 未注入实现时静默丢弃（core 单元测试/独立运行时不需要事件通道）。
pub fn emit(topic: &str, payload: Value) {
    if let Some(e) = EMITTER.get() {
        e.emit(topic, payload);
    }
}

pub mod topics {
    pub const SERVICE_STATE: &str = "service-state";
    pub const CREDITS_SNAPSHOT: &str = "credits-snapshot";
    pub const CHECKIN_RESULT: &str = "checkin-result";
    pub const MODELS_UPDATED: &str = "models-updated";
    pub const NOTICE: &str = "notice";
}
