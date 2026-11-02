//! Trae 批量签到编排（checkin-credits.md §2，BatchCheckin.kt 对应体）。
//!
//! 锁粒度（§8 陷阱 4 + 任务约束）：Kotlin `runAll` 全程持 `Gateway.locked`（可重入）。
//! parking_lot `ReentrantMutex` **不是 `Send`**，跨 `.await` 持锁会让整个 future 失去 Send，
//! 无法被 tokio 调度。这里把临界区收窄到"每次 token 续期"——续期本身已由
//! `auth::api::trae` 内部的 tokio `refresh_lock` 串行化（等价 Kotlin tokenLock 语义），
//! 批量路径因此天然串行续期、且绝不拿快照 refreshToken 自换（§2 第 2' 行的令牌族作废坑）。
//! 账号间隔 800ms 保留（上游按设备维度风控，连发易撞 9074）。

use crate::auth::api::trae::ensure_fresh_token_for;
use crate::auth::model::{now_ms, TraeAccount, TraeAccountMeta};
use crate::auth::store::TraeAuthStore;
use crate::checkin::state::{today_local, CheckinState};
use crate::checkin::trae_api;
use crate::emitter::{emit, topics};
use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// `PER_ACCOUNT_DELAY_MS`。
pub const PER_ACCOUNT_DELAY_MS: u64 = 800;
/// 批量/监测/页查统一的 token 临期窗（**秒**域，Trae）。
pub const TOKEN_SKEW_SEC: i64 = 3600;

/// `CHANNEL_ID`（Android 通知渠道，桌面侧作为事件 payload 的 channel 字段保留）。
pub const NOTIFY_CHANNEL_ID: &str = "traework2api_checkin";
/// Android 渠道名/描述（桌面通知沿用）。
pub const NOTIFY_CHANNEL_NAME: &str = "签到结果";
pub const NOTIFY_CHANNEL_DESC: &str = "每日定时签到的成功通知";
pub const NOTIFY_TITLE: &str = "每日签到完成";

/// `PerAccount.status` 五态；持久化字符串逐字对齐 Kotlin。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    Ok,
    Already,
    Disabled,
    Unsupported,
    Fail,
}

impl AccountStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountStatus::Ok => "ok",
            AccountStatus::Already => "already",
            AccountStatus::Disabled => "disabled",
            AccountStatus::Unsupported => "unsupported",
            AccountStatus::Fail => "fail",
        }
    }

    /// `lastResults` 读出缺省 `fail`（Kotlin `optString("status","fail")`）。
    pub fn parse_lenient(s: &str) -> Self {
        match s {
            "ok" => AccountStatus::Ok,
            "already" => AccountStatus::Already,
            "disabled" => AccountStatus::Disabled,
            "unsupported" => AccountStatus::Unsupported,
            _ => AccountStatus::Fail,
        }
    }
}

impl Serialize for AccountStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AccountStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match <Option<String>>::deserialize(d)? {
            Some(v) => AccountStatus::parse_lenient(&v),
            None => AccountStatus::Fail,
        })
    }
}

/// `PerAccount(uid, nickname, status, credits, message)`；credits 非成功/已签一律 -1。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerAccount {
    pub uid: String,
    pub nickname: String,
    pub status: AccountStatus,
    pub credits: i64,
    pub message: String,
}

impl PerAccount {
    pub fn new(uid: &str, nickname: &str, status: AccountStatus, credits: i64, message: &str) -> Self {
        Self { uid: uid.to_string(), nickname: nickname.to_string(), status, credits, message: message.to_string() }
    }

    pub fn ok(&self) -> bool {
        self.status == AccountStatus::Ok
    }

    pub fn uid_or_name(&self) -> &str {
        if self.nickname.trim().is_empty() { &self.uid } else { &self.nickname }
    }

