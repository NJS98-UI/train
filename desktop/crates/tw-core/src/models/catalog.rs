//! QoderModelCatalog（Kotlin `object QoderModelCatalog` 561 行逐函数对照，analysis §2）。
//! Qoder 侧“模型是协议的一部分”：请求要带 `model_config.key`，上游按 key 路由，
//! 因此必须有权威内置快照（BUILTIN 21 条）+ 宽容名字解析 + 三级降级目录（失败永不清空）。

use crate::auth::model::{now_ms, now_sec, qoder_normalize_mode, QoderAccount};
use crate::crypto::{cosy, machine};
use crate::models::repo::trunc200;
use crate::storage::DataStore;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

/// 兜底/默认模型 key（客户端不认识的名字一律落这里）。
pub const DEFAULT_KEY: &str = "auto";
/// 1M 上下文升级目标（唯一保证 1M 的内置项）。
pub const ONE_M_KEY: &str = "ultimate";
/// 客户端显式要 1M 上下文的约定后缀。
pub const ONE_M_SUFFIX: &str = "[1m]";
/// toModel 上下文缺省（analysis §2.4）。
pub const DEFAULT_CONTEXT: i64 = 180_000;
/// `[1m]` 升级阈值：目标模型上下文 < 900_000 才升级。
pub const ONE_M_THRESHOLD: i64 = 900_000;
/// 目录缓存 TTL：动态目录存在且拉取时刻在 3600s 内则跳过网络（任务要求的桌面新增，Android 无 TTL）。
pub const CATALOG_TTL_SECS: i64 = 3600;
/// design.md §Data Models：`catalog.qoder.json` = `{dynamic:[QModel…], fetchedAt}`（存的是**合并后**目录）。
pub const FILE: &str = "catalog.qoder.json";

const SCENES: [&str; 3] = ["chat", "byok_enterprise", "byok_teams"];

// ① COSY 签名目录（QoderUpstream.fetchModelListRaw）与 ② openapi 回落（QoderAuthApi.fetchModelCatalog）的 host/端点（analysis §2.5/§7）。
pub const INFER_BASE_CN: &str = "https://gateway.qoder.com.cn";
pub const INFER_BASE_GLOBAL: &str = "https://api2.qoder.sh";
pub const OPENAPI_BASE_CN: &str = "https://openapi.qoder.com.cn";
pub const OPENAPI_BASE_GLOBAL: &str = "https://openapi.qoder.sh";
pub const EP_MODELS: &str = "/api/v1/models";
pub const EP_MODEL_LIST: &str = "/api/v1/model/list";
const OPENAPI_COSY_VERSION: &str = "1.0.1";

pub fn infer_base(mode: &str) -> &'static str {
    if qoder_normalize_mode(mode) == crate::auth::model::QODER_MODE_GLOBAL { INFER_BASE_GLOBAL } else { INFER_BASE_CN }
}

pub fn openapi_base(mode: &str) -> &'static str {
    if qoder_normalize_mode(mode) == crate::auth::model::QODER_MODE_GLOBAL { OPENAPI_BASE_GLOBAL } else { OPENAPI_BASE_CN }
}

/// `Cosy-MachineOS` 桌面取值：analysis §2.5 指定按目标平台 `goarch_goos` 风格（amd64→x86_64、arm64→aarch64、macos→macos）。
pub fn cosy_machine_os() -> &'static str {
    use std::env::consts::{ARCH, OS};
    match (ARCH, OS) {
        ("x86_64", "linux") => "x86_64_linux",
        ("aarch64", "linux") => "aarch64_linux",
        ("x86_64", "macos") => "x86_64_macos",
        ("aarch64", "macos") => "aarch64_macos",
        ("x86_64", "windows") => "x86_64_windows",
        ("aarch64", "windows") => "aarch64_windows",
        _ => "x86_64_linux",
    }
}

/// 单条模型定义（对应 `QModel`；持久化字段名见 catalog.qoder.json）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QModel {
    /// 请求 `model_config` 用的内部 key。
    pub key: String,
    /// 展示名（也是客户端可用别名）。
    pub name: String,
    pub context_length: i64,
    /// 是否推理模型（支持 reasoning_effort）。
    pub reasoning: bool,
    /// 官方倍率；0.0=免费；**null=倍率未知**（诚实显示 "?"，禁止瞎填 1.0——analysis §2.1/§2.4）。
    pub price_factor: Option<f64>,
    pub aliases: Vec<String>,
    /// key 未经官方目录验证（内置清单已全部确认，默认 false）。
    pub unverified: bool,
}

fn qm(key: &str, name: &str, ctx: i64, reasoning: bool, price: Option<f64>, aliases: &[&str]) -> QModel {
    QModel {
        key: key.into(),
        name: name.into(),
        context_length: ctx,
        reasoning,
        price_factor: price,
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        unverified: false,
    }
}

impl QModel {
    /// 对应 `free`：priceFactor <= 0 才算免费；**null（未知）不算免费**。
    pub fn free(&self) -> bool {
        self.price_factor.map(|p| p <= 0.0).unwrap_or(false)
    }

    /// 对应 `priceLabel`：整数去小数尾巴（0.0→"0"、1.0→"1"、2.5→"2.5"、null→"?"）。
    pub fn price_label(&self) -> String {
        match self.price_factor {
            None => "?".into(),
            Some(v) => {
                if v == (v as i64) as f64 {
                    format!("{}", v as i64)
                } else {
                    v.to_string()
                }
            }
        }
    }
}

