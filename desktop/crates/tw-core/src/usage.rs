//! UsageStore 对应体（Kotlin `UsageStore.kt`，analysis §4；落盘 `usage.json` = [UsageRecord]，
//! design.md §Data Models）。进程内缓存 + 显式锁 + 每次写入全量重写文件（Android `.apply()` 同构）。
//!
//! 语义要点（逐条对照源码）：
//! - **FIFO ≤ 100**：超出从**最旧**（下标 0）逐条裁剪（`MAX=100`、`removeAt(0)`）。
//! - 记录只含统计字段（时间/模型/token 数/耗时/状态/错误类型），**绝不存凭据**（源码红线，
//!   analysis §4.2）；backend/uid 属 CreditMonitor 的接线，不进本层。
//! - 读接口 = 全量 `all()` + 分页/计数 + 聚合（首现序，对应 LinkedHashMap，analysis §4.3）。
//!   桌面版记录页（design.md `records_get` → `{items,agg}`）另给 `clear`（Android 无删除接口，
//!   桌面 UI 有清空按钮，属 design 新增语义）。
//! - 解析整体容错：坏数据当空（runCatching 同构），单条字段缺省见 [`from_value`]。

#[cfg(test)]
use std::path::Path;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde_json::Value;

use crate::auth::model::now_ms;
use crate::storage::DataStore;
use crate::types::{ModelAgg, UsageRecord};

/// `usage_records` key 的桌面映射 = 独立文件（design.md §Data Models）。
pub const FILE: &str = "usage.json";

/// `MAX = 100`。
pub const MAX: usize = 100;

/// 单条 → 落盘 JSON 对象（键序与分析 §4.1 表一致；`durationMs/errorType` 映射到
/// `ms`/`err`——types.rs 的 UsageRecord 字段名即 design.md 第 87 行的字段清单）。
fn to_value(r: &UsageRecord) -> Value {
    serde_json::json!({
        "ts": r.ts,
        "model": r.model,
        "prompt": r.prompt,
        "completion": r.completion,
        "total": r.total,
        "ms": r.ms,
        "ok": r.ok,
        "err": r.err,
    })
}

/// 落盘 JSON 对象 → 单条；缺字段容错（`ts` 缺省=当前时刻，其余 0/""/false，analysis §4.1）。
fn from_value(v: &Value) -> UsageRecord {
    use crate::storage::{json_bool, json_i64, json_str};
    UsageRecord {
        ts: json_i64(v, "ts", now_ms()),
        model: json_str(v, "model", ""),
        prompt: json_i64(v, "prompt", 0),
        completion: json_i64(v, "completion", 0),
        total: json_i64(v, "total", 0),
        ms: json_i64(v, "ms", 0),
        ok: json_bool(v, "ok", false),
        err: json_str(v, "err", ""),
    }
}

/// 对应 Kotlin `object UsageStore`（进程单例 + `lock` + `cached/loaded` 懒加载）。
pub struct UsageStore {
    data: Arc<DataStore>,
    state: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    items: Vec<UsageRecord>,
    loaded: bool,
}

impl UsageStore {
    pub fn new() -> Self {
        Self::with_store(crate::storage::store())
    }

    pub fn with_store(data: Arc<DataStore>) -> Self {
        UsageStore {
            data,
            state: Mutex::new(Cache::default()),
        }
    }

    /// 生产单例（懒初始化，要求 [`crate::storage::init`] 已执行——与其余 global store 同规）。
    pub fn global() -> Arc<UsageStore> {
        static G: OnceLock<Arc<UsageStore>> = OnceLock::new();
        G.get_or_init(|| Arc::new(UsageStore::new())).clone()
    }

    /// `ensureLoaded`：首次从盘懒加载；整体坏数据当空（不覆盖内存，直接置空表）。
    fn ensure_loaded(&self, c: &mut Cache) {
        if c.loaded {
            return;
        }
        c.loaded = true;
        if let Some(arr) = self.data.load_json_opt(FILE).and_then(|v| v.as_array().cloned()) {
            c.items = arr.iter().filter(|o| o.is_object()).map(from_value).collect();
        }
    }

    /// 落盘全量（`persist` 同构；写失败静默——Android runCatching 语义，下条记录会再试）。
    fn persist(&self, items: &[UsageRecord]) {
        let arr: Vec<Value> = items.iter().map(to_value).collect();
        let _ = self.data.save_json(FILE, &Value::Array(arr));
    }

