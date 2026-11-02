//! Qoder 批量领取编排（checkin-credits.md §4，QoderCheckin.kt 对应体）。
//!
//! 与 Trae [super::batch] **结构同构但完全独立**：各一套落盘 key、busy 标志、通知渠道/文案。
//! Qoder 不走本地转发、不持 Trae 的续期锁（Qoder 续期在 [super::qoder_api] 内部按账号串行），
//! 因此这里**不加任何全局锁**，只做"账号间隔 600ms + 逐账号错误隔离"。

use crate::auth::model::{now_ms, QoderAccount};
use crate::auth::store::QoderAuthStore;
use crate::checkin::qoder_api::{self, QClaimResult};
use crate::checkin::state::{today_local, CheckinState};
use crate::emitter::{emit, topics};
use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// Qoder 账号间隔（`QoderAuthApi.claimDailyAll` 内）。
pub const PER_ACCOUNT_DELAY_MS: u64 = 600;

pub const NOTIFY_CHANNEL_ID: &str = "qoder_checkin";
pub const NOTIFY_CHANNEL_NAME: &str = "Qoder 领取结果";
pub const NOTIFY_CHANNEL_DESC: &str = "Qoder 每日定时领取 Credits 的成功通知";
pub const NOTIFY_TITLE: &str = "Qoder 每日领取完成";

/// `status` 四态（Qoder 域没有 `disabled`）；持久化字符串逐字对齐 Kotlin。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QStatus {
    Ok,
    Already,
    Unsupported,
    Fail,
}

impl QStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QStatus::Ok => "ok",
            QStatus::Already => "already",
            QStatus::Unsupported => "unsupported",
            QStatus::Fail => "fail",
        }
    }

    /// 读出缺省 `fail`（Kotlin `optString("status","fail")`）。
    pub fn parse_lenient(s: &str) -> Self {
        match s {
            "ok" => QStatus::Ok,
            "already" => QStatus::Already,
            "unsupported" => QStatus::Unsupported,
            _ => QStatus::Fail,
        }
    }
}

impl Serialize for QStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match <Option<String>>::deserialize(d)? {
            Some(v) => QStatus::parse_lenient(&v),
            None => QStatus::Fail,
        })
    }
}

/// `PerAccount(uid, name, status, amount, message)`——字段名是 `name`/`amount`（Trae 用 nickname/credits）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QPerAccount {
    pub uid: String,
    pub name: String,
    pub status: QStatus,
    pub amount: i64,
    pub message: String,
}

impl QPerAccount {
    pub fn new(uid: &str, name: &str, status: QStatus, amount: i64, message: &str) -> Self {
        Self { uid: uid.to_string(), name: name.to_string(), status, amount, message: message.to_string() }
    }

    pub fn ok(&self) -> bool {
        self.status == QStatus::Ok
    }

    /// `statusLabel`（Qoder 版，§4 表）：amount=0 的成功是查看类活动，禁写 "+0" 骗人。
    pub fn status_label(&self) -> String {
        match self.status {
            QStatus::Ok => {
                if self.amount > 0 {
                    format!("✓ 领取成功 +{}", self.amount)
                } else if self.message.trim().is_empty() {
                    "✓ 已领取".to_string()
                } else {
                    format!("✓ 已领取（{}）", self.message)
                }
            }
            QStatus::Already => {
                if self.message.trim().is_empty() { "今日已领取".to_string() } else { self.message.clone() }
            }
            QStatus::Unsupported => "该区域/活动暂不可领（见消息）".to_string(),
            QStatus::Fail => format!("失败：{}", self.message),
        }
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("uid".into(), Value::String(self.uid.clone()));
        o.insert("name".into(), Value::String(self.name.clone()));
        o.insert("status".into(), Value::String(self.status.as_str().to_string()));
        o.insert("amount".into(), Value::Number(self.amount.into()));
        o.insert("message".into(), Value::String(self.message.clone()));
        Value::Object(o)
    }
}

