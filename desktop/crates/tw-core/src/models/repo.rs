//! ModelsRepo（Kotlin `object ModelsRepo`，ModelsRepo.kt 105 行逐函数对照，analysis §1）。
//! 只有上游拉回来的表，没有内置兜底清单；未刷且无磁盘缓存时 `current()` 返回**空数组**（合法态，
//! `/v1/models` 返回空列表、模型 Tab 显示「尚未加载」，是设计意图不是故障）。

use crate::storage::DataStore;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

/// prefs `traework2api_models`/`models_json` 的桌面映射：文件内容 = OpenAI 数组**原文**（design.md §Data Models）。
pub const FILE: &str = "models.trae.json";

/// `TraeRegion` agentHost：CN 实测可用；SG host 存在但 `get_detail_param` 未实测（trae-upstream.md §2.7：
/// SG 打到 core-normal.trae.ai 会失败 → 上层保持现有表），与 Kotlin「照发、等网络层失败」行为一致。
pub const AGENT_HOST_CN: &str = "https://trae-api-cn.mchost.guru";
pub const AGENT_HOST_SG: &str = "https://core-normal.trae.ai";

/// `TraeRegion.canQueryQuota` 等价闸：host 为空（未知区域的防御位）直接不发请求（analysis §1.5）。
pub fn trae_agent_host(region: &str) -> &'static str {
    match crate::auth::model::region_from_id(Some(region)) {
        crate::types::region::SG => AGENT_HOST_SG,
        _ => AGENT_HOST_CN,
    }
}

pub const EP_MODELS: &str = "/api/ide/v1/get_detail_param";
/// 必须 `solo_work_remote`：新模型只挂在该 function 下（trae-upstream.md §2.1 实测结论）。
pub const FUNCTION: &str = "solo_work_remote";
pub const DEFAULT_CONTEXT: i64 = 131_072;
pub const CREATED: i64 = 1_753_600_000;
pub const OWNED_BY: &str = "trae-solo";

// ---- SOLO 请求头常量（trae-upstream.md §2.3，chat/models 共用；models 场景 Accept=application/json）----
const APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
const IDE_VERSION: &str = "0.1.43";
const IDE_VERSION_CODE: &str = "20260716";
const DEVICE_BRAND: &str = "83DG";
const OS_VERSION: &str = "Windows 11 Pro";

/// `parseModels()` 输出项 = `/v1/models` 数组条目 = 落盘数组元素（固定 7 字段，字段顺序同 Kotlin）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub config_name: String,
    pub context_length: i64,
    pub object: String,
    pub owned_by: String,
    pub created: i64,
}

impl ModelEntry {
    fn to_value(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "config_name": self.config_name,
            "context_length": self.context_length,
            "object": self.object,
            "owned_by": self.owned_by,
            "created": self.created,
        })
    }
}

struct RepoState {
    /// null = 还没刷过（内存），磁盘可能仍缓存。对应 Kotlin `@Volatile dynamic: JSONArray?`。
    dynamic: Option<Vec<Value>>,
    /// `init(ctx)` 只装一次的幂等标志。
    loaded: bool,
}

/// 对应 Kotlin `object ModelsRepo`。生产走 [`ModelsRepo::global`]；测试 `with_store` 注入临时目录。
pub struct ModelsRepo {
    data: Arc<DataStore>,
    state: Mutex<RepoState>,
}

impl ModelsRepo {
    pub fn global() -> Arc<ModelsRepo> {
        static REPO: OnceLock<Arc<ModelsRepo>> = OnceLock::new();
        REPO.get_or_init(|| Arc::new(ModelsRepo::new())).clone()
    }

    pub fn new() -> Self {
        Self::with_store(crate::storage::store())
    }

    pub fn with_store(data: Arc<DataStore>) -> Self {
        Self { data, state: Mutex::new(RepoState { dynamic: None, loaded: false }) }
    }

    /// 对应 `init(ctx)`：冷启动装配磁盘缓存，必须在首个 `/v1/models` 前完成。
    /// 无值/损坏/解析失败/空数组 = 当没存过（保持空态）；仅当 length>0 才装入。幂等。
    pub fn load(&self) {
        let mut st = self.state.lock();
        if st.dynamic.is_some() || st.loaded {
            st.loaded = true;
            return;
        }
        st.loaded = true;
        let root = match self.data.load_json_opt(FILE) {
            Some(v) => v,
            None => return,
        };
        if let Value::Array(arr) = root {
            if !arr.is_empty() {
                st.dynamic = Some(arr);
            }
        }
    }

