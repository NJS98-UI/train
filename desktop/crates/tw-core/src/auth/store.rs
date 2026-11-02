//! TraeAuthStore / QoderAuthStore 的 Rust 移植（依据 `.monkeycode/analysis/auth-stores.md` §3/§4，
//! 已逐函数核对 Kotlin 源码 `TraeAuthStore.kt` / `QoderAuthStore.kt`）。
//!
//! 桌面版落盘方案（analysis §7.2 + design.md §Data Models）：
//! - `accounts.trae.json`  ↔ prefs `trae_accounts`(索引数组) + `trae_acc_<uid>_access/_refresh`(密文) + `trae_active_uid`
//! - `accounts.qoder.json` ↔ prefs `qoder_accounts` + `q_acc_<uid>_pat/_jt/_jrt/_fp` + `q_active_uid`
//!   （索引条目与 token **合并为单条 entry**：桌面版无 AndroidKeyStore，token 明文随条目存、文件 0600；
//!   JSON 字段名与 Android 导出格式逐字一致（`region`/`expiresAt`/`accessToken`…），导出文件可直接当存储读。）
//! - `trae_region` 全局区域 → `config.json → traeRegion`（经 `crate::config`）。
//! - `q_machine_id` 属 crypto 层：`QoderAuthStore.machineId` **不在本模块移植**。
//!
//! Android 历史包袱不移植（analysis §7 关键移植决策）：
//! - `migrateLegacyIfNeeded`（旧版单账号 5 key 迁移，含 `migrated` 进程内标志）：桌面版无旧数据，跳过。
//! - `repairMislabeledRegions`（每次 list/activeUid 按 uid 长度覆写区域）：会无差别覆盖显式区域，不移植；
//!   仅保留纯函数 [`TraeAuthStore::infer_region_from_uid`] 供读 Android 老备份参考（正常写入路径以显式 region 为准）。
//! - AES-GCM/KeyStore encrypt/decrypt：删除；"解密失败" 语义并入 "字段缺失/空白"。
//!
//! authFailed[uid] 标记（analysis §5）：Kotlin 侧是 `MainActivity.authFailedUid`（页面内存态、**按 uid 记**、
//! 从不落盘——存储层无 persisted rejected 标志）。Rust 同构：store 实例内 `Mutex<Option<String>>`，
//! 提供 mark/clear/查询三行为（对应 MainActivity 的三个维护点）。换号"最多 2 个账号"（同账号自救 + 1 候选）
//! 在 `LocalFakeServer.streamWithFailover`/`Gateway` **转发层**，不属存储层；本层只移植候选选择纯函数
//! [`TraeAuthStore::pick_failover_candidate`]。

use crate::auth::model::{
    QODER_MODE_CN, QoderAccount, QoderAccountMeta, QoderAccountView, QoderFingerprint, QoderSnapshot,
    TraeAccount, TraeAccountMeta, TraeAccountView, TraeSnapshot,
};
use crate::storage::DataStore;
use crate::types::region::CN as REGION_CN;
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::{Arc, OnceLock};

/// prefs `traework2api_prefs` / `qoder2api_prefs` 账号相关 key 的合并文件（design.md §Data Models）。
pub const TRAE_FILE: &str = "accounts.trae.json";
pub const QODER_FILE: &str = "accounts.qoder.json";

/// 对应 `TraeAuthStore.EXPORT_FORMAT`
pub const TRAE_EXPORT_FORMAT: &str = "traework2api-accounts";
/// 对应 `QoderAuthStore.EXPORT_FORMAT`
pub const QODER_EXPORT_FORMAT: &str = "traework2api-qoder-accounts";
/// 导出/交换文件外壳版本（与 Android `version:1` 逐字兼容）
pub const EXPORT_VERSION: u32 = 1;
/// 桌面存储文件版本（analysis §7.2）
const STORAGE_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// org.json 宽容取值 helpers（optString/optLong/optBoolean/jsonToStringList 语义）
// ---------------------------------------------------------------------------

/// `JSONObject.optString(key, def)`：string 原样；number 转字符串；缺失/null/其他类型→def。
fn opt_str(o: &Value, key: &str, def: &str) -> String {
    match o.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => def.to_string(),
    }
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// `JSONObject.optLong(key, def)`：数字直取；数字字符串可转则转；否则 def。
fn opt_long(o: &Value, key: &str, def: i64) -> i64 {
    match o.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(def),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(def),
        _ => def,
    }
}

/// `JSONObject.optBoolean(key, def)`。
fn opt_bool(o: &Value, key: &str, def: bool) -> bool {
    match o.get(key) {
        Some(Value::Bool(b)) => *b,
        _ => def,
    }
}