    /// `record(...)`：追加 + FIFO 裁剪最旧 + 全量重写落盘。
    pub fn put(&self, rec: &UsageRecord) {
        let mut c = self.state.lock();
        self.ensure_loaded(&mut c);
        c.items.push(rec.clone());
        while c.items.len() > MAX {
            c.items.remove(0);
        }
        // Kotlin 在 lock 内 persist（synchronized 全程），此处同构
        self.persist(&c.items);
    }

    /// `all()`：全量记录（时间序，最旧在前——追加序）。
    pub fn all(&self) -> Vec<UsageRecord> {
        let mut c = self.state.lock();
        self.ensure_loaded(&mut c);
        c.items.clone()
    }

    /// 记录条数。
    pub fn count(&self) -> usize {
        let mut c = self.state.lock();
        self.ensure_loaded(&mut c);
        c.items.len()
    }

    /// 分页读取（记录列表 UI；offset 越界返回空，limit 0 = 全部）。
    /// 列表尾部（最新）在前端倒序展示，本层只提供追加序切片。
    pub fn page(&self, offset: usize, limit: usize) -> Vec<UsageRecord> {
        let all = self.all();
        let end = if limit == 0 {
            all.len()
        } else {
            offset.saturating_add(limit).min(all.len())
        };
        if offset >= all.len() {
            return Vec::new();
        }
        all[offset..end].to_vec()
    }

    /// 桌面新增：清空（删文件 + 清缓存；对应记录页「清空」按钮）。
    pub fn clear(&self) {
        let mut c = self.state.lock();
        c.items.clear();
        c.loaded = true;
        let _ = std::fs::remove_file(self.data.path(FILE));
    }

    /// `aggregate`：LinkedHashMap 首现序（analysis §4.3），逐模型累计 calls/ok_calls/
    /// prompt/completion/total/ms（桌面 ModelAgg 形状 = types.rs 现成结构，比 Android 三元组
    /// 多出的字段是 design.md 记录页汇总卡要求，累计口径含失败记录——与 all() 全量一致）。
    pub fn aggregate(&self) -> Vec<ModelAgg> {
        let all = self.all();
        let mut out: Vec<ModelAgg> = Vec::new();
        for r in &all {
            if let Some(a) = out.iter_mut().find(|a| a.model == r.model) {
                a.calls += 1;
                if r.ok {
                    a.ok_calls += 1;
                }
                a.prompt += r.prompt;
                a.completion += r.completion;
                a.total += r.total;
                a.ms += r.ms;
            } else {
                out.push(ModelAgg {
                    model: r.model.clone(),
                    calls: 1,
                    ok_calls: if r.ok { 1 } else { 0 },
                    prompt: r.prompt,
                    completion: r.completion,
                    total: r.total,
                    ms: r.ms,
                });
            }
        }
        out
    }

    /// `records_get` 载荷：`{items:[…], agg:[…]}`（design.md §IPC 行 101）。
    pub fn snapshot(&self) -> Value {
        let all = self.all();
        let items: Vec<Value> = all.iter().map(to_value).collect();
        let agg: Vec<Value> = self
            .aggregate()
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or(Value::Null))
            .collect();
        serde_json::json!({ "items": items, "agg": agg, "count": all.len() })
    }

    /// 测试辅助：绕过内存缓存从盘重读（等价“换个进程重启”验证落盘）。
    #[cfg(test)]
    fn reload_from_disk(dir: &Path) -> Vec<UsageRecord> {
        let store = DataStore::new(dir);
        store
            .load_json_opt(FILE)
            .and_then(|v| v.as_array().cloned())
            .map(|arr| arr.iter().filter(|o| o.is_object()).map(from_value).collect())
            .unwrap_or_default()
    }
}

impl Default for UsageStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 模块级便捷口（网关 wave 的注入点：LocalFakeServer.streamWithFailover 落定处调用）
// ---------------------------------------------------------------------------

/// 对应 `Gateway.start` 注入的 `usageRecorder` → `UsageStore.record`。
pub fn record(rec: &UsageRecord) {
    UsageStore::global().put(rec);
}

pub fn all() -> Vec<UsageRecord> {
    UsageStore::global().all()
}

pub fn aggregate() -> Vec<ModelAgg> {
    UsageStore::global().aggregate()
}

pub fn snapshot() -> Value {
    UsageStore::global().snapshot()
}