    /// 对应 `current()`：从不返回 null；未刷时是空 Vec（空表合法）。
    pub fn current(&self) -> Vec<Value> {
        self.state.lock().dynamic.clone().unwrap_or_default()
    }

    /// `current()` 的 JSON 数组形态（网关 `/v1/models` 直出）。
    pub fn current_json(&self) -> Value {
        Value::Array(self.current())
    }

    /// 对应 `isDynamic`：UI 区分「尚未加载/已加载」。
    pub fn is_dynamic(&self) -> bool {
        self.state.lock().dynamic.is_some()
    }

    /// 对应 `applyFetched(arr)`：**空表一律拒绝覆盖**（那道 `length() > 0` 闸——刷新失败绝不能把
    /// 已有清单清空）。非空表置入内存并落盘；落盘失败静默（Kotlin runCatching 同构）。
    pub fn apply_fetched(&self, arr: Vec<Value>) {
        if arr.is_empty() {
            return;
        }
        if let Err(e) = self.data.save_json(FILE, &Value::Array(arr.clone())) {
            log_save_err(e);
        }
        self.state.lock().dynamic = Some(arr);
    }

    /// `fetch_trae_models` 产物直接装入的便捷口。
    pub fn apply_entries(&self, entries: Vec<ModelEntry>) {
        self.apply_fetched(entries.iter().map(ModelEntry::to_value).collect());
    }

    /// 供 IPC 的 `models_snapshot`（trae 部分）：{loaded, count, items(原表数组)}。
    pub fn snapshot(&self) -> Value {
        let items = self.current_json();
        json!({
            "loaded": self.is_dynamic(),
            "count": items.as_array().map(Vec::len).unwrap_or(0),
            "items": items,
        })
    }
}

impl Default for ModelsRepo {
    fn default() -> Self {
        Self::new()
    }
}

fn log_save_err(_e: String) {
    // Android: runCatching 静默。桌面版同样不打日志刷屏（磁盘满等异常留给下轮刷新自愈）。
}

// ---------------------------------------------------------------------------
// 拉取 + 解析（独立于 gateway/upstream；HTTP 用 reqwest 直连，只发 model/all 类请求，不发聊天）
// ---------------------------------------------------------------------------

/// `isChatModel()` 第 4 条的词形兜底黑名单（contains 匹配，config_name 先小写）。
const BLOCKED_WORDS: [&str; 9] =
    ["agent", "subagent", "summary", "sagitta", "aquila", "file_search", "explore_", "computer_use", "browser_use"];

/// 对应 `isBuiltinChatName`。
pub fn is_builtin_chat_name(id: &str) -> bool {
    let s = id.to_lowercase();
    if s.starts_with("custom_model") {
        return false;
    }
    BLOCKED_WORDS.iter().all(|w| !s.contains(w))
}

/// 对应 `isChatModel(cfg)`（判据顺序逐条对齐：config_source==1 → display_config 存在且非自定义
/// → 非 is_invisible_to_user → 名字词形）。任一条不满足即排除。
pub fn is_chat_model(cfg: &Value) -> bool {
    if cfg.get("config_source").and_then(Value::as_i64).unwrap_or(0) != 1 {
        return false;
    }
    let dc = cfg.get("display_config");
    if dc.and_then(|d| d.get("is_custom_model")).and_then(Value::as_bool) == Some(true) {
        return false;
    }
    if cfg.get("is_invisible_to_user").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    dc.is_some() && is_builtin_chat_name(opt_str(cfg, "config_name").trim())
}

fn opt_str(o: &Value, key: &str) -> String {
    match o.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn opt_i64(o: &Value, key: &str, def: i64) -> i64 {
    match o.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(def),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(def),
        _ => def,
    }
}

