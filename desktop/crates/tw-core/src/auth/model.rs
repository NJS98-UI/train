//! auth 层数据结构：对应 Kotlin `TraeAuthStore.{AccountMeta,Cred}` 与 `QoderAuthStore.{QMeta,QCred,QFingerprint}`。
//! 字段与序列化 key 依据 `.monkeycode/analysis/auth-stores.md` §3.1/§3.2/§4.5 + design.md §Data Models。
//!
//! 桌面版持久化文件（`accounts.trae.json` / `accounts.qoder.json`）把"索引条目 + token"**合并为单条 entry**
//! （analysis §7.2 的 per-entry 合并方案）：Kotlin 拆成 `trae_accounts`(索引 JSON) + `trae_acc_<uid>_*`(密文)
//! 两组 key 的原因是 AndroidKeyStore 绑机；桌面版无 KeyStore、明文 0600，拆 key 只剩坏处。
//! 落盘 JSON 的字段名与 Android 导出格式**逐字一致**（`region`/`expiresAt`/`accessToken`…），因此导出文件可直接当存储读。

use serde::{Deserialize, Serialize};

/// 对应 `TraeRegion.fromId`：trim + lowercase 精确匹配，未知/空白一律回退 CN。
/// 区域 id 只有 `cn`/`sg` 两个值（types::region），此处不需要枚举类型。
pub fn region_from_id(id: Option<&str>) -> &'static str {
    match id.unwrap_or("").trim().to_lowercase().as_str() {
        crate::types::region::SG => crate::types::region::SG,
        _ => crate::types::region::CN,
    }
}

/// 对应 `TraeRegion.isSupported`：国际服冻结为未开发，仅 `cn` 受支持（failover 加权用）。
pub fn region_is_supported(id: &str) -> bool {
    region_from_id(Some(id)) == crate::types::region::CN
}

pub const QODER_MODE_CN: &str = "cn";
pub const QODER_MODE_GLOBAL: &str = "global";

/// 未识别 mode 一律按 cn（QoderAuthApi §4.1"两套 host 绝不混"的兜底纪律）。
pub fn qoder_normalize_mode(mode: &str) -> &'static str {
    match mode.trim().to_lowercase().as_str() {
        QODER_MODE_GLOBAL => QODER_MODE_GLOBAL,
        _ => QODER_MODE_CN,
    }
}

/// `now_ms`：对应 `System.currentTimeMillis()`（本地墙钟，unix **毫秒**）。
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `now_sec`：Trae 侧时钟（`System.currentTimeMillis()/1000`，unix **秒**）。
/// Trae/Qoder 单位不同是已知坑（analysis §2.1），调用点勿交叉传。
pub fn now_sec() -> i64 {
    now_ms() / 1000
}

// ---------------------------------------------------------------------------
// Trae
// ---------------------------------------------------------------------------

/// 对应 `TraeAuthStore.AccountMeta`（索引条目，不含任何 token）。
/// 落盘 key 是 `region`（非 regionId），与 Android `trae_accounts`/导出条目逐字一致。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TraeAccountMeta {
    pub uid: String,
    pub nickname: String,
    #[serde(rename = "region")]
    pub region_id: String,
    /// unix **秒**；0=未知（一律判过期需续）
    pub expires_at: i64,
}

impl TraeAccountMeta {
    /// 对应 `AccountMeta.region` getter → 规范化的区域 id（"cn"|"sg"）。
    pub fn region(&self) -> &'static str {
        region_from_id(Some(&self.region_id))
    }

    /// 对应 `TraeRegion.isSupported`。
    pub fn is_supported(&self) -> bool {
        region_is_supported(&self.region_id)
    }
}

/// 对应 `TraeAuthStore.Cred`：meta 4 字段 + 两枚 token。
/// 持久化时作为 `accounts.trae.json` 的一条 account（per-entry 合并索引与 token）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TraeAccount {
    pub uid: String,
    pub nickname: String,
    #[serde(rename = "region")]
    pub region_id: String,
    /// unix 秒；0=未知
    pub expires_at: i64,
    pub access_token: String,
    pub refresh_token: String,
}

impl TraeAccount {
    /// 对应 `Cred.toMeta()`。
    pub fn to_meta(&self) -> TraeAccountMeta {
        TraeAccountMeta {
            uid: self.uid.clone(),
            nickname: self.nickname.clone(),
            region_id: self.region_id.clone(),
            expires_at: self.expires_at,
        }
    }

