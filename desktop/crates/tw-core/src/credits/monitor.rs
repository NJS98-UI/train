//! 转发后余额监测与自动换号（checkin-credits.md §5，CreditMonitor.kt 对应体）。
//!
//! 关键历史教训（必须原样保留）：**禁止一切定时轮询**。
//! `DEFAULT_INTERVAL_SEC == 0` 被 selfCheck 钉死——曾为 2 秒轮询 `ide_user_ent_usage`，
//! 8 小时 14400 次，上游按刷接口处理 → token 被判废，症状"突然查不到积分/没有签到权限"。
//! 唯一刷新入口是 [`note_forwarded`]：用户在用哪个平台、且真的有请求转发过，才查那**一个**当前账号。
//!
//! 节流与并发：每平台各自一份时间戳（互不压制）；`urgent=true`（本次转发失败）跳过 60 秒窗口
//! 但**仍然更新时间戳**；每次通过即起一次性后台任务（无队列、无多 tick），
//! 同平台叠加由 `IN_FLIGHT` 原子标志拦下（§5 尾注）。

use crate::auth::api::qoder;
use crate::auth::api::trae;
use crate::auth::model::{now_ms, TraeAccount};
use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::checkin::trae_api;
use crate::config;
use crate::credits::account;
use crate::emitter::{emit, topics};
use crate::upstream::trae::TraeRegion;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicI64, Ordering};

/// `FORWARD_POLL_MIN_MS`：转发后刷新节流窗口。
pub const FORWARD_POLL_MIN_MS: i64 = 60_000;
/// `DEFAULT_INTERVAL_SEC`：**0**（禁轮询，selfCheck 钉死）。
pub const DEFAULT_INTERVAL_SEC: i64 = 0;
/// token 续期 skew（**秒**域，Trae；与 Qoder 的毫秒域绝不可交叉传）。
pub const TOKEN_SKEW_SEC: i64 = 3600;
/// noteForwarded 路径的 Qoder jobToken skew（**毫秒**域；AccountCredits 侧是 1 小时，claim 侧是 60s）。
pub const MONITOR_JT_SKEW_MS: i64 = 60_000;
/// `GatewayBackend.TRAE` / `.QODER` 常量。
pub const BACKEND_TRAE: &str = "trae";
pub const BACKEND_QODER: &str = "qoder";
/// 见底阈值缺省与下限（老版本存的 0 读时抬回 5，否则 UI 四档全不亮像坏了）。
pub const DEFAULT_THRESHOLD: i64 = 5;
pub const MIN_THRESHOLD: i64 = 5;

/// `Snapshot`（内存 latest，`currentSnapshot()` 直接回它，进页面立即可见）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorSnapshot {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub trae_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub trae_balance: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub qoder_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub qoder_remaining: Option<f64>,
    pub auto_switch: bool,
    pub threshold: i64,
}

static LATEST: std::sync::OnceLock<std::sync::RwLock<Option<MonitorSnapshot>>> = std::sync::OnceLock::new();
static LAST_TRAE: AtomicI64 = AtomicI64::new(0);
static LAST_QODER: AtomicI64 = AtomicI64::new(0);
static IN_FLIGHT: AtomicI64 = AtomicI64::new(0); // bit0=trae, bit1=qoder

fn latest_cell() -> &'static std::sync::RwLock<Option<MonitorSnapshot>> {
    LATEST.get_or_init(|| std::sync::RwLock::new(None))
}

/// `currentSnapshot()`。
pub fn current_snapshot() -> Option<MonitorSnapshot> {
    latest_cell().read().expect("latest 读锁不内嵌").clone()
}

/// `start(ctx)` = 只记 appCtx + 日志；`stop()` = 空方法（无后台线程，"stop 语义"即无状态可停）。
pub fn start() {
    let _ = config::get();
}

pub fn stop() {}