/// 对应 `QoderAuthStore.jsonToStringList`（private）：数组（逐条转字符串、空白丢弃）或逗号分隔串，两种形态都吃。
fn json_to_string_list(raw: Option<&Value>) -> Vec<String> {
    match raw {
        Some(Value::Array(arr)) => {
            arr.iter().map(|v| opt_str_of(v)).filter(|s| !is_blank(s)).collect()
        }
        Some(Value::String(s)) => s.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// 任意 Value → 字符串（`optString` 对对象/数组返回 ""）。
fn opt_str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// `activeUid` 读侧：string→Some（含空串，由 active_uid 回退逻辑消化）；null/缺失→None。
fn opt_active(root: &Value) -> Option<String> {
    root.get("activeUid").and_then(|v| v.as_str()).map(str::to_string)
}

// ===========================================================================
// TraeAuthStore
// ===========================================================================

struct TraeFileState {
    active: Option<String>,
    accounts: Vec<TraeAccount>,
}

/// 对应 Kotlin `object TraeAuthStore`。
/// 生产走 [`TraeAuthStore::global`]（绑定全局 `crate::storage::store()`）；测试用 [`TraeAuthStore::with_store`] 注入临时目录。
pub struct TraeAuthStore {
    data: Arc<DataStore>,
    /// 文件级读-改-写锁：替代 Android SharedPreferences 的内部原子性（整文件重写必须串行）。
    io: Mutex<()>,
    /// 对应 `MainActivity.authFailedUid`（运行期、按 uid、不落盘）。
    auth_failed: Mutex<Option<String>>,
}

impl Default for TraeAuthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TraeAuthStore {
    pub fn global() -> Arc<TraeAuthStore> {
        static STORE: OnceLock<Arc<TraeAuthStore>> = OnceLock::new();
        STORE.get_or_init(|| Arc::new(TraeAuthStore::new())).clone()
    }

    pub fn new() -> Self {
        Self::with_store(crate::storage::store())
    }

    pub fn with_store(data: Arc<DataStore>) -> Self {
        Self { data, io: Mutex::new(()), auth_failed: Mutex::new(None) }
    }

    // ---- 文件读写（readIndex/writeMeta/toJson 的桌面等价物） ----

    /// 整文件损坏/缺失 → 空态（对齐 `readIndex`"整个 JSON 解析异常 → 空列表"）。
    fn read_state(&self) -> TraeFileState {
        let root = self.data.load_json_opt(TRAE_FILE).unwrap_or(Value::Null);
        let arr: Vec<Value> = match root.get("accounts") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let accounts = arr
            .iter()
            .filter_map(|o| {
                if !o.is_object() {
                    return None;
                }
                let uid = opt_str(o, "uid", "");
                if is_blank(&uid) {
                    return None; // readIndex：uid 空白丢弃该条
                }
                Some(TraeAccount {
                    uid,
                    nickname: opt_str(o, "nickname", ""),
                    region_id: opt_str(o, "region", REGION_CN),
                    expires_at: opt_long(o, "expiresAt", 0),
                    access_token: opt_str(o, "accessToken", ""),
                    refresh_token: opt_str(o, "refreshToken", ""),
                })
            })
            .collect();
        TraeFileState { active: opt_active(&root), accounts }
    }

    fn write_state(&self, st: &TraeFileState) {
        let accounts: Vec<Value> = st
            .accounts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "uid": a.uid,
                    "nickname": a.nickname,
                    "region": a.region_id,
                    "expiresAt": a.expires_at,
                    "accessToken": a.access_token,
                    "refreshToken": a.refresh_token,
                })
            })
            .collect();
        let shell = serde_json::json!({
            "format": TRAE_EXPORT_FORMAT,
            "version": STORAGE_VERSION,
            "activeUid": st.active.clone().unwrap_or_default(),
            "accounts": accounts,
        });
        let body = serde_json::to_string_pretty(&shell).unwrap_or_else(|_| "{}".into());
        let _ = self.data.save_text_private(TRAE_FILE, &body);
    }

    /// 读-改-写事务；闭包返回 (结果, 是否需落盘)。
    fn transact<R>(&self, f: impl FnOnce(&mut TraeFileState) -> (R, bool)) -> R {
        let _g = self.io.lock();
        let mut st = self.read_state();
        let (r, dirty) = f(&mut st);
        if dirty {
            self.write_state(&st);
        }
        r
    }

    fn peek<R>(&self, f: impl FnOnce(&TraeFileState) -> R) -> R {
        let _g = self.io.lock();
        f(&self.read_state())
    }

    /// 对应 `TraeAuthStore.list(context)`（migrate/repair 前两步在桌面版省略，见模块头注释）。
    pub fn list(&self) -> Vec<TraeAccountMeta> {
        self.peek(|st| st.accounts.iter().map(TraeAccount::to_meta).collect())
    }

    /// 对应 `TraeAuthStore.activeUid(context)`：索引空→None；存的值必须仍在索引里，否则回退第一个。
    pub fn active_uid(&self) -> Option<String> {
        self.peek(|st| {
            if st.accounts.is_empty() {
                return None;
            }
            st.active
                .as_deref()
                .filter(|s| st.accounts.iter().any(|a| &a.uid == *s))
                .map(str::to_string)
                .or_else(|| Some(st.accounts[0].uid.clone()))
        })
    }

    /// 对应 `TraeAuthStore.load(context)` = activeUid → loadByUid。
    pub fn load(&self) -> Option<TraeAccount> {
        self.active_uid().and_then(|uid| self.load_by_uid(&uid))
    }

    /// 对应 `TraeAuthStore.loadByUid(context, uid)`：
    /// 条目不存在→None；accessToken 缺失/空白→None（硬性条件）；refreshToken 缺失→空串（软条件）；
    /// expiresAt/nickname/regionId 随条目（Kotlin 里来自 meta，语义一致）。
    pub fn load_by_uid(&self, uid: &str) -> Option<TraeAccount> {
        self.peek(|st| {
            st.accounts.iter().find(|a| a.uid == uid).cloned().filter(|a| !is_blank(&a.access_token))
        })
    }

    /// 对应 `TraeAuthStore.save(context, cred)`：写 access+refresh → writeMeta → setActive。
    /// 同 uid 重登=原位覆盖（不产生重复条目），不影响别的账号，顺带成为当前账号。
    pub fn save(&self, cred: &TraeAccount) {
        self.transact(|st| {
            Self::upsert(st, cred.clone());
            st.active = Some(cred.uid.clone());
            ((), true)
        });
    }

    /// 对应 `TraeAuthStore.setActive(context, uid)`：uid 不在索引 → 忽略。
    pub fn set_active(&self, uid: &str) {
        self.transact(|st| {
            if st.accounts.iter().any(|a| a.uid == uid) {
                st.active = Some(uid.to_string());
                ((), true)
            } else {
                ((), false)
            }
        });
    }

    /// 对应 `TraeAuthStore.updateTokens(context, cred)`：**只写两枚 token，不改 active、不写 meta**
    /// （expiresAt 更新需调用方另用 [`update_meta`] 成对调用——refreshLocked 正是这么做的）。
    /// 账号不存在则忽略（不新建）。
    pub fn update_tokens(&self, cred: &TraeAccount) {
        let uid = cred.uid.clone();
        let (access, refresh) = (cred.access_token.clone(), cred.refresh_token.clone());
        self.transact(move |st| {
            match st.accounts.iter_mut().find(|a| a.uid == uid) {
                Some(a) => {
                    a.access_token = access;
                    a.refresh_token = refresh;
                    ((), true)
                }
                None => ((), false),
            }
        });
    }

    /// 对应 `TraeAuthStore.updateMeta(context, meta)`：账号不存在则忽略；不碰 token。
    pub fn update_meta(&self, meta: &TraeAccountMeta) {
        let uid = meta.uid.clone();
        let (nick, reg, exp) = (meta.nickname.clone(), meta.region_id.clone(), meta.expires_at);
        self.transact(move |st| {
            match st.accounts.iter_mut().find(|a| a.uid == uid) {
                Some(a) => {
                    a.nickname = nick;
                    a.region_id = reg;
                    a.expires_at = exp;
                    ((), true)
                }
                None => ((), false),
            }
        });
    }

    /// 对应 `TraeAuthStore.remove(context, uid)`：索引剔除该条 + 删两枚 token（同一 entry 内）；
    /// **若删的是当前账号**：active 写剩下第一个，删空写空串（此处序列化为 null，读取语义等价）。
    pub fn remove(&self, uid: &str) {
        let target = uid.to_string();
        self.transact(move |st| {
            st.accounts.retain(|a| a.uid != target);
            if st.active.as_deref() == Some(target.as_str()) {
                st.active = st.accounts.first().map(|a| a.uid.clone());
            }
            ((), true)
        });
    }

    /// 登录态判定（analysis §3.3 的硬条件）。
    pub fn is_logged_in(&self) -> bool {
        self.load().is_some()
    }

    /// 登出：删除当前账号（Kotlin 无独立 logout API，App 里登出＝移除当前账号）。
    /// 同时清掉它的 authFailed 标记（对应成功清除标记的方向）。
    pub fn logout(&self) {
        if let Some(uid) = self.active_uid() {
            self.remove(&uid);
            self.clear_auth_failed(&uid);
        }
    }

    /// 对应 `TraeAuthStore.pickFailoverCandidate(metas, failedUid)`（internal 纯函数）：
    /// 剔除失败号后**优先选受支持区域**（仅 CN，SG 冻结未开发）的下一个；没有受支持的就退而换任意别的；
    /// 只剩一个账号时 None（不换号）。"最多尝试 2 个账号"的循环在网关层，候选选择只给 1 个。
    pub fn pick_failover_candidate(metas: &[TraeAccountMeta], failed_uid: &str) -> Option<TraeAccountMeta> {
        let first_supported = metas.iter().find(|m| m.uid != failed_uid && m.is_supported());
        let first_other = metas.iter().find(|m| m.uid != failed_uid);
        first_supported.or(first_other).cloned()
    }

    /// 对应 `TraeAuthStore.syncRegionToActiveAccount(context)`：
    /// 当前账号自带 region 才是权威（手动选的 `trae_region` 会在切号后残留错值）；无账号时**什么都不动**
    /// （保留用户手动选的区域供登录用）。启动与切号/故障转移后必须调用。
    pub fn sync_region_to_active_account(&self) {
        let Some(meta) = self.active_uid().and_then(|uid| self.peek(|st| st.accounts.iter().find(|a| a.uid == uid).map(TraeAccount::to_meta))) else {
            return;
        };
        crate::config::update(|c| c.trae_region = meta.region().to_string());
    }

    /// 对应 `TraeAuthApi.expiredWithin(expiresAt, skewSec)`（唯一"要不要续"判据，§2.1）：
    /// `expires_at <= 0`（含未知）→ true；`now_sec + skew >= expires_at` → true。单位=**秒**，勿与 Qoder 毫秒交叉传。
    pub fn expired_within(expires_at: i64, skew_sec: i64) -> bool {
        if expires_at <= 0 {
            return true;
        }
        crate::auth::model::now_sec() + skew_sec >= expires_at
    }

    /// 对应 `TraeAuthStore.inferRegionFromUid(uid)`（internal）：数字长度 ≥19 → SG，否则 CN。
    /// 桌面版**不再在写入路径调用**（repair 不移植），仅保留供从 Android 迁移时判区域。
    pub fn infer_region_from_uid(uid: &str) -> &'static str {
        if uid.chars().filter(|c| c.is_ascii_digit()).count() >= 19 {
            crate::types::region::SG
        } else {
            REGION_CN
        }
    }

    /// 对应 `TraeAuthStore.exportJson(context)`（⚠️ 含明文 token，等价账号钥匙：只走本地文件/剪贴板，绝不联网、绝不写日志）。
    /// 外壳 {format,version:1,activeUid,accounts:[{uid,nickname,region,expiresAt,accessToken,refreshToken}]}；
    /// loadByUid 不成功的账号（缺 access）跳过；activeUid 缺省空串。
    pub fn export_json(&self) -> String {
        let entries: Vec<TraeAccount> = self.peek(|st| st.accounts.iter().filter(|a| !is_blank(&a.access_token)).cloned().collect());
        let accounts: Vec<Value> = entries
            .iter()
            .map(|a| {
                serde_json::json!({
                    "uid": a.uid,
                    "nickname": a.nickname,
                    "region": a.region_id,
                    "expiresAt": a.expires_at,
                    "accessToken": a.access_token,
                    "refreshToken": a.refresh_token,
                })
            })
            .collect();
        let active = self.active_uid().unwrap_or_default();
        let shell = serde_json::json!({
            "format": TRAE_EXPORT_FORMAT,
            "version": EXPORT_VERSION,
            "activeUid": active,
            "accounts": accounts,
        });
        serde_json::to_string_pretty(&shell).unwrap_or_else(|_| "{}".into())
    }

    /// 对应 `TraeAuthStore.importJson(context, json)`：按 uid 合并写回（存在覆盖、新增追加），**不 setActive**；
    /// 兼容顶层数组老格式；逐条：uid 空白跳过、accessToken 空白（optString 缺省 ""）跳过、
    /// refreshToken 缺省 ""、expiresAt 缺省 0、region 缺省 "cn"。
    /// `imported==0` → Err("JSON 里没有可导入的账号（缺 uid 或 accessToken）")；
    /// 外壳带 activeUid 且命中导入结果才 setActive。返回成功导入数。
    pub fn import_json(&self, json: &str) -> Result<usize, String> {
        let root: Value = serde_json::from_str(json.trim())
            .map_err(|_| "不是有效的账号 JSON".to_string())?;
        let accounts: Vec<Value> = match &root {
            Value::Object(_) => match root.get("accounts") {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            },
            Value::Array(a) => a.clone(),
            _ => return Err("不是有效的账号 JSON".to_string()),
        };
        let mut imported = 0usize;
        for o in accounts.iter().filter(|v| v.is_object()) {
            let uid = opt_str(o, "uid", "");
            if is_blank(&uid) {
                continue;
            }
            let access = opt_str(o, "accessToken", "");
            if is_blank(&access) {
                continue;
            }
            let cred = TraeAccount {
                uid,
                nickname: opt_str(o, "nickname", ""),
                region_id: opt_str(o, "region", REGION_CN),
                expires_at: opt_long(o, "expiresAt", 0),
                access_token: access,
                refresh_token: opt_str(o, "refreshToken", ""),
            };
            // importOne（private）：写 token+meta，不动当前选中账号
            self.transact(|st| {
                Self::upsert(st, cred.clone());
                ((), true)
            });
            imported += 1;
        }
        if imported == 0 {
            return Err("JSON 里没有可导入的账号（缺 uid 或 accessToken）".to_string());
        }
        if let Some(active) = root.get("activeUid").and_then(|v| v.as_str()).filter(|s| !is_blank(s)) {
            let hit = self.peek(|st| st.accounts.iter().any(|a| a.uid == active));
            if hit {
                self.set_active(active);
            }
        }
        Ok(imported)
    }

    /// 对应 `writeMeta`：按 uid **原位替换**，不存在则**追加末尾**——uid 是主键，顺序即列表顺序。
    fn upsert(st: &mut TraeFileState, cred: TraeAccount) {
        match st.accounts.iter().position(|a| a.uid == cred.uid) {
            Some(i) => st.accounts[i] = cred,
            None => st.accounts.push(cred),
        }
    }

    // ---- authFailed[uid] 标记（对应 MainActivity 维护点，运行期、不落盘） ----

    /// 记"该账号被服务端拒"（`MainActivity` 的 `authFailedUid = cred.uid`，按 uid 记绝不记全局 bool）。
    pub fn mark_auth_failed(&self, uid: &str) {
        *self.auth_failed.lock() = Some(uid.to_string());
    }

    /// 成功时清标记：仅当标记就是该 uid 才清（`if (authFailedUid == uid) authFailedUid = null`）。
    pub fn clear_auth_failed(&self, uid: &str) {
        let mut g = self.auth_failed.lock();
        if g.as_deref() == Some(uid) {
            *g = None;
        }
    }

    /// 当前被标记的 uid（快照/状态胶囊数据源）。
    pub fn auth_failed_uid(&self) -> Option<String> {
        self.auth_failed.lock().clone()
    }

    /// 对应 `MainActivity.authFailedFor(uid)` 第一来源：`authFailedUid == uid`；null/空白 uid 恒 false。
    /// 第二来源（AccountCredits 条目 error=="需重新登录"）属 credits 层，判定在 IPC 层合成。
    pub fn auth_failed_for(&self, uid: Option<&str>) -> bool {
        match uid {
            Some(u) if !is_blank(u) => self.auth_failed.lock().as_deref() == Some(u),
            _ => false,
        }
    }

    /// 供 IPC 层用的快照（`trae_state`：logged/activeUid/authFailedUid/regionId）。
    /// **绝不包含 token**（红线同 Kotlin："索引里绝不出现 token"）。
    pub fn snapshot(&self) -> Value {
        let (accounts, active) = self.peek(|st| {
            let views: Vec<TraeAccountView> = st
                .accounts
                .iter()
                .map(|a| TraeAccountView {
                    meta: a.to_meta(),
                    has_access_token: !is_blank(&a.access_token),
                    has_refresh_token: !is_blank(&a.refresh_token),
                })
                .collect();
            let active = if st.accounts.is_empty() {
                None
            } else {
                st.active
                    .as_deref()
                    .filter(|s| st.accounts.iter().any(|a| &a.uid == *s))
                    .map(str::to_string)
                    .or_else(|| Some(st.accounts[0].uid.clone()))
            };
            (views, active)
        });
        // 生效区域：从**本实例的 DataStore** 读 config.json（保持与注入目录一致），缺省 cn
        let cfg = self.data.load_json(crate::config::FILE, Value::Null);
        let region_id = match cfg.get("traeRegion").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => REGION_CN.to_string(),
        };
        let snap = TraeSnapshot {
            accounts,
            active_uid: active,
            auth_failed_uid: self.auth_failed_uid(),
            region_id,
        };
        serde_json::to_value(snap).expect("snapshot 可序列化")
    }
}