/// `BatchResult(results, date)`（Qoder 侧独立类型）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QBatchResult {
    pub results: Vec<QPerAccount>,
    pub date: String,
}

impl QBatchResult {
    /// `allOk`：非空 && 每项 ∈ {ok, already, unsupported}（Qoder 把 unsupported 也算处理完）。
    pub fn all_ok(&self) -> bool {
        !self.results.is_empty()
            && self.results.iter().all(|r| {
                matches!(r.status, QStatus::Ok | QStatus::Already | QStatus::Unsupported)
            })
    }

    pub fn ok_count(&self) -> usize {
        self.results.iter().filter(|r| r.ok()).count()
    }

    pub fn to_json(&self) -> Value {
        Value::Array(self.results.iter().map(|r| r.to_value()).collect())
    }

    /// 定时路径通知文本（`okCount==0` 不发）。单行带**总额**，Trae 不带。
    pub fn notify_texts(&self) -> Option<(String, String)> {
        let ok = self.results.iter().filter(|r| r.ok()).collect::<Vec<_>>();
        if ok.is_empty() {
            return None;
        }
        let total: i64 = ok.iter().map(|r| r.amount).sum();
        let detail = ok.iter().map(|r| format!("{} +{}", r.name, r.amount)).collect::<Vec<_>>().join("、");
        Some((format!("{} 个账号领取成功，共 +{total} Credits", ok.len()), format!("{} 个账号领取成功：{detail}", ok.len())))
    }
}

/// `lastResults`：last_results 或 last_run_date 缺失/损坏 → None；amount 缺省 0。
pub fn last_results(state: &CheckinState) -> Option<QBatchResult> {
    let date = state.qoder_last_run_date()?;
    let arr = state.qoder_last_results()?;
    let results: Vec<QPerAccount> = arr.iter().filter_map(q_from_value).collect();
    if results.is_empty() {
        None
    } else {
        Some(QBatchResult { results, date })
    }
}

fn q_from_value(v: &Value) -> Option<QPerAccount> {
    let o = v.as_object()?;
    let s = |k: &str| -> String { o.get(k).and_then(Value::as_str).unwrap_or("").to_string() };
    Some(QPerAccount {
        uid: s("uid"),
        name: s("name"),
        status: QStatus::parse_lenient(&s("status")),
        amount: o.get("amount").and_then(Value::as_i64).unwrap_or(0),
        message: s("message"),
    })
}

/// `lastRunToday`。
pub fn last_run_today(state: &CheckinState) -> bool {
    state.qoder_last_run_date().as_deref() == Some(today_local(now_ms()).as_str())
}

// ---------------------------------------------------------------------------
// 按账号已领标记（qoder_claimed，键为**裸 uid**，与 AutoCheckin 的前缀键两套，§8 陷阱 6）
// ---------------------------------------------------------------------------

/// `markClaimed`。
pub fn mark_claimed(state: &CheckinState, uid: &str) {
    state.put_qoder_claimed(uid, &today_local(now_ms()));
}

/// `setClaimed(claimed)`：按服务端事实改写（true 写今天 / false 删标记）；查不到传 None 不改。
pub fn set_claimed(state: &CheckinState, uid: &str, claimed: Option<bool>) {
    match claimed {
        Some(true) => mark_claimed(state, uid),
        Some(false) => state.remove_qoder_claimed(uid),
        None => {}
    }
}

/// `claimedToday`：`map[uid] == today()`，损坏/缺省 false。
pub fn claimed_today(state: &CheckinState, uid: &str) -> bool {
    state.qoder_claimed_map().get(uid).and_then(Value::as_str) == Some(today_local(now_ms()).as_str())
}

/// 只读校准（切号后用 `claimed_today_for` 改写本地标记）。
pub async fn calibrate_claimed(state: &CheckinState, store: &QoderAuthStore, cred: &QoderAccount) {
    let fact = qoder_api::claimed_today_for(state, store, cred).await;
    set_claimed(state, &cred.uid, fact);
}

// ---------------------------------------------------------------------------
// 批量编排
// ---------------------------------------------------------------------------

