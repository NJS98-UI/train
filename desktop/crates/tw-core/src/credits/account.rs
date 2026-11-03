//! 按账号积分查询与缓存（checkin-credits.md §6，AccountCredits.kt 对应体）。
//!
//! 与 [super::monitor] 的分工：monitor 是**转发链路守卫**（只盯当前账号、自动换号）；
//! 这里回答"账号页上每个号各剩多少"，需要遍历全部账号，因此触发方式必须**事件驱动**
//! （开页/手点刷新）——每 2s 遍历全号打上游是明确滥用（§5 历史教训：token 被判废）。
//!
//! 落盘动机（原样保留）：账号页每次 render 重建视图，只放内存切页回来会闪一片"查询中"；
//! 落盘后进页面**立刻显示上次的数**，后台刷新**原地更新**。
//! 桌面文件 = `credits.cache.json`，内部仍用 prefs 的键名 `cache_json` / `at`（§7.3 审计友好）。

use crate::auth::model::{now_ms, TraeAccount};
use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::checkin::trae_api;
use crate::storage::DataStore;
#[cfg(test)]
use crate::storage::json_i64;
use crate::upstream::trae::TraeRegion;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// 设计文档指定的数据文件（AccountCredits cache_json+at）。
pub const FILE: &str = "credits.cache.json";
/// `TTL_MS`：缓存新鲜期（手动 force 无视）。
pub const TTL_MS: i64 = 60_000;
/// 平台常量（条目主键前缀）。
pub const TRAE: &str = "trae";
pub const QODER: &str = "qoder";

/// 查积分重试参数（`retryFetch`）：最多 3 次，间隔 `400*i` ms（第 3 次后不再 sleep）。
pub const RETRY_MAX_ATTEMPTS: usize = 3;
pub const RETRY_BASE_DELAY_MS: u64 = 400;

/// §6 判定链里"需重新登录"的成因标记（Kotlin 读 `TraeUpstream.lastAuthFailed`）。
pub const ERR_NEED_LOGIN: &str = "需重新登录";
pub const ERR_QUERY_FAILED: &str = "查询失败";
pub const ERR_REGION: &str = "国际服不支持";
pub const ERR_LOCAL_MISS: &str = "本地凭证缺失";
/// §6 Trae 表末行的"异常类简名"在 Rust 的等价物（fetch 层不抛异常，以网络失败标注）。
pub const ERR_NETWORK: &str = "网络失败";

/// 条目主键 `"{platform}|{uid}"`。
pub fn item_key(platform: &str, uid: &str) -> String {
    format!("{platform}|{uid}")
}

/// `Item(platform, uid, remaining?, exceeded, error?, at)`；`at=0` 表示从未成功。
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub platform: String,
    pub uid: String,
    pub remaining: Option<f64>,
    pub exceeded: bool,
    pub error: Option<String>,
    pub at: i64,
}

impl Item {
    pub fn new(platform: &str, uid: &str, remaining: Option<f64>, exceeded: bool, error: Option<String>, at: i64) -> Self {
        Self { platform: platform.to_string(), uid: uid.to_string(), remaining, exceeded, error, at }
    }

    /// `known`：查到过数（哪怕 0）。
    pub fn known(&self) -> bool {
        self.remaining.is_some()
    }

    /// `format(remaining)`；**任何 error 一律 `—（原因）`，绝不显示旧数字**。
    /// （曾试过"旧数+括号原因"，屏幕上出现「600（token 失效）」自相矛盾读数被否掉。）
    pub fn label(&self) -> String {
        match &self.error {
            Some(e) if !e.trim().is_empty() => format!("—（{e}）"),
            _ => format_value(self.remaining),
        }
    }

    fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("platform".into(), Value::String(self.platform.clone()));
        o.insert("uid".into(), Value::String(self.uid.clone()));
        o.insert(
            "remaining".into(),
            match self.remaining {
                Some(v) => serde_json::Number::from_f64(v).map(Value::Number).unwrap_or(Value::Null),
                None => Value::Null,
            },
        );
        o.insert("exceeded".into(), Value::Bool(self.exceeded));
        o.insert("error".into(), self.error.clone().map(Value::String).unwrap_or_else(|| Value::String(String::new())));
        o.insert("at".into(), Value::Number(self.at.into()));
        Value::Object(o)
    }
}