// ===========================================================================
// QoderAuthStore
// ===========================================================================

struct QoderFileState {
    active: Option<String>,
    accounts: Vec<QoderAccount>,
}

/// 对应 Kotlin `object QoderAuthStore`。
/// `machineId`（`q_machine_id`）属 crypto 层，不在本模块。
pub struct QoderAuthStore {
    data: Arc<DataStore>,
    io: Mutex<()>,
    auth_failed: Mutex<Option<String>>,
}

impl Default for QoderAuthStore {
    fn default() -> Self {
        Self::new()
    }
}

impl QoderAuthStore {
    pub fn global() -> Arc<QoderAuthStore> {
        static STORE: OnceLock<Arc<QoderAuthStore>> = OnceLock::new();
        STORE.get_or_init(|| Arc::new(QoderAuthStore::new())).clone()
    }

    pub fn new() -> Self {
        Self::with_store(crate::storage::store())
    }

    pub fn with_store(data: Arc<DataStore>) -> Self {
        Self { data, io: Mutex::new(()), auth_failed: Mutex::new(None) }
    }

    fn read_state(&self) -> QoderFileState {
        let root = self.data.load_json_opt(QODER_FILE).unwrap_or(Value::Null);
        let arr: Vec<Value> = match root.get("accounts") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let accounts = arr.iter().filter_map(|o| QoderAccount::from_loose(o)).collect();
        QoderFileState { active: opt_active(&root), accounts }
    }

    fn write_state(&self, st: &QoderFileState) {
        // 存储形态（analysis §7.2）：fingerprint 以全名键嵌在 entry 里（derived Serialize 天然输出）
        let accounts: Vec<Value> = st.accounts.iter().map(|a| serde_json::to_value(a).unwrap_or(Value::Null)).collect();
        let shell = serde_json::json!({
            "format": QODER_EXPORT_FORMAT,
            "version": STORAGE_VERSION,
            "activeUid": st.active.clone().unwrap_or_default(),
            "accounts": accounts,
        });
        let body = serde_json::to_string_pretty(&shell).unwrap_or_else(|_| "{}".into());
        let _ = self.data.save_text_private(QODER_FILE, &body);
    }

    fn transact<R>(&self, f: impl FnOnce(&mut QoderFileState) -> (R, bool)) -> R {
        let _g = self.io.lock();
        let mut st = self.read_state();
        let (r, dirty) = f(&mut st);
        if dirty {
            self.write_state(&st);
        }
        r
    }

    fn peek<R>(&self, f: impl FnOnce(&QoderFileState) -> R) -> R {
        let _g = self.io.lock();
        f(&self.read_state())
    }

