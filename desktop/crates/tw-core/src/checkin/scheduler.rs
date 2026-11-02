//! 桌面定时调度（checkin-credits.md §3.1 / §3.2 / §3.3 / §4 / §9.3）。
//!
//! Android 侧"AlarmManager 单发闹钟 + 触发自续 + 无 BOOT 开机丢闹钟 + 进 App `maybeCatchUp` 兜底"
//! 的整套复杂度，在桌面收敛为**一个常驻 tokio 任务 + 一个 15 秒 tick**：
//!
//! | Android | 这里 |
//! |---|---|
//! | `scheduleNext` 单发闹钟（今天/明天 HH:mm:00.000，`<=now` 顺延一天） | [`next_run_at`] 纯函数算下一次毫秒；tick 里比较"今天 HH:mm 是否已到 && last_run_date != 今天" |
//! | 触发自续（`finally scheduleNext`） | 无需自续：条件式判定天然每天命中一次（`last_run_date` 落盘即去重） |
//! | 进程被杀/重启闹钟丢失 | 常驻任务 + 启动即 [`catchup::run_all_once`] / `maybe_catch_up`（§9.3 落点表） |
//! | `setTime` 重挂闹钟 | [`reschedule`] 唤醒 tick 立即重算 |
//! | `AutoCheckin.scheduleWindowAlarm`（非精确、SGT 10:05、自续） | 同一个 tick 里的开窗判定（[`window_arrived`]）；**故意非**精确到毫秒，窗口本身留 5 分钟余量 |
//! | `busy` AtomicBoolean CAS | 各编排模块自己的 `BusyGuard`（§2/§4） |
//!
//! 时区口径照抄现状：`HH:mm` 用**设备本地**，开窗用 **Asia/Singapore(+08:00 固定偏移)**，
//! done 日期按本地、window 日期按 SGT。

use crate::checkin::state::{today_local, today_sgt, CheckinState};
use chrono::TimeZone;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tokio::sync::Notify;

/// tick 粒度（§9.3：足够覆盖 Doze 式延迟容忍，又不至于空转）。
pub const TICK_MS: u64 = 15_000;
/// Trae 批量缺省时刻（`BatchCheckin.timeText` 缺省）。
pub const DEFAULT_TRAE_TIME: &str = "08:00";
/// Qoder 批量缺省时刻（`QoderCheckin.timeText` 缺省）。
pub const DEFAULT_QODER_TIME: &str = "09:00";
/// 一天毫秒数。
pub const DAY_MS: i64 = 86_400_000;

/// `timeText` 校验 + `timeMinutes`：`^\d{1,2}:\d{2}$`，h coerce 0..23、m coerce 0..59。
/// 损坏返回 None（调用方回落缺省值——Kotlin 用正则先拦，这里同构）。
pub fn parse_minutes(hhmm: &str) -> Option<i32> {
    let bytes = hhmm.as_bytes();
    if bytes.len() < 4 || bytes.len() > 5 {
        return None;
    }
    let colon = hhmm.find(':')?;
    let (h_s, m_s) = (&hhmm[..colon], &hhmm[colon + 1..]);
    if h_s.len() > 2 || m_s.len() != 2 {
        return None;
    }
    if !h_s.bytes().all(|b| b.is_ascii_digit()) || !m_s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = h_s.parse().unwrap_or(0);
    let m: i32 = m_s.parse().unwrap_or(0);
    Some(h.clamp(0, 23) * 60 + m.clamp(0, 59))
}

/// 有效分钟数（损坏回落 `default`，等价 Kotlin `?: "08:00"` 后再 split）。
pub fn minutes_or(hhmm: &str, default: &str) -> i32 {
    parse_minutes(hhmm).or_else(|| parse_minutes(default)).unwrap_or(8 * 60)
}

fn local_offset_sec(now_ms: i64) -> i32 {
    chrono::Local
        .timestamp_millis_opt(now_ms)
        .single()
        .map(|d| d.offset().local_minus_utc())
        .unwrap_or(0)
}

/// `scheduleNext` 的对齐算法：给定 `HH:mm`、当前毫秒、当前本地 UTC 偏移（秒），
/// 返回**今天或明天**该时刻 `HH:mm:00.000` 的 unix 毫秒（`<=now` 顺延一天）。
///
/// DST 说明：这里按"当前偏移"固定算一天（Kotlin Calendar `+1 DAY_OF_YEAR` 在跨越 DST 切换时
/// 会给出该日真实墙钟），差异只影响切换当日的闹钟偏移一分钟级；桌面常驻进程可接受，
/// 且每次 tick 重算，最坏延迟 15 秒即校正。
pub fn next_run_at(hhmm: &str, now_ms: i64, offset_sec: i32) -> i64 {
    next_run_at_minutes(minutes_or(hhmm, DEFAULT_TRAE_TIME), now_ms, offset_sec)
}

