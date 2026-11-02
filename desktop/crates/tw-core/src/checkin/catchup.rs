//! 跨平台开窗补签（checkin-credits.md §3.3，AutoCheckin.kt 对应体）。
//!
//! 与 §2/§4 的"用户可配时刻批量"是**两套并行机制**：这里时刻固定（Asia/Singapore 10:05 开窗），
//! 按**账号**记"今天已确认处理完"（`trae:{uid}` / `qoder:{uid}`），并允许当日多次重试
//! （活动 10:00 才开，若 9:50 记"今天已跑过"，10 点后永远不领）。
//!
//! 三条纪律原样保留：① 按账号记；② 只补没签的（先查状态，已签/无权限直接 markDone，绝不硬发 claim）；
//! ③ 单点失败不外抛、且不 markDone（留到下轮重试）。
//!
//! 判据统一（§8 陷阱 3 的移植决策）：Kotlin 这里 Trae 成功判定写的是 `code in 200..299`，
//! 而上游成功码是 `0`，实际靠"下一轮看到 checkedIn=true 走 markDone 分支"收敛。
//! 本实现改用与 [super::batch] 相同的 [super::trae_api::checkin_succeeded] 合取判据
//! （claim 后补查一次状态），行为与 Kotlin 最终一致但**当轮即收口**，不再有"下轮才收敛"的窗口。

use crate::auth::model::{TraeAccount, TraeAccountMeta};
use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::checkin::{qoder_api, qoder_batch, trae_api};
use crate::checkin::state::{today_local, CheckinState};
use chrono::{TimeZone, Timelike};
use serde_json::Map;

/// `ACTION_ALARM`（Android 侧 action 字符串，桌面保留为文档常量）。
pub const ACTION_AUTO: &str = "com.example.traework2api.AUTO_CHECKIN";
/// `REQ_AUTO`（Android 闹钟 request code，桌面保留为文档常量）。
pub const ALARM_REQUEST: i32 = 8801;
/// `RETRY_COOLDOWN_MS`。
pub const RETRY_COOLDOWN_MS: i64 = 600_000;
/// `WINDOW_HOUR`（活动 10:00 UTC+8 开，留 5 分钟余量）。
pub const WINDOW_HOUR: u32 = 10;
/// `WINDOW_MINUTE`。
pub const WINDOW_MINUTE: u32 = 5;

/// 平台标识（done 键前缀，§8 陷阱 6：两套键格式别混）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Trae,
    Qoder,
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Trae => "trae",
            Platform::Qoder => "qoder",
        }
    }
}

/// `"$platform:$uid"`。
pub fn done_key(platform: Platform, uid: &str) -> String {
    format!("{}:{}", platform.as_str(), uid)
}

/// `isDone(key)` / `doneToday(platform, uid)`：界面用它显示「已签到」而**零请求**。
pub fn is_done(state: &CheckinState, platform: Platform, uid: &str) -> bool {
    let today = today_local(crate::auth::model::now_ms());
    state.auto_done_map().get(&done_key(platform, uid)).and_then(serde_json::Value::as_str) == Some(today.as_str())
}

pub fn done_today(state: &CheckinState, platform: Platform, uid: &str) -> bool {
    is_done(state, platform, uid)
}

/// `markDone(key)`：只动该键（读整 map→put→整串写回由 [CheckinState::mark_auto_done] 负责）。
pub fn mark_done(state: &CheckinState, platform: Platform, uid: &str) {
    state.mark_auto_done(&done_key(platform, uid), &today_local(crate::auth::model::now_ms()));
}

/// 待处理项（`平台 to uid`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingItem {
    pub platform: Platform,
    pub uid: String,
}