/// 装载条目：缺 platform/uid 跳过；坏 JSON 整盘弃由调用方处理。
fn item_from_value(v: &Value) -> Option<Item> {
    let o = v.as_object()?;
    let platform = o.get("platform").and_then(Value::as_str).unwrap_or("");
    let uid = o.get("uid").and_then(Value::as_str).unwrap_or("");
    if platform.is_empty() || uid.is_empty() {
        return None;
    }
    let error = match o.get("error") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    Some(Item {
        platform: platform.to_string(),
        uid: uid.to_string(),
        remaining: o.get("remaining").and_then(Value::as_f64),
        exceeded: o.get("exceeded").and_then(Value::as_bool).unwrap_or(false),
        error,
        at: o.get("at").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// `format(v)`：null → `—`；整数值不带小数点；否则一位小数（`12.5`）。
pub fn format_value(v: Option<f64>) -> String {
    match v {
        None => "—".to_string(),
        Some(f) if !f.is_finite() => "—".to_string(),
        Some(f) if f == f.trunc() => format!("{}", f as i64),
        Some(f) => format!("{:.1}", f),
    }
}

/// `Snapshot`：条目按 `"{platform}|{uid}"` 排序保存（BTreeMap 保证序列化稳定）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub items: BTreeMap<String, Item>,
    pub at: i64,
    pub loading: bool,
}

impl Snapshot {
    pub fn get(&self, platform: &str, uid: &str) -> Option<&Item> {
        self.items.get(&item_key(platform, uid))
    }

    pub fn trae(&self, uid: &str) -> Option<&Item> {
        self.get(TRAE, uid)
    }

    pub fn qoder(&self, uid: &str) -> Option<&Item> {
        self.get(QODER, uid)
    }

    pub fn items_of(&self, platform: &str) -> Vec<&Item> {
        self.items.values().filter(|i| i.platform == platform).collect()
    }

    /// `known(p)`：该平台查到数的账号数。
    pub fn known(&self, platform: &str) -> usize {
        self.items_of(platform).iter().filter(|i| i.known()).count()
    }

    /// `total(p)`：该平台条目总数。
    pub fn total(&self, platform: &str) -> usize {
        self.items_of(platform).len()
    }

    pub fn failed(&self, platform: &str) -> usize {
        self.items_of(platform).iter().filter(|i| !i.known()).count()
    }

    /// `complete(p) = total>0 && known == total`。
    pub fn complete(&self, platform: &str) -> bool {
        let t = self.total(platform);
        t > 0 && self.known(platform) == t
    }

    /// `sum(p)`：**只累加查到数的账号**；一个都没查到 → None（显示 `—`，显示 0 会被误读成"真没额度了"）。
    pub fn sum(&self, platform: &str) -> Option<f64> {
        let known: Vec<f64> = self.items_of(platform).iter().filter_map(|i| i.remaining).collect();
        if known.is_empty() {
            None
        } else {
            Some(known.iter().sum())
        }
    }

    /// `total_label(p)`：只报数字或 `—`（"（2/3 个账号）"式标注已从文案移除，selfCheck 钉死；
    /// 需要下界说明的 UI 可自行用 `complete()` + `known()/total()` 拼接）。
    pub fn total_label(&self, platform: &str) -> String {
        format_value(self.sum(platform))
    }

    /// 两平台总合计：任一平台查到数即计入；两边都没查到 → None。
    pub fn grand_sum(&self) -> Option<f64> {
        match (self.sum(TRAE), self.sum(QODER)) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        }
    }

    fn to_array(&self) -> Vec<Value> {
        self.items.values().map(|i| i.to_value()).collect()
    }
}

// ---------------------------------------------------------------------------
// 全局单例（内存 + 磁盘双态）
// ---------------------------------------------------------------------------

static STORE: OnceLock<Arc<DataStore>> = OnceLock::new();
static SNAPSHOT: OnceLock<Mutex<Snapshot>> = OnceLock::new();
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn store() -> Arc<DataStore> {
    STORE.get_or_init(|| crate::storage::store()).clone()
}

fn cell() -> &'static Mutex<Snapshot> {
    SNAPSHOT.get_or_init(|| Mutex::new(Snapshot::default()))
}

