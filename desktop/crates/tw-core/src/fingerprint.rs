//! 本地 API Key 派生与机器指纹（对应 Android `Gateway.apiKey/derivedKey`）。
//! 性质保持不变：同机恒稳、确定派生、**不落盘 key 本身**（design.md / requirements R8）。

use crate::types::region;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

static KEY_CACHE: OnceLock<String> = OnceLock::new();
static INSTALL_ID: OnceLock<String> = OnceLock::new();

/// 机器稳定身份：
/// - Windows：注册表 `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`（等价 Widevine 的硬件/系统级身份）
/// - Linux：`/etc/machine-id`（退化 `/var/lib/dbus/machine-id`）
/// - macOS：`IOPlatformUUID`
/// - 全部失败：持久化随机 UUID 文件 `device_identity`（0600；首启生成后恒定）
pub fn machine_fingerprint() -> Option<String> {
    #[cfg(windows)]
    {
        use winreg::enums::*;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        hklm.open_subkey_with_flags("SOFTWARE\\Microsoft\\Cryptography", KEY_READ)
            .and_then(|k| k.get_value::<String, _>("MachineGuid"))
            .ok()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
    }
    #[cfg(target_os = "linux")]
    {
        for p in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(p) {
                let t = s.trim().to_lowercase();
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        text.split('"')
            .skip_while(|s| *s != "IOPlatformUUID")
            .nth(1)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// 兜底持久身份（仅当 OS 指纹不可读时使用），生成后恒定。
fn fallback_identity() -> String {
    let st = crate::storage::store();
    if let Some(s) = st.load_text("device_identity") {
        let t = s.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = st.save_text_private("device_identity", &id);
    id
}

fn device_id() -> String {
    match machine_fingerprint() {
        Some(raw) => hex_sha256(&format!("tm:{}", raw)),
        None => hex_sha256(&format!("tm-fb:{}", fallback_identity())),
    }
}

/// 安装标识：对齐 Android「签名参与派生，不同安装不共享」的意图。
pub fn install_id() -> String {
    INSTALL_ID.get_or_init(|| match crate::storage::store().load_text("app_install_id") {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            let id = uuid::Uuid::new_v4().to_string();
            let _ = crate::storage::store().save_text_private("app_install_id", &id);
            id
        }
    })
    .clone()
}

/// `sk-` + sha256(`<安装标识>|<机器指纹哈希>`) 前 32 hex（与 Android 格式一致：sk- + 32 hex）。
pub fn api_key() -> String {
    KEY_CACHE
        .get_or_init(|| {
            let input = format!("{}|{}", install_id(), device_id());
            format!("sk-{}", hex_sha256(&input).chars().take(32).collect::<String>())
        })
        .clone()
}

pub(crate) fn hex_sha256(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{:02x}", b)).collect()
}

/// 对应 `Gateway.selfCheck`：格式 + 确定性（钉死"每次取值都变→全客户端 401"回归）。
pub fn self_check() -> Result<(), String> {
    let a = api_key();
    if a.is_empty() || !a.starts_with("sk-") {
        return Err("自检失败：本地 API Key 格式不对".into());
    }
    if a != api_key() {
        return Err("自检失败：本地 API Key 不稳定（每次取值都变会让所有客户端 401）".into());
    }
    if a.chars().count() != 35 {
        return Err("自检失败：派生 Key 长度不对（应为 sk- + 32 hex）".into());
    }
    Ok(())
}

/// 概览页转发区域显示辅助（与 TraeRegion.current 无关的纯函数保留给 UI）。
pub fn region_label(id: &str) -> &'static str {
    if id == region::SG {
        "国际版 (SG)"
    } else {
        "国服 (CN)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format_and_stability() {
        crate::storage::init(std::env::temp_dir().join(format!("tw-fp-test-{}", std::process::id())));
        let a = api_key();
        assert!(a.starts_with("sk-"));
        assert_eq!(a.chars().count(), 35);
        assert_eq!(a, api_key());
        assert!(self_check().is_ok());
    }

    #[test]
    fn hex_sha256_known_vector() {
        assert_eq!(hex_sha256("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