/// [`next_run_at`] 的已解析版（Qoder 缺省时刻不同，用这个传自己的缺省分钟）。
pub fn next_run_at_minutes(minutes: i32, now_ms: i64, offset_sec: i32) -> i64 {
    let off = offset_sec as i64 * 1000;
    let day_start = (now_ms + off).div_euclid(DAY_MS) * DAY_MS - off;
    let target = day_start + minutes as i64 * 60_000;
    if target <= now_ms {
        target + DAY_MS
    } else {
        target
    }
}

/// 当天该时刻的毫秒（可过去，用于到期判定）。
fn today_at(minutes: i32, now_ms: i64, offset_sec: i32) -> i64 {
    let off = offset_sec as i64 * 1000;
    let day_start = (now_ms + off).div_euclid(DAY_MS) * DAY_MS - off;
    day_start + minutes as i64 * 60_000
}

/// 到期条件（§3.1 语义 + 桌面"错过时刻"兜底）：开关开 && 今天 HH:mm 已到 && 今天没跑过。
///
/// 比 Android 更强的一点：进程 09:30 才启动、时刻设 08:00，这里**当天会跑**（`<=now` 不丢），
/// 对应 §9.3「顺带覆盖错过 HH:mm 才启动的场景」。
pub fn due_at(
    enabled: bool,
    hhmm: &str,
    default_time: &str,
    now_ms: i64,
    offset_sec: i32,
    last_run_date: Option<&str>,
    today: &str,
) -> bool {
    if !enabled {
        return false;
    }
    if last_run_date == Some(today) {
        return false;
    }
    today_at(minutes_or(hhmm, default_time), now_ms, offset_sec) <= now_ms
}

/// SGT 当天 10:05:00.000 的毫秒（不顺延）。
pub fn sgt_window_today(now_ms: i64) -> i64 {
    let sgt = chrono::FixedOffset::east_opt(8 * 3600).expect("固定偏移恒可用");
    let t = sgt.timestamp_millis_opt(now_ms).single().expect("毫秒可定位");
    let d = t.date_naive();
    crate::checkin::catchup::window_dt(&sgt, d).timestamp_millis()
}

/// [`catchup::next_window_at`] 的转口（保持 API 草案 §9 的 `scheduler::next_window_at`）
pub fn next_window_at(now_ms: i64) -> i64 {
    crate::checkin::catchup::next_window_at(now_ms)
}

/// 开窗是否已到（配合 `auto_window_date` 去重使用）。
pub fn window_arrived(now_ms: i64) -> bool {
    now_ms >= sgt_window_today(now_ms)
}

/// `status()`（§9.2 IPC 草案：前端显示"下次几点跑"）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SchedulerStatus {
    pub running: bool,
    pub trae_enabled: bool,
    pub trae_time: String,
    pub trae_next_at: Option<i64>,
    pub qoder_enabled: bool,
    pub qoder_time: String,
    pub qoder_next_at: Option<i64>,
    pub window_enabled: bool,
    pub window_next_at: i64,
    pub tick_ms: u64,
}

/// 任务句柄：`stop` 靠 `cancel` 标志，`reschedule` 靠 `wake`（等价"通知重算 tick"）。
#[derive(Clone)]
pub struct Handle {
    cancel: std::sync::Arc<AtomicBool>,
    wake: std::sync::Arc<Notify>,
}

impl Handle {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    /// 用户改时刻/开关：唤醒 tick 立即重算，无需重建任务（§9.3「setTime → 通知任务重算」）。
    pub fn reschedule(&self) {
        self.wake.notify_waiters();
    }
}

static SCHED: OnceLock<std::sync::Mutex<Option<Handle>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static std::sync::Mutex<Option<Handle>> {
    SCHED.get_or_init(|| std::sync::Mutex::new(None))
}

/// 已启动句柄（供 `reschedule` 使用；未启动则什么都不做——闹钟本就未挂）。
pub fn handle() -> Option<Handle> {
    slot().lock().unwrap().clone()
}

/// §3.1/§3.3 的"自愈重挂"：桌面等价物是唤醒 tick。
pub fn reschedule() {
    if let Some(h) = handle() {
        h.reschedule();
    }
}