fn set_snapshot(s: Snapshot) {
    *cell().lock().expect("credits 快照锁不跨 await") = s;
}

/// `currentSnapshot()`：内存快照（进页面立即可见，不等查询）；首次调用会先从磁盘装载。
pub fn snapshot() -> Snapshot {
    let mut guard = cell().lock().expect("credits 快照锁不跨 await");
    if guard.at == 0 && guard.items.is_empty() {
        *guard = load_cache_of(&store());
    }
    guard.clone()
}

fn load_cache_of(data: &DataStore) -> Snapshot {
    // 坏 JSON 整盘弃（runCatching 同构）
    let Some(root) = data.load_json_opt(FILE) else {
        return Snapshot::default();
    };
    let arr = root.get("cache_json").and_then(Value::as_array);
    let items = match arr {
        Some(a) => a.iter().filter_map(item_from_value).map(|i| (item_key(&i.platform, &i.uid), i)).collect(),
        None => BTreeMap::new(),
    };
    Snapshot { items, at: root.get("at").and_then(Value::as_i64).unwrap_or(0), loading: false }
}

/// `loadCache`（独立可调，便于测试与迁移）。
pub fn load_cache() -> Snapshot {
    load_cache_of(&store())
}

/// `saveCache`：整盘落 `{"cache_json":[...],"at":...}`。
pub fn save_cache(snap: &Snapshot) {
    let mut o = Map::new();
    o.insert("cache_json".into(), Value::Array(snap.to_array()));
    o.insert("at".into(), Value::Number(snap.at.into()));
    let _ = store().save_json(FILE, &Value::Object(o));
}

/// `fresh()`：内存 snapshot.at 距今 <TTL。
pub fn is_fresh(snap: &Snapshot, now_ms: i64) -> bool {
    // Kotlin 原文只判 `< TTL`；这里加 `now >= at`：时钟被往回调时差值为负会**永久判新鲜**，
    // 整页读数再也不刷新（桌面必现的死锁），所以按保守侧处理：回拨一律过期、重查。
    snap.at > 0 && now_ms >= snap.at && now_ms - snap.at < TTL_MS
}

/// `label(item)`：条目不存在 → `—`。
pub fn label_of(item: Option<&Item>) -> String {
    item.map(Item::label).unwrap_or_else(|| "—".to_string())
}

/// `forget(platform, uid)`：删账号时抹条目（**纯本地零请求**——旧版在此调全量刷新等于删一号问全号）。
pub fn forget(platform: &str, uid: &str) {
    let mut s = snapshot();
    if s.items.remove(&item_key(platform, uid)).is_some() {
        save_cache(&s.clone());
        set_snapshot(s);
    }
}

/// `clear()`：全清（内存 + 磁盘写空盘）。
pub fn clear() {
    let empty = Snapshot::default();
    save_cache(&empty);
    set_snapshot(empty);
}

/// 手工写入/更新某条目（monitor 的"换了另外一个号就刷另外一个号"回写用）。
pub fn put_item(item: Item) {
    let mut s = snapshot();
    let at_now = item.at;
    s.items.insert(item_key(&item.platform, &item.uid), item);
    s.at = at_now.max(s.at);
    save_cache(&s.clone());
    set_snapshot(s);
}

// ---------------------------------------------------------------------------
// 刷新编排（事件驱动，禁轮询）
// ---------------------------------------------------------------------------

fn retry_delays_ms() -> [u64; RETRY_MAX_ATTEMPTS] {
    [0, RETRY_BASE_DELAY_MS, RETRY_BASE_DELAY_MS * 2]
}