    /// 对应 `QoderAuthStore.list(context)`。
    pub fn list(&self) -> Vec<QoderAccountMeta> {
        self.peek(|st| st.accounts.iter().map(QoderAccount::to_meta).collect())
    }

    /// 对应 `QoderAuthStore.activeUid(context)`：索引空→None；存的值不在索引 → 回退第一个。
    pub fn active_uid(&self) -> Option<String> {
        self.peek(|st| {
            if st.accounts.is_empty() {
                return None;
            }
            st.active
                .as_deref()
                .filter(|s| st.accounts.iter().any(|a| &a.uid == *s))
                .map(str::to_string)
                .or_else(|| Some(st.accounts[0].uid.clone()))
        })
    }

    /// 对应 `QoderAuthStore.setActive(context, uid)`：不在索引则忽略。
    pub fn set_active(&self, uid: &str) {
        let target = uid.to_string();
        self.transact(move |st| {
            if st.accounts.iter().any(|a| a.uid == target) {
                st.active = Some(target.clone());
                ((), true)
            } else {
                ((), false)
            }
        });
    }

    /// 对应 `QoderAuthStore.load(context)`。
    pub fn load(&self) -> Option<QoderAccount> {
        self.active_uid().and_then(|uid| self.load_by_uid(&uid))
    }

    /// 对应 `QoderAuthStore.loadByUid(context, uid)`：
    /// 条目不存在→None；**pat 与 jobToken 双空白**→None（硬条件）；其余缺失一律空串（软条件）。
    pub fn load_by_uid(&self, uid: &str) -> Option<QoderAccount> {
        self.peek(|st| {
            st.accounts.iter().find(|a| a.uid == uid).cloned().filter(|a| !(is_blank(&a.pat) && is_blank(&a.job_token)))
        })
    }

    /// 对应 `QoderAuthStore.save(context, cred)`：写三类凭据 → writeMeta → setActive。
    /// 传入值的 `fingerprint` 字段被忽略（指纹只能经 [`set_fingerprint`] 走，同 Kotlin：QCred 无该字段）。
    pub fn save(&self, cred: &QoderAccount) {
        let cred = cred.clone();
        self.transact(move |st| {
            let uid = cred.uid.clone();
            let keep_fp = st.accounts.iter().find(|a| a.uid == uid).and_then(|a| a.fingerprint.clone());
            let mut entry = cred;
            entry.fingerprint = keep_fp;
            Self::upsert(st, entry.clone());
            st.active = Some(entry.uid);
            ((), true)
        });
    }

    /// 对应 `QoderAuthStore.updateTokens(context, cred)`：与 Trae 版的差异——**顺带 writeMeta**
    /// （expiresAt/组织材料随凭据一起更新），仍不动 active、不动指纹；账号不存在则忽略。
    pub fn update_tokens(&self, cred: &QoderAccount) {
        let cred = cred.clone();
        self.transact(move |st| {
            let uid = cred.uid.clone();
            match st.accounts.iter().position(|a| a.uid == uid) {
                Some(i) => {
                    let keep_fp = st.accounts[i].fingerprint.clone();
                    let mut entry = cred;
                    entry.fingerprint = keep_fp;
                    st.accounts[i] = entry;
                    ((), true)
                }
                None => ((), false),
            }
        });
    }

    /// 对应 `QoderAuthStore.remove(context, uid)`：删三枚凭据 + 索引剔除 + active 回退。
    /// 修正版：Android 源码漏删 `q_acc_<uid>_fp`（analysis §4.5 标注"Rust 移植时可顺手修正"）——此处一并清除指纹。
    pub fn remove(&self, uid: &str) {
        let target = uid.to_string();
        self.transact(move |st| {
            st.accounts.retain(|a| a.uid != target);
            if st.active.as_deref() == Some(target.as_str()) {
                st.active = st.accounts.first().map(|a| a.uid.clone());
            }
            ((), true)
        });
    }

    /// 登录态判定（load 非空即"可登录态"）。
    pub fn is_logged_in(&self) -> bool {
        self.load().is_some()
    }

    /// 登出：删除当前账号（同 Trae：Kotlin 里"登出"即移除账号）。
    pub fn logout(&self) {
        if let Some(uid) = self.active_uid() {
            self.remove(&uid);
            self.clear_auth_failed(&uid);
        }
    }

    /// 对应 `QoderAuthStore.fingerprint(context, uid)`：无/解不开→None。
    pub fn fingerprint(&self, uid: &str) -> Option<QoderFingerprint> {
        self.peek(|st| st.accounts.iter().find(|a| a.uid == uid).and_then(|a| a.fingerprint.clone()))
    }

    /// 对应 `QoderAuthStore.setFingerprint(context, uid, fp)`：账号不存在则忽略（不新建）。
    pub fn set_fingerprint(&self, uid: &str, fp: &QoderFingerprint) {
        let target = uid.to_string();
        let fp = fp.clone();
        self.transact(move |st| match st.accounts.iter_mut().find(|a| a.uid == target) {
            Some(a) => {
                a.fingerprint = Some(fp.clone());
                ((), true)
            }
            None => ((), false),
        });
    }

    /// 对应 `QoderAuthStore.clearFingerprint(context, uid)`。
    pub fn clear_fingerprint(&self, uid: &str) {
        let target = uid.to_string();
        self.transact(move |st| match st.accounts.iter_mut().find(|a| a.uid == target) {
            Some(a) => {
                a.fingerprint = None;
                ((), true)
            }
            None => ((), false),
        });
    }

    /// 对应 `QoderAuthApi.validJobTokenFor` 第 1 步的新鲜判据：`now_ms + skew_ms < expires_at_ms`。
    /// 单位=**毫秒**（Qoder 域），与 Trae 秒域禁止交叉（analysis §6）。
    pub fn job_token_fresh(expires_at_ms: i64, skew_ms: i64) -> bool {
        crate::auth::model::now_ms() + skew_ms < expires_at_ms
    }

    /// 对应 §4.4 第 1 步"过期/临期"半句（`now + skewMillis < expiresAt` 取反）。单位=**毫秒**。
    pub fn qoder_expired_within(expires_at_ms: i64, skew_ms: i64) -> bool {
        !(crate::auth::model::now_ms() + skew_ms < expires_at_ms)
    }
    /// 对应 `QoderAuthStore.exportJson(context)`（⚠️ 含明文 PAT/jobToken/jrt + 指纹）。
    /// loadByUid 不成功（pat+jt 双空）的条目跳过；`fingerprint` 与 Android 导出逐字兼容 = 短键 JSON **字符串**
    /// （无指纹为空串），桌面自己再导入时两种形态（字符串/全名对象）都吃。
    pub fn export_json(&self) -> String {
        let entries = self.peek(|st| st.accounts.clone());
        let accounts: Vec<Value> = entries
            .iter()
            .filter(|a| !(is_blank(&a.pat) && is_blank(&a.job_token)))
            .map(|a| {
                serde_json::json!({
                    "uid": a.uid,
                    "name": a.name,
                    "email": a.email,
                    "mode": a.mode,
                    "expiresAt": a.expires_at,
                    "organizationId": a.organization_id,
                    "organizationName": a.organization_name,
                    "organizationTags": a.organization_tags,
                    "dataPolicyAgreed": a.data_policy_agreed,
                    "isDataPolicyModifiable": a.is_data_policy_modifiable,
                    "pat": a.pat,
                    "jobToken": a.job_token,
                    "jobRefreshToken": a.job_refresh_token,
                    "fingerprint": a.fingerprint.as_ref().map(QoderFingerprint::encode_short_json).unwrap_or_default(),
                })
            })
            .collect();
        let active = self.active_uid().unwrap_or_default();
        let shell = serde_json::json!({
            "format": QODER_EXPORT_FORMAT,
            "version": EXPORT_VERSION,
            "activeUid": active,
            "accounts": accounts,
        });
        serde_json::to_string_pretty(&shell).unwrap_or_else(|_| "{}".into())
    }

