//! 全局配置（config.json），语义映射见 analysis/checkin-credits.md §7.3 与 design.md。

use crate::storage::{json_bool, json_i64, store};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

pub const FILE: &str = "config.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CheckinConfig {
    /// BatchCheckin：`schedule_enabled`（默认 false）
    pub trae_enabled: bool,
    /// BatchCheckin：`schedule_time`（默认 08:00，HH:mm 校验失败回落）
    pub trae_time: String,
    /// QoderCheckin：`schedule_enabled` / `schedule_time`（默认 09:00）
    pub qoder_enabled: bool,
    pub qoder_time: String,
}

impl Default for CheckinConfig {
    fn default() -> Self {
        Self { trae_enabled: false, trae_time: "08:00".into(), qoder_enabled: false, qoder_time: "09:00".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StyleConfig {
    /// AppStyle 风格 id：indigo | graphite | moss | sunset（首个默认）
    pub style_id: String,
    /// light | dark | system（Android DayNight 等值）
    pub mode: String,
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self { style_id: "indigo".into(), mode: "system".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    /// prefs`traework2api_prefs`/`gateway_backend`：trae|qoder|auto，默认 auto
    pub gateway_backend: String,
    /// prefs`traework2api_prefs`/`trae_region`：cn|sg
    pub trae_region: String,
    /// CreditMonitor：auto_switch_enabled（默认 true）/ threshold（默认 5，下限 5）
    pub auto_switch_enabled: bool,
    pub auto_switch_threshold: i64,
    pub checkin: CheckinConfig,
    pub style: StyleConfig,
    /// Android 关窗即停→桌面等值：点 X 最小化到托盘，默认开
    pub close_to_tray: bool,
    /// 开机自启：Android 刻意不做；桌面默认保持关，显式开启
    pub autostart: bool,
    /// 安装标识（首启生成，参与 API Key 派生的可选加盐；对齐 Android「不同签名不共享 key」意图）
    pub app_install_id: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            gateway_backend: crate::types::BACKEND_AUTO.into(),
            trae_region: crate::types::region::CN.into(),
            auto_switch_enabled: true,
            auto_switch_threshold: 5,
            checkin: CheckinConfig::default(),
            style: StyleConfig::default(),
            close_to_tray: true,
            autostart: false,
            app_install_id: crate::fingerprint::install_id(),
        }
    }
}

static CACHE: OnceLock<RwLock<Option<AppConfig>>> = OnceLock::new();

fn cache() -> &'static RwLock<Option<AppConfig>> {
    CACHE.get_or_init(|| RwLock::new(None))
}

/// 读全量配置（带进程缓存 + 自愈默认值）。
pub fn get() -> AppConfig {
    {
        let g = cache().read();
        if let Some(c) = g.as_ref() {
            return c.clone();
        }
    }
    let loaded = load_from_disk();
    cache().write().replace(loaded.clone());
    loaded
}

fn load_from_disk() -> AppConfig {
    let raw = store().load_json(FILE, Value::Null);
    if let Ok(c) = serde_json::from_value::<AppConfig>(raw.clone()) {
        return normalize(c);
    }
    let d = AppConfig::default();
    // 缺文件/损坏时落一份默认值（便于审计；语义仍等同 SharedPreferences 的默认值）
    if !raw.is_object() {
        store().save_json(FILE, &serde_json::to_value(&d).unwrap_or_default()).ok();
    }
    d
}

pub fn normalize(mut c: AppConfig) -> AppConfig {
    if c.gateway_backend != crate::types::BACKEND_TRAE && c.gateway_backend != crate::types::BACKEND_QODER {
        c.gateway_backend = crate::types::BACKEND_AUTO.into();
    }
    if c.trae_region != crate::types::region::SG {
        c.trae_region = crate::types::region::CN.into();
    }
    // auto_switch_threshold：coerceAtLeast(5)
    if c.auto_switch_threshold < 5 {
        c.auto_switch_threshold = 5;
    }
    if !is_hhmm(&c.checkin.trae_time) {
        c.checkin.trae_time = "08:00".into();
    }
    if !is_hhmm(&c.checkin.qoder_time) {
        c.checkin.qoder_time = "09:00".into();
    }
    c
}

/// `HH:mm` 校验（BatchCheckin.setTime 语义：非法回落）。
pub fn is_hhmm(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let h = (bytes[0] as i32 - 48) * 10 + (bytes[1] as i32 - 48);
    let mm = (bytes[3] as i32 - 48) * 10 + (bytes[4] as i32 - 48);
    !bytes.iter().any(|b| !b.is_ascii_digit() && *b != b':') && (0..24).contains(&h) && (0..60).contains(&mm)
}

/// 局部更新后立即落盘 + 回填缓存。
pub fn update(f: impl FnOnce(&mut AppConfig)) {
    let mut c = get();
    f(&mut c);
    c = normalize(c);
    store().save_json(FILE, &serde_json::to_value(&c).expect("config 可序列化")).ok();
    *cache().write() = Some(c);
}

/// 兼容测试断言用的快捷 getter。
pub fn field_str(name: &str) -> String {
    let c = get();
    match name {
        "gateway_backend" => c.gateway_backend,
        "trae_region" => c.trae_region,
        _ => String::new(),
    }
}

pub fn debug_raw() -> Value {
    let v = store().load_json(FILE, Value::Null);
    let _ = (json_bool(&v, "autoSwitchEnabled", true), json_i64(&v, "autoSwitchThreshold", 5));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_validation() {
        assert!(is_hhmm("08:00"));
        assert!(is_hhmm("23:59"));
        assert!(!is_hhmm("24:00"));
        assert!(!is_hhmm("8:00"));
        assert!(!is_hhmm("08-00"));
        assert!(!is_hhmm("08:60"));
    }

    /// normalize/Default 会触碰全局 store（install_id），测试先注入临时目录。
    fn ensure_store() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir = std::env::temp_dir().join(format!("tw-core-config-test-{}", std::process::id()));
            crate::storage::init(dir);
        });
    }

    #[test]
    fn threshold_floor_and_backend_fall_back_to_auto() {
        ensure_store();
        let c = AppConfig {
            auto_switch_threshold: 2,
            gateway_backend: "bogus".into(),
            ..Default::default()
        };
        let n = normalize(c);
        assert_eq!(n.auto_switch_threshold, 5);
        assert_eq!(n.gateway_backend, "auto");
    }
}