/// 对应 `parseModels(root)`：`config_info_list` 缺失/全被过滤 → 空 Vec（调用方按失败处理，保留现状——
/// 这就是「拉取失败降级到兜底」的判定，不依赖 gateway/upstream）。
pub fn parse_trae_models(root: &Value) -> Vec<ModelEntry> {
    let mut out = Vec::new();
    let list = match root.get("config_info_list").and_then(Value::as_array) {
        Some(a) => a,
        None => return out,
    };
    for cfg in list.iter().filter(|v| v.is_object()) {
        let id = opt_str(cfg, "config_name").trim().to_string();
        if id.is_empty() {
            continue;
        }
        if !is_chat_model(cfg) {
            continue;
        }
        let name = cfg
            .get("display_config")
            .map(|d| opt_str(d, "display_name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let mut ctx = cfg
            .get("model_detail_list")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map(|m| opt_i64(m, "max_input_tokens", 0))
            .unwrap_or(0);
        if ctx <= 0 {
            ctx = DEFAULT_CONTEXT;
        }
        out.push(ModelEntry {
            id: id.clone(),
            name: if name.is_empty() { id.clone() } else { name },
            config_name: id,
            context_length: ctx,
            object: "model".into(),
            owned_by: OWNED_BY.into(),
            created: CREATED,
        });
    }
    out
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("http 客户端构建失败: {e}"))
}

/// `TraeUpstream.fetchModels` 的独立移植：**只拉模型表，不发聊天请求**，不引用 gateway/upstream。
///
/// 成功判据对齐 Kotlin：HTTP 非 2xx / 网络异常 / 无 `config_info_list` / 解析出空表 → Err
/// （Kotlin 的 `null`）。调用方拿到 Err 时**必须什么都不改**（磁盘缓存/现有表继续顶着），
/// 成功才 [`ModelsRepo::apply_entries`]。token 只进内存与请求头，绝不写入 Err 文案/日志。
pub async fn fetch_trae_models(token: &str, uid: &str, region: &str) -> Result<Vec<ModelEntry>, String> {
    let host = trae_agent_host(region);
    if host.is_empty() {
        // canQueryQuota==false 闸（analysis §1.5）：不发请求。
        return Err("当前区域不可用，未拉取模型表".into());
    }
    let body = json!({
        "function": FUNCTION,
        "config_names": null,
        "need_prompt": false,
        "current_config_info": null,
        "poly_prompt": true,
        "mode_type": null,
        "agent_type": null,
    });
    let mut req = http_client()?
        .post(format!("{host}{EP_MODELS}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", format!("Trae/{IDE_VERSION}"))
        .header("Authorization", format!("Cloud-IDE-JWT {token}"))
        .header("X-Cloudide-Token", token)
        .header("X-Ide-Token", token)
        .header("X-App-Id", APP_ID)
        .header("X-App-Version", "default")
        .header("X-Ide-Version", IDE_VERSION)
        .header("X-Ide-Version-Code", IDE_VERSION_CODE)
        .header("X-App-Version-Code", IDE_VERSION_CODE)
        .header("X-Ide-Version-Type", "stable")
        .header("X-Device-Type", "windows")
        .header("X-OS-Version", OS_VERSION)
        .header("X-Device-Brand", DEVICE_BRAND)
        .header("Request-Traffic-Type", "prod")
        .json(&body);
    if !uid.trim().is_empty() {
        // 裸 uid 头必须带（trae-upstream.md §2.3 #7），仅当非空白才发。
        req = req.header("X-Uid", uid);
    }
    let resp = req.send().await.map_err(|e| format!("传输异常: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("http {}: {}", status.as_u16(), trunc200(&text)));
    }
    let root: Value = serde_json::from_str(&text).map_err(|_| "上游响应不是 JSON".to_string())?;
    let entries = parse_trae_models(&root);
    if entries.is_empty() {
        return Err("上游模型表解析后为空".into());
    }
    Ok(entries)
}

pub(crate) fn trunc200(s: &str) -> String {
    s.chars().take(200).collect()
}

// ---------------------------------------------------------------------------
// 测试（ModelsRepo.selfCheck 三条钉死的语义 + 持久化 + 解析过滤；临时目录注入，不触网）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tmp_store(tag: &str) -> Arc<DataStore> {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tw-models-repo-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(DataStore::new(&dir))
    }

    fn entry(id: &str) -> Value {
        json!({"id": id, "object": "model"})
    }

    #[test]
    fn self_check_empty_allowed_and_empty_table_rejected() {
        // Kotlin selfCheck 三行为：未刷新为空 / 空表拒覆盖 / 非空可装入且 current 跟随。
        let r = ModelsRepo::with_store(tmp_store("selfcheck"));
        assert!(r.current().is_empty());
        assert!(!r.is_dynamic());
        r.apply_fetched(vec![]);
        assert!(!r.is_dynamic(), "空表不得覆盖");
        let probe = json!({"id": "selfcheck-probe"});
        r.apply_fetched(vec![probe.clone()]);
        assert!(r.is_dynamic());
        assert_eq!(r.current(), vec![probe]);
    }

    #[test]
    fn refresh_failure_never_clears_and_disk_cache_roundtrip_reload() {
        let st = tmp_store("persist");
        let r = ModelsRepo::with_store(st.clone());
        r.apply_fetched(vec![entry("a"), entry("b")]);
        // 再刷失败（空/Err）→ 现表保留（那道闸在 apply_fetched；网络失败调用方根本不 apply）。
        r.apply_fetched(vec![]);
        assert_eq!(r.current().len(), 2);
        // 文件 = 数组原文，字段逐字。
        let raw = st.load_json(FILE, Value::Null);
        assert_eq!(raw.as_array().unwrap().len(), 2);
        // 模拟冷启动：新实例 load() 从磁盘恢复。
        let r2 = ModelsRepo::with_store(st);
        r2.load();
        assert!(r2.is_dynamic());
        assert_eq!(r2.current().len(), 2);
    }

    #[test]
    fn load_treats_empty_or_missing_as_unloaded() {
        let st = tmp_store("loadempty");
        st.save_json(FILE, &Value::Array(vec![])).unwrap();
        let r = ModelsRepo::with_store(st);
        r.load();
        assert!(!r.is_dynamic(), "空数组磁盘缓存要当没存过（等下次刷新覆盖）");
        assert!(r.current().is_empty());
    }

    #[test]
    fn parse_models_filters_non_chat_and_fills_defaults() {
        let root = json!({"config_info_list": [
            // 正常官方内置聊天项：名字过滤全过，display_name 有值。
            {"config_name":"DeepSeek-V4-Pro","config_source":1,"display_config":{"display_name":"DeepSeek V4 Pro","is_custom_model":false},
             "model_detail_list":[{"max_input_tokens":200_000}]},
            // 无 max_input_tokens → 回落 131072；无 display_name → 名称回落 id。
            {"config_name":"glm-4.7","config_source":1,"display_config":{}},
            // config_source=3 用户自接入第三方 → 排除。
            {"config_name":"mistral-small-4","config_source":3,"display_config":{"display_name":"肖恩ai"}},
            // is_invisible_to_user=true（上游隐藏项如 glm-5）→ 排除。
            {"config_name":"glm-5","config_source":1,"is_invisible_to_user":true,"display_config":{}},
            // is_custom_model=true → 排除。
            {"config_name":"x","config_source":1,"display_config":{"is_custom_model":true}},
            // 词形黑名单：subagent、空 config_name、无 display_config。
            {"config_name":"file_search_agent","config_source":1,"display_config":{}},
            {"config_name":"  ","config_source":1,"display_config":{}},
            {"config_name":"sagitta","config_source":1,"display_config":{},"is_invisible_to_user":false},
            {"config_name":"no-display-config","config_source":1},
        ]});
        let got = parse_trae_models(&root);
        assert_eq!(got.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["DeepSeek-V4-Pro", "glm-4.7"]);
        assert_eq!(got[0].to_value(), json!({
            "id":"DeepSeek-V4-Pro","name":"DeepSeek V4 Pro","config_name":"DeepSeek-V4-Pro",
            "context_length":200000,"object":"model","owned_by":"trae-solo","created":1753600000
        }));
        assert_eq!(got[1].name, "glm-4.7");
        assert_eq!(got[1].context_length, DEFAULT_CONTEXT);
        // 空表判定：解析不出条目 → 调用方保留现状（fetch 里转成 Err）。
        assert!(parse_trae_models(&json!({})).is_empty());
        assert!(parse_trae_models(&json!({"config_info_list": []})).is_empty());
    }

    #[test]
    fn blocked_words_and_custom_model_prefix() {
        assert!(!is_builtin_chat_name("browser_use_subagent"));
        assert!(!is_builtin_chat_name("custom_model_7"));
        assert!(!is_builtin_chat_name("summary"));
        assert!(!is_builtin_chat_name("EXPLORE_THINGS"));
        assert!(is_builtin_chat_name("DeepSeek-V4-Pro"));
        assert!(is_builtin_chat_name("Kimi-K2"));
    }

    #[test]
    fn region_gate_host() {
        assert_eq!(trae_agent_host("cn"), AGENT_HOST_CN);
        assert_eq!(trae_agent_host("SG"), AGENT_HOST_SG);
        assert_eq!(trae_agent_host("unknown-xx"), AGENT_HOST_CN, "未知/空白一律回退 CN（region_from_id 语义）");
    }
}