/// 每平台节流判据（纯函数，注入时钟可测）。
///
/// `urgent` 跳过窗口但仍由调用方盖时间戳（"不能让用户干等节流窗口才换号"）。
pub fn should_poll(last_at: i64, now_ms: i64, urgent: bool) -> bool {
    urgent || last_at == 0 || now_ms - last_at >= FORWARD_POLL_MIN_MS
}

fn stamp(backend: &str, now_ms: i64) {
    match backend {
        BACKEND_QODER => LAST_QODER.store(now_ms, Ordering::SeqCst),
        _ => LAST_TRAE.store(now_ms, Ordering::SeqCst),
    }
}

fn last_at(backend: &str) -> i64 {
    match backend {
        BACKEND_QODER => LAST_QODER.load(Ordering::Relaxed),
        _ => LAST_TRAE.load(Ordering::Relaxed),
    }
}

/// 唯一刷新入口。`backend` 取 `"trae"`/`"qoder"`；`uid` 空白/None 回落当前 active 账号。
///
/// 通过节流即**起一次性后台任务**（`tokio::spawn`），本函数不阻塞、不返回结果；
/// 结果落到 `latest` 并发 `credits-snapshot` 事件（与 [account] 的按账号快照合并后发出）。
pub fn note_forwarded(backend: &str, uid: Option<String>, urgent: bool) {
    note_forwarded_at(backend, uid, urgent, now_ms());
}

/// 时钟可注入版（单测免等 60s）。
pub fn note_forwarded_at(backend: &str, uid: Option<String>, urgent: bool, now_ms: i64) {
    let b = if backend == BACKEND_QODER { BACKEND_QODER } else { BACKEND_TRAE };
    if !should_poll(last_at(b), now_ms, urgent) {
        return;
    }
    stamp(b, now_ms); // 进窗即记，不等查询完成
    let mask = if b == BACKEND_QODER { 2 } else { 1 };
    let prev = IN_FLIGHT.fetch_or(mask, Ordering::SeqCst);
    if prev & mask != 0 {
        return; // 同平台已有一轮在跑，不叠加
    }
    let uid = uid.unwrap_or_default();
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        IN_FLIGHT.fetch_and(!mask, Ordering::SeqCst); // 无运行时：释放标志，等价 Kotlin runCatching 吞掉
        return;
    };
    let backend = b.to_string();
    rt.spawn(async move {
        let snap = poll(&backend, &uid, urgent).await;
        *latest_cell().write().expect("latest 写锁不内嵌") = Some(snap.clone());
        emit_snapshot(&snap);
        IN_FLIGHT.fetch_and(!mask, Ordering::SeqCst);
    });
}

/// 查询 + 见底判定 + 换号 + 对新号补查（§5 全流程）。`forward_failed=urgent`。
pub async fn poll(backend: &str, wanted_uid: &str, forward_failed: bool) -> MonitorSnapshot {
    let cfg = config::get();
    let auto_switch = cfg.auto_switch_enabled;
    let threshold = cfg.auto_switch_threshold.max(MIN_THRESHOLD);
    if backend == BACKEND_QODER {
        let (uid, remaining) = poll_qoder(wanted_uid, forward_failed, auto_switch, threshold).await;
        MonitorSnapshot { trae_uid: None, trae_balance: None, qoder_uid: Some(uid), qoder_remaining: remaining, auto_switch, threshold }
    } else {
        let (uid, balance) = poll_trae(wanted_uid, forward_failed, auto_switch, threshold).await;
        MonitorSnapshot { trae_uid: Some(uid), trae_balance: balance, qoder_uid: None, qoder_remaining: None, auto_switch, threshold }
    }
}