/// `retryFetch`：单账号最多 3 次，间隔 400·i ms（针对"同一平台只有一个号查不到"的限流/瞬时 5xx）。
async fn retry_fetch<F, Fut>(mut f: F) -> Option<f64>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Option<i64>>,
{
    for (i, delay) in retry_delays_ms().iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(*delay)).await;
        }
        if let Some(v) = f(i).await {
            return Some(v as f64);
        }
    }
    None
}

/// Trae 单账号判定链（§6 表）。失败**保留上一次 remaining 与 at**，只写 error。
pub async fn fetch_trae_item(store: &TraeAuthStore, meta_uid: &str, old: Option<&Item>, now_ms: i64) -> Item {
    let keep = |err: &str| Item::new(TRAE, meta_uid, old.and_then(|o| o.remaining), false, Some(err.to_string()), old.map(|o| o.at).unwrap_or(0));
    let cred: TraeAccount = match store.load_by_uid(meta_uid) {
        Some(c) => c,
        None => return keep(ERR_LOCAL_MISS),
    };
    let region = TraeRegion::of(&cred.region_id);
    if !region.is_supported() || !trae_api::can_query_quota(&region) {
        return keep(ERR_REGION); // 不硬打上游，明确标注而非显示 0
    }
    let Some(token) = crate::auth::api::trae::restart_with_valid_token_for(store, &cred, crate::checkin::batch::TOKEN_SKEW_SEC).await else {
        return keep(ERR_NEED_LOGIN);
    };
    let uid_c = cred.uid.clone();
    let first = retry_fetch(|_| trae_api::fetch_usage(&region, &token, &uid_c)).await;
    if let Some(bal) = first {
        return Item::new(TRAE, meta_uid, Some(bal), bal <= 0.0, None, now_ms);
    }
    // 查不到 → 强制续一次再试（token 未到期已被判废的常见成因）
    let t2 = crate::auth::api::trae::force_refresh_token_for(store, &cred).await;
    if let Some(t2) = &t2 {
        if !t2.trim().is_empty() && t2 != &token {
            let uid_c2 = cred.uid.clone();
            let t2c = t2.clone();
            if let Some(bal) = retry_fetch(|_| trae_api::fetch_usage(&region, &t2c, &uid_c2)).await {
                return Item::new(TRAE, meta_uid, Some(bal), bal <= 0.0, None, now_ms);
            }
        }
    }
    let err = if trae_api::last_auth_failed() || store.auth_failed_for(Some(meta_uid)) { ERR_NEED_LOGIN } else { ERR_QUERY_FAILED };
    keep(err)
}

/// Qoder 单账号判定链（§6）。**绝不为了"看一眼"触发 setActive**（那会把查看变成偷偷换号）。
pub async fn fetch_qoder_item(store: &QoderAuthStore, uid: &str, old: Option<&Item>, now_ms: i64) -> Item {
    let keep = |err: &str| Item::new(QODER, uid, old.and_then(|o| o.remaining), false, Some(err.to_string()), old.map(|o| o.at).unwrap_or(0));
    let Some(cred) = store.load_by_uid(uid) else {
        return keep(ERR_LOCAL_MISS);
    };
    let jt = match crate::auth::api::qoder::valid_job_token_for(store, &cred, crate::auth::api::qoder::JOB_SKEW_MS).await {
        Some(t) if !t.trim().is_empty() => t,
        _ if !cred.job_token.trim().is_empty() => cred.job_token.clone(),
        _ => return keep(ERR_NEED_LOGIN),
    };
    let mode = cred.mode.clone();
    let got = crate::checkin::qoder_api::fetch_credits(&jt, &mode).await;
    match got {
        Some(c) => Item::new(QODER, uid, Some(c.remaining), c.exceeded, None, now_ms),
        None => keep(ERR_QUERY_FAILED),
    }
}