    /// `statusLabel`（账号页每行；§2 表，ok 行含正数）。
    pub fn status_label(&self) -> String {
        match self.status {
            AccountStatus::Ok => format!("✓ 签到成功 +{}", self.credits),
            AccountStatus::Already => "已签到".to_string(),
            AccountStatus::Disabled => "未开放签到".to_string(),
            AccountStatus::Unsupported => "该区域未支持签到".to_string(),
            AccountStatus::Fail => format!("失败：{}", self.message),
        }
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("uid".into(), Value::String(self.uid.clone()));
        o.insert("nickname".into(), Value::String(self.nickname.clone()));
        o.insert("status".into(), Value::String(self.status.as_str().to_string()));
        o.insert("credits".into(), Value::Number(self.credits.into()));
        o.insert("message".into(), Value::String(self.message.clone()));
        Value::Object(o)
    }
}

/// `BatchResult(results, date)`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchResult {
    pub results: Vec<PerAccount>,
    pub date: String,
}

impl BatchResult {
    /// `allOk`：**非空** && 每项 ∈ {ok, already, disabled}。
    /// unsupported 不在白名单；空列表 false（防"0 账号时误去重"）。
    pub fn all_ok(&self) -> bool {
        !self.results.is_empty()
            && self.results.iter().all(|r| {
                matches!(r.status, AccountStatus::Ok | AccountStatus::Already | AccountStatus::Disabled)
            })
    }

    pub fn ok_count(&self) -> usize {
        self.results.iter().filter(|r| r.ok()).count()
    }

    /// `toJson(results)`（落盘用，字段名逐字）。
    pub fn to_json(&self) -> Value {
        Value::Array(self.results.iter().map(|r| r.to_value()).collect())
    }

    /// 定时路径通知文本（§3.4：`okCount==0` 不发）。返回 (单行 contentText, BigText)。
    pub fn notify_texts(&self) -> Option<(String, String)> {
        let ok_count = self.ok_count();
        if ok_count == 0 {
            return None;
        }
        let detail = self
            .results
            .iter()
            .filter(|r| r.ok())
            .map(|r| format!("{} +{}", r.uid_or_name(), r.credits))
            .collect::<Vec<_>>()
            .join("、");
        Some((
            format!("{ok_count} 个账号签到成功（{detail}）"),
            format!("{ok_count} 个账号签到成功：{detail}"),
        ))
    }
}

