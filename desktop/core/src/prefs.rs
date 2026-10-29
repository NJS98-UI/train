use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use base64::Engine;
use rand::RngCore;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 桌面版持久化层：等价于 Android 的 SharedPreferences + AndroidKeyStore AES-GCM。
/// - prefs.json   —— 全部非敏感键值（键名与 Android 版一致）
/// - secrets.json —— token 密文（AES-256-GCM，主密钥在 key.bin）
/// - key.bin      —— 32 字节主密钥，首次运行生成
pub struct Storage {
    dir: PathBuf,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl Storage {
    pub fn new(dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&dir);
        Self { dir }
    }

    pub fn dir(&self) -> &Path { &self.dir }

    fn prefs_path(&self) -> PathBuf { self.dir.join("prefs.json") }
    fn secrets_path(&self) -> PathBuf { self.dir.join("secrets.json") }
    fn key_path(&self) -> PathBuf { self.dir.join("key.bin") }

    fn read_json(&self, p: &Path) -> HashMap<String, String> {
        let mut out = HashMap::new();
        if let Ok(s) = fs::read_to_string(p) {
            if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(&s) {
                for (k, v) in o {
                    let sv = match v {
                        Value::String(s) => s,
                        other => other.to_string(),
                    };
                    out.insert(k, sv);
                }
            }
        }
        out
    }

    fn write_json(&self, p: &Path, m: &HashMap<String, String>) {
        let obj: serde_json::Map<String, Value> =
            m.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        let tmp = p.with_extension("tmp");
        if let Ok(mut f) = fs::File::create(&tmp) {
            let _ = f.write_all(serde_json::to_string_pretty(&obj).unwrap_or_default().as_bytes());
        }
        let _ = fs::rename(&tmp, p);
    }

    pub fn load_prefs(&self) -> HashMap<String, String> { self.read_json(&self.prefs_path()) }
    pub fn save_prefs(&self, m: &HashMap<String, String>) { self.write_json(&self.prefs_path(), m) }
    pub fn load_secrets(&self) -> HashMap<String, String> { self.read_json(&self.secrets_path()) }
    pub fn save_secrets(&self, m: &HashMap<String, String>) { self.write_json(&self.secrets_path(), m) }

    fn master_key(&self) -> Option<[u8; 32]> {
        if let Ok(k) = fs::read(self.key_path()) {
            if k.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&k);
                return Some(arr);
            }
        }
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        if let Ok(mut f) = fs::File::create(self.key_path()) {
            let _ = f.write_all(&k);
        }
        Some(k)
    }

    /// 密文格式：base64(nonce):base64(ciphertext) —— 与 Android 版 "b64(iv):b64(ct)" 同形
    pub fn encrypt(&self, plain: &str) -> String {
        let Some(key) = self.master_key() else { return String::new() };
        let cipher = Aes256Gcm::new_from_slice(&key).expect("aes key");
        let mut nonce_b = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_b);
        let ct = cipher
            .encrypt(aes_gcm::Nonce::from_slice(&nonce_b), plain.as_bytes())
            .unwrap_or_default();
        format!("{}:{}", b64().encode(nonce_b), b64().encode(ct))
    }

    pub fn decrypt(&self, blob: &str) -> Option<String> {
        let (nb, cb) = blob.split_once(':')?;
        let nonce = b64().decode(nb.trim()).ok()?;
        let ct = b64().decode(cb.trim()).ok()?;
        let key = self.master_key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
        let pt = cipher
            .decrypt(aes_gcm::Nonce::from_slice(&nonce), ct.as_ref())
            .ok()?;
        String::from_utf8(pt).ok()
    }
}