/// 内置 21 条兜底快照——**逐条转写自 Kotlin `QoderModelCatalog.kt:74-96`**（与 analysis §2.2 JSON 一致，
/// 顺序即 UI 顺序）。analysis 红字矛盾裁决：Android `selfCheck` 的 “size==20、首条=auto” 是清单增改后
/// 没跟上的陈旧断言，本移植按源码 21 条现状为基线（首条 `qwen3-coder-plus`，`auto` 在第 2 位），单测锁 21。
pub fn builtin() -> Vec<QModel> {
    vec![
        qm("qwen3-coder-plus", "Qwen3 Coder Plus", 180_000, false, None, &[]),
        qm("auto", "Auto", 180_000, false, Some(0.5), &[]),
        qm("ultimate", "极致", 1_000_000, true, Some(0.8), &["Ultimate"]),
        qm("performance", "快速", 1_000_000, false, None, &["Performance"]),
        qm("efficient", "经济", 180_000, false, None, &["Efficient"]),
        qm("smodel", "Sonus", 180_000, true, Some(8.0), &["smodel"]),
        qm("cmodel", "Cantus", 200_000, true, Some(4.0), &[]),
        qm("qmodel_38max", "Qwen3.8-Max 2", 1_000_000, true, Some(0.5), &["Qwen3.8-Max"]),
        qm("qfmodel", "Qwen3.8-Flash", 1_000_000, true, Some(0.0), &["Qwen3.8-Flash-1M"]),
        qm("kmodel_latest", "Kimi-K3.1-Pro", 1_000_000, false, Some(1.0), &["Kimi-K3"]),
        qm("kmodel", "Kimi-K2.8-Preview", 256_000, true, Some(0.8), &["Kimi-K2.7-Code"]),
        qm("gmodel", "GLM-5.3", 1_000_000, true, Some(0.8), &[]),
        qm("gfmodel", "GLM-5.3-Flash", 1_000_000, true, Some(0.1), &[]),
        qm("dmodel", "DeepSeek-V4-Pro", 1_000_000, true, Some(0.5), &[]),
        qm("dfmodel", "DeepSeek-Flash", 1_000_000, true, Some(0.0), &["DeepSeek-V4-Flash"]),
        qm("mmodel", "MiniMax-M3.1-Flash", 1_000_000, false, Some(0.1), &["MiniMax-M3"]),
        // ---- 官方 UI 已下架但 key 仍有效的老模型（兼容旧客户端；lite 未写倍率=null 未知）----
        qm("lite", "Lite", 180_000, false, None, &[]),
        qm("qmodel_latest", "Qwen3.7-Max", 1_000_000, true, Some(0.1), &[]),
        qm("qmodel", "Qwen3.7-Plus", 1_000_000, true, Some(0.04), &[]),
        qm("q37fmodel", "Qwen3.7-Flash", 1_000_000, true, Some(0.1), &[]),
        qm("gm51model", "GLM-5.2", 1_000_000, true, Some(0.6), &[]),
    ]
}

/// 客户端 model 名解析结果（对应 `Resolved`）。
#[derive(Clone, Debug)]
pub struct Resolved {
    pub model: QModel,
    /// 客户端原始写法带 `[1m]` 后缀的标记。
    pub one_m_suffix: bool,
}

impl Resolved {
    /// 发给上游的模型 key。
    pub fn key(&self) -> &str {
        &self.model.key
    }

    /// 对外展示 id（带 `[1m]` 时保后缀，与客户端原始请求一致）。
    pub fn public_id(&self) -> String {
        if self.one_m_suffix {
            format!("{}{}", self.model.key, ONE_M_SUFFIX)
        } else {
            self.model.key.clone()
        }
    }
}

struct CatalogState {
    dynamic: Option<Vec<QModel>>,
    fetched_at: i64,
}

/// 对应 Kotlin `object QoderModelCatalog`（进程单例 + `with_store` 注入版）。
pub struct QoderModelCatalog {
    data: Arc<DataStore>,
    state: Mutex<CatalogState>,
    /// 每模型一个稳定 `q2a-<uuid>` 会话 id（进程内缓存，不持久化——重启换新只丢上游历史）。
    sessions: Mutex<HashMap<String, String>>,
    last_raw: Mutex<Option<String>>,
    last_error: Mutex<Option<String>>,
}

/// 三级降级链的产出（source："signed" | "openapi" | "builtin"）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSyncOutcome {
    pub source: String,
    pub total: usize,
    pub free: usize,
    pub dynamic: bool,
    pub error: Option<String>,
}

impl QoderModelCatalog {
    pub fn global() -> Arc<QoderModelCatalog> {
        static CAT: OnceLock<Arc<QoderModelCatalog>> = OnceLock::new();
        CAT.get_or_init(|| Arc::new(QoderModelCatalog::new())).clone()
    }

    pub fn new() -> Self {
        Self::with_store(crate::storage::store())
    }

    pub fn with_store(data: Arc<DataStore>) -> Self {
        Self { data, state: Mutex::new(CatalogState { dynamic: None, fetched_at: 0 }), sessions: Mutex::new(HashMap::new()), last_raw: Mutex::new(None), last_error: Mutex::new(None) }
    }

    /// 当前生效清单（动态优先，否则内置 21 条）。永不为空。
    pub fn current(&self) -> Vec<QModel> {
        self.state.lock().dynamic.clone().unwrap_or_else(builtin)
    }

    /// 兼容旧引用名 `ALL`。
    pub fn all(&self) -> Vec<QModel> {
        self.current()
    }

    pub fn has_dynamic(&self) -> bool {
        self.state.lock().dynamic.is_some()
    }

    pub fn dynamic_fetched_at(&self) -> i64 {
        self.state.lock().fetched_at
    }

    /// 设置动态目录：**null/空 = 退回内置**（绝不是清空目录——空目录聊天全废）。非空走合并语义。
    pub fn set_dynamic(&self, entries: Option<Vec<QModel>>) {
        let merged = match entries {
            Some(e) if !e.is_empty() => Some(self.merge(&e)),
            _ => None,
        };
        self.state.lock().dynamic = merged;
    }

    /// 三级降级链的**纯判定**（与网络无关，供单测直接驱动）：
    /// ① signed_raw 解析 ≥1 条 → 采用；② 否则 openapi_raw 解析 ≥1 条 → 采用；
    /// ③ 都没有产出 → **什么都不改**（dynamic 维持磁盘缓存或内置，失败永不清空）。
    pub fn resolve_sync_chain(&self, signed_raw: Option<&str>, openapi_raw: Option<&str>, at_ms: i64) -> CatalogSyncOutcome {
        if let Some(raw) = signed_raw {
            let parsed = parse_catalog_json(raw);
            if !parsed.is_empty() {
                return self.apply_catalog(parsed, raw, at_ms, "signed");
            }
        }
        if let Some(raw) = openapi_raw {
            let parsed = parse_catalog_json(raw);
            if !parsed.is_empty() {
                return self.apply_catalog(parsed, raw, at_ms, "openapi");
            }
        }
        // ③ 兜底：只为 UI「上次拉取原文」记录先到达的原文（对齐 MainActivity 的记录时机）。
        *self.last_raw.lock() = signed_raw.or(openapi_raw).map(str::to_string);
        self.outcome("builtin", Some("两级拉取均无产出，保持当前目录（内置或磁盘缓存）".into()))
    }

