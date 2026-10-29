use serde::{Deserialize, Serialize};

/// Trae 账号元数据（不含 token）—— 对齐 TraeAuthStore.AccountMeta
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraeAccountMeta {
    pub uid: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(rename = "region", default = "default_region")]
    pub region_id: String,
    #[serde(default)]
    pub expires_at: i64,
}
fn default_region() -> String { "cn".into() }

/// Trae 完整凭证 —— 对齐 TraeAuthStore.Cred
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraeCred {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    pub uid: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default = "default_region")]
    pub region_id: String,
}

/// Qoder 账号元数据 —— 对齐 QoderAuthStore.QMeta
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QoderMeta {
    pub uid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default = "qmode")]
    pub mode: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub organization_name: String,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    #[serde(default)]
    pub data_policy_agreed: bool,
    #[serde(default = "dtrue")]
    pub is_data_policy_modifiable: bool,
}
fn qmode() -> String { "cn".into() }
fn dtrue() -> bool { true }

/// Qoder 完整凭证 —— 对齐 QoderAuthStore.QCred
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QoderCred {
    #[serde(default)]
    pub pat: String,
    #[serde(default)]
    pub job_token: String,
    #[serde(default)]
    pub job_refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    pub uid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default = "qmode")]
    pub mode: String,
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub organization_name: String,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    #[serde(default)]
    pub data_policy_agreed: bool,
    #[serde(default = "dtrue")]
    pub is_data_policy_modifiable: bool,
}

impl QoderCred {
    /// 是否具备走 COSY 聊天的最小条件（uid + jobToken 必须有）
    pub fn chat_ready(&self) -> bool { !self.uid.is_empty() && !self.job_token.is_empty() }
    pub fn to_meta(&self) -> QoderMeta {
        QoderMeta {
            uid: self.uid.clone(),
            name: self.name.clone(),
            email: self.email.clone(),
            mode: self.mode.clone(),
            expires_at: self.expires_at,
            organization_id: self.organization_id.clone(),
            organization_name: self.organization_name.clone(),
            organization_tags: self.organization_tags.clone(),
            data_policy_agreed: self.data_policy_agreed,
            is_data_policy_modifiable: self.is_data_policy_modifiable,
        }
    }
}

impl TraeCred {
    pub fn to_meta(&self) -> TraeAccountMeta {
        TraeAccountMeta {
            uid: self.uid.clone(),
            nickname: self.nickname.clone(),
            region_id: self.region_id.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// 设备指纹 —— 对齐 QoderAuthStore.QFingerprint
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QFingerprint {
    pub machine_id: String,
    pub machine_token: String,
    #[serde(default = "qf_type")]
    pub machine_type: String,
    #[serde(default = "qf_os")]
    pub machine_os: String,
    #[serde(default = "qf_host")]
    pub hostname: String,
    #[serde(default = "qf_client")]
    pub client_type: String,
    #[serde(default = "qf_ver")]
    pub version: String,
}
fn qf_type() -> String { "5".into() }
fn qf_os() -> String { "aarch64_darwin".into() }
fn qf_host() -> String { "localhost".into() }
fn qf_client() -> String { "10".into() }
fn qf_ver() -> String { "1.1.69".into() }