    /// 对应 `QoderAuthStore.importJson(context, json)`：按 uid 合并、不 setActive（除非外壳 activeUid 命中）；
    /// 逐条：uid 空白跳过；**pat 与 jobToken 双空白**跳过；`fingerprint` 非空且可解才写入（老备份缺字段→当作本机派生）。
    /// `imported==0` → Err("JSON 里没有可导入的 Qoder 账号（缺 uid 或 pat）")。
    pub fn import_json(&self, json: &str) -> Result<usize, String> {
        let root: Value = serde_json::from_str(json.trim())
            .map_err(|_| "不是有效的 Qoder 账号 JSON".to_string())?;
        let accounts: Vec<Value> = match &root {
            Value::Object(_) => match root.get("accounts") {
                Some(Value::Array(a)) => a.clone(),
                _ => Vec::new(),
            },
            Value::Array(a) => a.clone(),
            _ => return Err("不是有效的 Qoder 账号 JSON".to_string()),
        };
        let mut imported = 0usize;
        for o in accounts.iter().filter(|v| v.is_object()) {
            let uid = opt_str(o, "uid", "");
            if is_blank(&uid) {
                continue;
            }
            let pat = opt_str(o, "pat", "");
            let jt = opt_str(o, "jobToken", "");
            if is_blank(&pat) && is_blank(&jt) {
                continue;
            }
            let mut cred = match QoderAccount::from_loose(o) {
                Some(c) => c,
                None => continue,
            };
            cred.uid = uid.clone();
            // from_loose 已解析 fingerprint（字符串=Android 短键 JSON / 对象=桌面全名）；
            // 缺失/空白 → None → upsert 时保留库内旧指纹（Kotlin：importOne 不碰 _fp key）。
            let target = uid.clone();
            self.transact(move |st| {
                let keep_fp = match &cred.fingerprint {
                    Some(_) => None, // 导入带来了新指纹 → 直接覆盖
                    None => st.accounts.iter().find(|a| a.uid == target).and_then(|a| a.fingerprint.clone()),
                };
                let mut entry = cred.clone();
                if entry.fingerprint.is_none() {
                    entry.fingerprint = keep_fp;
                }
                Self::upsert(st, entry);
                ((), true)
            });
            imported += 1;
        }
        if imported == 0 {
            return Err("JSON 里没有可导入的 Qoder 账号（缺 uid 或 pat）".to_string());
        }
        if let Some(active) = root.get("activeUid").and_then(|v| v.as_str()).filter(|s| !is_blank(s)) {
            let hit = self.peek(|st| st.accounts.iter().any(|a| a.uid == active));
            if hit {
                self.set_active(active);
            }
        }
        Ok(imported)
    }

    fn upsert(st: &mut QoderFileState, cred: QoderAccount) {
        match st.accounts.iter().position(|a| a.uid == cred.uid) {
            Some(i) => {
                let fp = st.accounts[i].fingerprint.clone();
                let mut cred = cred;
                if cred.fingerprint.is_none() {
                    cred.fingerprint = fp;
                }
                st.accounts[i] = cred;
            }
            None => st.accounts.push(cred),
        }
    }

    // ---- authFailed[uid] 标记（Qoder 侧与 Trae 同构；链路上游 §5：fetchCredits 吞异常无 lastAuthFailed 等价物） ----

    pub fn mark_auth_failed(&self, uid: &str) {
        *self.auth_failed.lock() = Some(uid.to_string());
    }

    pub fn clear_auth_failed(&self, uid: &str) {
        let mut g = self.auth_failed.lock();
        if g.as_deref() == Some(uid) {
            *g = None;
        }
    }

    pub fn auth_failed_uid(&self) -> Option<String> {
        self.auth_failed.lock().clone()
    }

    pub fn auth_failed_for(&self, uid: Option<&str>) -> bool {
        match uid {
            Some(u) if !is_blank(u) => self.auth_failed.lock().as_deref() == Some(u),
            _ => false,
        }
    }

    /// 供 IPC 层用的快照（qoder 状态：logged 列表 + current + authFailed）。**绝不包含 token/PAT/指纹**。
    pub fn snapshot(&self) -> Value {
        let snap = self.peek(|st| {
            let accounts: Vec<QoderAccountView> = st
                .accounts
                .iter()
                .map(|a| QoderAccountView {
                    has_pat: !is_blank(&a.pat),
                    has_job_token: !is_blank(&a.job_token),
                    has_job_refresh_token: !is_blank(&a.job_refresh_token),
                    chat_ready: a.chat_ready(),
                    has_fingerprint: a.fingerprint.is_some(),
                    meta: a.to_meta(),
                })
                .collect();
            let active = if st.accounts.is_empty() {
                None
            } else {
                st.active
                    .as_deref()
                    .filter(|s| st.accounts.iter().any(|a| &a.uid == *s))
                    .map(str::to_string)
                    .or_else(|| Some(st.accounts[0].uid.clone()))
            };
            QoderSnapshot { accounts, active_uid: active, auth_failed_uid: self.auth_failed_uid() }
        });
        serde_json::to_value(snap).expect("snapshot 可序列化")
    }
}

/// 桌面全名指纹对象（analysis §7.2 schema）→ QoderFingerprint；
/// `machineId` 或 `machineToken` trim 后空白 → None；其余字段缺失/空白回退默认值（与短键 decode 同语义）。
fn fingerprint_from_long_object(o: &Value) -> Option<QoderFingerprint> {
    let id = opt_str(o, "machineId", "").trim().to_string();
    let tk = opt_str(o, "machineToken", "").trim().to_string();
    if id.is_empty() || tk.is_empty() {
        return None;
    }
    let fb = |k: &str, d: &str| -> String {
        let s = opt_str(o, k, d);
        if is_blank(&s) {
            d.to_string()
        } else {
            s
        }
    };
    Some(QoderFingerprint {
        machine_id: id,
        machine_token: tk,
        machine_type: fb("machineType", "5"),
        machine_os: fb("machineOS", "aarch64_darwin"),
        hostname: fb("hostname", "localhost"),
        client_type: fb("clientType", "10"),
        version: fb("version", "1.1.69"),
    })
}

impl QoderAccount {
    /// 宽容反序列化 entry（tags 数组/逗号串两形态、expiresAt null/数字字符串、布尔缺省、mode 缺省 cn 原样保留）。
    /// 对应 Kotlin `readIndex` + `importJson` 逐条读取的 org.json optXxx 缺省语义。
    fn from_loose(o: &Value) -> Option<Self> {
        let uid = opt_str(o, "uid", "");
        if is_blank(&uid) {
            return None;
        }
        let fingerprint = match o.get("fingerprint") {
            Some(Value::String(s)) if !is_blank(s) => QoderFingerprint::decode_short_json(s),
            Some(Value::Object(_)) => fingerprint_from_long_object(o.get("fingerprint").unwrap()),
            _ => None,
        };
        Some(Self {
            uid,
            name: opt_str(o, "name", ""),
            email: opt_str(o, "email", ""),
            mode: opt_str(o, "mode", QODER_MODE_CN),
            expires_at: opt_long(o, "expiresAt", 0),
            organization_id: opt_str(o, "organizationId", ""),
            organization_name: opt_str(o, "organizationName", ""),
            organization_tags: json_to_string_list(o.get("organizationTags")),
            data_policy_agreed: opt_bool(o, "dataPolicyAgreed", false),
            is_data_policy_modifiable: opt_bool(o, "isDataPolicyModifiable", true),
            pat: opt_str(o, "pat", ""),
            job_token: opt_str(o, "jobToken", ""),
            job_refresh_token: opt_str(o, "jobRefreshToken", ""),
            fingerprint,
        })
    }
}