    fn apply_catalog(&self, parsed: Vec<QModel>, raw: &str, at_ms: i64, source: &str) -> CatalogSyncOutcome {
        let merged = self.merge(&parsed);
        {
            let mut st = self.state.lock();
            st.dynamic = Some(merged);
            st.fetched_at = at_ms;
        }
        *self.last_raw.lock() = Some(raw.to_string());
        *self.last_error.lock() = None;
        self.save_file(at_ms);
        self.outcome(source, None)
    }

    fn outcome(&self, source: &str, error: Option<String>) -> CatalogSyncOutcome {
        CatalogSyncOutcome {
            source: source.to_string(),
            total: self.current().len(),
            free: self.free_count(),
            dynamic: self.has_dynamic(),
            error,
        }
    }

    pub fn last_raw(&self) -> Option<String> {
        self.last_raw.lock().clone()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().clone()
    }

    /// 合并语义（Kotlin `merge` 逐条复刻，analysis §2.3）：内置顺序不变；同 key：
    /// 名称/倍率以内置为准、上下文取 max（内置≤0 用上游；上游更大用上游）、推理位取 or、
    /// 上游名进别名、unverified 置 false；上游新 key 按出现顺序追加末尾。
    pub fn merge(&self, entries: &[QModel]) -> Vec<QModel> {
        if entries.is_empty() {
            return builtin();
        }
        let base = builtin();
        let mut out = Vec::with_capacity(base.len() + entries.len());
        let mut used: HashSet<String> = HashSet::new();
        for b in base {
            let m = match entries.iter().find(|d| d.key == b.key) {
                None => b,
                Some(d) => {
                    // 先克隆基线名：字面量里 `name` 会 move 掉 b.name，别名过滤需要它。
                    let base_name = b.name.clone();
                    QModel {
                        // 上游是英文名+基础价，覆盖会「和官方 APP 对不上」——名称/倍率以内置为准。
                        name: b.name,
                        context_length: if b.context_length <= 0 {
                            d.context_length
                        } else if d.context_length > b.context_length {
                            d.context_length
                        } else {
                            b.context_length
                        },
                        reasoning: b.reasoning || d.reasoning,
                        price_factor: b.price_factor.or(d.price_factor),
                        aliases: {
                            let mut v = b.aliases.clone();
                            v.push(d.name.clone());
                            v.dedup();
                            v.retain(|a| !a.trim().is_empty() && *a != base_name);
                            v
                        },
                        key: b.key,
                        unverified: false,
                    }
                }
            };
            used.insert(m.key.clone());
            out.push(m);
        }
        for e in entries {
            if used.insert(e.key.clone()) {
                out.push(e.clone());
            }
        }
        out
    }

    /// 冷启动装载（对应 `load(ctx)`）：读 design.md 的合并后目录；解不出条目则保持内置。
    /// 条目直接装入（**不再次 merge**——磁盘里已是合并产物，重灌会把「上游名进别名」洗掉）。
    pub fn load(&self) {
        let root = match self.data.load_json_opt(FILE) {
            Some(v) => v,
            None => return,
        };
        let dyn_arr = match root.get("dynamic").and_then(Value::as_array) {
            Some(a) => a,
            None => return,
        };
        let parsed: Vec<QModel> = dyn_arr.iter().filter_map(|o| serde_json::from_value::<QModel>(o.clone()).ok()).collect();
        if parsed.is_empty() {
            return;
        }
        let fetched_at = crate::storage::json_i64(&root, "fetchedAt", 0);
        let mut st = self.state.lock();
        st.dynamic = Some(parsed);
        st.fetched_at = fetched_at;
    }

    /// 落盘当前动态目录（fetchedAt=at）。dynamic 为空时等同于 [`clear`](Self::clear)。
    pub fn save_file(&self, at_ms: i64) {
        let st = self.state.lock();
        match &st.dynamic {
            None => {
                let _ = std::fs::remove_file(self.data.path(FILE));
            }
            Some(list) => {
                let body = json!({ "dynamic": list, "fetchedAt": at_ms });
                let _ = self.data.save_json(FILE, &body);
            }
        }
    }

    /// 退回内置清单并删磁盘缓存。
    pub fn clear(&self) {
        {
            let mut st = self.state.lock();
            st.dynamic = None;
            st.fetched_at = 0;
        }
        let _ = std::fs::remove_file(self.data.path(FILE));
    }

    // ---- TTL（任务新增：自动同步节流） ----

    /// `has_dynamic && 0 < now - fetched_at < TTL`（严格小于；now 由调用方注入，便于测试）。
    pub fn is_fresh_at(&self, now_ms: i64) -> bool {
        self.needs_sync_at_checker(now_ms, false)
    }

    /// 是否应当再拉目录：没有动态目录 / fetchedAt 无效 / 超 TTL → true。
    pub fn needs_sync_at(&self, now_ms: i64) -> bool {
        self.needs_sync_at_checker(now_ms, true)
    }

    fn needs_sync_at_checker(&self, now_ms: i64, invert: bool) -> bool {
        let st = self.state.lock();
        let fresh = st.dynamic.is_some() && st.fetched_at > 0 && now_ms.saturating_sub(st.fetched_at) < CATALOG_TTL_SECS * 1000;
        if invert { !fresh } else { fresh }
    }

    /// 便捷口：以本机时钟判断。
    pub fn needs_sync(&self) -> bool {
        self.needs_sync_at(now_ms())
    }

    // ---- 客户端模型名解析（对应 resolve / findByIdentifier / byKey） ----