fn to_per_account(meta_name: &str, uid: &str, r: &QClaimResult) -> QPerAccount {
    let name = if meta_name.trim().is_empty() { uid.to_string() } else { meta_name.to_string() };
    QPerAccount::new(uid, &name, QStatus::parse_lenient(&r.status), r.amount, &r.message)
}

/// `runAll`：全部已保存 Qoder 账号逐个领取（600ms 间隔、逐账号错误隔离）；
/// `ok`/`already` 都打已领标记（只记 ok 会让已领的号每天重复补跑）；跑完落盘。
pub async fn run_all(state: &CheckinState, store: &QoderAuthStore) -> QBatchResult {
    run_all_with_delay(state, store, PER_ACCOUNT_DELAY_MS).await
}

/// 间隔可注入（单测免等 600ms）。
pub async fn run_all_with_delay(state: &CheckinState, store: &QoderAuthStore, delay_ms: u64) -> QBatchResult {
    let metas = store.list();
    let mut results: Vec<QPerAccount> = Vec::with_capacity(metas.len());
    for (i, meta) in metas.iter().enumerate() {
        if i > 0 && delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        let r = qoder_api::claim_daily_for_uid(state, store, &meta.uid).await;
        let p = to_per_account(&meta.name, &meta.uid, &r);
        // 领过 / 确认无可领都算收工，不再每天补跑
        if matches!(p.status, QStatus::Ok | QStatus::Already) {
            mark_claimed(state, &p.uid);
        }
        results.push(p);
    }
    let date = today_local(now_ms());
    let br = QBatchResult { results, date: date.clone() };
    state.set_qoder_run(&date, &br.to_json());
    br
}

/// `notifyIfScheduled`：定时路径专用（手动路径不发——用户就在屏幕前）。
pub fn notify_if_scheduled(result: &QBatchResult) {
    let (text, big) = match result.notify_texts() {
        Some(t) => t,
        None => return,
    };
    let mut p = Map::new();
    p.insert("platform".into(), Value::String("qoder".into()));
    p.insert("channel".into(), Value::String(NOTIFY_CHANNEL_ID.into()));
    p.insert("title".into(), Value::String(NOTIFY_TITLE.into()));
    p.insert("text".into(), Value::String(text));
    p.insert("summary".into(), Value::String(big));
    p.insert("okCount".into(), Value::Number(result.ok_count().into()));
    emit(topics::CHECKIN_RESULT, Value::Object(p));
}

/// 独立 busy 标志（与 Trae 那条互不参与）。RAII 释放，future 被丢弃也不泄漏。
static BUSY: AtomicBool = AtomicBool::new(false);

struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

fn try_acquire() -> Option<BusyGuard> {
    BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| BusyGuard)
}

pub fn is_busy() -> bool {
    BUSY.load(Ordering::Relaxed)
}

/// `maybeCatchUp`：与 Trae 逐行同构（开关关→false；开着先自愈重挂；今天全领→跳过；busy 抢不到→false）。
pub async fn maybe_catch_up(state: &CheckinState, store: &QoderAuthStore) -> bool {
    if !crate::config::get().checkin.qoder_enabled {
        return false;
    }
    crate::checkin::scheduler::reschedule();
    if last_run_today(state) {
        if let Some(last) = last_results(state) {
            if last.all_ok() {
                return false;
            }
        }
    }
    let _guard = match try_acquire() {
        Some(g) => g,
        None => return false,
    };
    // Kotlin maybeCatchUp 只 runAll 不发通知（通知专属闹钟路径，§3.4）
    run_all(state, store).await;
    true
}

/// `QoderCheckinReceiver` 的对应体：runAll + 通知（自续由 scheduler tick 负责）。
pub async fn run_scheduled(state: &CheckinState, store: &QoderAuthStore) -> QBatchResult {
    let _guard = match try_acquire() {
        Some(g) => g,
        None => return QBatchResult { results: Vec::new(), date: today_local(now_ms()) },
    };
    let br = run_all(state, store).await;
    notify_if_scheduled(&br);
    br
}

