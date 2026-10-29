use crate::prefs::Storage;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 全局应用状态（替代 Android 的 Context + 各 object 单例里的 prefs 访问）。
pub struct AppState {
    pub store: Storage,
    pub prefs: Mutex<HashMap<String, String>>,
    pub secrets: Mutex<HashMap<String, String>>,
    /// Trae 凭证串行锁（refresh token 轮换，并发续期会让先换到的失效）
    pub trae_lock: tokio::sync::Mutex<()>,
    /// Qoder 凭证串行锁（与 Trae 分开，两边互不卡）
    pub qoder_lock: tokio::sync::Mutex<()>,
    pub http: reqwest::Client,
    pub server: Mutex<Option<crate::gateway::server::ServerHandle>>,
}

impl AppState {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        let store = Storage::new(dir);
        let prefs = store.load_prefs();
        let secrets = store.load_secrets();
        let http = reqwest::Client::builder()
            .user_agent("TraeWork2API-Desktop/0.4.17")
            .build()
            .unwrap_or_default();
        Arc::new(Self {
            store,
            prefs: Mutex::new(prefs),
            secrets: Mutex::new(secrets),
            trae_lock: tokio::sync::Mutex::new(()),
            qoder_lock: tokio::sync::Mutex::new(()),
            http,
            server: Mutex::new(None),
        })
    }

    // ---- prefs 键值 ----
    pub fn pref(&self, k: &str) -> Option<String> {
        self.prefs.lock().ok()?.get(k).cloned()
    }
    pub fn pref_or(&self, k: &str, d: &str) -> String {
        self.pref(k).unwrap_or_else(|| d.to_string())
    }
    pub fn set_pref(&self, k: &str, v: &str) {
        if let Ok(mut m) = self.prefs.lock() {
            m.insert(k.to_string(), v.to_string());
        }
        self.persist_prefs();
    }
    pub fn del_pref(&self, k: &str) {
        if let Ok(mut m) = self.prefs.lock() {
            m.remove(k);
        }
        self.persist_prefs();
    }

    // ---- 加密 token ----
    pub fn secret(&self, k: &str) -> Option<String> {
        let blob = self.secrets.lock().ok()?.get(k).cloned()?;
        self.store.decrypt(&blob)
    }
    pub fn secret_blob(&self, k: &str) -> Option<String> {
        self.secrets.lock().ok()?.get(k).cloned()
    }
    pub fn set_secret(&self, k: &str, plain: &str) {
        let blob = self.store.encrypt(plain);
        if let Ok(mut m) = self.secrets.lock() {
            m.insert(k.to_string(), blob);
        }
        self.persist_secrets();
    }
    pub fn del_secret(&self, k: &str) {
        if let Ok(mut m) = self.secrets.lock() {
            m.remove(k);
        }
        self.persist_secrets();
    }

    pub fn persist_prefs(&self) {
        if let Ok(m) = self.prefs.lock() {
            self.store.save_prefs(&m);
        }
    }
    pub fn persist_secrets(&self) {
        if let Ok(m) = self.secrets.lock() {
            self.store.save_secrets(&m);
        }
    }

    /// 本机稳定指纹（API Key 派生用；对齐 Android「签名+设备ID」的确定性思路）
    pub fn machine_fingerprint(&self) -> String {
        for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(p) {
                let t = s.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
        if let Ok(t) = std::fs::read_to_string("/sys/class/dmi/id/product_uuid") {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return t;
            }
        }
        hostname_fallback()
    }
}

fn hostname_fallback() -> String {
    std::env::var("HOSTNAME").unwrap_or_else(|_| "traework2api-host".into())
}