/// `pending()` 的纯计算内核：凡 `doneMap["{plat}:{uid}"] != today` 的都算待处理。
/// 一旦确认"已签/无权限/已领取"即 markDone → 后续轮**一个请求都不发**。
pub fn pending_keys(
    trae_uids: &[String],
    qoder_uids: &[String],
    done: &Map<String, serde_json::Value>,
    today: &str,
) -> Vec<PendingItem> {
    let mut out = Vec::with_capacity(trae_uids.len() + qoder_uids.len());
    for uid in trae_uids {
        if done.get(&done_key(Platform::Trae, uid)).and_then(serde_json::Value::as_str) != Some(today) {
            out.push(PendingItem { platform: Platform::Trae, uid: uid.clone() });
        }
    }
    for uid in qoder_uids {
        if done.get(&done_key(Platform::Qoder, uid)).and_then(serde_json::Value::as_str) != Some(today) {
            out.push(PendingItem { platform: Platform::Qoder, uid: uid.clone() });
        }
    }
    out
}

pub fn pending(state: &CheckinState, trae: &TraeAuthStore, qoder: &QoderAuthStore) -> Vec<PendingItem> {
    let t: Vec<String> = trae.list().iter().map(|m: &TraeAccountMeta| m.uid.clone()).collect();
    let q: Vec<String> = qoder.list().iter().map(|m| m.uid.clone()).collect();
    pending_keys(&t, &q, &state.auto_done_map(), &today_local(crate::auth::model::now_ms()))
}

/// `alreadyRanToday`：注意这是 `pending().isEmpty()`，去重实际由**按账号** done 决定，本函数只做展示。
pub fn already_ran_today(state: &CheckinState, trae: &TraeAuthStore, qoder: &QoderAuthStore) -> bool {
    pending(state, trae, qoder).is_empty()
}

/// `shouldAttempt` 的纯计算内核：有待处理 && 距上次尝试 ≥ 冷却。
/// `last_try=0`（从未）等价"立刻可跑"。
pub fn should_attempt_at(pending_len: usize, last_try_ms: i64, now_ms: i64) -> bool {
    // last_try=0 语义为"从未跑过 / requestRunSoon 清零" → 立刻放行；时钟回拨为负 → 不放行
    pending_len > 0 && (last_try_ms == 0 || now_ms - last_try_ms >= RETRY_COOLDOWN_MS)
}

pub fn should_attempt(state: &CheckinState, trae: &TraeAuthStore, qoder: &QoderAuthStore) -> bool {
    should_attempt_at(
        pending(state, trae, qoder).len(),
        state.auto_last_try(),
        crate::auth::model::now_ms(),
    )
}

/// `requestRunSoon`：冷却置 0 —— 新登录/新粘贴账号必须立刻可补签（用户加完号就切走，等不到下一轮）。
pub fn request_run_soon(state: &CheckinState) {
    state.set_auto_last_try(0);
}

/// SGT 指定日期（NaiveDate）的 10:05:00.000 → DateTime（固定偏移无 DST，恒存在）。
pub(crate) fn window_dt(sgt: &chrono::FixedOffset, day: chrono::NaiveDate) -> chrono::DateTime<chrono::FixedOffset> {
    let naive = day
        .and_hms_opt(WINDOW_HOUR, WINDOW_MINUTE, 0)
        .expect("自检失败：10:05 恒为合法墙上时间");
    sgt.from_local_datetime(&naive).single().expect("固定偏移无 DST")
}

/// `nextWindowAt`：SGT 今天或明天的 10:05:00.000（`<=now` 顺延一天），返回 unix 毫秒。
pub fn next_window_at(now_ms: i64) -> i64 {
    let sgt = chrono::FixedOffset::east_opt(8 * 3600).expect("固定 +08:00 恒可用");
    let now = sgt.timestamp_millis_opt(now_ms).single().expect("毫秒时间戳可定位");
    let mut day = now.date_naive();
    let mut target = window_dt(&sgt, day).timestamp_millis();
    if target <= now_ms {
        day += chrono::Duration::days(1);
        target = window_dt(&sgt, day).timestamp_millis();
    }
    target
}

// ---------------------------------------------------------------------------
// 跑一轮
// ---------------------------------------------------------------------------

fn display_name(nickname: &str, uid: &str) -> String {
    if nickname.trim().is_empty() { uid.to_string() } else { nickname.to_string() }
}