    /// types.rs 的 `TraeCred`（登录流/API 层用）与本持久化条目互转，字段一一对应。
    pub fn from_cred(c: &crate::types::TraeCred) -> Self {
        Self {
            uid: c.uid.clone(),
            nickname: c.nickname.clone(),
            region_id: c.region_id.clone(),
            expires_at: c.expires_at,
            access_token: c.access_token.clone(),
            refresh_token: c.refresh_token.clone(),
        }
    }

    pub fn to_cred(&self) -> crate::types::TraeCred {
        crate::types::TraeCred {
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            uid: self.uid.clone(),
            nickname: self.nickname.clone(),
            region_id: self.region_id.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Qoder
// ---------------------------------------------------------------------------

/// 对应 `QoderAuthStore.QMeta`（索引条目，含 COSY 聊天身份材料，全部明文随索引存）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QoderAccountMeta {
    pub uid: String,
    pub name: String,
    pub email: String,
    /// "cn" | "global"
    pub mode: String,
    /// jobToken 到期，unix **毫秒**；0=未知
    pub expires_at: i64,
    pub organization_id: String,
    pub organization_name: String,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    /// 序列化时必须显式写出（缺省语义由自定义 Deserialize 补默认值）
    #[serde(deserialize_with = "de_bool_default", default)]
    pub data_policy_agreed: bool,
    /// 默认 **true**（Kotlin `optBoolean(..., true)`；字段缺失=缺省 true，显式 null 也回落 true）
    #[serde(default = "default_true", deserialize_with = "de_bool_default_true")]
    pub is_data_policy_modifiable: bool,
}

fn de_bool_default<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(d)?.unwrap_or(false))
}

fn default_true() -> bool {
    true
}

fn de_bool_default_true<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(d)?.unwrap_or(true))
}

/// 对应 `QoderAuthStore.QFingerprint`：抓包真机指纹（campaigns desktop 档必需，不可自造，见 analysis §4.7）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QoderFingerprint {
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub machine_os: String,
    pub hostname: String,
    pub client_type: String,
    pub version: String,
}

impl Default for QoderFingerprint {
    fn default() -> Self {
        Self {
            machine_id: String::new(),
            machine_token: String::new(),
            machine_type: "5".into(),
            machine_os: "aarch64_darwin".into(),
            hostname: "localhost".into(),
            client_type: "10".into(),
            version: "1.1.69".into(),
        }
    }
}

impl QoderFingerprint {
    /// 对应 Kotlin `encodeFingerprint`：密文内/Android 导出里的 **短键 JSON 字符串**（id/tk/ty/os/hn/ct/ver）。
    pub fn encode_short_json(&self) -> String {
        serde_json::json!({
            "id": self.machine_id,
            "tk": self.machine_token,
            "ty": self.machine_type,
            "os": self.machine_os,
            "hn": self.hostname,
            "ct": self.client_type,
            "ver": self.version,
        })
        .to_string()
    }

    /// 对应 Kotlin `decodeFingerprint`：短键 JSON 字符串 → 指纹；`id` 或 `tk` trim 后空白 → None；
    /// 各字段缺失/空白回退默认值。
    pub fn decode_short_json(plain: &str) -> Option<Self> {
        let o: serde_json::Value = serde_json::from_str(plain).ok()?;
        let get = |k: &str, d: &str| -> String {
            o.get(k).and_then(|v| v.as_str()).unwrap_or(d).trim().to_string()
        };
        let id = get("id", "");
        let tk = get("tk", "");
        if id.is_empty() || tk.is_empty() {
            return None;
        }
        let non_blank = |k: &str, d: &str| -> String {
            let v = get(k, d);
            if v.is_empty() {
                d.to_string()
            } else {
                v
            }
        };
        Some(Self {
            machine_id: id,
            machine_token: tk,
            machine_type: non_blank("ty", "5"),
            machine_os: non_blank("os", "aarch64_darwin"),
            hostname: non_blank("hn", "localhost"),
            client_type: non_blank("ct", "10"),
            version: non_blank("ver", "1.1.69"),
        })
    }
}

/// 对应 `QoderAuthStore.QCred`：meta 全字段 + pat/jobToken/jobRefreshToken。
/// 持久化为 `accounts.qoder.json` 的一条 account；`fingerprint` Some=已抓到真机指纹。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QoderAccount {
    pub uid: String,
    pub name: String,
    pub email: String,
    pub mode: String,
    /// unix **毫秒**；0=未知
    pub expires_at: i64,
    pub organization_id: String,
    pub organization_name: String,
    #[serde(default)]
    pub organization_tags: Vec<String>,
    #[serde(deserialize_with = "de_bool_default", default)]
    pub data_policy_agreed: bool,
    /// 默认 **true**（同 `QoderAccountMeta`：缺失=null 都回落 true）
    #[serde(default = "default_true", deserialize_with = "de_bool_default_true")]
    pub is_data_policy_modifiable: bool,
    pub pat: String,
    pub job_token: String,
    pub job_refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<QoderFingerprint>,
}

