//! 跨模块共享的数据结构。字段命名与 Kotlin 侧一一对应（serde camelCase 落盘）。

use serde::{Deserialize, Serialize};

pub const BACKEND_TRAE: &str = "trae";
pub const BACKEND_QODER: &str = "qoder";
pub const BACKEND_AUTO: &str = "auto";

/// 对应 `TraeAuthStore.Cred`。`expires_at` 为 **Unix 秒**（0=未知，一律视为需续）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TraeCred {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(rename = "expiresAt")]
    pub expires_at: i64,
    pub uid: String,
    pub nickname: String,
    #[serde(rename = "regionId")]
    pub region_id: String,
}

/// 对应 `QoderAuthStore.QCred`。`expires_at` 为 **Unix 毫秒**。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QCred {
    pub pat: String,
    pub job_token: String,
    pub job_refresh_token: String,
    pub expires_at: i64,
    pub uid: String,
    pub name: String,
    pub email: String,
    pub mode: String,
    pub organization_id: String,
    pub organization_name: String,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    pub data_policy_agreed: bool,
    pub is_data_policy_modifiable: bool,
}

impl QCred {
    pub fn chat_ready(&self) -> bool {
        !self.uid.trim().is_empty() && !self.job_token.trim().is_empty()
    }
}

/// 对应 `UsageStore` 单条记录（FIFO ≤ 100）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageRecord {
    pub ts: i64,
    pub model: String,
    pub prompt: i64,
    pub completion: i64,
    pub total: i64,
    pub ms: i64,
    pub ok: bool,
    pub err: String,
}

/// 对应 `UsageStore.ModelAgg`（记录页汇总卡）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelAgg {
    pub model: String,
    pub calls: i64,
    pub ok_calls: i64,
    pub prompt: i64,
    pub completion: i64,
    pub total: i64,
    pub ms: i64,
}

/// 保活/常驻体检项（对应 Android keepAlivePage 的 Item 形状）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckItem {
    pub title: String,
    pub ok: bool,
    pub why: String,
    pub auto_fixable: bool,
}

/// 区域 id：仅 `cn` 受支持签到/额度，`sg` 为国际版（可聊天、能力受限）。
pub mod region {
    pub const CN: &str = "cn";
    pub const SG: &str = "sg";
}
