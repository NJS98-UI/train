//! AccountBackup 对应体（Kotlin `AccountBackup.kt` 逐行对照，analysis §5）。
//! 把 Trae+Qoder 两套账号打包进**同一个 JSON**：`{"format":"traework2api-backup","version":1,"trae":{…},"qoder":{…}}`，
//! 其中 `trae`/`qoder` 是各自 AuthStore `export_json` 的整对象（明文 token/PAT）。
//!
//! 安全红线（源码文档原话）：备份文件等价账号钥匙，只走本地文件/剪贴板，绝不联网、绝不写日志。
//! 桌面落盘位置（design.md §IPC `backup_export(→{path})` 的落地形态）：`<data>/backups/`
//! 子目录、时间戳文件名 `traework2api-backup-<YYYYmmddHHMMSS>.json`、0600 权限
//! （[`DataStore::save_text_private`]）。Android 端走 SAF 用户选路径，桌面版由调用方
//! （IPC/文件对话框 wave）决定是把该文件另存或直传 `import_json`。
//!
//! 导入三分支（analysis §5.2 逐条）：
//! 1. 根不是 JSON 对象（serde 解析失败）→ Err("不是有效的账号备份 JSON")；
//! 2. **备份外壳**：根含 `trae` 或 `qoder` 任一**对象**节点 → 各自 import（异常吞掉记 0，
//!    `runCatching.getOrDefault(0)` 同构）；两计数之和 ≤0 → Err("备份里没有任何可导入的账号…")；
//! 3. **单份导出兼容**：按根 `format` 识别——大小写不敏感含 `"qoder"` → Qoder，其余
//!    （含 format 缺失/null/非字符串）→ Trae，整包喂对应 store（其自身错误**直接上抛**）。

use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::storage::DataStore;

/// 备份外壳 format（Kotlin `AccountBackup` 类注释 + `exportAll`）。
pub const BACKUP_FORMAT: &str = "traework2api-backup";
/// 备份外壳 version。
pub const BACKUP_VERSION: u32 = 1;

/// `isQoderFormat`：`format != null && format.contains("qoder", ignoreCase)`。
pub fn is_qoder_format(format: Option<&str>) -> bool {
    format.is_some_and(|f| f.to_lowercase().contains("qoder"))
}

/// 导入计数（对应 Kotlin `data class ImportSummary(trae, qoder)`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub trae: usize,
    pub qoder: usize,
}

impl ImportSummary {
    /// design.md §IPC `backup_import` 的返回形状 `{traeN, qoderN}`。
    pub fn to_value(&self) -> Value {
        serde_json::json!({ "traeN": self.trae, "qoderN": self.qoder })
    }
}

/// 备份器：持有两套 auth store + 数据目录（导出文件落 `backups/` 子目录）。
pub struct AccountBackup {
    trae: Arc<TraeAuthStore>,
    qoder: Arc<QoderAuthStore>,
    data: Arc<DataStore>,
}

impl AccountBackup {
    pub fn new() -> Self {
        let data = crate::storage::store();
        AccountBackup {
            trae: TraeAuthStore::global(),
            qoder: QoderAuthStore::global(),
            data,
        }
    }

    /// 测试/自定义注入口：三件套同源临时目录。
    pub fn with_parts(data: Arc<DataStore>) -> Self {
        AccountBackup {
            trae: Arc::new(TraeAuthStore::with_store(data.clone())),
            qoder: Arc::new(QoderAuthStore::with_store(data.clone())),
            data,
        }
    }

    pub fn global() -> Arc<AccountBackup> {
        static G: OnceLock<Arc<AccountBackup>> = OnceLock::new();
        G.get_or_init(|| Arc::new(AccountBackup::new())).clone()
    }

    /// `exportAll` 的对象形态。子节点解析失败时按空对象 `{}`，保持外壳键齐全。
    pub fn export_value(&self) -> Value {
        let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap_or_else(|_| Value::Object(Default::default()));
        serde_json::json!({
            "format": BACKUP_FORMAT,
            "version": BACKUP_VERSION,
            "trae": parse(self.trae.export_json()),
            "qoder": parse(self.qoder.export_json()),
        })
    }