// ---------------------------------------------------------------------------
// 测试（临时目录注入）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("tw-usage-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn rec(ts: i64, model: &str, total: i64, ok: bool) -> UsageRecord {
        UsageRecord {
            ts,
            model: model.to_string(),
            prompt: total / 2,
            completion: total - total / 2,
            total,
            ms: 100,
            ok,
            err: if ok { String::new() } else { "http:401".into() },
        }
    }

    #[test]
    fn fifo_trims_oldest_and_persists_field_names() {
        let d = tmpdir("fifo");
        let store = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        for i in 0..(MAX as i64 + 25) {
            store.put(&rec(1_000 + i, "m", 3, true));
        }
        let all = store.all();
        assert_eq!(all.len(), MAX);
        assert_eq!(all[0].ts, 1025, "裁掉最旧 25 条，队首应为 ts=1025");
        assert_eq!(all[MAX - 1].ts, 1124);
        // 落盘字段逐字检查（design.md 行 87：usage.json 数组元素的键）
        let raw = std::fs::read_to_string(store.data.path(FILE)).expect("usage.json");
        let v: Value = serde_json::from_str(&raw).unwrap();
        let first = &v[0];
        let keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
        for k in ["ts", "model", "prompt", "completion", "total", "ms", "ok", "err"] {
            assert!(keys.contains(&k), "缺字段 {k}");
        }
        assert_eq!(first["ts"], serde_json::json!(1025));
        // 重启视角：新实例从盘重载（不触碰旧内存缓存）
        let re = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        assert_eq!(
            serde_json::to_value(re.all()).unwrap(),
            serde_json::to_value(all).unwrap(),
            "落盘内容可完整重载"
        );
    }

    #[test]
    fn corrupt_file_reads_empty_and_next_put_recovers() {
        let d = tmpdir("corrupt");
        std::fs::write(d.join(FILE), "{ this is not json ]").unwrap();
        let store = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        assert!(store.all().is_empty(), "坏数据当空（runCatching 同构）");
        store.put(&rec(7, "m", 2, true));
        assert_eq!(store.count(), 1, "写一条即恢复整表");
    }

    #[test]
    fn from_value_defaults_match_android() {
        // ts 缺省 = 当前时刻；其余 0 / "" / false（analysis §4.1 表）
        let r = from_value(&serde_json::json!({"model": "x"}));
        assert_eq!(r.model, "x");
        assert!(r.ts > 0);
        assert_eq!((r.prompt, r.completion, r.total, r.ms), (0, 0, 0, 0));
        assert!(!r.ok);
        assert_eq!(r.err, "");
        // 数组外/类型歪的条目在 ensure_loaded 里被 is_object 过滤（对应 Kotlin optJSONObject continue）
    }

    #[test]
    fn page_and_count() {
        let d = tmpdir("page");
        let store = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        for i in 0..5 {
            store.put(&rec(100 + i, "m", 1, true));
        }
        assert_eq!(store.count(), 5);
        assert_eq!(store.page(1, 2).iter().map(|r| r.ts).collect::<Vec<_>>(), vec![101, 102]);
        assert_eq!(store.page(0, 0).len(), 5, "limit0=全部");
        assert!(store.page(10, 3).is_empty(), "offset 越界=空");
        assert_eq!(store.page(3, 99).len(), 2, "end 夹到表尾");
    }

    #[test]
    fn aggregate_keeps_first_seen_order() {
        let d = tmpdir("agg");
        let store = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        store.put(&rec(1, "b", 10, true));
        store.put(&rec(2, "a", 20, false));
        store.put(&rec(3, "b", 30, true));
        let agg = store.aggregate();
        assert_eq!(
            agg.iter().map(|a| a.model.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"],
            "首现序（非字典序，LinkedHashMap 语义）"
        );
        assert_eq!(agg[0].calls, 2);
        assert_eq!(agg[0].ok_calls, 2);
        assert_eq!(agg[0].total, 40);
        assert_eq!(agg[0].completion, 20);
        assert_eq!(agg[1].calls, 1);
        assert_eq!(agg[1].ok_calls, 0, "失败记录计数进 calls、不进 ok_calls");
        assert_eq!(agg[1].total, 20, "失败记录 token 仍累计（口径=all() 全量）");
        assert_eq!(agg[1].ms, 100);
    }

    #[test]
    fn snapshot_shape_and_clear() {
        let d = tmpdir("snap");
        let store = UsageStore::with_store(Arc::new(DataStore::new(&d)));
        store.put(&rec(9, "m", 4, true));
        let s = store.snapshot();
        assert_eq!(s["count"], serde_json::json!(1));
        assert_eq!(s["items"][0]["model"], serde_json::json!("m"));
        assert_eq!(s["agg"][0]["model"], serde_json::json!("m"));

        store.clear();
        assert!(store.all().is_empty());
        assert!(!store.data.exists(FILE), "clear 删文件（对应 remove_file 实现）");
        assert!(UsageStore::reload_from_disk(&d).is_empty(), "重启后仍为空");
        // 清空后再写入恢复正常
        store.put(&rec(11, "n", 1, true));
        assert_eq!(store.count(), 1);
    }
}