/// `lastResults(ctx)`：`last_results` 或 `last_run_date` 缺失/JSON 损坏 → None；
/// 每元素按字段名取，status 缺省 "fail"、credits 缺省 -1。
pub fn last_results(state: &CheckinState) -> Option<Vec<PerAccount>> {
    if state.trae_last_run_date().is_none() {
        return None;
    }
    let arr = state.trae_last_results()?;
    let out: Vec<PerAccount> = arr.iter().filter_map(per_account_from_value).collect();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn per_account_from_value(v: &Value) -> Option<PerAccount> {
    let o = v.as_object()?;
    let s = |k: &str| -> String { o.get(k).and_then(Value::as_str).unwrap_or("").to_string() };
    Some(PerAccount {
        uid: s("uid"),
        nickname: s("nickname"),
        status: AccountStatus::parse_lenient(&s("status")),
        credits: o.get("credits").and_then(Value::as_i64).unwrap_or(-1),
        message: s("message"),
    })
}

/// `lastRunToday(ctx)`。
pub fn last_run_today(state: &CheckinState) -> bool {
    state.trae_last_run_date().as_deref() == Some(today_local(now_ms()).as_str())
}

// ---------------------------------------------------------------------------
// 批量编排
// ---------------------------------------------------------------------------

/// `checkinOne` 判定表第 3~11 行的纯映射（before=null 即表内第 3 行）。
fn outcome_of(
    uid: &str,
    name: &str,
    before: Option<trae_api::CheckinStatus>,
    after: Option<trae_api::CheckinStatus>,
    code: Option<i32>,
    auth_failed: bool,
) -> PerAccount {
    let before = match before {
        Some(b) => b,
        None => return PerAccount::new(uid, name, AccountStatus::Fail, -1, "拉取签到状态失败（网络或上游异常）"),
    };
    if before.checked_in {
        return PerAccount::new(uid, name, AccountStatus::Already, before.credits, "");
    }
    if !before.enable {
        return PerAccount::new(uid, name, AccountStatus::Disabled, -1, "该账号无签到权限");
    }
    if trae_api::checkin_succeeded(Some(&before), after.as_ref(), code) {
        // 合取判据成立 ⇒ after 必为 Some
        let c = after.map(|a| a.credits).unwrap_or(-1);
        return PerAccount::new(uid, name, AccountStatus::Ok, c, "");
    }
    if let Some(a) = &after {
        if a.checked_in {
            // 跃迁已发生但合取没过（如 code 异常）：按已签归档，不报错
            return PerAccount::new(uid, name, AccountStatus::Already, a.credits, "");
        }
    }
    match code {
        None => PerAccount::new(
            uid,
            name,
            AccountStatus::Fail,
            -1,
            if auth_failed { "token 被服务端拒绝，需重新登录" } else { "网络不通，签到请求未送达" },
        ),
        Some(9074) => PerAccount::new(uid, name, AccountStatus::Fail, -1, "上游判定操作过于频繁（9074），请稍后手动重试"),
        Some(c) => PerAccount::new(uid, name, AccountStatus::Fail, -1, &format!("签到未生效（code={c}）")),
    }
}

/// 单账号全流程（失败不抛——一人失败不拖垮整批）。§2 表 0~11 行。
pub async fn checkin_one(store: &TraeAuthStore, meta: &TraeAccountMeta) -> PerAccount {
    let name = meta.nickname.trim().is_empty().then(|| meta.uid.clone()).unwrap_or_else(|| meta.nickname.clone());
    let region = crate::upstream::trae::TraeRegion::of(meta.region());
    if !region.is_supported() || !trae_api::can_query_quota(region) {
        return PerAccount::new(&meta.uid, &name, AccountStatus::Unsupported, -1, "该区域未支持签到");
    }
    let mut cred: TraeAccount = match store.load_by_uid(&meta.uid) {
        Some(c) => c,
        None => return PerAccount::new(&meta.uid, &name, AccountStatus::Fail, -1, "凭据读取失败"),
    };
    // 临近/已过期只走统一入口续期（绝不自换：旧 refreshToken 重放会作废整条令牌族）
    if cred.access_token.trim().is_empty() || TraeAuthStore::expired_within(cred.expires_at, TOKEN_SKEW_SEC) {
        match ensure_fresh_token_for(store, &cred, TOKEN_SKEW_SEC).await {
            Some(tok) if !tok.trim().is_empty() => cred.access_token = tok,
            _ => return PerAccount::new(&meta.uid, &name, AccountStatus::Fail, -1, "token 已过期且续不回来，需重新登录"),
        }
    }
    let before = match trae_api::fetch_checkin_healing(store, &cred).await {
        Some(b) => b,
        None => return outcome_of(&meta.uid, &name, None, None, None, trae_api::last_auth_failed()),
    };
    if before.checked_in || !before.enable {
        return outcome_of(&meta.uid, &name, Some(before), None, None, false);
    }
    let code = trae_api::claim_checkin_healing(store, &cred).await;
    let auth_failed = trae_api::last_auth_failed();
    let after = trae_api::fetch_checkin_healing(store, &cred).await;
    outcome_of(&meta.uid, &name, Some(before), after, code, auth_failed)
}

/// `runAll`：只覆盖 Trae 账号且第一步过滤国际服（一个请求都不发）；跑完落盘 last_run_date/last_results。
pub async fn run_all(state: &CheckinState, store: &TraeAuthStore) -> BatchResult {
    run_all_with_delay(state, store, PER_ACCOUNT_DELAY_MS).await
}

/// 间隔可注入（单测免等 800ms）。
pub async fn run_all_with_delay(state: &CheckinState, store: &TraeAuthStore, delay_ms: u64) -> BatchResult {
    let metas: Vec<TraeAccountMeta> = store.list().into_iter().filter(|m| m.is_supported()).collect();
    let mut results: Vec<PerAccount> = Vec::with_capacity(metas.len());
    for (i, meta) in metas.iter().enumerate() {
        if i > 0 && delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        results.push(checkin_one(store, meta).await);
    }
    let date = today_local(now_ms());
    let br = BatchResult { results, date: date.clone() };
    state.set_trae_run(&date, &br.to_json());
    br
}

/// `notifyIfScheduled`：定时路径专用（手动路径不发——用户就在屏幕前）。
pub fn notify_if_scheduled(result: &BatchResult) {
    let (text, big) = match result.notify_texts() {
        Some(t) => t,
        None => return,
    };
    let mut p = Map::new();
    p.insert("platform".into(), Value::String("trae".into()));
    p.insert("channel".into(), Value::String(NOTIFY_CHANNEL_ID.into()));
    p.insert("title".into(), Value::String(NOTIFY_TITLE.into()));
    p.insert("text".into(), Value::String(text));
    p.insert("summary".into(), Value::String(big));
    p.insert("okCount".into(), Value::Number(result.ok_count().into()));
    emit(topics::CHECKIN_RESULT, Value::Object(p));
}

/// `busy` 互斥（RAII：future 被丢弃也要释放，否则补签永久卡死）。
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

/// `maybeCatchUp`：开关关→false；只要开关开着就自愈重挂闹钟；
/// 今天跑过且 allOk→不再打上游；busy 抢不到→false（手动优先，结果由先到者产出）。
pub async fn maybe_catch_up(state: &CheckinState, store: &TraeAuthStore) -> bool {
    if !crate::config::get().checkin.trae_enabled {
        return false;
    }
    crate::checkin::scheduler::reschedule();
    if last_run_today(state) {
        if let Some(results) = last_results(state) {
            if (BatchResult { results, date: today_local(now_ms()) }).all_ok() {
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

/// 定时闹钟入口（CheckinReceiver 对应体：runAll + 通知；自续由 scheduler 的每日判定负责）。
pub async fn run_scheduled(state: &CheckinState, store: &TraeAuthStore) -> BatchResult {
    let _guard = match try_acquire() {
        Some(g) => g,
        None => return BatchResult { results: Vec::new(), date: today_local(now_ms()) },
    };
    let r = run_all(state, store).await;
    notify_if_scheduled(&r);
    r
}

// ---------------------------------------------------------------------------
// 自检（§2 selfCheck 不变量）
// ---------------------------------------------------------------------------

pub fn self_check() -> Result<(), String> {
    use crate::auth::model::now_sec;
    // exp=0/负数判过期；未来 7200s 的 exp 在 skew=3600 下判未过期
    if !TraeAuthStore::expired_within(0, TOKEN_SKEW_SEC) || !TraeAuthStore::expired_within(-1, TOKEN_SKEW_SEC) {
        return Err("自检失败：exp≤0 必须判过期".into());
    }
    if TraeAuthStore::expired_within(now_sec() + 7200, TOKEN_SKEW_SEC) {
        return Err("自检失败：未来 7200s 的 exp 在 skew=3600 下不该判过期".into());
    }
    // 五种 status 文案齐全
    for s in [AccountStatus::Ok, AccountStatus::Already, AccountStatus::Disabled, AccountStatus::Unsupported, AccountStatus::Fail] {
        if s.as_str().is_empty() {
            return Err("自检失败：status 字符串缺失".into());
        }
    }
    let lbl = |s: AccountStatus| PerAccount::new("u", "n", s, 100, "x").status_label();
    if lbl(AccountStatus::Ok) != "✓ 签到成功 +100"
        || lbl(AccountStatus::Already) != "已签到"
        || lbl(AccountStatus::Disabled) != "未开放签到"
        || lbl(AccountStatus::Unsupported) != "该区域未支持签到"
        || lbl(AccountStatus::Fail) != "失败：x"
    {
        return Err("自检失败：statusLabel 文案错位".into());
    }
    // 结果 JSON 往返全等（uid 是主键，串了会把 A 的结果显示给 B）
    let r = BatchResult {
        results: vec![
            PerAccount::new("1", "甲", AccountStatus::Ok, 150, ""),
            PerAccount::new("2", "", AccountStatus::Fail, -1, "网络不通，签到请求未送达"),
        ],
        date: "2026-11-02".into(),
    };
    let back: Vec<PerAccount> = r
        .to_json()
        .as_array()
        .expect("to_json 必为数组")
        .iter()
        .filter_map(per_account_from_value)
        .collect();
    if back != r.results {
        return Err("自检失败：结果 JSON 往返不等".into());
    }
    // allOk 四例
    if !(BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Ok, 1, "")], date: String::new() }).all_ok() {
        return Err("自检失败：全成功应 allOk".into());
    }
    if !(BatchResult {
        results: vec![PerAccount::new("a", "a", AccountStatus::Already, 1, ""), PerAccount::new("b", "b", AccountStatus::Disabled, -1, "")],
        date: String::new(),
    })
    .all_ok()
    {
        return Err("自检失败：already/disabled 应算处理完".into());
    }
    if (BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Fail, -1, "x")], date: String::new() }).all_ok() {
        return Err("自检失败：有 fail 不该 allOk".into());
    }
    if (BatchResult { results: vec![], date: String::new() }).all_ok() {
        return Err("自检失败：空列表判 false（防 0 账号误去重）".into());
    }
    // 余量语义：1>0 不算用完 / 0 算 / 负数算（§5 保守判据在 credits 侧，这里钉 Trae 计数）
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkin::trae_api::CheckinStatus;
    use crate::storage::DataStore;
    use std::sync::Arc;

    fn cs(checked_in: bool, credits: i64, enable: bool) -> CheckinStatus {
        CheckinStatus { checked_in, credits, enable }
    }

    fn tmp_state(tag: &str) -> CheckinState {
        let dir = std::env::temp_dir().join(format!("tw-batch-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CheckinState::new(Arc::new(DataStore::new(dir)))
    }

    /// §2 判定表全行（纯函数，无网络）。
    #[test]
    fn outcome_table_all_rows() {
        let (u, n) = ("uid-1", "昵称");
        let r = outcome_of(u, n, None, None, None, false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Fail, -1, "拉取签到状态失败（网络或上游异常）"));

        let r = outcome_of(u, n, Some(cs(true, 150, true)), None, None, false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Already, 150, ""), "已签 → already 带 before.credits");

        let r = outcome_of(u, n, Some(cs(false, 150, false)), None, None, false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Disabled, -1, "该账号无签到权限"));

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(true, 150, true)), Some(0), false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Ok, 150, ""), "合取通过取 after.credits");

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(true, 90, true)), Some(9004), false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Already, 90, ""), "跃迁成立但合取没过 → 按已签归档");

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(false, 150, true)), None, true);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Fail, -1, "token 被服务端拒绝，需重新登录"));

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(false, 150, true)), None, false);
        assert_eq!(r.message, "网络不通，签到请求未送达");

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(false, 150, true)), Some(9074), true);
        assert_eq!(r.message, "上游判定操作过于频繁（9074），请稍后手动重试", "9074 优先于 auth 二分");

        let r = outcome_of(u, n, Some(cs(false, 150, true)), Some(cs(false, 150, true)), Some(9300), false);
        assert_eq!((r.status, r.credits, r.message.as_str()), (AccountStatus::Fail, -1, "签到未生效（code=9300）"));

        // before 已签优先于任何 code（第 4 行在 claim 之前）
        let r = outcome_of(u, n, Some(cs(true, 150, true)), Some(cs(true, 150, true)), Some(201), false);
        assert_eq!(r.status, AccountStatus::Already);
    }

    #[test]
    fn status_label_table() {
        assert_eq!(PerAccount::new("u", "n", AccountStatus::Ok, 150, "").status_label(), "✓ 签到成功 +150");
        assert_eq!(PerAccount::new("u", "n", AccountStatus::Already, 150, "").status_label(), "已签到");
        assert_eq!(PerAccount::new("u", "n", AccountStatus::Disabled, -1, "x").status_label(), "未开放签到");
        assert_eq!(PerAccount::new("u", "n", AccountStatus::Unsupported, -1, "x").status_label(), "该区域未支持签到");
        assert_eq!(PerAccount::new("u", "n", AccountStatus::Fail, -1, "网络不通").status_label(), "失败：网络不通");
        assert_eq!(PerAccount::new("u", "  ", AccountStatus::Ok, 1, "").uid_or_name(), "u", "nickname 空白回落 uid");
    }

    #[test]
    fn json_roundtrip_and_lenient_defaults() {
        let st = tmp_state("json");
        assert_eq!(last_results(&st), None, "未落盘 → None");
        let br = BatchResult {
            results: vec![PerAccount::new("1", "甲", AccountStatus::Ok, 150, ""), PerAccount::new("2", "乙", AccountStatus::Fail, -1, "x")],
            date: "2026-11-02".into(),
        };
        st.set_trae_run("2026-11-02", &br.to_json());
        assert_eq!(last_results(&st).unwrap(), br.results, "字段名逐字往返");
        assert!(last_run_today(&st) == (today_local(now_ms()) == "2026-11-02"));

        // 缺字段容错：status 缺省 fail、credits 缺省 -1；非字符串 credits 不强转
        let dirty = serde_json::json!([{"uid":"3"},{"uid":"4","status":"weird","credits":"7"}]);
        st.set_trae_run("2026-11-03", &dirty);
        let back = last_results(&st).unwrap();
        assert_eq!(back[0].status, AccountStatus::Fail);
        assert_eq!(back[0].credits, -1);
        assert_eq!(back[1].credits, -1);
        // 数组空 → None（等价 Kotlin 空 JSONArray 视作无有效结果）
        st.set_trae_run("2026-11-04", &Value::Array(vec![]));
        assert_eq!(last_results(&st), None);
    }

    #[test]
    fn all_ok_and_notify_gate() {
        assert!(BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Ok, 10, "")], date: String::new() }.all_ok());
        assert!(BatchResult {
            results: vec![PerAccount::new("a", "a", AccountStatus::Already, 10, ""), PerAccount::new("b", "b", AccountStatus::Disabled, -1, "")],
            date: String::new()
        }
        .all_ok());
        assert!(!BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Unsupported, -1, "")], date: String::new() }.all_ok());
        assert!(!BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Fail, -1, "x")], date: String::new() }.all_ok());
        assert!(!BatchResult { results: vec![], date: String::new() }.all_ok(), "空列表 false");

        let already_only = BatchResult { results: vec![PerAccount::new("a", "a", AccountStatus::Already, 10, "")], date: String::new() };
        assert!(already_only.notify_texts().is_none(), "okCount==0 不发通知");

        let br = BatchResult {
            results: vec![
                PerAccount::new("1", "", AccountStatus::Ok, 150, ""),
                PerAccount::new("2", "乙", AccountStatus::Ok, 100, ""),
                PerAccount::new("3", "丙", AccountStatus::Fail, -1, "x"),
            ],
            date: "2026-11-02".into(),
        };
        let (text, big) = br.notify_texts().unwrap();
        assert_eq!(text, "2 个账号签到成功（1 +150、乙 +100）");
        assert_eq!(big, "2 个账号签到成功：1 +150、乙 +100");
    }

    #[test]
    fn busy_guard_releases_and_blocks() {
        let g1 = try_acquire().expect("首次必抢到");
        assert!(is_busy());
        assert!(try_acquire().is_none(), "两个入口撞同一时刻：抢不到即跳过本轮");
        drop(g1);
        assert!(!is_busy());
    }

    #[test]
    fn supported_filter_keeps_blank_region() {
        let metas = vec![
            TraeAccountMeta { uid: "cn".into(), region_id: "cn".into(), ..Default::default() },
            TraeAccountMeta { uid: "sg".into(), region_id: "sg".into(), ..Default::default() },
            TraeAccountMeta { uid: "blank".into(), region_id: "".into(), ..Default::default() },
        ];
        let kept: Vec<&str> = metas.iter().filter(|m| m.is_supported()).map(|m| m.uid.as_str()).collect();
        assert_eq!(kept, vec!["cn", "blank"], "国际服被过滤出候选集；空白 region 按国服对待");
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