    /// `exportAll`：`root.toString(2)` = 2 空格缩进 pretty（serde_json::to_string_pretty 同宽度）。
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.export_value()).unwrap_or_else(|_| "{}".into())
    }

    /// 桌面导出落盘：`backups/traework2api-backup-<YYYYmmddHHMMSS>.json`（0600），
    /// 返回**绝对路径字符串**（design.md `backup_export(→{path})`）。
    pub fn export_file(&self) -> Result<String, String> {
        let dir = self.data.dir().join("backups");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
        let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
        let name = format!("backups/traework2api-backup-{ts}.json");
        // save_text 不建父目录，故 name 用相对 dir 的路径前先确保 dir 存在（上面已建）。
        self.data.save_text_private(&name, &self.export_json())?;
        Ok(self.data.path(&name).to_string_lossy().into_owned())
    }

    /// `importAll`：三分支（见模块头注释），语义逐行对照 Kotlin :36-:55。
    pub fn import_json(&self, json: &str) -> Result<ImportSummary, String> {
        let root: Value = serde_json::from_str(json)
            .map_err(|_| "不是有效的账号备份 JSON".to_string())?;
        if !root.is_object() {
            return Err("不是有效的账号备份 JSON".to_string());
        }
        let trae_node = root.get("trae").filter(|v| v.is_object());
        let qoder_node = root.get("qoder").filter(|v| v.is_object());
        if trae_node.is_some() || qoder_node.is_some() {
            // 外壳分支：各自 import 失败吞掉记 0（含「JSON 里没有可导入的账号」类错误）。
            let t = match trae_node {
                Some(n) => serde_json::to_string(n)
                    .ok()
                    .and_then(|s| self.trae.import_json(&s).ok())
                    .unwrap_or(0),
                None => 0,
            };
            let q = match qoder_node {
                Some(n) => serde_json::to_string(n)
                    .ok()
                    .and_then(|s| self.qoder.import_json(&s).ok())
                    .unwrap_or(0),
                None => 0,
            };
            if t + q == 0 {
                return Err("备份里没有任何可导入的账号（token/PAT 可能缺失或 JSON 损坏）".to_string());
            }
            return Ok(ImportSummary { trae: t, qoder: q });
        }
        // 单份导出兼容：store 自身错误直接上抛（Kotlin 这里不包 runCatching）。
        let format = root.get("format").and_then(|v| v.as_str());
        if is_qoder_format(format) {
            Ok(ImportSummary {
                trae: 0,
                qoder: self.qoder.import_json(json)?,
            })
        } else {
            Ok(ImportSummary {
                trae: self.trae.import_json(json)?,
                qoder: 0,
            })
        }
    }
}