/// 不变量自检（§4 selfCheck 同构）。
pub fn self_check() -> Result<(), String> {
    if PER_ACCOUNT_DELAY_MS != 600 {
        return Err("自检失败：Qoder 账号间隔应 600ms".into());
    }
    if CONFIG_TIME_DEFAULT != "09:00" {
        return Err("自检失败：Qoder 默认时刻应 09:00".into());
    }
    let src = vec![
        QPerAccount::new("q-1", "甲", QStatus::Ok, 100, ""),
        QPerAccount::new("q-2", "乙", QStatus::Fail, 0, "拉取活动失败"),
    ];
    let br = QBatchResult { results: src.clone(), date: "2026-11-02".into() };
    let back: Vec<QPerAccount> = br
        .to_json()
        .as_array()
        .expect("to_json 必为数组")
        .iter()
        .filter_map(q_from_value)
        .collect();
    if back != src {
        return Err("自检失败：Qoder 领取结果往返不一致".into());
    }
    for s in [QStatus::Ok, QStatus::Already, QStatus::Unsupported, QStatus::Fail] {
        if s.as_str().is_empty() || QPerAccount::new("u", "n", s, 0, "x").status_label().trim().is_empty() {
            return Err(format!("自检失败：status={} 缺文案", s.as_str()));
        }
    }
    let label = |s: QStatus, amt: i64, msg: &str| QPerAccount::new("u", "n", s, amt, msg).status_label();
    if label(QStatus::Ok, 100, "") != "✓ 领取成功 +100"
        || label(QStatus::Ok, 0, "") != "✓ 已领取"
        || label(QStatus::Ok, 0, "领取 2 条（无 Credits）") != "✓ 已领取（领取 2 条（无 Credits））"
        || label(QStatus::Already, 0, "") != "今日已领取"
        || label(QStatus::Already, 0, "已领取 3 条") != "已领取 3 条"
        || label(QStatus::Unsupported, 0, "x") != "该区域/活动暂不可领（见消息）"
        || label(QStatus::Fail, 0, "x") != "失败：x"
    {
        return Err("自检失败：statusLabel 文案错位".into());
    }
    if !(QBatchResult { results: vec![QPerAccount::new("a", "a", QStatus::Ok, 1, "")], date: String::new() }).all_ok() {
        return Err("自检失败：全成功应 allOk".into());
    }
    if !(QBatchResult { results: vec![QPerAccount::new("a", "a", QStatus::Already, 0, "")], date: String::new() }).all_ok() {
        return Err("自检失败：已领应 allOk".into());
    }
    if (QBatchResult { results: vec![QPerAccount::new("a", "a", QStatus::Fail, 0, "x")], date: String::new() }).all_ok() {
        return Err("自检失败：有失败不应 allOk".into());
    }
    if (QBatchResult { results: vec![], date: String::new() }).all_ok() {
        return Err("自检失败：空结果不应 allOk".into());
    }
    Ok(())
}