/// `pollTrae`：uid=Wanted 否则 active；无凭证/区域不支持 → `(uid, None)`。
/// 见底（保守）`bal <= threshold`；不可用 `bal == null && forwardFailed`（查不到数**且**本次由转发失败触发，
/// 才认定这号本身出问题；查得到数时只按阈值判——防网络抖动误换）。
pub async fn poll_trae(wanted_uid: &str, forward_failed: bool, auto_switch: bool, threshold: i64) -> (String, Option<i64>) {
    let store = TraeAuthStore::global();
    let uid = if wanted_uid.trim().is_empty() { store.active_uid().unwrap_or_default() } else { wanted_uid.to_string() };
    if uid.is_empty() {
        return (uid, None);
    }
    let Some(cred) = store.load_by_uid(&uid) else {
        return (uid, None);
    };
    let region = TraeRegion::of(&cred.region_id);
    if !region.is_supported() || !trae_api::can_query_quota(&region) {
        return (uid, None);
    }
    let bal = fetch_trae_balance(&store, &cred, &region).await;
    let exhausted = bal.is_some_and(|b| b <= threshold);
    let unusable = bal.is_none() && forward_failed;
    if !(exhausted || unusable) || !auto_switch {
        write_through(account::TRAE, &uid, bal.map(|b| b as f64), forward_failed);
        return (uid, bal);
    }
    match pick_trae_funded(&store, &uid, threshold).await {
        Some(next) => {
            // 下一个请求即走新号：setActive + 同步区域
            store.set_active(&next.uid);
            store.sync_region_to_active_account();
            let nbal = match store.load_by_uid(&next.uid) {
                Some(c) => {
                    let r = TraeRegion::of(&c.region_id);
                    fetch_trae_balance(&store, &c, &r).await
                }
                None => None,
            };
            // "换了另外一个号就刷另外一个号"
            write_through(account::TRAE, &next.uid, nbal.map(|b| b as f64), false);
            store.clear_auth_failed(&next.uid);
            (next.uid, nbal)
        }
        // 找不到候选 → 维持原号与原读数
        None => {
            write_through(account::TRAE, &uid, bal.map(|b| b as f64), forward_failed);
            (uid, bal)
        }
    }
}

/// `pickTraeFunded`：遍历其它受支持区域账号，逐个取有效 token 查余额，**第一个** `bal > threshold` 者返回。
pub async fn pick_trae_funded(store: &TraeAuthStore, exclude_uid: &str, threshold: i64) -> Option<crate::auth::model::TraeAccountMeta> {
    let mut cand: Vec<crate::auth::model::TraeAccountMeta> = store
        .list()
        .into_iter()
        .filter(|m| m.uid != exclude_uid && m.is_supported())
        .collect();
    cand.sort_by(|a, b| switch_pick_cmp(a, b, store));
    for m in cand {
        let Some(cred) = store.load_by_uid(&m.uid) else { continue };
        let region = TraeRegion::of(&cred.region_id);
        if let Some(b) = fetch_trae_balance(store, &cred, &region).await {
            if b > threshold {
                return Some(m);
            }
        }
    }
    None
}

/// Qoder 同构：见底 `credits != null && !hasCredit`（余量 Double、`hasCredit=!exceeded&&remaining>0`）；
/// 不可用 `credits == null && forwardFailed`；换号**只 setActive**（Qoder 无区域同步概念）。
pub async fn poll_qoder(wanted_uid: &str, forward_failed: bool, auto_switch: bool, threshold: i64) -> (String, Option<f64>) {
    let store = QoderAuthStore::global();
    let uid = if wanted_uid.trim().is_empty() { store.active_uid().unwrap_or_default() } else { wanted_uid.to_string() };
    if uid.is_empty() {
        return (uid, None);
    }
    let Some(cred) = store.load_by_uid(&uid) else {
        return (uid, None);
    };
    let credits = fetch_qoder_credits(&store, &cred).await;
    let bal = credits.as_ref().map(|c| c.remaining);
    // Trae 阈值是整数计数，Qoder 是 Double 余量：见底判据按 hasCredit，阈值仅在"余量<=threshold"时附加
    let exhausted = credits.as_ref().is_some_and(|c| !c.has_credit() || c.remaining <= threshold as f64);
    let unusable = credits.is_none() && forward_failed;
    if !(exhausted || unusable) || !auto_switch {
        write_through(account::QODER, &uid, bal, forward_failed);
        return (uid, bal);
    }
    match pick_qoder_funded(&store, &uid).await {
        Some(next) => {
            store.set_active(&next.uid);
            let nrem = match store.load_by_uid(&next.uid) {
                Some(c) => fetch_qoder_credits(&store, &c).await.map(|x| x.remaining),
                None => None,
            };
            write_through(account::QODER, &next.uid, nrem, false);
            store.clear_auth_failed(&next.uid);
            (next.uid, nrem)
        }
        None => {
            write_through(account::QODER, &uid, bal, forward_failed);
            (uid, bal)
        }
    }
}