impl QoderAccount {
    /// 对应 `QCred.toMeta()`。
    pub fn to_meta(&self) -> QoderAccountMeta {
        QoderAccountMeta {
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

    /// 对应 `QCred.chatReady`（`isNotBlank` 语义：trim 后非空白）。
    pub fn chat_ready(&self) -> bool {
        !self.uid.trim().is_empty() && !self.job_token.trim().is_empty()
    }
}

// ---------------------------------------------------------------------------
// snapshot（IPC 层展示用：账号列表 + current 指针 + authFailed 标记，绝不带 token）
// ---------------------------------------------------------------------------

/// Trae snapshot 单条视图 = meta + 派生标志。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeAccountView {
    #[serde(flatten)]
    pub meta: TraeAccountMeta,
    /// 该账号是否存有非空白 accessToken（`load_by_uid` 能否成功的硬条件）
    pub has_access_token: bool,
    pub has_refresh_token: bool,
}

/// `TraeAuthStore.snapshot()` 返回体（对齐 design.md `trae_state`：logged/activeUid/authFailedUid）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraeSnapshot {
    pub accounts: Vec<TraeAccountView>,
    pub active_uid: Option<String>,
    /// 对应 MainActivity.authFailedUid（运行期标记，按 uid 记，不落盘）
    pub auth_failed_uid: Option<String>,
    /// 生效区域 = config.json 的 `trae_region`（`syncRegionToActiveAccount` 的落点）
    pub region_id: String,
}

/// Qoder snapshot 单条视图。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderAccountView {
    #[serde(flatten)]
    pub meta: QoderAccountMeta,
    pub has_pat: bool,
    pub has_job_token: bool,
    pub has_job_refresh_token: bool,
    pub chat_ready: bool,
    pub has_fingerprint: bool,
}

/// `QoderAuthStore.snapshot()` 返回体。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderSnapshot {
    pub accounts: Vec<QoderAccountView>,
    pub active_uid: Option<String>,
    pub auth_failed_uid: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_from_id_falls_back_to_cn() {
        assert_eq!(region_from_id(Some("cn")), "cn");
        assert_eq!(region_from_id(Some(" SG ")), "sg");
        assert_eq!(region_from_id(Some("jp")), "cn");
        assert_eq!(region_from_id(Some("")), "cn");
        assert_eq!(region_from_id(None), "cn");
        assert!(region_is_supported("cn"));
        // Kotlin 语义：`fromId("")`→CN→isSupported=true（空白区域就是按国服对待）
        assert!(region_is_supported(""));
        assert!(!region_is_supported("sg"));
    }

    #[test]
    fn qoder_mode_defaults_to_cn() {
        assert_eq!(qoder_normalize_mode("GLOBAL"), "global");
        assert_eq!(qoder_normalize_mode("bogus"), "cn");
        assert_eq!(qoder_normalize_mode(""), "cn");
    }

    #[test]
    fn fingerprint_short_json_roundtrip_and_guards() {
        // 对应 QoderAuthStore.selfCheck 的指纹往返断言
        let fp = QoderFingerprint {
            machine_id: "mid-123".into(),
            machine_token: "mtok_abc-DEF_456".into(),
            ..Default::default()
        };
        assert_eq!(QoderFingerprint::decode_short_json(&fp.encode_short_json()), Some(fp));
        // 空 id/tk → None
        assert_eq!(QoderFingerprint::decode_short_json(r#"{"id":"","tk":""}"#), None);
        // 字段缺失/空白 → 回退默认值
        let partial = QoderFingerprint::decode_short_json(r#"{"id":"a","tk":"b","ty":""}"#).unwrap();
        assert_eq!(partial.machine_type, "5");
        assert_eq!(partial.machine_os, "aarch64_darwin");
        assert_eq!(partial.version, "1.1.69");
    }

    #[test]
    fn qoder_meta_legacy_defaults_deserialize() {
        // 旧索引无组织字段 → 读出默认（dataPolicyAgreed=false，isDataPolicyModifiable=true）
        let m: QoderAccountMeta =
            serde_json::from_str(r#"{"uid":"old","name":"n","email":"e","mode":"cn","expiresAt":1}"#).unwrap();
        assert_eq!(m.data_policy_agreed, false);
        assert_eq!(m.is_data_policy_modifiable, true);
        assert!(m.organization_tags.is_empty());
    }
}