    /// 规则：`[1m]` 后缀识别（大小写不敏感）→ key→name→alias 三段 icase 匹配 → 未命中回落
    /// `auto`（列表里没有 auto 则首项）→ 带 `[1m]` 且需求与模型上下文都 < 900k 时升级 `ultimate`。
    pub fn resolve(&self, raw_model: &str, max_input_tokens: i64) -> Resolved {
        let list = self.current();
        let trimmed = raw_model.trim();
        let one_m = trimmed.to_lowercase().ends_with(ONE_M_SUFFIX);
        let bare = if one_m { trimmed[..trimmed.len() - ONE_M_SUFFIX.len()].trim() } else { trimmed };

        let found = if bare.is_empty() { None } else { find_by_identifier(&list, bare) };
        let mut model = match found.or_else(|| list.iter().find(|m| m.key == DEFAULT_KEY).cloned()) {
            Some(m) => m.clone(),
            None => list[0].clone(),
        };
        if one_m && max_input_tokens < ONE_M_THRESHOLD && model.context_length < ONE_M_THRESHOLD {
            if let Some(u) = list.iter().find(|m| m.key == ONE_M_KEY) {
                model = u.clone();
            }
        }
        Resolved { model, one_m_suffix: one_m }
    }

    /// 精确等值查当前列表（**区分大小写、不做别名解析**）。
    pub fn by_key(&self, key: &str) -> Option<QModel> {
        self.current().into_iter().find(|m| m.key == key)
    }

    pub fn free_count(&self) -> usize {
        self.current().iter().filter(|m| m.free()).count()
    }

    /// 每模型一个稳定 session_id：`"q2a-" + 小写 UUIDv4`（长度 40）。同模型稳定、异模型不同。
    pub fn session_for(&self, model_key: &str) -> String {
        let mut map = self.sessions.lock();
        if let Some(s) = map.get(model_key) {
            return s.clone();
        }
        let s = format!("q2a-{}", cosy::format_lower_uuid(&cosy::new_reverse_mask_uuid()));
        map.insert(model_key.to_string(), s.clone());
        s
    }

    /// 每次请求新生成 requestId（= `QoderCosyCrypto.newRequestId` 转发口）。
    pub fn new_request_id() -> String {
        cosy::new_request_id()
    }

    /// `/v1/models` 的 data 数组（analysis §2.7 逐字段；`price_factor` 仅非 null 才写、
    /// `unverified` 仅 true 才写、`aliases=([key]+aliases).distinct()` 去掉与 id 相同项且非空才写；
    /// `created` 整个数组共用本次调用时刻的 unix 秒）。
    pub fn openai_models(&self) -> Vec<Value> {
        let created = now_sec();
        self.current()
            .iter()
            .map(|m| {
                let public_id = if m.name.trim().is_empty() { m.key.clone() } else { m.name.clone() };
                let mut o = json!({
                    "id": public_id,
                    "object": "model",
                    "created": created,
                    "owned_by": "qoder",
                    "name": m.name,
                    "key": m.key,
                    "context_length": m.context_length,
                    "is_free": m.free(),
                });
                if let Some(p) = m.price_factor {
                    o["price_factor"] = json!(p);
                }
                if m.unverified {
                    o["unverified"] = json!(true);
                }
                let mut aliases: Vec<String> = Vec::with_capacity(m.aliases.len() + 1);
                aliases.push(m.key.clone());
                for a in &m.aliases {
                    if !aliases.contains(a) {
                        aliases.push(a.clone());
                    }
                }
                aliases.retain(|a| *a != public_id);
                if !aliases.is_empty() {
                    o["aliases"] = json!(aliases);
                }
                o
            })
            .collect()
    }

    /// 供 IPC 的目录快照（Qoder 部分）：{hasDynamic,fetchedAt,fresh,total,free,items}。
    pub fn snapshot(&self) -> Value {
        json!({
            "hasDynamic": self.has_dynamic(),
            "fetchedAt": self.dynamic_fetched_at(),
            "fresh": self.is_fresh_at(now_ms()),
            "total": self.current().len(),
            "free": self.free_count(),
            "items": self.openai_models(),
        })
    }

    // ---- 三级降级链的网络层（① COSY 签名 / ② openapi；③=resolve_sync_chain 的 builtin 分支）----

    /// 全链入口：jt 空白→直落 ③；① 失败（含解析 0 条）才试 ②；两级都无产出 → 保持现状。
    /// “能解析出 ≥1 条模型”是唯一成功判据（HTTP 状态码只是次要线索，analysis §8.3）。
    pub async fn sync_qoder_catalog(&self, cred: &QoderAccount, job_token: &str) -> CatalogSyncOutcome {
        let jt = job_token.trim();
        if jt.is_empty() {
            return self.outcome("builtin", Some("没有可用的 jobToken，继续用当前目录".into()));
        }
        let signed = self.fetch_catalog_signed(cred, jt).await;
        if let Ok(raw) = &signed {
            if !parse_catalog_json(raw).is_empty() {
                return self.resolve_sync_chain(Some(raw.as_str()), None, now_ms());
            }
        }
        let openapi = self.fetch_catalog_openapi(jt, &cred.mode).await;
        self.resolve_sync_chain(
            signed.as_deref().ok(),
            openapi.as_deref().ok(),
            now_ms(),
        )
    }
}

impl Default for QoderModelCatalog {
    fn default() -> Self {
        Self::new()
    }
}

fn find_by_identifier(list: &[QModel], identifier: &str) -> Option<QModel> {
    list.iter()
        .find(|m| m.key.eq_ignore_ascii_case(identifier))
        .or_else(|| list.iter().find(|m| m.name.eq_ignore_ascii_case(identifier)))
        .or_else(|| list.iter().find(|m| m.aliases.iter().any(|a| a.eq_ignore_ascii_case(identifier))))
        .cloned()
}

// ---------------------------------------------------------------------------
// ① COSY 签名目录 / ② openapi 回落 —— 独立 reqwest 实现（不 import gateway/upstream）
// ---------------------------------------------------------------------------