pub async fn pick_qoder_funded(store: &QoderAuthStore, exclude_uid: &str) -> Option<crate::auth::model::QoderAccountMeta> {
    let mut cand: Vec<crate::auth::model::QoderAccountMeta> = store.list().into_iter().filter(|m| m.uid != exclude_uid).collect();
    cand.sort_by(|a, b| {
        // Qoder 无区域维度：按 uid 升序稳定遍历（Kotlin 是 list 原序，list 已按添加序落盘）
        a.uid.cmp(&b.uid)
    });
    for m in cand {
        let Some(cred) = store.load_by_uid(&m.uid) else { continue };
        if let Some(c) = fetch_qoder_credits(store, &cred).await {
            if c.has_credit() {
                return Some(m);
            }
        }
    }
    None
}

/// 换号候选排序（design.md：`region_is_supported > trust > remaining > balance`，tie uid 升序）。
/// Trae 侧候选已先过滤受支持区域，这里只剩"非 authFailed 优先 → uid 升序"。
fn switch_pick_cmp(a: &crate::auth::model::TraeAccountMeta, b: &crate::auth::model::TraeAccountMeta, store: &TraeAuthStore) -> std::cmp::Ordering {
    let trust = |m: &crate::auth::model::TraeAccountMeta| store.auth_failed_for(Some(&m.uid));
    trust(a).cmp(&trust(b)).then_with(|| a.uid.cmp(&b.uid))
}

/// 余额查询：有效 token（临期先续）→ `fetch_usage`；失败即 None。
async fn fetch_trae_balance(store: &TraeAuthStore, cred: &TraeAccount, region: &TraeRegion) -> Option<i64> {
    let token = trae::restart_with_valid_token_for(store, cred, TOKEN_SKEW_SEC).await?;
    if token.trim().is_empty() {
        return None;
    }
    trae_api::fetch_usage(region, &token, &cred.uid).await
}

async fn fetch_qoder_credits(store: &QoderAuthStore, cred: &crate::auth::model::QoderAccount) -> Option<crate::checkin::qoder_api::QCredits> {
    let jt = match qoder::valid_job_token_for(store, cred, MONITOR_JT_SKEW_MS).await {
        Some(t) if !t.trim().is_empty() => t,
        _ if !cred.job_token.trim().is_empty() => cred.job_token.clone(),
        _ => return None,
    };
    crate::checkin::qoder_api::fetch_credits(&jt, &cred.mode).await
}

/// 把 monitor 的单号读数写进按账号缓存（同一份数给账号页显示，避免两套读数打架）。
fn write_through(platform: &str, uid: &str, value: Option<f64>, forward_failed: bool) {
    let old = account::snapshot().get(platform, uid).cloned();
    let at = now_ms();
    let item = match value {
        Some(v) => account::Item::new(platform, uid, Some(v), v <= 0.0, None, at),
        None => {
            // 失败保留旧 remaining 与 at，只写 error（§6 规则 9）
            let err = if forward_failed { account::ERR_QUERY_FAILED } else { account::ERR_QUERY_FAILED };
            account::Item::new(platform, uid, old.as_ref().and_then(|o| o.remaining), false, Some(err.to_string()), old.as_ref().map(|o| o.at).unwrap_or(0))
        }
    };
    account::put_item(item);
}