// ===========================================================================
// 单元测试（Kotlin selfCheck 逐条平移 + 存储语义。每个测试用独立临时目录注入，互不污染、可并行）
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn tmp_store(tag: &str) -> Arc<DataStore> {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tw-auth-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::remove_dir_all(&dir);
        Arc::new(DataStore::new(&dir))
    }

    fn trae(tag: &str) -> TraeAuthStore {
        TraeAuthStore::with_store(tmp_store(tag))
    }

    fn qoder(tag: &str) -> QoderAuthStore {
        QoderAuthStore::with_store(tmp_store(tag))
    }

    fn acc(uid: &str, nick: &str, region: &str, exp: i64, access: &str, refresh: &str) -> TraeAccount {
        TraeAccount {
            uid: uid.into(),
            nickname: nick.into(),
            region_id: region.into(),
            expires_at: exp,
            access_token: access.into(),
            refresh_token: refresh.into(),
        }
    }

    const CN_UID: &str = "3695871032827027";
    const SG_UID: &str = "7687189514112844821";

    // ---- TraeAuthStore.selfCheck 平移 ----

    /// Kotlin selfCheck：索引 JSON 往返无损 + 区域按 id 命中。
    #[test]
    fn trae_index_roundtrip_and_region_parse() {
        let s = trae("roundtrip");
        s.save(&acc(SG_UID, "intl-user", "sg", 1791341100, "at-s", "rt-s"));
        s.save(&acc(CN_UID, "cn-user", "cn", 0, "at-c", "rt-c"));
        let metas = s.list();
        assert_eq!(
            metas,
            vec![
                TraeAccountMeta { uid: SG_UID.into(), nickname: "intl-user".into(), region_id: "sg".into(), expires_at: 1791341100 },
                TraeAccountMeta { uid: CN_UID.into(), nickname: "cn-user".into(), region_id: "cn".into(), expires_at: 0 },
            ],
            "uid 是主键，写回原位替换不受重存顺序影响"
        );
        // 同目录重新打开 = 文件往返无损
        let s2 = TraeAuthStore::with_store(Arc::new(DataStore::new(s.data.dir())));
        assert_eq!(s2.list(), s.list());
        assert_eq!(metas[0].region(), "sg");
        assert_eq!(metas[1].region(), "cn");
    }

    /// Kotlin selfCheck：16 位 uid→CN、19 位→SG。
    #[test]
    fn trae_infer_region_from_uid_samples() {
        assert_eq!(TraeAuthStore::infer_region_from_uid(CN_UID), "cn");
        assert_eq!(TraeAuthStore::infer_region_from_uid(SG_UID), "sg");
        assert_eq!(TraeAuthStore::infer_region_from_uid("abc123"), "cn");
    }

    /// Kotlin selfCheck：failover 四场景（SG 挂→换 CN；只剩 SG→给 SG；单号→None；CN+CN2+SG 换 CN→给 CN2）。
    #[test]
    fn trae_failover_four_scenarios() {
        let sg = TraeAccountMeta { uid: SG_UID.into(), nickname: "intl".into(), region_id: "sg".into(), expires_at: 0 };
        let cn = TraeAccountMeta { uid: CN_UID.into(), nickname: "cn".into(), region_id: "cn".into(), expires_at: 0 };
        let cn2 = TraeAccountMeta { uid: "3695871032827028".into(), nickname: "cn2".into(), region_id: "cn".into(), expires_at: 0 };
        assert_eq!(TraeAuthStore::pick_failover_candidate(&[sg.clone(), cn.clone()], SG_UID), Some(cn.clone()));
        assert_eq!(TraeAuthStore::pick_failover_candidate(&[cn.clone(), sg.clone()], CN_UID), Some(sg.clone()));
        assert_eq!(TraeAuthStore::pick_failover_candidate(&[cn.clone()], CN_UID), None);
        assert_eq!(TraeAuthStore::pick_failover_candidate(&[cn.clone(), cn2.clone(), sg.clone()], CN_UID), Some(cn2));
    }

    /// TraeAuthApi.selfCheck：expiredWithin 边界（exp=0/负/已过→判过期；now+60 在 skew=3600 内→续；now+skew+60→不续）。
    #[test]
    fn trae_expired_within_boundaries() {
        let now = crate::auth::model::now_sec();
        assert!(TraeAuthStore::expired_within(0, 3600));
        assert!(TraeAuthStore::expired_within(-5, 3600));
        assert!(TraeAuthStore::expired_within(now - 1, 3600));
        assert!(TraeAuthStore::expired_within(now + 60, 3600));
        assert!(!TraeAuthStore::expired_within(now + 3600 + 60, 3600));
    }

    /// Qoder 域判据（validJobTokenFor 第 1 步取反）：毫秒 skew。
    #[test]
    fn qoder_job_token_fresh_millis_domain() {
        let now = crate::auth::model::now_ms();
        assert!(!QoderAuthStore::job_token_fresh(0, 60_000));
        assert!(!QoderAuthStore::job_token_fresh(now + 30_000, 60_000));
        assert!(QoderAuthStore::job_token_fresh(now + 360_000, 60_000));
        assert!(QoderAuthStore::qoder_expired_within(0, 60_000));
        assert!(!QoderAuthStore::qoder_expired_within(now + 360_000, 60_000));
    }

    // ---- current 指针 / 增删 / 更新分离 ----

    #[test]
    fn trae_current_pointer_and_save_overwrite() {
        let s = trae("current");
        s.save(&acc("a", "A", "cn", 1, "at-a", "rt-a"));
        s.save(&acc("b", "B", "cn", 2, "at-b", "rt-b"));
        assert_eq!(s.active_uid(), Some("b".into()));
        s.set_active("a");
        assert_eq!(s.active_uid(), Some("a".into()));
        s.set_active("ghost"); // 不在索引 → 忽略
        assert_eq!(s.active_uid(), Some("a".into()));
        // 同 uid 重登 = 原位覆盖，不重复、不动别的号
        s.save(&acc("a", "A2", "cn", 9, "at-a-new", "rt-a-new"));
        let metas = s.list();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].uid, "a");
        assert_eq!(metas[0].nickname, "A2");
        assert_eq!(s.active_uid(), Some("a".into()));
    }

    #[test]
    fn trae_remove_fallback_rules() {
        let s = trae("remove");
        s.save(&acc("a", "A", "cn", 1, "at-a", "rt-a"));
        s.save(&acc("b", "B", "cn", 2, "at-b", "rt-b"));
        s.save(&acc("c", "C", "cn", 3, "at-c", "rt-c"));
        // 删非当前号（当前=c 由最后 save 决定）：不碰 active
        s.remove("a");
        assert_eq!(s.active_uid(), Some("c".into()));
        // 删当前号：active → 剩下的第一个（b）
        s.remove("c");
        assert_eq!(s.active_uid(), Some("b".into()));
        // 删空：active 落空 → None（Kotlin 写空串，读取语义等价）
        s.remove("b");
        assert_eq!(s.list().len(), 0);
        assert_eq!(s.active_uid(), None);
        // 删的号 token 一并消失
        let raw = s.data.load_json(TRAE_FILE, Value::Null);
        assert_eq!(raw.get("accounts").and_then(|v| v.as_array()).map(|a| a.len()), Some(0));
    }

    #[test]
    fn trae_load_hard_and_soft_conditions() {
        let s = trae("loadcond");
        s.save(&acc("a", "A", "cn", 0, "", "")); // access 空白 → 硬条件失败
        assert!(s.load_by_uid("a").is_none());
        assert_eq!(s.list().len(), 1); // 索引仍在（loadByUid 失败≠条目消失）
        s.save(&acc("b", "B", "cn", 0, "  ", "")); // 全空白 access 同样判失败（isNotBlank）
        assert!(s.load_by_uid("b").is_none());
        s.save(&acc("c", "C", "cn", 0, "at-c", "")); // refresh 缺失=软条件，返回空串
        let c = s.load_by_uid("c").expect("access 非空白即可加载");
        assert_eq!(c.refresh_token, "");
        assert!(s.load_by_uid("ghost").is_none());
    }

    #[test]
    fn trae_update_tokens_does_not_touch_meta_or_active() {
        let s = trae("updtok");
        s.save(&acc("x", "X", "cn", 100, "at-x", "rt-x"));
        s.save(&acc("y", "Y", "cn", 200, "at-y", "rt-y"));
        // 后台续期 x：只动两枚 token；expiresAt 保持旧值（refreshLocked 靠另调 updateMeta 成对写）
        s.update_tokens(&acc("x", "IGNORED", "cn", 999, "at-x-fresh", "rt-x-rotated"));
        let x = s.load_by_uid("x").unwrap();
        assert_eq!(x.access_token, "at-x-fresh");
        assert_eq!(x.refresh_token, "rt-x-rotated");
        assert_eq!(x.expires_at, 100);
        assert_eq!(x.nickname, "X");
        assert_eq!(x.region_id, "cn");
        assert_eq!(s.active_uid(), Some("y".into()), "updateTokens 不切当前号");
        // 补齐 meta 半边（analysis §2.4 步骤 8 的成对调用）
        s.update_meta(&TraeAccountMeta { uid: "x".into(), nickname: "X".into(), region_id: "cn".into(), expires_at: 999 });
        assert_eq!(s.load_by_uid("x").unwrap().expires_at, 999);
        // update_meta/update_tokens 对不存在的账号：忽略、不新建
        s.update_meta(&TraeAccountMeta { uid: "ghost".into(), ..Default::default() });
        s.update_tokens(&acc("ghost", "", "cn", 0, "at", "rt"));
        assert_eq!(s.list().len(), 2);
    }

    // ---- 导出/导入 ----

    /// Android v1 导出（外壳 + 老字段名/缺省）必须直接可导：uid 空白跳过、accessToken 空白跳过、
    /// region 缺省 cn、imported==0 报错文案逐字、activeUid 命中才切。
    #[test]
    fn trae_import_android_export_and_errors() {
        let s = trae("import");
        let legacy = r#"{
          "format":"traework2api-accounts","version":1,"activeUid":"7687189514112844821",
          "accounts":[
            {"uid":"7687189514112844821","nickname":"intl","expiresAt":1791341100,"accessToken":"at-1","refreshToken":"rt-1"},
            {"uid":"   ","nickname":"bad","accessToken":"x"},
            {"uid":"no-access","nickname":"N","accessToken":"  "},
            {"uid":"3695871032827027","nickname":"cn","accessToken":"at-2"}
          ]}"#;
        let n = s.import_json(legacy).unwrap();
        assert_eq!(n, 2, "缺 uid/accessToken 空白的两条应跳过");
        assert_eq!(s.active_uid(), Some(SG_UID.into()), "activeUid 命中导入结果应切换");
        let metas = s.list();
        assert_eq!(metas[1].region_id, "cn", "region 缺省 cn");
        assert_eq!(metas[1].expires_at, 0, "expiresAt 缺省 0");
        assert_eq!(s.load_by_uid(SG_UID).unwrap().refresh_token, "rt-1");
        // 顶层数组老格式
        let s2 = trae("import-arr");
        assert_eq!(s2.import_json(r#"[{"uid":"u1","accessToken":"a1"}]"#).unwrap(), 1);
        assert_eq!(s2.active_uid(), Some("u1".into()), "无外壳时不动 active（索引仅一条，回退即它）");
        // 错误文案逐字
        assert_eq!(s2.import_json("not json at all").unwrap_err(), "不是有效的账号 JSON");
        assert_eq!(
            s2.import_json(r#"{"accounts":[{"uid":"x","accessToken":""}]}"#).unwrap_err(),
            "JSON 里没有可导入的账号（缺 uid 或 accessToken）"
        );
    }

    #[test]
    fn trae_export_import_roundtrip() {
        let a = trae("expa");
        a.save(&acc("u1", "N1", "cn", 123, "at-1", "rt-1"));
        a.save(&acc("u2", "N2", "sg", 456, "at-2", ""));
        a.set_active("u1");
        let text = a.export_json();
        assert!(text.contains("\"format\": \"traework2api-accounts\""));
        assert!(text.contains("\"region\": \"sg\""));
        let b = trae("expb");
        assert_eq!(b.import_json(&text).unwrap(), 2);
        assert_eq!(b.list(), a.list());
        assert_eq!(b.active_uid(), Some("u1".into()));
        assert_eq!(b.load_by_uid("u2").unwrap().access_token, "at-2");
    }

    #[test]
    fn trae_corrupt_file_reads_empty() {
        let d = tmp_store("corrupt");
        d.save_text_private(TRAE_FILE, "{ definitly ] not json").unwrap();
        let s = TraeAuthStore::with_store(d);
        assert!(s.list().is_empty());
        assert_eq!(s.active_uid(), None);
        assert!(s.load().is_none());
    }

    // ---- authFailed 标记（MainActivity 语义） ----

    #[test]
    fn trae_auth_failed_marked_per_uid() {
        let s = trae("authfail");
        s.save(&acc("good", "G", "cn", 1, "at-g", ""));
        s.save(&acc("bad", "B", "cn", 1, "at-b", ""));
        s.mark_auth_failed("bad");
        assert!(s.auth_failed_for(Some("bad")));
        assert!(!s.auth_failed_for(Some("good"))); // 按 uid 记，绝不牵连别的号
        assert!(!s.auth_failed_for(None));
        assert!(!s.auth_failed_for(Some("  ")));
        s.clear_auth_failed("good"); // 只清匹配的（if authFailedUid==uid 才置 null）
        assert_eq!(s.auth_failed_uid().as_deref(), Some("bad"));
        s.clear_auth_failed("bad");
        assert_eq!(s.auth_failed_uid(), None);
    }

    #[test]
    fn trae_logout_removes_current_only() {
        let s = trae("logout");
        s.save(&acc("a", "A", "cn", 0, "at-a", "rt"));
        s.save(&acc("b", "B", "sg", 0, "at-b", "rt"));
        s.mark_auth_failed("b");
        s.logout();
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.active_uid(), Some("a".into()));
        assert_eq!(s.auth_failed_uid(), None, "登出应带走该号的被拒标记");
        assert!(s.is_logged_in());
        s.logout();
        assert!(!s.is_logged_in());
    }

    // ---- snapshot ----

    #[test]
    fn trae_snapshot_has_no_tokens() {
        let s = trae("snap");
        s.save(&acc(SG_UID, "intl", "sg", 1791341100, "SECRET-ACCESS", "SECRET-REFRESH"));
        s.set_active(SG_UID);
        s.mark_auth_failed(SG_UID);
        let v = s.snapshot();
        let text = v.to_string();
        assert!(!text.contains("SECRET-ACCESS"), "红线：快照绝不带 token");
        assert!(!text.contains("SECRET-REFRESH"));
        assert!(!text.contains("accessToken") && !text.contains("refreshToken"));
        assert_eq!(v["activeUid"].as_str(), Some(SG_UID));
        assert_eq!(v["authFailedUid"].as_str(), Some(SG_UID));
        assert_eq!(v["regionId"].as_str(), Some("cn"), "无 config.json 时缺省 cn");
        assert_eq!(v["accounts"][0]["uid"].as_str(), Some(SG_UID));
        assert_eq!(v["accounts"][0]["nickname"].as_str(), Some("intl"));
        assert_eq!(v["accounts"][0]["region"].as_str(), Some("sg"));
        assert_eq!(v["accounts"][0]["hasAccessToken"].as_bool(), Some(true));
        assert_eq!(v["accounts"][0]["hasRefreshToken"].as_bool(), Some(true));
    }

    // ---- QoderAuthStore ----

    fn qacc(uid: &str, name: &str, mode: &str, exp: i64, pat: &str, jt: &str, jrt: &str) -> QoderAccount {
        QoderAccount {
            uid: uid.into(),
            name: name.into(),
            mode: mode.into(),
            expires_at: exp,
            pat: pat.into(),
            job_token: jt.into(),
            job_refresh_token: jrt.into(),
            ..Default::default()
        }
    }

    #[test]
    fn qoder_roundtrip_current_and_load_conditions() {
        let s = qoder("qbase");
        s.save(&qacc("q1", "张三", "cn", 1791341100000, "pt-1", "jt-1", "jrt-1"));
        assert!(s.load_by_uid("q1").is_some());
        let s2 = QoderAuthStore::with_store(Arc::new(DataStore::new(s.data.dir())));
        assert_eq!(s2.list(), s.list());
        assert_eq!(s2.load().unwrap().job_token, "jt-1");
        // 无 access 概念：硬条件 = pat 与 jt 双空白
        s.save(&qacc("q2", "李四", "global", 0, "", "", ""));
        assert_eq!(s.active_uid().as_deref(), Some("q2"));
        assert!(s.load_by_uid("q2").is_none(), "pat+jt 双空必须判不可用");
        // 但索引里条目还在、删除当前号回退剩下的第一个
        assert_eq!(s.list().len(), 2);
        s.remove("q2");
        assert_eq!(s.active_uid().as_deref(), Some("q1"));
        // 只有 pat（jt 空）是合法的（软条件）
        s2.save(&qacc("q3", "王五", "cn", 0, "pt-3", "", ""));
        let q3 = s2.load_by_uid("q3").unwrap();
        assert_eq!(q3.job_token, "");
        assert!(!q3.chat_ready());
    }

    /// Qoder updateTokens 与 Trae 的差异：**顺带写 Meta**（analysis §4.5）。
    #[test]
    fn qoder_update_tokens_also_writes_meta() {
        let s = qoder("qupdtok");
        s.save(&qacc("q1", "旧名", "cn", 111, "pt-1", "jt-1", "jrt-1"));
        s.save(&qacc("q2", "其它", "cn", 222, "pt-2", "jt-2", "jrt-2"));
        let mut fresh = qacc("q1", "新名", "global", 333, "pt-1", "jt-1-new", "jrt-1-rot");
        fresh.organization_id = "org-9".into();
        fresh.organization_tags = vec!["team-a".into()];
        fresh.is_data_policy_modifiable = false;
        s.update_tokens(&fresh);
        let q1 = s.load_by_uid("q1").unwrap();
        assert_eq!(q1.job_token, "jt-1-new");
        assert_eq!(q1.name, "新名", "updateTokens 会 writeMeta（expiresAt/组织材料一起落）");
        assert_eq!(q1.expires_at, 333);
        assert_eq!(q1.organization_tags, vec!["team-a".to_string()]);
        assert_eq!(q1.is_data_policy_modifiable, false);
        assert_eq!(s.active_uid().as_deref(), Some("q2"), "不动当前号");
        s.update_tokens(&qacc("ghost", "", "cn", 0, "p", "j", ""));
        assert_eq!(s.list().len(), 2, "不存在的号不新建");
    }

    /// QoderAuthStore.selfCheck：指纹往返 / 空指纹 null / 组织字段往返 / 旧索引兼容 + remove 连指纹一起删（修正版）。
    #[test]
    fn qoder_fingerprint_and_remove_cleans_fp() {
        let s = qoder("qfp");
        s.save(&qacc("q1", "n", "cn", 1, "pt", "jt", ""));
        assert!(s.fingerprint("q1").is_none());
        let fp = QoderFingerprint { machine_id: "mid-123".into(), machine_token: "mtok_abc-DEF_456".into(), ..Default::default() };
        s.set_fingerprint("q1", &fp);
        assert_eq!(s.fingerprint("q1").as_ref(), Some(&fp));
        // 持久化 + 重开可见
        let s2 = QoderAuthStore::with_store(Arc::new(DataStore::new(s.data.dir())));
        assert_eq!(s2.fingerprint("q1").as_ref(), Some(&fp));
        // token 更新不清指纹（与 Kotlin 分离存储等价）
        s2.update_tokens(&qacc("q1", "n", "cn", 9, "pt", "jt2", ""));
        assert_eq!(s2.fingerprint("q1").as_ref(), Some(&fp));
        // remove 修正：Android 源码漏删 _fp，此处删除账号时指纹一并消失
        s2.remove("q1");
        let raw = s2.data.load_json(QODER_FILE, Value::Null);
        assert!(!raw.to_string().contains("mtok_abc-DEF_456"), "remove 必须连 q_acc_<uid>_fp 等值一起清除");
        // set/clear 对不存在的账号：忽略
        s2.set_fingerprint("ghost", &fp);
        s2.clear_fingerprint("ghost");
        assert_eq!(s2.fingerprint("ghost"), None);
        // 空指纹（id 或 tk 空白）decode 出 null
        assert!(QoderFingerprint::decode_short_json(r#"{"id":"","tk":"x"}"#).is_none());
    }

    #[test]
    fn qoder_index_tags_variants_and_legacy_defaults() {
        let d = tmp_store("qtags");
        d.save_text_private(
            QODER_FILE,
            r#"{"format":"traework2api-qoder-accounts","version":2,"activeUid":"",
               "accounts":[
                 {"uid":"a","name":"A","email":"","mode":"cn","expiresAt":5,"organizationTags":"team-a, team-b,, "},
                 {"uid":"b","name":"B","email":"x","mode":"BOGUS","expiresAt":null,"organizationTags":["t1","","t2"],"pat":"pt-b","jobToken":"jt-b"},
                 {"uid":" ","name":"drop me"}
               ]}"#,
        )
        .unwrap();
        let s = QoderAuthStore::with_store(d);
        let metas = s.list();
        assert_eq!(metas.len(), 2, "uid 空白的条目丢弃");
        assert_eq!(metas[0].organization_tags, vec!["team-a".to_string(), "team-b".to_string()], "逗号串形态兼容");
        assert_eq!(metas[1].organization_tags, vec!["t1".to_string(), "t2".to_string()], "数组内空白丢弃");
        assert_eq!(metas[1].expires_at, 0, "expiresAt null → 0");
        // 旧索引缺组织字段 → 默认值（dataPolicyAgreed=false / isDataPolicyModifiable=true）
        let legacy = QoderAccountMeta {
            uid: "old".into(),
            name: "n".into(),
            mode: "cn".into(),
            expires_at: 1,
            is_data_policy_modifiable: true,
            ..Default::default()
        };
        let parsed: QoderAccountMeta =
            serde_json::from_str(r#"{"uid":"old","name":"n","email":"e","mode":"cn","expiresAt":1}"#).unwrap_or(legacy.clone());
        assert_eq!(parsed.data_policy_agreed, legacy.data_policy_agreed);
        assert_eq!(parsed.is_data_policy_modifiable, true);
    }

    #[test]
    fn qoder_export_import_roundtrip_android_format() {
        let a = qoder("qexp");
        let mut q1 = qacc("q1", "张三", "cn", 1791341100000, "pt-1", "jt-1", "jrt-1");
        q1.organization_id = "org-9".into();
        q1.organization_tags = vec!["team-a".into(), "team-b".into()];
        q1.data_policy_agreed = true;
        q1.is_data_policy_modifiable = false;
        a.save(&q1);
        a.set_fingerprint("q1", &QoderFingerprint { machine_id: "mid".into(), machine_token: "tok".into(), ..Default::default() });
        let mut q2 = qacc("q2", "李四", "global", 0, "", "jt-2", "");
        q2.pat = String::new();
        a.save(&q2); // pat 空 + jt 非空 = 合法
        let text = a.export_json();
        assert!(text.contains("\"format\": \"traework2api-qoder-accounts\""));
        // 与 Android 导出逐字兼容：fingerprint 是短键 JSON 字符串
        assert!(text.contains("\\\"id\\\":\\\"mid\\\""), "导出指纹应为短键字符串: {text}");
        let b = qoder("qexp-b");
        assert_eq!(b.import_json(&text).unwrap(), 2);
        assert_eq!(b.list(), a.list());
        assert_eq!(b.fingerprint("q1"), a.fingerprint("q1"), "短键字符串导入应还原指纹");
        assert_eq!(b.active_uid().as_deref(), Some("q2"), "activeUid 命中应切换");
        assert_eq!(b.load_by_uid("q2").unwrap().job_token, "jt-2");
        // 导入老备份（无 fingerprint 字段）→ 不报错、视为本机派生
        let c = qoder("qexp-old");
        let old = r#"[{"uid":"x","name":"X","email":"","mode":"global","expiresAt":1,"pat":"pt-x","jobToken":"jt-x"}]"#;
        assert_eq!(c.import_json(old).unwrap(), 1);
        assert_eq!(c.fingerprint("x"), None);
        // 错误路径：全跳光 → 逐字文案；无效 JSON
        let e = qoder("qexp-err");
        assert_eq!(
            e.import_json(r#"{"accounts":[{"uid":"z","pat":"","jobToken":""}]}"#).unwrap_err(),
            "JSON 里没有可导入的 Qoder 账号（缺 uid 或 pat）"
        );
        assert_eq!(e.import_json("<garbage>").unwrap_err(), "不是有效的 Qoder 账号 JSON");
    }

    #[test]
    fn qoder_snapshot_and_auth_failed() {
        let s = qoder("qsnap");
        s.save(&qacc("q1", "张三", "cn", 123, "SECRET-PAT", "SECRET-JT", "SECRET-JRT"));
        s.set_fingerprint("q1", &QoderFingerprint { machine_id: "SECRET-MID".into(), machine_token: "SECRET-TOK".into(), ..Default::default() });
        s.mark_auth_failed("q1");
        let v = s.snapshot();
        let text = v.to_string();
        for secret in ["SECRET-PAT", "SECRET-JT", "SECRET-JRT", "SECRET-MID", "SECRET-TOK"] {
            assert!(!text.contains(secret), "快照绝不带凭据/指纹: {secret}");
        }
        assert_eq!(v["activeUid"].as_str(), Some("q1"));
        assert_eq!(v["authFailedUid"].as_str(), Some("q1"));
        assert_eq!(v["accounts"][0]["chatReady"].as_bool(), Some(true));
        assert_eq!(v["accounts"][0]["hasPat"].as_bool(), Some(true));
        assert_eq!(v["accounts"][0]["hasFingerprint"].as_bool(), Some(true));
        assert_eq!(v["accounts"][0]["name"].as_str(), Some("张三"));
        s.clear_auth_failed("q1");
        assert_eq!(s.snapshot()["authFailedUid"], Value::Null);
    }

    /// 文件落盘权限 0600（analysis §7.2"明文 JSON + 文件权限 0600"）。
    #[test]
    #[cfg(unix)]
    fn account_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let a = trae("perm");
        a.save(&acc("u", "N", "cn", 1, "at", "rt"));
        let mode = std::fs::metadata(a.data.path(TRAE_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let q = qoder("perm-q");
        q.save(&qacc("u", "N", "cn", 1, "pt", "jt", ""));
        let mode = std::fs::metadata(q.data.path(QODER_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