/// `QoderCheckin` 的 `schedule_time` 缺省值。
pub const CONFIG_TIME_DEFAULT: &str = "09:00";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DataStore;
    use std::sync::Arc;

    fn tmp_state(tag: &str) -> CheckinState {
        let dir = std::env::temp_dir().join(format!("tw-qbatch-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CheckinState::new(Arc::new(DataStore::new(dir)))
    }

    #[test]
    fn status_label_table_qoder() {
        let label = |s: QStatus, amt: i64, msg: &str| QPerAccount::new("u", "n", s, amt, msg).status_label();
        assert_eq!(label(QStatus::Ok, 120, ""), "✓ 领取成功 +120");
        assert_eq!(label(QStatus::Ok, 0, ""), "✓ 已领取", "查看类活动禁写 +0");
        assert_eq!(label(QStatus::Ok, 0, "领取 2 条（无 Credits）"), "✓ 已领取（领取 2 条（无 Credits））");
        assert_eq!(label(QStatus::Already, 0, ""), "今日已领取");
        assert_eq!(label(QStatus::Already, 0, "无待领活动（1 条均已处理）"), "无待领活动（1 条均已处理）");
        assert_eq!(label(QStatus::Unsupported, 0, "x"), "该区域/活动暂不可领（见消息）");
        assert_eq!(label(QStatus::Fail, 0, "凭据读取失败"), "失败：凭据读取失败");
    }

    #[test]
    fn all_ok_includes_unsupported_excludes_fail() {
        assert!(QBatchResult {
            results: vec![QPerAccount::new("a", "a", QStatus::Unsupported, 0, ""), QPerAccount::new("b", "b", QStatus::Already, 0, "")],
            date: String::new()
        }
        .all_ok());
        assert!(!QBatchResult { results: vec![QPerAccount::new("a", "a", QStatus::Fail, 0, "x")], date: String::new() }.all_ok());
        assert!(!QBatchResult { results: vec![], date: String::new() }.all_ok(), "空列表 false");
    }

    #[test]
    fn json_roundtrip_lenient_read() {
        let st = tmp_state("json");
        assert_eq!(last_results(&st), None);
        let br = QBatchResult {
            results: vec![QPerAccount::new("1", "甲", QStatus::Ok, 100, ""), QPerAccount::new("2", "乙", QStatus::Already, 0, "今日已领取")],
            date: "2026-11-02".into(),
        };
        st.set_qoder_run("2026-11-02", &br.to_json());
        assert_eq!(last_results(&st).unwrap(), br);

        // status 缺省 fail、amount 缺省 0
        st.set_qoder_run("2026-11-03", &serde_json::json!([{"uid":"3"}]));
        let back = last_results(&st).unwrap();
        assert_eq!(back.results[0].status, QStatus::Fail);
        assert_eq!(back.results[0].amount, 0);
        // date 缺失 → None
        let st2 = tmp_state("nodate");
        st2.set_qoder_run("2026-11-02", &br.to_json());
        st2.set_qoder_run("", &Value::Array(vec![]));
        assert!(last_results(&st2).is_none() || last_results(&st2).unwrap().results.is_empty());
    }

    #[test]
    fn notify_texts_with_total_amount() {
        assert!(QBatchResult { results: vec![QPerAccount::new("a", "a", QStatus::Already, 0, "")], date: String::new() }.notify_texts().is_none());
        let br = QBatchResult {
            results: vec![
                QPerAccount::new("1", "甲", QStatus::Ok, 100, ""),
                QPerAccount::new("2", "乙", QStatus::Ok, 50, ""),
                QPerAccount::new("3", "丙", QStatus::Fail, 0, "x"),
            ],
            date: "2026-11-02".into(),
        };
        let (text, big) = br.notify_texts().unwrap();
        assert_eq!(text, "2 个账号领取成功，共 +150 Credits", "Qoder 单行带总额（Trae 不带）");
        assert_eq!(big, "2 个账号领取成功：甲 +100、乙 +50");
    }

    #[test]
    fn claimed_marker_per_account() {
        let st = tmp_state("claimed");
        assert!(!claimed_today(&st, "q-1"));
        mark_claimed(&st, "q-1");
        assert!(claimed_today(&st, "q-1"), "写今天即已领");
        assert!(!claimed_today(&st, "q-2"), "按账号记，一号不影响另一号");
        set_claimed(&st, "q-1", Some(false));
        assert!(!claimed_today(&st, "q-1"), "false 删标记");
        set_claimed(&st, "q-1", Some(true));
        set_claimed(&st, "q-1", None);
        assert!(claimed_today(&st, "q-1"), "None（查不到）不改显示");
    }

    #[test]
    fn to_per_account_falls_back_to_uid() {
        let r = QClaimResult { status: "fail".into(), amount: 0, message: "凭据读取失败".into() };
        let p = to_per_account("  ", "q-9", &r);
        assert_eq!(p.name, "q-9");
        assert_eq!(p.status, QStatus::Fail);
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
