//! checkin 层：Trae 批量签到 / Qoder 每日领取 / 跨平台开窗补签 / 定时调度。
//! 权威依据 = `.monkeycode/analysis/checkin-credits.md` §1~§4 + §9。
//!
//! 模块划分：
//! - [`state`]：`checkin.state.json` 持久化（last_run_date/last_results/qoder_claimed/auto_*）与日期口径；
//! - [`trae_api`]：§1.1 设备三件套、19 头 ug 请求、status/claim/usage、成功合取判据、last_auth_failed；
//! - [`qoder_api`]：§1.2 两档身份头、空列表升档阶梯、claim 状态机与文案、quota/usage；
//! - [`batch`]：§2 Trae 批量编排（PerAccount/BatchResult/落盘/续期契约）；
//! - [`qoder_batch`]：§4 Qoder 批量编排（600ms 间隔、claimed 标记、只读校准）；
//! - [`catchup`]：§3.3 AutoCheckin 跨平台补签（按账号 markDone、10 分钟 cooldown）；
//! - [`scheduler`]：§3.1/§3.3/§9.3 tokio 任务版闹钟（HH:mm 本地时刻 + SGT 10:05 开窗 + 启动自愈）。
//!
//! 入口命名：Trae/Qoder 两套批量各自的同名 `run_all` / `maybe_catch_up` 通过模块前缀区分
//! （`batch::run_all` / `qoder_batch::run_all`），本层只 re-export 不冲突的项。

pub mod batch;
pub mod catchup;
pub mod qoder_api;
pub mod qoder_batch;
pub mod scheduler;
pub mod state;
pub mod trae_api;

pub use batch::{checkin_one, last_results as trae_last_results, notify_if_scheduled as trae_notify_if_scheduled, run_all as trae_run_all, run_scheduled as trae_run_scheduled, BatchResult, PerAccount};
pub use catchup::{done_today as auto_done_today, is_done as auto_is_done, next_window_at, pending, request_run_soon, run_all_once, should_attempt, should_attempt_at, Platform};
pub use qoder_api::{claim_daily_for_cred, claim_daily_for_uid, claimed_today_for, fetch_credits, QCampaign, QClaimResult, QCredits};
pub use qoder_batch::{claimed_today, last_results as qoder_last_results, mark_claimed, notify_if_scheduled as qoder_notify_if_scheduled, run_all as qoder_run_all, run_scheduled as qoder_run_scheduled, set_claimed, QBatchResult, QPerAccount};
pub use scheduler::{due_now, next_run_at, start, stop, reschedule, Due, Handle as SchedulerHandle, SchedulerStatus};
pub use state::{today_local, today_sgt, CheckinState};
pub use trae_api::{checkin_succeeded, claim_checkin, derive_device_ids, fetch_checkin, fetch_checkin_healing, fetch_usage, last_auth_failed, claim_checkin_healing, CheckinStatus, DeviceIds, UgFailure};

/// §9.2：模块级自检入口（appcore 首启调用，等价 Kotlin object selfCheck）。
pub fn self_check() -> Result<(), String> {
    state::self_check()?;
    trae_api::self_check()?;
    qoder_api::self_check()?;
    batch::self_check()?;
    qoder_batch::self_check()?;
    catchup::self_check()?;
    scheduler::self_check()?;
    Ok(())
}

/// 定时路径统一入口（§9.3）：Trae → Qoder → 跨平台开窗补签，串行执行、互不阻塞。
pub async fn scheduled_round(state: &CheckinState) {
    let trae = crate::auth::store::TraeAuthStore::global();
    let qoder = crate::auth::store::QoderAuthStore::global();
    let _ = trae_run_scheduled(state, &trae).await;
    let _ = qoder_run_scheduled(state, &qoder).await;
    let _ = catchup::run_all_once(state, &trae, &qoder).await;
}