async fn trae_items(state: &CheckinState, store: &TraeAuthStore, todo: &[PendingItem], parts: &mut Vec<String>) {
    let names: Vec<(String, String)> = store.list().iter().map(|m| (m.uid.clone(), m.nickname.clone())).collect();
    for item in todo.iter().filter(|p| p.platform == Platform::Trae) {
        let uid = &item.uid;
        let cred: TraeAccount = match store.load_by_uid(uid) {
            Some(c) => c,
            None => continue,
        };
        if cred.access_token.trim().is_empty() {
            continue; // 不 markDone：留到下轮（续期后即可用）
        }
        let before = match trae_api::fetch_checkin_healing(store, &cred).await {
            Some(b) => b,
            None => continue,
        };
        if before.checked_in || !before.enable {
            mark_done(state, Platform::Trae, uid); // 已签 / 未开放 → 今天不用再管这个号
            continue;
        }
        let claimed = trae_api::claim_checkin_healing(store, &cred).await;
        if claimed.is_none() {
            continue;
        }
        let after = trae_api::fetch_checkin_healing(store, &cred).await;
        if !trae_api::checkin_succeeded(Some(&before), after.as_ref(), claimed) {
            continue; // 未确认生效 → 不 markDone，下轮重试
        }
        mark_done(state, Platform::Trae, uid);
        let name = names
            .iter()
            .find(|(u, _)| u == uid)
            .map(|(_, n)| display_name(n, uid))
            .unwrap_or_else(|| uid.clone());
        parts.push(format!("Trae {name} 已签到"));
    }
}

/// `runAllOnce`：未到冷却/无待处理返回 None；摘要 `parts.join(" · ")`（**只记日志，不发通知**，
/// 通知专属 §2/§4 的闹钟路径）。
pub async fn run_all_once(state: &CheckinState, trae: &TraeAuthStore, qoder: &QoderAuthStore) -> Option<String> {
    run_all_once_at(state, trae, qoder, crate::auth::model::now_ms()).await
}