/// 合并 monitor 与 account 两侧，发前端消费的 `credits-snapshot`
/// （字段名对齐 当前工作区/desktop/frontend/src/store/credits.ts 的 CreditsSnapshot）。
pub fn emit_snapshot(mon: &MonitorSnapshot) {
    emit(topics::CREDITS_SNAPSHOT, snapshot_json(Some(mon.clone())));
}

/// 快照 JSON：前端最小集 `loading/at/trae/qoder/total/error` 为必出字段，其余为桌面扩展。
pub fn snapshot_json(mon: Option<MonitorSnapshot>) -> Value {
    let acc = account::snapshot();
    let mut o = serde_json::Map::new();
    o.insert("loading".into(), Value::Bool(acc.loading));
    o.insert("at".into(), Value::Number(acc.at.into()));
    // 前端 trae/qoder 读数：优先 monitor 的当前账号数（刚查过的最新鲜），否则回落到按账号合计
    let trae_val = match (&mon, acc.sum(account::TRAE)) {
        (Some(m), _) if m.trae_balance.is_some() => m.trae_balance.map(|b| b as f64),
        (_, s) => s,
    };
    let qoder_val = match (&mon, acc.sum(account::QODER)) {
        (Some(m), _) if m.qoder_remaining.is_some() => m.qoder_remaining,
        (_, s) => s,
    };
    let num = |v: Option<f64>| v.map(|f| serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)).unwrap_or(Value::Null);
    o.insert("trae".into(), num(trae_val));
    o.insert("qoder".into(), num(qoder_val));
    o.insert("total".into(), num(match (trae_val, qoder_val) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }));
    let errors: Vec<String> = acc.items.values().filter(|i| i.error.as_ref().is_some_and(|e| !e.is_empty())).map(|i| format!("{}|{}:{}", i.platform, i.uid, i.error.clone().unwrap_or_default())).collect();
    o.insert("error".into(), if errors.is_empty() { Value::Null } else { Value::String(errors.join("; ")) });

    // 桌面扩展字段（审计/明细用）
    o.insert("traeTotal".into(), num(acc.sum(account::TRAE)));
    o.insert("qoderTotal".into(), num(acc.sum(account::QODER)));
    o.insert("traeLabel".into(), Value::String(acc.total_label(account::TRAE)));
    o.insert("qoderLabel".into(), Value::String(acc.total_label(account::QODER)));
    o.insert("totalAt".into(), Value::Number(acc.at.into()));
    o.insert("items".into(), Value::Array(acc.items.values().map(|i| item_json(i)).collect()));
    o.insert("authFailed".into(), {
        let mut m = serde_json::Map::new();
        m.insert(account::TRAE.into(), TraeAuthStore::global().auth_failed_uid().map(Value::String).unwrap_or(Value::Null));
        m.insert(account::QODER.into(), QoderAuthStore::global().auth_failed_uid().map(Value::String).unwrap_or(Value::Null));
        Value::Object(m)
    });
    if let Some(m) = &mon {
        let mut mm = serde_json::Map::new();
        for (k, v) in [
            ("traeUid", m.trae_uid.clone().map(Value::String).unwrap_or(Value::Null)),
            ("traeBalance", m.trae_balance.map(|b| Value::Number(b.into())).unwrap_or(Value::Null)),
            ("qoderUid", m.qoder_uid.clone().map(Value::String).unwrap_or(Value::Null)),
            ("qoderRemaining", m.qoder_remaining.map(|f| serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)).unwrap_or(Value::Null)),
            ("autoSwitch", Value::Bool(m.auto_switch)),
            ("threshold", Value::Number(m.threshold.into())),
        ] {
            mm.insert(k.into(), v);
        }
        o.insert("monitor".into(), Value::Object(mm));
    }
    Value::Object(o)
}

