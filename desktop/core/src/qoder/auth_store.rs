// PORT-TODO(agent=qoder): 逐行移植 QoderAuthStore.kt（多账号存取 + 设备指纹 + 导出导入）。
// 存储映射（桌面版）：
//   prefs 键值 → AppState.prefs（键名与 Android 完全一致：qoder_accounts / q_active_uid /
//                q_machine_id）
//   token/指纹密文 → AppState.secrets（键名同 Android：q_acc_<uid>_pat/_jt/_jrt/_fp）
//   AndroidKeyStore AES-GCM → AppState.secret()/set_secret()
// 使用 crate::types::{QoderMeta, QoderCred, QFingerprint}。
use crate::state::AppState;
use crate::types::{QFingerprint, QoderCred, QoderMeta};

pub const EXPORT_FORMAT: &str = "traework2api-qoder-accounts";

pub fn list(_app: &AppState) -> Vec<QoderMeta> { todo!("port QoderAuthStore.kt") }
pub fn active_uid(_app: &AppState) -> Option<String> { todo!("port QoderAuthStore.kt") }
pub fn set_active(_app: &AppState, _uid: &str) { todo!("port QoderAuthStore.kt") }
pub fn load(_app: &AppState) -> Option<QoderCred> { todo!("port QoderAuthStore.kt") }
pub fn load_by_uid(_app: &AppState, _uid: &str) -> Option<QoderCred> { todo!("port QoderAuthStore.kt") }
pub fn save(_app: &AppState, _cred: &QoderCred) { todo!("port QoderAuthStore.kt") }
pub fn update_tokens(_app: &AppState, _cred: &QoderCred) { todo!("port QoderAuthStore.kt") }
pub fn remove(_app: &AppState, _uid: &str) { todo!("port QoderAuthStore.kt") }
pub fn machine_id(_app: &AppState) -> String { todo!("port QoderAuthStore.machineId") }
pub fn fingerprint(_app: &AppState, _uid: &str) -> Option<QFingerprint> { todo!("port QoderAuthStore.kt") }
pub fn set_fingerprint(_app: &AppState, _uid: &str, _fp: &QFingerprint) { todo!("port QoderAuthStore.kt") }
pub fn clear_fingerprint(_app: &AppState, _uid: &str) { todo!("port QoderAuthStore.kt") }
pub fn export_json(_app: &AppState) -> String { todo!("port QoderAuthStore.kt") }
pub fn import_json(_app: &AppState, _json: &str) -> Result<i64, String> { todo!("port QoderAuthStore.kt") }
pub fn self_check() { todo!("port QoderAuthStore.kt selfCheck") }