/// 时间可注入版。
pub async fn run_all_once_at(state: &CheckinState, trae: &TraeAuthStore, qoder: &QoderAuthStore, now_ms: i64) -> Option<String> {
    let todo = pending(state, trae, qoder);
    if !should_attempt_at(todo.len(), state.auto_last_try(), now_ms) {
        return None;
    }
    state.set_auto_last_try(now_ms);
    let mut parts: Vec<String> = Vec::new();

    trae_items(state, trae, &todo, &mut parts).await;

    // Qoder：有未处理项才跑整批（复用定时领取那一套，含 600ms 间隔与结果落盘）
    if todo.iter().any(|p| p.platform == Platform::Qoder) {
        let r = qoder_batch::run_all(state, qoder).await;
        for p in &r.results {
            // 领到了或确认无可领 → 这个号今天收工；失败留着下轮重试（用裸 uid 的 qoder_claimed 之外，
            // 这里再记平台前缀键，两套键各自服务各自的读取方）
            if matches!(p.status, qoder_batch::QStatus::Ok | qoder_batch::QStatus::Already) {
                mark_done(state, Platform::Qoder, &p.uid);
            }
        }
        parts.push(format!("Qoder {}", r.results.iter().map(qoder_batch::QPerAccount::status_label).collect::<Vec<_>>().join("；")));
    }

    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

/// 供 UI/日志显示"这一轮做了哪些号"（不发请求）。
pub async fn claim_one_qoder(state: &CheckinState, qoder: &QoderAuthStore, uid: &str) -> qoder_api::QClaimResult {
    qoder_api::claim_daily_for_uid(state, qoder, uid).await
}

/// 不变量自检（§3.3 selfCheck）。
pub fn self_check() -> Result<(), String> {
    // 日期键必须随日期变化：写死会让"每天一次"退化成"永远只跑一次"
    let today = today_local(crate::auth::model::now_ms());
    if today.len() != 10 || today[4..5] != *"-" || today[7..8] != *"-" {
        return Err("自检失败：日期格式不对".into());
    }
    if RETRY_COOLDOWN_MS < 60_000 {
        return Err("自检失败：重试冷却过短".into());
    }
    // 账号维度的键必须带平台前缀，否则 Trae/Qoder 撞 uid 时互相顶掉
    if done_key(Platform::Trae, "abc") == done_key(Platform::Qoder, "abc") {
        return Err("自检失败：账号键必须带平台前缀".into());
    }
    // 窗口必须落在 10:00 之后几分钟：算成 9:55 就等于永远抢不到当天活动
    let sgt = chrono::FixedOffset::east_opt(8 * 3600).expect("固定偏移");
    let cal = sgt.timestamp_millis_opt(next_window_at(0)).single().expect("epoch 可定位");
    if cal.hour() != WINDOW_HOUR || cal.minute() != WINDOW_MINUTE {
        return Err(format!("自检失败：开窗时刻应为 {WINDOW_HOUR:02}:{WINDOW_MINUTE:02}，实为 {cal:?}"));
    }
    let now = crate::auth::model::now_ms();
    if next_window_at(now) <= now {
        return Err("自检失败：下一次开窗必须在未来".into());
    }
    // 冷却边界：last_try=0（从未跑/requestRunSoon 清零）立刻放行；非零则整 10 分钟才放行；无待处理永不打
    if !should_attempt_at(1, 0, 1) {
        return Err("自检失败：last_try=0 必须立刻放行".into());
    }
    if should_attempt_at(1, 1_000, 1_000 + RETRY_COOLDOWN_MS - 1) || !should_attempt_at(1, 1_000, 1_000 + RETRY_COOLDOWN_MS) {
        return Err("自检失败：冷却边界不对（差 1ms 不放、整 10 分钟放）".into());
    }
    if should_attempt_at(0, 0, i64::MAX) {
        return Err("自检失败：无待处理绝不打上游".into());
    }
    if should_attempt_at(1, 5_000, 4_000) {
        return Err("自检失败：时钟回拨不放行".into());
    }
    Ok(())
}

/// `runAllOnce` 的桌面补充语义：SGT 开窗任务只在"当天还没跑过开窗轮"时触发一次，
/// 跑完记日期；跨天自动重来（Kotlin 靠闹钟自续，这里靠日期戳去重）。
pub fn window_due(state: &CheckinState, now_ms: i64) -> bool {
    state.auto_window_date().as_deref() != Some(crate::checkin::state::today_sgt(now_ms).as_str())
}

pub fn mark_window_ran(state: &CheckinState, now_ms: i64) {
    state.set_auto_window_date(&crate::checkin::state::today_sgt(now_ms));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkin::state::{today_local, today_sgt};
    use crate::storage::DataStore;
    use serde_json::{json, Value};
    use std::sync::Arc;

    fn tmp_state(tag: &str) -> CheckinState {
        let dir = std::env::temp_dir().join(format!("tw-catchup-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CheckinState::new(Arc::new(DataStore::new(dir)))
    }

    fn done_of(pairs: &[(&str, &str)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), json!(*v));
        }
        m
    }

    #[test]
    fn keys_are_platform_prefixed() {
        assert_eq!(done_key(Platform::Trae, "abc"), "trae:abc");
        assert_eq!(done_key(Platform::Qoder, "abc"), "qoder:abc");
        assert_ne!(done_key(Platform::Trae, "abc"), done_key(Platform::Qoder, "abc"), "撞 uid 不得互顶");
    }

    #[test]
    fn pending_kernel_is_per_account_and_per_date() {
        let today = today_local(crate::auth::model::now_ms());
        let trae = vec!["t1".to_string(), "t2".to_string()];
        let qoder = vec!("q1".to_string());
        // 全空表 → 全待处理，顺序 Trae 后 Qoder
        let p = pending_keys(&trae, &qoder, &done_of(&[]), &today);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].uid, "t1");
        assert_eq!(p[2].platform, Platform::Qoder);

        // t1 今天已确认 → 只剩 t2/q1（"只触发未签到的"）
        let done = done_of(&[("trae:t1", today.as_str()), ("qoder:q1", "2020-01-01")]);
        let p = pending_keys(&trae, &qoder, &done, &today);
        assert_eq!(p.iter().map(|x| (x.platform, x.uid.clone())).collect::<Vec<_>>(),
                   vec![(Platform::Trae, "t2".to_string()), (Platform::Qoder, "q1".to_string())],
                   "昨日日期不算已完成，必须重新捡起");

        // 全部今天确认 → 空
        let done = done_of(&[("trae:t1", today.as_str()), ("trae:t2", today.as_str()), ("qoder:q1", today.as_str())]);
        assert!(pending_keys(&trae, &qoder, &done, &today).is_empty());

        // done 值损坏（非字符串）→ 按待处理（runCatching 同构的宽容方向：宁可多跑一轮）
        let mut bad = Map::new();
        bad.insert("trae:t1".into(), json!(12345));
        assert_eq!(pending_keys(&trae, &[], &bad, &today).len(), 2);
    }

    #[test]
    fn cooldown_boundaries() {
        assert!(!should_attempt_at(0, 0, 10_000), "无待处理绝不打上游");
        assert!(!should_attempt_at(1, 1_000, 1_000 + RETRY_COOLDOWN_MS - 1), "差 1ms 未到冷却");
        assert!(should_attempt_at(1, 1_000, 1_000 + RETRY_COOLDOWN_MS), "整 10 分钟放行");
        assert!(should_attempt_at(1, 0, 1), "last_try=0（从未/requestRunSoon）立刻可跑");
        // 时钟回拨(now<last_try) → 差值为负，不放行
        assert!(!should_attempt_at(1, 5_000, 4_000));
    }

    #[test]
    fn next_window_is_sgt_1005_strictly_future() {
        // epoch(=1970-01-01 00:00 UTC = SGT 08:00) → 当天 10:05 SGT = 02:05 UTC = 7_500_000 ms
        assert_eq!(next_window_at(0), 2 * 3_600_000 + 5 * 60_000, "开窗必须落在 SGT 10:05（UTC 02:05:00.000）");
        // 已过 10:05 → 顺延明天同一时刻，恒 +24h
        let win0 = 2 * 3_600_000 + 5 * 60_000;
        let w1 = next_window_at(win0);
        assert_eq!(w1, win0 + 86_400_000, "等于开窗时刻本身也要顺延（对齐 Kotlin <=now 语义）");
        let w2 = next_window_at(win0 + 1);
        assert_eq!(w2, w1);
        // 任意时刻都在未来
        for now in [0i64, win0, 86_399_999, 1_760_000_000_000] {
            assert!(next_window_at(now) > now, "下一次开窗必须在未来: now={now}");
        }
    }

    #[test]
    fn mark_done_roundtrip_only_touches_one_key() {
        let st = tmp_state("mark");
        mark_done(&st, Platform::Trae, "t1");
        assert!(is_done(&st, Platform::Trae, "t1"));
        assert!(!is_done(&st, Platform::Qoder, "t1"), "平台前缀隔离");
        assert_eq!(st.auto_done_map().len(), 1);
        mark_done(&st, Platform::Qoder, "t1");
        assert_eq!(st.auto_done_map().len(), 2, "只动该键，别的一条不丢");
        let other = today_local(crate::auth::model::now_ms() - 86_400_000);
        st.mark_auto_done("trae:old", &other);
        assert!(!is_done(&st, Platform::Trae, "old"), "昨日日期判未完成");
    }

    #[test]
    fn request_run_soon_clears_cooldown() {
        let st = tmp_state("cool");
        st.set_auto_last_try(crate::auth::model::now_ms());
        assert_eq!(st.auto_last_try() > 0, true);
        request_run_soon(&st);
        assert_eq!(st.auto_last_try(), 0, "新账号加进来必须立刻可补签");
    }

    #[test]
    fn display_name_falls_back_to_uid() {
        assert_eq!(display_name("", "u1"), "u1");
        assert_eq!(display_name("  ", "u1"), "u1");
        assert_eq!(display_name("甲", "u1"), "甲");
    }

    #[test]
    fn window_run_dedupes_by_sgt_date() {
        let st = tmp_state("window");
        let now = 1_760_000_000_000i64; // 2025-10-09 前后任意毫秒
        assert!(window_due(&st, now), "从未跑过开窗轮 → 该跑");
        mark_window_ran(&st, now);
        assert!(!window_due(&st, now), "同一 SGT 日内不重复跑");
        assert_eq!(st.auto_window_date().unwrap(), today_sgt(now));
        // 跨 SGT 日 → 重新该跑
        assert!(window_due(&st, now + 86_400_000 + 1));
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