impl QoderModelCatalog {
    fn http_client(&self) -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| format!("http 客户端构建失败: {e}"))
    }

    /// ① `GET {inferBase}/algo/api/v2/model/list`（空 body；签路径不含 `/algo`）。
    /// 失败 Err 的文案与 `noteCatalogAttempt` 语义一致（供 fallback toast 的前 120 字）。
    async fn fetch_catalog_signed(&self, cred: &QoderAccount, job_token: &str) -> Result<String, String> {
        let runtime = cosy::generate_runtime_fields(
            &cred.uid,
            job_token,
            &cred.organization_id,
            &cred.organization_tags,
            cred.data_policy_agreed,
        );
        let unix_seconds = (now_ms() / 1000).to_string();
        let payload_b64 = cosy::authorization_payload_default_version(&QoderModelCatalog::new_request_id(), &runtime.encrypt_user_info);
        let authorization = cosy::authorization_model_list(&payload_b64, &runtime.key, &unix_seconds);
        let url = format!("{}{}", infer_base(&cred.mode), cosy::NATIVE_MODEL_LIST_PATH);
        let mut req = self
            .http_client()?
            .get(&url)
            .header("Accept", "application/json")
            .header("Authorization", authorization)
            .header("Cache-Control", "no-cache")
            .header("Cosy-Business-Product", cosy::SCENE_BUSINESS_PRODUCT)
            .header("Cosy-Business-Type", cosy::SCENE_BUSINESS_TYPE)
            .header("Cosy-ClientType", cosy::SCENE_CLIENT_TYPE)
            .header("Cosy-Data-Policy", if cred.data_policy_agreed { "agree" } else { "disagree" })
            .header("Cosy-Date", &unix_seconds)
            .header("Cosy-Key", &runtime.key)
            .header("Cosy-MachineId", machine::machine_uuid_in(&self.data))
            .header("Cosy-MachineOS", cosy_machine_os())
            .header("Cosy-Scene", cosy::SCENE_SCENE)
            .header("Cosy-User", &cred.uid)
            .header("Cosy-Version", cosy::COSY_VERSION)
            .header("Login-Version", "v2");
        // 刻意不发 User-Agent（analysis §2.5①；reqwest 默认也不附加）。可选组织头非空才带。
        if !cred.organization_id.is_empty() {
            req = req.header("Cosy-Organization-Id", &cred.organization_id);
        }
        if !cred.organization_tags.is_empty() {
            req = req.header("Cosy-Organization-Tags", cred.organization_tags.join(","));
        }
        let resp = req.send().await.map_err(|e| self.note_err(format!("传输异常: {e}")))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let why = format!("签名目录 HTTP {}: {}", status.as_u16(), trunc200(&text));
            *self.last_raw.lock() = Some(text);
            return Err(self.note_err(why));
        }
        Ok(text)
    }

    /// ② openapi 候选笛卡尔积：paths(EP_MODELS, EP_MODEL_LIST) × methods(GET, POST(体 `{}`))。
    /// 非 2xx 不抛、继续下一候选；首个 2xx 原文记 last_raw；解析 ≥1 条即返回，全部失败 Err(原因拼“；”)。
    async fn fetch_catalog_openapi(&self, job_token: &str, mode: &str) -> Result<String, String> {
        let client = self.http_client()?;
        let base = openapi_base(mode);
        let mut fails: Vec<String> = Vec::new();
        for path in [EP_MODELS, EP_MODEL_LIST] {
            for method in ["GET", "POST"] {
                let mut req = client
                    .request(if method == "POST" { reqwest::Method::POST } else { reqwest::Method::GET }, format!("{base}{path}"))
                    .header("Authorization", format!("Bearer {job_token}"))
                    .header("Accept", "application/json")
                    .header("User-Agent", "traework2api")
                    .header("Cosy-Version", OPENAPI_COSY_VERSION)
                    .header("Cosy-ClientType", cosy::SCENE_CLIENT_TYPE);
                if method == "POST" {
                    req = req.header("Content-Type", "application/json").body("{}");
                }
                let text = match req.send().await {
                    Err(e) => {
                        fails.push(format!("{method} {path} → 异常 {e}"));
                        continue;
                    }
                    Ok(r) => {
                        let status = r.status();
                        let t = r.text().await.unwrap_or_default();
                        if !status.is_success() {
                            fails.push(format!("{method} {path} → HTTP {}", status.as_u16()));
                            continue;
                        }
                        t
                    }
                };
                if self.last_raw.lock().is_none() {
                    *self.last_raw.lock() = Some(text.clone());
                }
                if !parse_catalog_json(&text).is_empty() {
                    return Ok(text);
                }
                fails.push(format!("{method} {path} → 内容不像目录"));
            }
        }
        Err(self.note_err(if fails.is_empty() { "全部候选都失败".into() } else { fails.join("；") }))
    }

    fn note_err(&self, msg: String) -> String {
        *self.last_error.lock() = Some(msg.clone());
        msg
    }
}

// ---------------------------------------------------------------------------
// parseCatalogJson（对应 §2.4 的容错解析，纯函数、无状态）
// ---------------------------------------------------------------------------