/// 是否运行中。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// current snapshot of schedule state + 下次触发毫秒（未启动也照算，UI 用来显示"预计时刻"）。
pub fn status() -> SchedulerStatus {
    let cfg = crate::config::get();
    let now = crate::auth::model::now_ms();
    let off = local_offset_sec(now);
    let trae = cfg.checkin.trae_time.clone();
    let qoder = cfg.checkin.qoder_time.clone();
    SchedulerStatus {
        running: is_running(),
        trae_enabled: cfg.checkin.trae_enabled,
        trae_time: trae.clone(),
        trae_next_at: if cfg.checkin.trae_enabled { Some(next_run_at_minutes(minutes_or(&trae, DEFAULT_TRAE_TIME), now, off)) } else { None },
        qoder_enabled: cfg.checkin.qoder_enabled,
        qoder_time: qoder.clone(),
        qoder_next_at: if cfg.checkin.qoder_enabled { Some(next_run_at_minutes(minutes_or(&qoder, DEFAULT_QODER_TIME), now, off)) } else { None },
        window_enabled: true,
        window_next_at: next_window_at(now),
        tick_ms: TICK_MS,
    }
}

/// 纯函数：给定时钟与配置，算出这一 tick 该跑哪些任务（三条独立判定 + 开窗去重）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Due {
    pub trae: bool,
    pub qoder: bool,
    pub window: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn due_now(
    trae_enabled: bool,
    trae_time: &str,
    qoder_enabled: bool,
    qoder_time: &str,
    trae_last: Option<&str>,
    qoder_last: Option<&str>,
    window_last: Option<&str>,
    now_ms: i64,
    offset_sec: i32,
) -> Due {
    let today = today_local(now_ms);
    Due {
        trae: due_at(trae_enabled, trae_time, DEFAULT_TRAE_TIME, now_ms, offset_sec, trae_last, &today),
        qoder: due_at(qoder_enabled, qoder_time, DEFAULT_QODER_TIME, now_ms, offset_sec, qoder_last, &today),
        window: window_arrived(now_ms) && window_last != Some(today_sgt(now_ms).as_str()),
    }
}

/// 启动常驻任务（`try_current` 失败=无 tokio 运行时 → Err，不 panic）。
/// 已在运行则幂等返回既有句柄。
pub fn start(state: std::sync::Arc<CheckinState>) -> Result<Handle, String> {
    if let Some(h) = handle() {
        if !h.cancel.load(Ordering::SeqCst) {
            RUNNING.store(true, Ordering::SeqCst);
            return Ok(h);
        }
    }
    let rt = tokio::runtime::Handle::try_current().map_err(|_| "调度任务启动失败：当前无 tokio 运行时".to_string())?;
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let wake = std::sync::Arc::new(Notify::new());
    let h = Handle { cancel: cancel.clone(), wake: wake.clone() };
    *slot().lock().unwrap() = Some(h.clone());
    RUNNING.store(true, Ordering::SeqCst);
    rt.spawn(async move { run_loop(state, cancel, wake).await });
    Ok(h)
}

/// 停止常驻任务（等价 Android `cancelAlarm`；下一次 `start` 会重挂）。
pub fn stop() {
    if let Some(h) = handle() {
        h.stop();
    }
    *slot().lock().unwrap() = None;
    RUNNING.store(false, Ordering::SeqCst);
}

/// 主循环体（单测可直接喂 fake 时钟驱动 `tick_once`）。
pub async fn tick_once(state: &CheckinState) {
    let cfg = crate::config::get();
    let now = crate::auth::model::now_ms();
    let off = local_offset_sec(now);
    let due = due_now(
        cfg.checkin.trae_enabled,
        &cfg.checkin.trae_time,
        cfg.checkin.qoder_enabled,
        &cfg.checkin.qoder_time,
        state.trae_last_run_date().as_deref(),
        state.qoder_last_run_date().as_deref(),
        state.auto_window_date().as_deref(),
        now,
        off,
    );
    if due.trae {
        // run_scheduled = busy 抢锁 + runAll + 通知（通知只在 okCount>0 时发）
        crate::checkin::batch::run_scheduled(state, &crate::auth::store::TraeAuthStore::global()).await;
    }
    if due.qoder {
        crate::checkin::qoder_batch::run_scheduled(state, &crate::auth::store::QoderAuthStore::global()).await;
    }
    if due.window {
        state.set_auto_window_date(&today_sgt(now));
        crate::checkin::catchup::run_all_once(state, &crate::auth::store::TraeAuthStore::global(), &crate::auth::store::QoderAuthStore::global()).await;
    }
}