/// Trae 全量刷新（先 Trae 后 Qoder，各 list 异常→空表；这里 store API 不抛，直接空表）。
pub async fn fetch_all(state_now: i64) -> Snapshot {
    let trae = TraeAuthStore::global();
    let qoder = QoderAuthStore::global();
    let mut items = BTreeMap::new();
    for meta in trae.list() {
        let old = snapshot().trae(&meta.uid).cloned();
        let it = fetch_trae_item(&trae, &meta.uid, old.as_ref(), state_now).await;
        items.insert(item_key(TRAE, &meta.uid), it);
        // 逐号之间让出运行时，等价 Kotlin 的 100ms interleave（避免连打）
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    for meta in qoder.list() {
        let old = snapshot().qoder(&meta.uid).cloned();
        let it = fetch_qoder_item(&qoder, &meta.uid, old.as_ref(), state_now).await;
        items.insert(item_key(QODER, &meta.uid), it);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Snapshot { items, at: state_now, loading: false }
}

/// 单账号刷新：先查 Trae store 命中则刷 `trae|uid`，否则查 Qoder store 刷 `qoder|uid`；
/// **其它账号条目原样保留（不查≠清零）**。
pub async fn fetch_one(uid: &str, state_now: i64) -> Snapshot {
    let trae = TraeAuthStore::global();
    let mut s = snapshot();
    if trae.list().iter().any(|m| m.uid == uid) {
        let old = s.trae(uid).cloned();
        s.items.insert(item_key(TRAE, uid), fetch_trae_item(&trae, uid, old.as_ref(), state_now).await);
    } else {
        let qoder = QoderAuthStore::global();
        if qoder.list().iter().any(|m| m.uid == uid) {
            let old = s.qoder(uid).cloned();
            s.items.insert(item_key(QODER, uid), fetch_qoder_item(&qoder, uid, old.as_ref(), state_now).await);
        }
    }
    s.at = state_now;
    s.loading = false;
    s
}

/// `refreshAll(force, onlyUid)`：
/// `!force && fresh()` → 不碰上游直接回当前快照；`in_flight` CAS 防连点多轮；
/// 整批异常兜底 `loading=false`（Rust 侧 fetch 不抛，等价收尾仍复位）。
pub async fn refresh_all(force: bool, only_uid: Option<&str>) -> Snapshot {
    let now = now_ms();
    let cur = snapshot();
    if !force && is_fresh(&cur, now) {
        return cur;
    }
    if IN_FLIGHT.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return cur;
    }
    set_snapshot(Snapshot { loading: true, ..cur });
    let out = match only_uid {
        Some(uid) => fetch_one(uid, now).await,
        None => fetch_all(now).await,
    };
    save_cache(&out.clone());
    set_snapshot(out.clone());
    IN_FLIGHT.store(false, Ordering::SeqCst);
    out
}

pub fn is_in_flight() -> bool {
    IN_FLIGHT.load(Ordering::Relaxed)
}

/// §6/§7/§8 不变量自检（不碰网络）。
pub fn self_check() -> Result<(), String> {
    if TTL_MS != 60_000 {
        return Err("自检失败：积分缓存 TTL 必须 60s".into());
    }
    if RETRY_MAX_ATTEMPTS != 3 || retry_delays_ms() != [0, 400, 800] {
        return Err("自检失败：重试次数/间隔表错位".into());
    }
    // format：null→—，整数不带小数点，否则一位小数
    if format_value(None) != "—" || format_value(Some(1234.0)) != "1234" || format_value(Some(12.53)) != "12.5" || format_value(Some(0.0)) != "0" {
        return Err("自检失败：format 读数规则不对".into());
    }
    // label：任何 error → —（原因），绝不显示旧数字
    let with_err = Item::new(TRAE, "u", Some(600.0), false, Some("token 失效".into()), 1);
    if with_err.label() != "—（token 失效）" {
        return Err("自检失败：有 error 必须屏蔽旧数字".into());
    }
    if label_of(None) != "—" {
        return Err("自检失败：无条目显示 —".into());
    }
    // 合计只计查到数的账号；一个都没查到是 —（不是 0）
    let mut s = Snapshot::default();
    s.items.insert(item_key(TRAE, "a"), Item::new(TRAE, "a", Some(150.0), false, None, 1));
    s.items.insert(item_key(TRAE, "b"), Item::new(TRAE, "b", None, false, Some(ERR_QUERY_FAILED.into()), 0));
    if s.total_label(TRAE) != "150" {
        return Err("自检失败：有 1 个账号失败时合计仍显示 150（无残留标注）".into());
    }
    if s.total_label(QODER) != "—" || s.grand_sum() != Some(150.0) {
        return Err("自检失败：空平台合计应为 —".into());
    }
    let mut empty = Snapshot::default();
    empty.items.insert(item_key(TRAE, "x"), Item::new(TRAE, "x", None, false, Some(ERR_REGION.into()), 0));
    if empty.total_label(TRAE) != "—" || empty.complete(TRAE) || empty.failed(TRAE) != 1 || empty.total(TRAE) != 1 {
        return Err("自检失败：known/total/complete/failed 计数不对".into());
    }
    if empty.items[&item_key(TRAE, "x")].label() != "—（国际服不支持）" {
        return Err("自检失败：国际服标注缺失".into());
    }
    // 条目主键带平台，Trae/Qoder 撞 uid 不互顶
    if item_key(TRAE, "1") == item_key(QODER, "1") {
        return Err("自检失败：条目主键必须带平台".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Arc<DataStore> {
        let dir = std::env::temp_dir().join(format!("tw-credits-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Arc::new(DataStore::new(dir))
    }

    #[test]
    fn format_rules() {
        assert_eq!(format_value(None), "—");
        assert_eq!(format_value(Some(1234.0)), "1234", "整数不带小数点");
        assert_eq!(format_value(Some(12.53)), "12.5", "非整数一位小数");
        assert_eq!(format_value(Some(0.0)), "0");
        assert_eq!(format_value(Some(-3.0)), "-3", "负数照旧");
        assert_eq!(format_value(Some(f64::NAN)), "—", "非有限值不可显示成数字");
    }

    #[test]
    fn label_never_shows_stale_number() {
        let ok = Item::new(TRAE, "u", Some(150.0), false, None, 9);
        assert_eq!(ok.label(), "150");
        assert_eq!(ok.known(), true);
        let err = Item::new(TRAE, "u", Some(600.0), false, Some("token 失效".into()), 1);
        assert_eq!(err.label(), "—（token 失效）", "旧数字绝不显示");
        assert_eq!(err.known(), true, "error 不影响 known");
        let none_err = Item::new(TRAE, "u", None, false, Some("".into()), 0);
        assert_eq!(none_err.label(), "—", "空 error 视同无 error");
        let exceeded = Item::new(TRAE, "u", Some(0.0), true, None, 5);
        assert_eq!(exceeded.label(), "0", "查到 0 就显示 0（与\"没查到\"的 — 区分）");
    }

    #[test]
    fn totals_count_only_known() {
        let mut s = Snapshot::default();
        s.items.insert(item_key(TRAE, "a"), Item::new(TRAE, "a", Some(100.0), false, None, 1));
        s.items.insert(item_key(TRAE, "b"), Item::new(TRAE, "b", Some(50.5), false, None, 1));
        s.items.insert(item_key(TRAE, "c"), Item::new(TRAE, "c", None, false, Some(ERR_QUERY_FAILED.into()), 0));
        s.items.insert(item_key(QODER, "d"), Item::new(QODER, "d", Some(200.0), false, None, 1));
        assert_eq!(s.known(TRAE), 2);
        assert_eq!(s.total(TRAE), 3);
        assert_eq!(s.failed(TRAE), 1);
        assert!(!s.complete(TRAE));
        assert!(s.complete(QODER));
        // 有失败时合计是**下界**，仍只报数字
        assert_eq!(s.total_label(TRAE), "150.5");
        assert_eq!(s.total_label(QODER), "200");
        assert_eq!(s.sum(TRAE), Some(150.5));
        assert_eq!(s.grand_sum(), Some(350.5));
        let mut s2 = Snapshot::default();
        s2.items.insert(item_key(TRAE, "z"), Item::new(TRAE, "z", None, false, Some(ERR_NEED_LOGIN.into()), 0));
        assert_eq!(s2.sum(TRAE), None, "一个都没查到 → —（显示 0 会被误读成真没额度）");
        assert_eq!(s2.total_label(TRAE), "—");
        assert_eq!(s2.grand_sum(), None);
    }

    #[test]
    fn cache_disk_roundtrip_and_forgiving_parse() {
        let data = tmp_store("cache");
        let mut s = Snapshot::default();
        s.items.insert(item_key(TRAE, "1"), Item::new(TRAE, "1", Some(150.0), false, None, 7));
        s.items.insert(item_key(QODER, "2"), Item::new(QODER, "2", None, false, Some("查询失败".into()), 0));
        s.at = 12345;
        let mut o = Map::new();
        o.insert("cache_json".into(), Value::Array(s.to_array()));
        o.insert("at".into(), Value::Number(s.at.into()));
        data.save_json(FILE, &Value::Object(o.clone())).unwrap();

        let back = load_cache_of(&data);
        assert_eq!(back.items, s.items, "字段名逐字往返（remaining=null 也要保 null）");
        assert_eq!(back.at, 12345);
        assert_eq!(json_i64(&Value::Object(o.clone()), "at", 0), 12345);

        // 坏 JSON / 缺 cache_json / 缺 platform 或 uid 条目 → 整盘弃或逐条跳过
        data.save_json(FILE, &Value::String("not json".into())).ok();
        assert_eq!(load_cache_of(&data).items.len(), 0);
        data.save_json(FILE, &serde_json::json!({"at": 9, "cache_json": [{"uid":"x"},{"platform":"trae"},{"platform":"trae","uid":"y","remaining":3.0,"at":1}]}))
            .unwrap();
        let partial = load_cache_of(&data);
        assert_eq!(partial.items.len(), 1, "缺主键的条目跳过");
        assert_eq!(partial.at, 9, "批次时间另存");
        assert_eq!(partial.items[&item_key(TRAE, "y")].remaining, Some(3.0));
    }

    #[test]
    fn freshness_window() {
        assert!(is_fresh(&Snapshot { at: 1_000_000, ..Default::default() }, 1_000_000 + TTL_MS - 1));
        assert!(!is_fresh(&Snapshot { at: 1_000_000, ..Default::default() }, 1_000_000 + TTL_MS), "整 60s 判过期");
        assert!(!is_fresh(&Snapshot::default(), 10_000), "at=0（从未刷新）判过期");
        assert!(!is_fresh(&Snapshot { at: 15_000, ..Default::default() }, 10_000), "时钟回拨（now<at）也判过期（保守重查）");
    }

    #[test]
    fn global_snapshot_and_forget_are_local_only() {
        let _g = crate::credits::GLOBAL_TEST_LOCK.blocking_lock();
        let mut s = Snapshot::default();
        s.items.insert(item_key(TRAE, "keep"), Item::new(TRAE, "keep", Some(1.0), false, None, 1));
        s.items.insert(item_key(TRAE, "drop"), Item::new(TRAE, "drop", Some(2.0), false, None, 1));
        s.at = 1;
        set_snapshot(s.clone());
        save_cache(&s);
        forget(TRAE, "drop");
        let now = snapshot();
        assert!(now.items.contains_key(&item_key(TRAE, "keep")), "只删目标条目");
        assert!(!now.items.contains_key(&item_key(TRAE, "drop")));
        clear();
        assert!(snapshot().items.is_empty());
    }

    #[test]
    fn item_key_prefixed_per_platform() {
        assert_eq!(item_key(TRAE, "7"), "trae|7");
        assert_eq!(item_key(QODER, "7"), "qoder|7");
        assert_ne!(item_key(TRAE, "7"), item_key(QODER, "7"));
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