fn opt_str(o: &Value, key: &str) -> String {
    match o.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn ctx_from_config(cc: &Value) -> i64 {
    match cc.as_object() {
        None => 0,
        Some(map) => map
            .values()
            .filter_map(|v| v.as_object())
            .map(|o| crate::storage::json_i64(&Value::Object(o.clone()), "token_count", 0))
            .max()
            .unwrap_or(0),
    }
}

/// 单条 `toModel(o)`（analysis §2.4 逐规则）：key|model_key 回落、enable 显式 false/0 排除、
/// 名称三级回落、上下文三级回落（缺省 180000）、**price_factor 缺字段/null = 未知（绝不默认 1.0）**。
fn to_model(o: &Value) -> Option<QModel> {
    let key_raw = opt_str(o, "key");
    let key = if key_raw.trim().is_empty() { opt_str(o, "model_key").trim().to_string() } else { key_raw.trim().to_string() };
    if key.is_empty() {
        return None;
    }
    match o.get("enable") {
        Some(Value::Bool(b)) if !*b => return None,
        Some(Value::Number(n)) => {
            if n.as_f64().map(|f| f == 0.0).unwrap_or(false) {
                return None;
            }
        }
        _ => {}
    }
    let name_raw = opt_str(o, "display_name");
    let name_raw = if name_raw.trim().is_empty() { opt_str(o, "name") } else { name_raw };
    let name = if name_raw.trim().is_empty() { key.clone() } else { name_raw };

    let mut ctx = crate::storage::json_i64(o, "max_input_tokens", 0);
    if ctx <= 0 {
        ctx = o.get("context_config").map(ctx_from_config).unwrap_or(0);
    }
    if ctx <= 0 {
        ctx = DEFAULT_CONTEXT;
    }

    let price_factor = match o.get("price_factor") {
        None | Some(Value::Null) => None,
        Some(v) => v.as_f64().filter(|f| f.is_finite()),
    };
    Some(QModel {
        key,
        name,
        context_length: ctx,
        reasoning: o.get("is_reasoning").and_then(Value::as_bool).unwrap_or(false),
        price_factor,
        aliases: Vec::new(),
        unverified: false,
    })
}

fn looks_like_model(o: &Value) -> bool {
    let has_key = o.get("key").is_some() || o.get("model_key").is_some();
    let has_meta = ["price_factor", "display_name", "max_input_tokens", "context_config", "is_reasoning", "format"]
        .iter()
        .any(|k| o.get(k).is_some());
    has_key && has_meta
}

fn collect(node: &Value, out: &mut Vec<QModel>, seen: &mut HashSet<String>) {
    match node {
        Value::Object(map) => {
            if looks_like_model(node) {
                if let Some(m) = to_model(node) {
                    if seen.insert(m.key.clone()) {
                        out.push(m);
                    }
                }
            } else {
                for v in map.values() {
                    collect(v, out, seen);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                collect(v, out, seen);
            }
        }
        _ => {}
    }
}

/// 容错解析上游目录 JSON，认三种根形状（分组→递归扫描）；**解析 0 条 = 失败**，调用方必须保留原清单。
/// 与 Kotlin 的一处差异：递归扫描的对象子键迭代顺序为 serde_json 字典序（Kotlin JSONObject 为哈希序），
/// 仅影响无法判定优先级的未知包装格式里新 key 的追加次序，合并后内置项顺序始终不变。
pub fn parse_catalog_json(raw: &str) -> Vec<QModel> {
    let t = raw.trim();
    if !(t.starts_with('[') || t.starts_with('{')) {
        return Vec::new();
    }
    let root: Value = match serde_json::from_str(t) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if root.is_object() {
        let mut grouped = false;
        for scene in SCENES {
            if let Some(arr) = root.get(scene).and_then(Value::as_array) {
                grouped = true;
                for o in arr.iter().filter(|v| v.is_object()) {
                    if let Some(m) = to_model(o) {
                        if seen.insert(m.key.clone()) {
                            out.push(m);
                        }
                    }
                }
            }
        }
        if grouped && !out.is_empty() {
            return out;
        }
    }
    collect(&root, &mut out, &mut seen);
    out
}

// ---------------------------------------------------------------------------
// 测试（selfCheck 陈旧断言按 21 条基线修正；缓存 TTL / 降级链 / 合并语义 / 解析容错）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tmp_store(tag: &str) -> Arc<DataStore> {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tw-qoder-catalog-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(DataStore::new(&dir))
    }

    fn cat(tag: &str) -> QoderModelCatalog {
        QoderModelCatalog::with_store(tmp_store(tag))
    }

    #[test]
    fn builtin_snapshot_is_21_and_keys_unique_free_two() {
        // 分析文档矛盾裁决的基线：源码 21 条、首条 qwen3-coder-plus、无 unverified、免费恰 2。
        let b = builtin();
        assert_eq!(b.len(), 21);
        assert_eq!(b[0].key, "qwen3-coder-plus");
        assert_eq!(b[1].key, DEFAULT_KEY);
        assert_eq!(b.iter().map(|m| m.key.clone()).collect::<HashSet<_>>().len(), 21);
        assert!(b.iter().all(|m| !m.unverified));
        assert_eq!(b.iter().filter(|m| m.free()).count(), 2);
        let keys: Vec<&str> = b.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "qwen3-coder-plus", "auto", "ultimate", "performance", "efficient", "smodel", "cmodel", "qmodel_38max",
                "qfmodel", "kmodel_latest", "kmodel", "gmodel", "gfmodel", "dmodel", "dfmodel", "mmodel", "lite",
                "qmodel_latest", "qmodel", "q37fmodel", "gm51model",
            ]
        );
        // 逐字段抽查（对齐 Kotlin 74-96 行）：中文名/上下文/倍率/别名。
        let u = &b[2];
        assert_eq!((u.name.as_str(), u.context_length, u.reasoning, u.price_factor, u.aliases.as_slice()), ("极致", 1_000_000, true, Some(0.8), &["Ultimate".to_string()][..]));
        assert_eq!(b[18].price_factor, Some(0.04)); // qmodel 0.04
        assert_eq!(b[6].context_length, 200_000); // cmodel
        assert_eq!(b[10].context_length, 256_000); // kmodel
        assert_eq!(b[16].price_factor, None); // lite 没写倍率=未知
    }

    #[test]
    fn price_label_and_free_semantics() {
        assert_eq!(QModel { price_factor: Some(0.0), ..Default::default() }.price_label(), "0");
        assert_eq!(QModel { price_factor: Some(1.0), ..Default::default() }.price_label(), "1");
        assert_eq!(QModel { price_factor: Some(2.5), ..Default::default() }.price_label(), "2.5");
        assert_eq!(QModel { price_factor: None, ..Default::default() }.price_label(), "?");
        assert!(QModel { price_factor: Some(0.0), ..Default::default() }.free());
        assert!(!QModel { price_factor: Some(0.5), ..Default::default() }.free());
        assert!(!QModel { price_factor: None, ..Default::default() }.free(), "未知倍率不算免费");
    }

    #[test]
    fn resolve_key_name_alias_default_and_one_m() {
        let c = cat("resolve");
        assert_eq!(c.resolve("qmodel", 0).key(), "qmodel");
        assert_eq!(c.resolve("Ultimate", 0).key(), "ultimate"); // 展示名（icase）
        assert_eq!(c.resolve("GLM-5.3", 0).key(), "gmodel");
        assert_eq!(c.resolve("NONEXISTENT", 0).key(), DEFAULT_KEY);
        assert_eq!(c.resolve("", 0).key(), DEFAULT_KEY);
        assert_eq!(c.resolve("Kimi-K3", 0).key(), "kmodel_latest"); // 旧别名
        assert_eq!(c.resolve("DeepSeek-V4-Flash", 0).key(), "dfmodel");
        assert_eq!(c.resolve("Qwen3.8-Max", 0).key(), "qmodel_38max");

        let lm = c.resolve("lite[1m]", 200_000);
        assert!(lm.one_m_suffix);
        assert_eq!(lm.key(), ONE_M_KEY, "上下文不足 900k 应升级到 ultimate");
        assert!(lm.public_id().ends_with(ONE_M_SUFFIX));
        let lm2 = c.resolve("qmodel[1m]", 1_000_000);
        assert_eq!(lm2.key(), "qmodel", "已够 1M 不该升级");
        assert_eq!(lm2.public_id(), "qmodel[1m]");
    }

    #[test]
    fn parse_catalog_cli_grouped_fixture_matches_selfcheck() {
        // Kotlin selfCheck 的 CLI 分组 fixture 原样搬运：4 条、0 倍率、model_key 回落、context_config 取值。
        let cli = r#"
            {"chat":[
              {"key":"auto","display_name":"Auto","price_factor":0.0,"max_input_tokens":180000},
              {"model_key":"free-model","name":"Free Model","price_factor":0.0,
               "context_config":{"default":{"token_count":256000}}},
              {"key":"paid","display_name":"Paid","price_factor":3.0,"is_reasoning":true,
               "max_input_tokens":1000000}
            ],"byok_teams":[{"key":"team-only","display_name":"Team","price_factor":1.0}]}
        "#;
        let parsed = parse_catalog_json(cli);
        assert_eq!(parsed.len(), 4);
        assert_eq!((parsed[0].key.as_str(), parsed[0].price_factor), ("auto", Some(0.0)));
        assert_eq!((parsed[1].key.as_str(), parsed[1].context_length), ("free-model", 256_000));
        assert!(parsed[2].reasoning);
        assert_eq!(parsed[3].key, "team-only");

        // 扁平数组 / OpenAI 包装 / 垃圾输入
        let flat = parse_catalog_json(r#"[{"key":"a","display_name":"A","price_factor":2}]"#);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].price_factor, Some(2.0));
        let wrapped = parse_catalog_json(r#"{"data":{"models":[{"key":"m","format":"x","price_factor":null}]}}"#);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].price_factor, None, "显式 null 也保持「未知」语义");
        assert!(parse_catalog_json("").is_empty());
        assert!(parse_catalog_json("not json").is_empty());
        assert!(parse_catalog_json(r#"{"foo":"bar"}"#).is_empty());
        // enable 闸：仅显式 false / 数字 0 排除。
        assert!(parse_catalog_json(r#"[{"key":"a","display_name":"A","enable":false}]"#).is_empty());
        assert_eq!(parse_catalog_json(r#"[{"key":"a","display_name":"A","enable":0}]"#).len(), 0);
        assert_eq!(parse_catalog_json(r#"[{"key":"a","display_name":"A","enable":true}]"#).len(), 1);
    }

    #[test]
    fn merge_semantics_builtin_order_and_gains_and_aliases() {
        // Kotlin selfCheck 场景：4 条目录 → 内置 21 + 新 3；名称/倍率内置为准；上游英文名进别名。
        let c = cat("merge");
        let cli = r#"
            {"chat":[
              {"key":"auto","display_name":"Auto-Upstream","price_factor":0.0,"max_input_tokens":180000},
              {"model_key":"free-model","name":"Free Model","price_factor":0.0,"context_config":{"default":{"token_count":256000}}},
              {"key":"paid","display_name":"Paid","price_factor":3.0,"is_reasoning":true,"max_input_tokens":1000000}
            ],"byok_teams":[{"key":"team-only","display_name":"Team","price_factor":1.0}]}
        "#;
        let parsed = parse_catalog_json(cli);
        let merged = c.merge(&parsed);
        assert_eq!(merged.len(), 21 + 3);
        assert_eq!(merged[0].key, "qwen3-coder-plus", "内置顺序保持（首条仍为源码基线）");
        let auto = merged.iter().find(|m| m.key == DEFAULT_KEY).unwrap();
        assert_eq!(auto.price_factor, Some(0.5), "同 key 倍率保留内置值");
        assert_eq!(auto.name, "Auto", "上游显示名不得覆盖内置名");
        assert_eq!(auto.aliases, vec!["Auto-Upstream".to_string()], "上游名字留作别名");
        assert_eq!(merged.last().unwrap().key, "team-only", "新 key 追加末尾");
        // 空 entries = 内置；setDynamic(null/空) = 退回内置而不是清目录。
        assert_eq!(c.merge(&[]).len(), 21);
        c.set_dynamic(Some(parsed.clone()));
        assert_eq!(c.current().len(), 24);
        c.set_dynamic(Some(vec![]));
        assert!(!c.has_dynamic());
        assert_eq!(c.current().len(), 21, "解析失败/空清单绝不目录清空");
        // 推理位取 or：上游缺省 false 不得冲掉内置 true。
        let only_auto = parse_catalog_json(r#"[{"key":"ultimate","display_name":"Ultimate"}]"#);
        let merged2 = c.merge(&only_auto);
        assert!(merged2.iter().find(|m| m.key == "ultimate").unwrap().reasoning);
    }

    #[test]
    fn openai_models_shape_fields() {
        let c = cat("openai");
        let arr = c.openai_models();
        assert_eq!(arr.len(), 21);
        // 内置首条 qwen3-coder-plus：id=官方显示名、内部 key 保留在 key 字段。
        assert_eq!(arr[0]["id"], json!("Qwen3 Coder Plus"));
        assert_eq!(arr[0]["key"], json!("qwen3-coder-plus"));
        assert_eq!(arr[0]["object"], json!("model"));
        assert_eq!(arr[0]["owned_by"], json!("qoder"));
        assert!(arr[0]["created"].is_i64());
        assert!(arr[0]["context_length"].is_i64());
        assert_eq!(arr[0]["is_free"], json!(false));
        assert!(arr[0].get("price_factor").is_none(), "倍率未知不写字段");
        // Kotlin :405：别名恒 = ([key]+aliases).distinct() 去掉与 id 相同项。首条 name≠key，
        // 所以 aliases 必写且含 key（兼容"直接填 key"的老客户端）——内置全部条目名≠key，
        // 条目级「不写 aliases」只发生在 name==key 时（下方动态目录构造验证）。
        assert_eq!(arr[0]["aliases"], json!(["qwen3-coder-plus"]), "key 进别名");
        let auto = &arr[1];
        assert_eq!(auto["id"], json!("Auto"));
        assert_eq!(auto["price_factor"], json!(0.5));
        assert_eq!(auto["aliases"], json!(["auto"]), "key 进别名");
        let ult = arr.iter().find(|o| o["key"] == json!("ultimate")).unwrap();
        assert_eq!(ult["is_free"], json!(false));
        assert_eq!(ult["price_factor"], json!(0.8));
        // name==key 且无别名 → 唯一「省略 aliases」情形（内置条目名全部 ≠ key，所以逐条都会写 key）。
        let c2 = cat("openai-bare");
        c2.set_dynamic(Some(vec![QModel {
            key: "solo".into(),
            name: "solo".into(),
            context_length: 1234,
            reasoning: false,
            price_factor: None,
            aliases: vec![],
            unverified: true,
        }]));
        let bare = c2.openai_models().into_iter().find(|o| o["key"] == json!("solo")).unwrap();
        assert_eq!(bare["id"], json!("solo"));
        assert!(bare.get("aliases").is_none(), "过滤后别名空则省略键");
        assert_eq!(bare["unverified"], json!(true));
        assert!(bare.get("price_factor").is_none());
        // resolve 用官方显示名可解析回 key（三段匹配闭环）。
        assert_eq!(c.resolve("Qwen3 Coder Plus", 0).key(), "qwen3-coder-plus");
    }

    #[test]
    fn sync_chain_falls_back_never_clears_ttl_and_persistence() {
        let st = tmp_store("chain");
        let c = QoderModelCatalog::with_store(st.clone());

        // 全链失败：两级 raw 都无产出 → 什么都不改（仍在内置），失败原因可读。
        let o1 = c.resolve_sync_chain(None, None, now_ms());
        assert_eq!(o1.source, "builtin");
        assert_eq!(o1.total, 21);
        assert!(!o1.dynamic);

        // ① HTTP 200 但业务壳解析 0 条 → 照样算失败，继续试 ②；② 有产出 → openapi。
        let good = r#"{"chat":[{"key":"new-free","display_name":"新免费","price_factor":0.0,"max_input_tokens":1000000}]}"#;
        let o2 = c.resolve_sync_chain(Some(r#"{"code":403,"message":"nope"}"#), Some(good), 1_700_000_000_000);
        assert_eq!(o2.source, "openapi");
        assert!(o2.dynamic);
        assert_eq!(o2.total, 22);
        assert_eq!(o2.free, 3, "内置 2 免费 + new-free");
        assert_eq!(c.dynamic_fetched_at(), 1_700_000_000_000);
        assert_eq!(c.last_raw().as_deref(), Some(good));

        // 落盘 = 合并后目录 + fetchedAt（design.md §Data Models）；且能重载。
        let raw = st.load_json(FILE, Value::Null);
        assert_eq!(raw["fetchedAt"], json!(1_700_000_000_000i64));
        assert_eq!(raw["dynamic"].as_array().unwrap().len(), 22);
        let c2 = QoderModelCatalog::with_store(st.clone());
        c2.load();
        assert!(c2.has_dynamic());
        assert_eq!(c2.current().len(), 22);
        // 载入的目录不再次 merge（上游别名不被洗掉）。
        assert_eq!(c2.by_key("new-free").unwrap().name, "新免费");

        // TTL 3600s：拉取时刻 + 3599s 内新鲜，+ 3601s 需再同步；无动态目录永远不新鲜。
        assert!(c2.is_fresh_at(1_700_000_000_000 + CATALOG_TTL_SECS * 1000 - 1));
        assert!(!c2.is_fresh_at(1_700_000_000_000 + CATALOG_TTL_SECS * 1000));
        assert!(!c2.needs_sync_at(1_700_000_000_000 + CATALOG_TTL_SECS * 1000 - 1));
        assert!(c2.needs_sync_at(1_700_000_000_000 + CATALOG_TTL_SECS * 1000));
        let empty = cat("ttl-empty");
        assert!(empty.needs_sync_at(now_ms()), "没有动态目录时永远需要拉");
        assert!(!empty.is_fresh_at(now_ms()));

        // clear() 回退内置并删文件。
        c2.clear();
        assert!(!c2.has_dynamic());
        assert_eq!(c2.current().len(), 21);
        assert!(!st.exists(FILE));
    }

    #[test]
    fn signed_then_openapi_priority_order() {
        // ① 有产出时无条件优先（成功判据=解析≥1 条，而非 HTTP 码）。
        let c = cat("priority");
        let good1 = r#"[{"key":"one-up","display_name":"OneUp","price_factor":0.5}]"#;
        let good2 = r#"[{"key":"two-up","display_name":"TwoUp","price_factor":0.5}]"#;
        let o = c.resolve_sync_chain(Some(good1), Some(good2), now_ms());
        assert_eq!(o.source, "signed");
        assert!(c.by_key("one-up").is_some());
        assert!(c.by_key("two-up").is_none());
    }

    #[test]
    fn session_for_stable_shape() {
        let c = cat("session");
        let s = c.session_for("auto");
        assert_eq!(s, c.session_for("auto"));
        assert_ne!(s, c.session_for("ultimate"));
        assert!(s.starts_with("q2a-"));
        assert_eq!(s.chars().count(), 4 + 36);
    }

    #[test]
    fn base_hosts_follow_mode() {
        assert_eq!(infer_base("global"), INFER_BASE_GLOBAL);
        assert_eq!(infer_base("cn"), INFER_BASE_CN);
        assert_eq!(infer_base(""), INFER_BASE_CN, "未知 mode 一律按 cn 兜底");
        assert_eq!(openapi_base("GLOBAL"), OPENAPI_BASE_GLOBAL);
        assert_eq!(openapi_base("cn"), OPENAPI_BASE_CN);
    }
}