fn item_json(i: &account::Item) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("platform".into(), Value::String(i.platform.clone()));
    o.insert("uid".into(), Value::String(i.uid.clone()));
    o.insert("remaining".into(), i.remaining.map(|f| serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)).unwrap_or(Value::Null));
    o.insert("exceeded".into(), Value::Bool(i.exceeded));
    o.insert("error".into(), Value::String(i.error.clone().unwrap_or_default()));
    o.insert("at".into(), Value::Number(i.at.into()));
    o.insert("label".into(), Value::String(i.label()));
    Value::Object(o)
}

/// `credits_snapshot` 命令入口（不触发查询，只回当前内存/磁盘快照）。
pub fn snapshot_json_now() -> Value {
    snapshot_json(current_snapshot())
}

/// 不变量自检（§5 selfCheck）。
pub fn self_check() -> Result<(), String> {
    if DEFAULT_INTERVAL_SEC != 0 {
        return Err("自检失败：禁止一切定时轮询（DEFAULT_INTERVAL_SEC 必须 0）".into());
    }
    if FORWARD_POLL_MIN_MS < 60_000 {
        return Err("自检失败：转发后刷新节流必须 >= 60s".into());
    }
    if MIN_THRESHOLD != 5 || DEFAULT_THRESHOLD != 5 {
        return Err("自检失败：见底阈值下限固定 5".into());
    }
    // 节流：60s 内二次不查；urgent 跳节流；恰好 60s 放行
    if should_poll(1_000_000, 1_000_000 + FORWARD_POLL_MIN_MS - 1, false) {
        return Err("自检失败：节流窗口内不该再查".into());
    }
    if !should_poll(1_000_000, 1_000_000 + FORWARD_POLL_MIN_MS, false) || !should_poll(1_000_000, 1_000_000 + 1, true) {
        return Err("自检失败：urgent / 窗口结束必须放行".into());
    }
    if !should_poll(0, 1, false) {
        return Err("自检失败：从未查过（0）必须放行".into());
    }
    // 余量语义（保守）：余 1 > 阈值 0 不算用完 / 余 0 算 / 负数算；null 绝不触发换号
    let exhausted = |bal: Option<i64>, threshold: i64| bal.is_some_and(|b| b <= threshold);
    if exhausted(Some(1), 0) || !exhausted(Some(0), 0) || !exhausted(Some(-5), 0) {
        return Err("自检失败：余量 1/0/负数 的见底判定错位".into());
    }
    if exhausted(None, 5) {
        return Err("自检失败：查不到数绝不因阈值触发换号（保守原则）".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DataStore;
    use std::sync::Arc;

    fn tmp_store(tag: &str) -> Arc<DataStore> {
        let dir = std::env::temp_dir().join(format!("tw-monitor-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Arc::new(DataStore::new(dir))
    }

    #[test]
    fn throttle_is_per_platform_and_urgent_still_stamps() {
        // 纯判据（时间戳表由 note_forwarded_at 使用，同一函数被两平台各自传入自己的 last_at）
        let base = 5_000_000;
        assert!(should_poll(0, base, false));
        assert!(!should_poll(base, base + FORWARD_POLL_MIN_MS - 1, false));
        assert!(should_poll(base, base + FORWARD_POLL_MIN_MS, false));
        assert!(should_poll(base, base + 1, true), "转发失败必须立刻可查（不等节流窗口）");
        // 两平台互不压制：Trae 的时间戳不影响 Qoder 的判据
        assert!(should_poll(base + FORWARD_POLL_MIN_MS, base + 2 * FORWARD_POLL_MIN_MS, false));
        assert!(!should_poll(base, base + 1000, false));
    }

    #[test]
    fn snapshot_shape_matches_frontend_contract() {
        let v = snapshot_json(Some(MonitorSnapshot { trae_uid: Some("1".into()), trae_balance: Some(120), qoder_uid: None, qoder_remaining: None, auto_switch: true, threshold: 5 }));
        let o = v.as_object().unwrap();
        for k in ["loading", "at", "trae", "qoder", "total", "error"] {
            assert!(o.contains_key(k), "前端 CreditsSnapshot 缺字段 {k}");
        }
        assert_eq!(o["trae"], serde_json::json!(120.0));
        assert_eq!(o["qoder"], Value::Null);
        assert_eq!(o["total"], serde_json::json!(120.0));
        assert_eq!(o["error"], Value::Null);
        assert_eq!(o["monitor"]["threshold"], serde_json::json!(5));
        assert_eq!(o["monitor"]["traeUid"], serde_json::json!("1"));
        assert!(o["authFailed"].is_object() && o["items"].is_array());
    }

    #[test]
    fn monitor_snapshot_serde_uses_camel_case() {
        let s = MonitorSnapshot { trae_uid: Some("u".into()), trae_balance: Some(3), ..Default::default() };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["traeUid"], serde_json::json!("u"));
        assert_eq!(v["traeBalance"], serde_json::json!(3));
        assert!(v.get("qoderUid").is_none(), "None 字段不占位");
        let back: MonitorSnapshot = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn switch_pick_prefers_trusted_then_uid() {
        // 纯排序：authFailed 者靠后，其余按 uid 升序（候选已过滤区域）
        let mk = |uid: &str| crate::auth::model::TraeAccountMeta { uid: uid.into(), region_id: "cn".into(), ..Default::default() };
        let store = TraeAuthStore::with_store(tmp_store("cmp"));
        store.save(&TraeAccount { uid: "b".into(), region_id: "cn".into(), ..Default::default() });
        store.save(&TraeAccount { uid: "a".into(), region_id: "cn".into(), ..Default::default() });
        store.mark_auth_failed("a");
        let mut v = vec![mk("b"), mk("a")];
        v.sort_by(|x, y| switch_pick_cmp(x, y, &store));
        assert_eq!(v[0].uid, "b", "被标记 authFailed 的号靠后");
        let mut v2 = vec![mk("z"), mk("y")];
        v2.sort_by(|x, y| switch_pick_cmp(x, y, &store));
        assert_eq!(v2[0].uid, "y", "同级按 uid 升序稳定");
    }

    #[test]
    fn write_through_keeps_stale_value_with_error() {
        // 失败条目保留旧 remaining / at，只写 error（§6 规则 9）
        let _g = crate::credits::GLOBAL_TEST_LOCK.blocking_lock();
        account::put_item(account::Item::new(account::TRAE, "w1", Some(300.0), false, None, 42));
        write_through(account::TRAE, "w1", None, true);
        let it = account::snapshot().trae("w1").cloned().unwrap();
        assert_eq!(it.remaining, Some(300.0));
        assert_eq!(it.at, 42);
        assert_eq!(it.error.as_deref(), Some(account::ERR_QUERY_FAILED));
        assert_eq!(it.label(), "—（查询失败）", "读数一律屏蔽旧值");
        account::forget(account::TRAE, "w1");
    }

    #[test]
    fn note_forwarded_without_runtime_is_safe() {
        // 无 tokio 运行时环境：抢窗口、盖时间戳、释放 in-flight，不 panic
        let before = last_at(BACKEND_TRAE);
        note_forwarded_at(BACKEND_TRAE, Some("nope".into()), true, before + 12_345);
        assert_eq!(last_at(BACKEND_TRAE), before + 12_345, "urgent 跳节流但仍盖时间戳");
        assert_eq!(IN_FLIGHT.load(Ordering::SeqCst) & 1, 0, "起不了任务也要释放同平台标志");
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