/// 常驻任务：启动即补签，然后 15s tick（§9.3 的落点）。
pub async fn run_loop(state: std::sync::Arc<CheckinState>, cancel: std::sync::Arc<AtomicBool>, wake: std::sync::Arc<Notify>) {
    // 开机/启动补签：等价 Android「进 App 必调 maybeCatchUp」+「AutoCheckin 冷却语义原样保留」
    let trae = crate::auth::store::TraeAuthStore::global();
    let qoder = crate::auth::store::QoderAuthStore::global();
    crate::checkin::batch::maybe_catch_up(&state, &trae).await;
    crate::checkin::qoder_batch::maybe_catch_up(&state, &qoder).await;
    crate::checkin::catchup::run_all_once(&state, &trae, &qoder).await;

    let tick = std::time::Duration::from_millis(TICK_MS);
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        // notified() future 先建好再进 select，避免 stop()/reschedule() 落在 sleep 与注册之间的窗口丢信号；
        // 丢信号最坏也只是等满一个 tick（15s），不会漏任务。
        let woke = wake.notified();
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = woke => {}
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        tick_once(&state).await;
    }
    RUNNING.store(false, Ordering::SeqCst);
}

/// 不变量自检（§3.1 对齐语义 + §7 常量表）。
pub fn self_check() -> Result<(), String> {
    if DEFAULT_TRAE_TIME != "08:00" || DEFAULT_QODER_TIME != "09:00" {
        return Err("自检失败：缺省时刻应为 08:00 / 09:00".into());
    }
    if parse_minutes("08:00") != Some(480) || parse_minutes("9:05") != Some(545) {
        return Err("自检失败：HH:mm 解析不对".into());
    }
    if parse_minutes("8:5").is_some() || parse_minutes("").is_some() || parse_minutes("24:00") != Some(23 * 60) {
        return Err("自检失败：损坏时刻必须回落（不得当成 0 点跑）".into());
    }
    let off = 8 * 3600;
    // 早于时刻 → 今天；晚于/等于时刻 → 明天（Kotlin `<=now` 语义）
    let before = next_run_at("10:00", 1_760_000_000_000, off);
    let morn = today_at(600, 1_760_000_000_000, off);
    if !(before == morn || before == morn + DAY_MS) {
        return Err("自检失败：next_run_at 必须落在当地 10:00 整".into());
    }
    if (before + off as i64 * 1000) % 60_000 != 0 {
        return Err("自检失败：秒/毫秒必须清零".into());
    }
    // 时刻损坏回落缺省；超范围是夹住（Kotlin coerceIn），不是回落
    if minutes_or("", DEFAULT_TRAE_TIME) != 480 || minutes_or(":00", DEFAULT_QODER_TIME) != 540 {
        return Err("自检失败：损坏时刻应回落缺省值".into());
    }
    if minutes_or("99:99", DEFAULT_TRAE_TIME) != 23 * 60 + 59 {
        return Err("自检失败：99:99 必须夹到 23:59（coerceIn 语义）".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: i32 = 8 * 3600; // 固定 +08:00，便于精确断言

    fn ms(y_md_hm: &str) -> i64 {
        // "2026-11-02T00:00" 按 +08:00 解释成毫秒
        let sgt = chrono::FixedOffset::east_opt(OFF * 1000 / 1000).unwrap();
        chrono::NaiveDateTime::parse_from_str(y_md_hm, "%Y-%m-%dT%H:%M")
            .map(|d| sgt.from_local_datetime(&d).single().unwrap().timestamp_millis())
            .unwrap()
    }

    #[test]
    fn parse_minutes_table() {
        assert_eq!(parse_minutes("08:00"), Some(480));
        assert_eq!(parse_minutes("8:00"), Some(480), "Kotlin 正则允许 1 位小时");
        assert_eq!(parse_minutes("23:59"), Some(1439));
        assert_eq!(parse_minutes("00:00"), Some(0));
        // 超出范围按 coerce 夹住（Kotlin coerceIn 语义，不报错）
        assert_eq!(parse_minutes("99:99"), Some(23 * 60 + 59));
        // 损坏 → None（调用方回落缺省）
        for bad in ["", "8", "8:0", ":00", "08:000", "a:00", "8:0a", "-1:00"] {
            assert_eq!(parse_minutes(bad), None, "损坏值 {bad:?} 必须判无效");
        }
    }

    #[test]
    fn next_run_alignment() {
        // 07:00 时设 08:00 → 今天 08:00
        let now = ms("2026-11-02T07:00");
        assert_eq!(next_run_at("08:00", now, OFF), ms("2026-11-02T08:00"));
        // 恰好等于时刻 → 明天（`<= now` 顺延，绝不能当天重复触发）
        let at = ms("2026-11-02T08:00");
        assert_eq!(next_run_at("08:00", at, OFF), ms("2026-11-03T08:00"));
        // 已过 → 明天
        assert_eq!(next_run_at("08:00", ms("2026-11-02T23:59"), OFF), ms("2026-11-03T08:00"));
        // 秒/毫秒清零
        let messy = ms("2026-11-02T07:59") + 59_999;
        assert_eq!(next_run_at("08:00", messy, OFF) % 1000, 0);
        // 损坏时刻回落 08:00
        assert_eq!(next_run_at("", ms("2026-11-02T07:00"), OFF), ms("2026-11-02T08:00"));
        // 月/年边界：12-31 23:59 → 次年 01-01 08:00
        assert_eq!(next_run_at("08:00", ms("2026-12-31T23:59"), OFF), ms("2027-01-01T08:00"));
        // 负偏移（UTC-5）同样自洽
        let off_w = -5 * 3600;
        let now_w = {
            let w = chrono::FixedOffset::east_opt(off_w * 1000 / 1000).unwrap();
            chrono::NaiveDateTime::parse_from_str("2026-11-02T07:00", "%Y-%m-%dT%H:%M")
                .map(|d| w.from_local_datetime(&d).single().unwrap().timestamp_millis())
                .unwrap()
        };
        assert_eq!(next_run_at("08:00", now_w, off_w), now_w + 3_600_000);
    }

    #[test]
    fn due_rule_gating() {
        let at8 = ms("2026-11-02T08:00");
        let today = today_local(at8);
        // 未到点 / 到点 / 今天已跑
        assert!(!due_at(true, "08:00", DEFAULT_TRAE_TIME, at8 - 1, OFF, None, &today));
        assert!(due_at(true, "08:00", DEFAULT_TRAE_TIME, at8, OFF, None, &today), "到点即跑（不再像 Android 那样错过）");
        assert!(!due_at(true, "08:00", DEFAULT_TRAE_TIME, at8, OFF, Some(&today.clone()), &today), "今天跑过即去重");
        // 开关关 → 永不
        assert!(!due_at(false, "08:00", DEFAULT_TRAE_TIME, at8 + 1000, OFF, None, &today));
        // 昨天跑过 → 今天照跑
        assert!(due_at(true, "08:00", DEFAULT_TRAE_TIME, at8, OFF, Some("2026-11-01"), &today));
        // 进程 15:00 才启动、时刻 08:00 → 当天补跑
        assert!(due_at(true, "08:00", DEFAULT_TRAE_TIME, ms("2026-11-02T15:00"), OFF, None, &today));
    }

    #[test]
    fn window_arrival_and_next() {
        // 09:00 SGT 未到窗；10:05 整到窗
        assert!(!window_arrived(ms("2026-11-02T09:00")));
        assert!(window_arrived(ms("2026-11-02T10:05")));
        assert_eq!(sgt_window_today(ms("2026-11-02T00:00")), ms("2026-11-02T10:05"));
        // next_window_at 恒在未来且落在 10:05
        let n = next_window_at(ms("2026-11-02T10:05"));
        assert_eq!(n, ms("2026-11-03T10:05"));
        assert!(n > ms("2026-11-02T10:05"));
    }

    #[test]
    fn due_now_combines_three() {
        let at = ms("2026-11-02T10:05");
        let local_today = today_local(at);
        let sgt_today = today_sgt(at);
        let d = due_now(true, "08:00", true, "09:00", None, None, None, at, OFF);
        assert!(d.trae && d.qoder && d.window, "10:05 已过 08:00/09:00 两条时刻且开窗未跑");
        let d2 = due_now(true, "08:00", true, "09:00", Some(&local_today), Some(&local_today), Some(&sgt_today), at, OFF);
        assert!(!d2.trae && !d2.qoder && !d2.window, "各自按各自日期口径去重");
        let d3 = due_now(true, "08:00", false, "09:00", None, None, Some(&sgt_today), at, OFF);
        assert!(d3.trae && !d3.qoder && !d3.window, "平台开关独立生效");
    }

    #[test]
    fn reschedule_is_noop_before_start() {
        // reschedule 在未启动时不 panic（闹钟本就未挂的等价语义）
        if handle().is_none() {
            reschedule();
        }
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
