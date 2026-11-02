//! JSON 文件存储：取代 Android SharedPreferences。原子写 + 损坏自愈（analysis 各文档的 prefs→文件映射见 design.md §Data Models）。

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub struct DataStore {
    dir: PathBuf,
}

impl DataStore {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let _ = fs::create_dir_all(&dir);
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// 读 JSON；缺失或损坏返回 None；损坏时把原文件改名为 `*.corrupt-<ts>` 留存（对齐 prefs 容错语义）。
    pub fn load_json_opt(&self, name: &str) -> Option<Value> {
        let p = self.path(name);
        let raw = fs::read_to_string(&p).ok()?;
        match serde_json::from_str::<Value>(&raw) {
            Ok(v) => Some(v),
            Err(_) => {
                let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
                let _ = fs::rename(&p, self.path(&format!("{}.corrupt-{}", name, ts)));
                None
            }
        }
    }

    pub fn load_json(&self, name: &str, default: Value) -> Value {
        self.load_json_opt(name).unwrap_or(default)
    }

    /// 原子写：先写 tmp 再 rename。
    pub fn save_json(&self, name: &str, v: &Value) -> Result<(), String> {
        let tmp = self.path(&format!("{}.tmp", name));
        let body = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
        fs::write(&tmp, body).map_err(|e| e.to_string())?;
        fs::rename(&tmp, self.path(name)).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn load_text(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path(name)).ok()
    }

    pub fn save_text(&self, name: &str, s: &str) -> Result<(), String> {
        fs::write(self.path(name), s).map_err(|e| e.to_string())
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name).exists()
    }

    /// 0600 权限（非 Windows 尽力而为；Windows 忽略）。
    pub fn save_text_private(&self, name: &str, s: &str) -> Result<(), String> {
        self.save_text(name, s)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(self.path(name), fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

static STORE: OnceLock<Arc<DataStore>> = OnceLock::new();

pub fn init(dir: impl AsRef<Path>) {
    let _ = STORE.set(Arc::new(DataStore::new(dir)));
}

pub fn store() -> Arc<DataStore> {
    STORE.get().expect("DataStore 未初始化").clone()
}

/// 通用取值 helper：把 prefs 的 "getString/getInt/getBool 带默认" 语义搬过来。
pub fn json_str(v: &Value, key: &str, default: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string).unwrap_or_else(|| default.to_string())
}

pub fn json_i64(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
}

pub fn json_bool(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_self_heal_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tw-storage-test-{}", std::process::id()));
        let st = DataStore::new(&dir);
        st.save_text("a.json", "{ not json").unwrap();
        assert!(st.load_json_opt("a.json").is_none());
        assert!(std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with("a.json.corrupt-")));
        let v = serde_json::json!({"x": 1});
        st.save_json("b.json", &v).unwrap();
        assert_eq!(st.load_json("b.json", serde_json::json!(null)), v);
        assert_eq!(json_i64(&st.load_json("b.json", v.clone()), "x", 9), 1);
        assert_eq!(json_str(&st.load_json("b.json", v.clone()), "none", "def"), "def");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
