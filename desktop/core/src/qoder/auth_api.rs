// PORT-TODO(agent=qoder): 逐行移植 QoderAuthApi.kt。
// 先 grep MainActivity.kt 里所有 `QoderAuthApi.` 调用点，全部移植。
use crate::state::AppState;
use crate::types::QoderCred;

pub const MODE_CN: &str = "cn";
pub const MODE_GLOBAL: &str = "global";

/// jobToken 续期阈值判断（skew 单位=毫秒；对齐 Kotlin validJobTokenFor）
pub async fn valid_job_token_for(_app: &AppState, _cred: &QoderCred, _skew_ms: i64) -> Option<String> {
    todo!("port QoderAuthApi.kt")
}
/// 401 自救：强制续一次 jobToken
pub async fn force_refresh_job_token(_app: &AppState, _cred: &QoderCred) -> Option<String> {
    todo!("port QoderAuthApi.kt")
}
pub async fn fetch_model_catalog(_jt: &str, _mode: &str) -> Option<String> { todo!("port QoderAuthApi.kt") }
/// 粘贴 PAT 导入账号（对齐 Kotlin 的 PAT 登录/换 jobToken 流程）
pub async fn login_with_pat(_app: &AppState, _pat: &str) -> Result<QoderCred, String> {
    todo!("port QoderAuthApi.kt")
}
pub fn infer_base(_mode: &str) -> String { todo!("port QoderAuthApi.kt") }
pub fn last_catalog_raw() -> String { todo!("port QoderAuthApi.kt") }
pub fn last_catalog_error() -> Option<String> { todo!("port QoderAuthApi.kt") }
pub fn self_check() { todo!("port QoderAuthApi.kt selfCheck") }