impl Default for AccountBackup {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 测试（临时目录注入；外壳往返 / 单份兼容三分支 / 坏 node 计 0 / 文件形态）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("tw-backup-test-{}-{}-{}", std::process::id(), tag, n));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn trae_export_json(uid: &str) -> String {
        json!({
            "format": crate::auth::store::TRAE_EXPORT_FORMAT,
            "version": crate::auth::store::EXPORT_VERSION,
            "activeUid": uid,
            "accounts": [{
                "uid": uid, "nickname": "nick", "region": "cn",
                "expiresAt": 1893456000000i64,
                "accessToken": "at-1", "refreshToken": "rt-1"
            }]
        })
        .to_string()
    }

    fn qoder_export_json(uid: &str) -> String {
        json!({
            "format": crate::auth::store::QODER_EXPORT_FORMAT,
            "version": crate::auth::store::EXPORT_VERSION,
            "activeUid": uid,
            "accounts": [{
                "uid": uid, "name": "qn", "email": "q@n.cc", "mode": "cn",
                "expiresAt": 1893456000000i64,
                "organizationId": "org", "organizationName": "", "organizationTags": [],
                "dataPolicyAgreed": false, "isDataPolicyModifiable": false,
                "pat": "pt-1", "jobToken": "jt-1", "jobRefreshToken": "jrt-1",
                "fingerprint": ""
            }]
        })
        .to_string()
    }

    // ---- is_qoder_format 三断言（Kotlin selfCheck 原样移植）----

    #[test]
    fn format_identification_matches_selfcheck() {
        assert!(is_qoder_format(Some("traework2api-qoder-accounts")));
        assert!(!is_qoder_format(Some("traework2api-accounts")));
        assert!(!is_qoder_format(None));
        assert!(is_qoder_format(Some("QODER-anything")), "大小写不敏感");
    }

    // ---- 外壳导出形态 ----

    #[test]
    fn export_shell_shape_and_indent() {
        let d = tmpdir("shell");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        bk.trae
            .import_json(&trae_export_json("t1"))
            .expect("trae 预置");
        bk.qoder
            .import_json(&qoder_export_json("q1"))
            .expect("qoder 预置");

        let v = bk.export_value();
        assert_eq!(v["format"], json!(BACKUP_FORMAT));
        assert_eq!(v["version"], json!(1));
        assert_eq!(v["trae"]["format"], json!("traework2api-accounts"));
        assert_eq!(v["qoder"]["format"], json!("traework2api-qoder-accounts"));
        assert_eq!(v["trae"]["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(v["trae"]["accounts"][0]["accessToken"], json!("at-1"));
        assert_eq!(v["qoder"]["activeUid"], json!("q1"));

        let s = bk.export_json();
        assert!(
            s.contains("\n  \"format\""),
            "2 空格 pretty（toString(2) 同构）: {s}"
        );
    }

    // ---- 外壳往返 ----

    #[test]
    fn shell_roundtrip_imports_both() {
        let src = tmpdir("rt-src");
        let dst = tmpdir("rt-dst");
        let bk_src = AccountBackup::with_parts(Arc::new(DataStore::new(&src)));
        bk_src
            .trae
            .import_json(&trae_export_json("t1"))
            .expect("trae1");
        bk_src
            .trae
            .import_json(&trae_export_json("t2"))
            .expect("trae2");
        bk_src
            .qoder
            .import_json(&qoder_export_json("q1"))
            .expect("qoder1");

        let bundle = bk_src.export_json();
        let bk_dst = AccountBackup::with_parts(Arc::new(DataStore::new(&dst)));
        let sum = bk_dst.import_json(&bundle).expect("外壳导入");
        assert_eq!(sum, ImportSummary { trae: 2, qoder: 1 });
        assert_eq!(bk_dst.trae.list().len(), 2);
        assert_eq!(bk_dst.qoder.list().len(), 1);
        // activeUid 命中导入结果 → 保持同一个号（analysis §5.2 合并语义尾条）
        assert_eq!(bk_dst.qoder.active_uid().as_deref(), Some("q1"));
    }

    // ---- 单份导出兼容 ----

    #[test]
    fn single_export_qoder_format_goes_qoder_store() {
        let d = tmpdir("single-q");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        let one = qoder_export_json("q9");
        let sum = bk.import_json(&one).unwrap();
        assert_eq!(sum, ImportSummary { trae: 0, qoder: 1 });
        assert_eq!(bk.qoder.list().len(), 1);
        assert!(bk.trae.list().is_empty(), "Qoder 备份绝不能喂给 Trae 解析");
    }

    #[test]
    fn single_export_trae_and_missing_format_go_trae_store() {
        let d = tmpdir("single-t");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        assert_eq!(bk.import_json(&trae_export_json("t9")).unwrap().trae, 1);
        assert!(!is_qoder_format(None), "format 缺失 → Trae 兜底分支");
        // 顶层数组在备份外壳层就被拒（Kotlin `JSONObject("[…]")` 直接抛，老数组格式
        // 的兼容发生在各 store 的 import_json 内部，不经 AccountBackup）
        let legacy = json!([{"uid":"t10","accessToken":"at-10"}]).to_string();
        assert_eq!(
            bk.import_json(&legacy).unwrap_err(),
            "不是有效的账号备份 JSON"
        );
        // 直喂 store 才认老数组格式（store 自身行为，验证两层的分工）
        assert_eq!(bk.trae.import_json(&legacy).unwrap(), 1);
        assert_eq!(bk.trae.list().len(), 2);
    }

    // ---- 坏 node / 空备份 ----

    #[test]
    fn bad_nodes_count_zero_and_error_message_exact() {
        let d = tmpdir("bad");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        let shell = json!({
            "format": BACKUP_FORMAT, "version": 1,
            "trae": {"format": "traework2api-accounts", "accounts": []},
            "qoder": "not-an-object"
        });
        let err = bk.import_json(&shell.to_string()).unwrap_err();
        assert_eq!(
            err,
            "备份里没有任何可导入的账号（token/PAT 可能缺失或 JSON 损坏）",
            "报错文案逐字对齐 Kotlin :43"
        );
        // 单 trae 空账号也算 0 → 同一报错
        let shell2 = json!({"format": BACKUP_FORMAT, "trae": {"accounts": []}});
        assert!(bk.import_json(&shell2.to_string()).is_err());
    }

    #[test]
    fn invalid_root_is_not_backup_json() {
        let d = tmpdir("badjson");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        assert_eq!(
            bk.import_json("{ nope ]").unwrap_err(),
            "不是有效的账号备份 JSON"
        );
        // 合法 JSON 但不是对象（Kotlin JSONObject(json) 对数组也抛）
        assert_eq!(
            bk.import_json("[1,2,3]").unwrap_err(),
            "不是有效的账号备份 JSON"
        );
    }

    // ---- 混合外壳：一半能导一半坏 ----

    #[test]
    fn shell_partial_success() {
        let d = tmpdir("partial");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        let trae_v: Value = serde_json::from_str(&trae_export_json("t1")).unwrap();
        let shell = json!({
            "format": BACKUP_FORMAT, "version": 1,
            "trae": trae_v,
            "qoder": {"format": "traework2api-qoder-accounts", "accounts": [{"uid": "  ", "pat": "x"}]}
        });
        let sum = bk.import_json(&shell.to_string()).unwrap();
        assert_eq!(sum, ImportSummary { trae: 1, qoder: 0 }, "坏 qoder 条目吞掉记 0");
    }

    // ---- 导出文件 ----

    #[test]
    fn export_file_naming_dir_and_mode() {
        let d = tmpdir("file");
        let bk = AccountBackup::with_parts(Arc::new(DataStore::new(&d)));
        bk.trae.import_json(&trae_export_json("t1")).expect("seed");
        let path = bk.export_file().expect("导出落盘");
        let p = PathBuf::from(&path);
        assert!(p.starts_with(d.join("backups")));
        let fname = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(fname.starts_with("traework2api-backup-"), "{fname}");
        assert!(fname.ends_with(".json"));
        assert!(p.exists());
        let content = std::fs::read_to_string(&p).unwrap();
        let v: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["format"], json!(BACKUP_FORMAT));
        assert_eq!(v["trae"]["accounts"].as_array().unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "备份文件 0600（明文 token 红线）");
        }
    }
}
