//! 签到域的持久化（BatchCheckin/QoderCheckin/AutoCheckin 的 SharedPreferences 桌面映射）。
//!
//! design.md §Data Models：`checkin.state.json` 承载
//! `last_run_date/last_results`（trae/qoder 两段）、`qoder_claimed`、
//! `auto_checkin_done`、`auto_checkin_last_try`；key 名沿用 checkin-credits.md §7.3 原文，
//! 便于审计与旧数据迁移。**定时开关/时刻不在这里**——归 `config.json` 的
//! `checkin.{traeEnabled,traeTime,qoderEnabled,qoderTime}`（config::CheckinConfig）。
//!
//! 桌面新增键（Android 由 AlarmManager 承载、桌面需要等价去重）：
//! - `auto_checkin_window_date`：SGT 10:05 开窗链路的"今天已触发"标记（取代一次性闹钟自续）；
//! - `qoder_campaign_variant`：`{mode: "minimal"|"desktop"}`，对应 prefs `qoder_campaign_variant_{mode}`。

use crate::storage::DataStore;
use serde_json::{Map, Value};
use std::sync::Arc;

/// design.md 数据文件清单里的签到状态文件。
pub const FILE: &str = "checkin.state.json";

pub struct CheckinState {
    data: Arc<DataStore>,
}

impl CheckinState {
    pub fn new(data: Arc<DataStore>) -> Self {
        Self { data }
    }

    pub fn global() -> Self {
        Self::new(crate::storage::store())
    }

    fn root(&self) -> Map<String, Value> {
        self.data
            .load_json_opt(FILE)
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    fn save(&self, mut map: Map<String, Value>) {
        let _ = self.data.save_json(FILE, &Value::Object(std::mem::take(&mut map)));
    }

    fn section<'a>(&'a self, map: &'a mut Map<String, Value>, name: &str) -> &'a mut Map<String, Value> {
        if !map.get(name).map(Value::is_object).unwrap_or(false) {
            map.insert(name.into(), Value::Object(Map::new()));
        }
        map.get_mut(name).unwrap().as_object_mut().expect("section 已被规范化为对象")
    }

    // ---- Trae（BatchCheckin §2/§7.3 文件 traework2api_checkin） ----

    pub fn trae_last_run_date(&self) -> Option<String> {
        section_str(&self.root(), "trae", "last_run_date")
    }

    pub fn set_trae_run(&self, date: &str, results: &Value) {
        let mut map = self.root();
        {
            let s = self.section(&mut map, "trae");
            s.insert("last_run_date".into(), Value::String(date.to_string()));
            s.insert("last_results".into(), results.clone());
        }
        self.save(map);
    }

    pub fn trae_last_results(&self) -> Option<Vec<Value>> {
        let map = self.root();
        let arr = map.get("trae")?.get("last_results")?.as_array()?.clone();
        Some(arr)
    }

    // ---- Qoder（QoderCheckin §4/§7.3 文件 qoder_checkin） ----

    pub fn qoder_last_run_date(&self) -> Option<String> {
        section_str(&self.root(), "qoder", "last_run_date")
    }

    pub fn set_qoder_run(&self, date: &str, results: &Value) {
        let mut map = self.root();
        {
            let s = self.section(&mut map, "qoder");
            s.insert("last_run_date".into(), Value::String(date.to_string()));
            s.insert("last_results".into(), results.clone());
        }
        self.save(map);
    }

    pub fn qoder_last_results(&self) -> Option<Vec<Value>> {
        let map = self.root();
        let arr = map.get("qoder")?.get("last_results")?.as_array()?.clone();
        Some(arr)
    }

    /// `qoder_claimed`：`{uid: "yyyy-MM-dd"}`（**裸 uid 键**，与 auto_checkin_done 的带前缀键是两套，§7 陷阱 6）。
    pub fn qoder_claimed_map(&self) -> Map<String, Value> {
        let map = self.root();
        map.get("qoder")
            .and_then(|s| s.get("qoder_claimed"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub fn put_qoder_claimed(&self, uid: &str, date: &str) {
        self.edit_qoder_claimed(|m| {
            m.insert(uid.to_string(), Value::String(date.to_string()));
        });
    }

    pub fn remove_qoder_claimed(&self, uid: &str) {
        self.edit_qoder_claimed(|m| {
            m.remove(uid);
        });
    }

    fn edit_qoder_claimed(&self, f: impl FnOnce(&mut Map<String, Value>)) {
        let mut map = self.root();
        {
            let s = self.section(&mut map, "qoder");
            if !s.get("qoder_claimed").map(Value::is_object).unwrap_or(false) {
                s.insert("qoder_claimed".into(), Value::Object(Map::new()));
            }
            f(s.get_mut("qoder_claimed").unwrap().as_object_mut().expect("已规范化为对象"));
        }
        self.save(map);
    }

    // ---- AutoCheckin（§3.3，prefs traework2api_prefs） ----

    /// `auto_checkin_done`：`{"trae:{uid}"/"qoder:{uid}": "yyyy-MM-dd"}`；坏数据当空表（runCatching 同构）。
    pub fn auto_done_map(&self) -> Map<String, Value> {
        self.root()
            .get("auto_checkin_done")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    pub fn mark_auto_done(&self, key: &str, date: &str) {
        let mut map = self.root();
        if !map.get("auto_checkin_done").map(Value::is_object).unwrap_or(false) {
            map.insert("auto_checkin_done".into(), Value::Object(Map::new()));
        }
        map.get_mut("auto_checkin_done")
            .unwrap()
            .as_object_mut()
            .expect("已规范化为对象")
            .insert(key.to_string(), Value::String(date.to_string()));
        self.save(map);
    }

    /// `auto_checkin_last_try`（unix 毫秒；0=从未）。
    pub fn auto_last_try(&self) -> i64 {
        crate::storage::json_i64(&Value::Object(self.root()), "auto_checkin_last_try", 0)
    }

    pub fn set_auto_last_try(&self, ms: i64) {
        let mut map = self.root();
        map.insert("auto_checkin_last_try".into(), Value::Number(ms.into()));
        self.save(map);
    }

    /// 桌面新增：SGT 开窗去重日期（`yyyy-MM-dd`，按 Asia/Singapore）。
    pub fn auto_window_date(&self) -> Option<String> {
        self.root().get("auto_checkin_window_date").and_then(Value::as_str).map(str::to_string)
    }

    pub fn set_auto_window_date(&self, date: &str) {
        let mut map = self.root();
        map.insert("auto_checkin_window_date".into(), Value::String(date.to_string()));
        self.save(map);
    }

    // ---- Qoder 活动档位（§1.2 阶梯：`qoder_campaign_variant_{mode}`） ----

    /// 只认 desktop/minimal，其余（含缺失）一律 minimal（campaignVariant 语义）。
    pub fn campaign_variant(&self, mode: &str) -> &'static str {
        let root = self.root();
        let raw = root
            .get("qoder_campaign_variant")
            .and_then(|m| m.get(mode))
            .and_then(Value::as_str)
            .unwrap_or("");
        match raw {
            "desktop" => "desktop",
            _ => "minimal",
        }
    }

    pub fn remember_campaign_variant(&self, mode: &str, variant: &str) {
        let mut map = self.root();
        if !map.get("qoder_campaign_variant").map(Value::is_object).unwrap_or(false) {
            map.insert("qoder_campaign_variant".into(), Value::Object(Map::new()));
        }
        map.get_mut("qoder_campaign_variant")
            .unwrap()
            .as_object_mut()
            .expect("已规范化为对象")
            .insert(mode.to_string(), Value::String(variant.to_string()));
        self.save(map);
    }
}

fn section_str(map: &Map<String, Value>, section: &str, key: &str) -> Option<String> {
    map.get(section)?.get(key)?.as_str().map(str::to_string)
}

// ---------------------------------------------------------------------------
// 日期口径（§7.1：`yyyy-MM-dd` Locale.US 语义 = ISO 零填充；done 日期按设备本地、开窗按 SGT）
// ---------------------------------------------------------------------------

/// 设备本地时区的 `yyyy-MM-dd`（today() 语义，now_ms 注入便于测试）。
pub fn today_local(now_ms: i64) -> String {
    naive_local(now_ms, true).map(|n| n.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// Asia/Singapore（UTC+8，无 DST）的 `yyyy-MM-dd`。
pub fn today_sgt(now_ms: i64) -> String {
    naive_local(now_ms, false).map(|n| n.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// §7.1 日期口径不变量（键写死会让"每天一次"退化成"永远只跑一次"）。
pub fn self_check() -> Result<(), String> {
    let is_date = |s: &str| {
        s.len() == 10 && s.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() })
    };
    if !is_date(&today_local(0)) {
        return Err("自检失败：日期必须是零填充 yyyy-MM-dd".into());
    }
    // 开窗按 SGT 换算：UTC 23:30 在 SGT 已是次日（钉死"两个时区各算各的"）
    use chrono::{TimeZone, Utc};
    let ms = Utc.with_ymd_and_hms(2026, 11, 2, 23, 30, 0).single().expect("固定日期可定位").timestamp_millis();
    if today_sgt(ms) != "2026-11-03" {
        return Err(format!("自检失败：SGT 日期换算不对，得 {}", today_sgt(ms)));
    }
    Ok(())
}

fn naive_local(now_ms: i64, local: bool) -> Option<chrono::NaiveDateTime> {
    use chrono::{Local, TimeZone, Utc};
    if local {
        Local.timestamp_millis_opt(now_ms).single().map(|d| d.naive_local())
    } else {
        let sgt = chrono::FixedOffset::east_opt(8 * 3600)?;
        let dt = Utc.timestamp_millis_opt(now_ms).single()?;
        Some(dt.with_timezone(&sgt).naive_local())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn state(tag: &str) -> CheckinState {
        let dir = std::env::temp_dir().join(format!("tw-checkin-state-{}-{}", std::process::id(), tag));
        let _ = std::fs::create_dir_all(&dir);
        CheckinState::new(Arc::new(DataStore::new(dir)))
    }

    #[test]
    fn trae_and_qoder_sections_isolated() {
        let s = state("sec");
        s.set_trae_run("2026-11-02", &serde_json::json!([{"uid": "a"}]));
        s.set_qoder_run("2026-11-01", &serde_json::json!([]));
        assert_eq!(s.trae_last_run_date().as_deref(), Some("2026-11-02"));
        assert_eq!(s.qoder_last_run_date().as_deref(), Some("2026-11-01"));
        assert_eq!(s.trae_last_results().unwrap().len(), 1);
        assert_eq!(s.qoder_last_results().unwrap().len(), 0);
    }

    #[test]
    fn claimed_and_done_maps_roundtrip_only_touch_own_key() {
        let s = state("maps");
        s.put_qoder_claimed("u1", "2026-11-02");
        s.put_qoder_claimed("u2", "2026-11-02");
        s.remove_qoder_claimed("u1");
        let m = s.qoder_claimed_map();
        assert!(!m.contains_key("u1"));
        assert_eq!(m.get("u2").and_then(Value::as_str), Some("2026-11-02"));

        s.mark_auto_done("trae:abc", "2026-11-02");
        s.mark_auto_done("qoder:abc", "2026-11-03");
        let d = s.auto_done_map();
        // §3.3 自检：键必须带平台前缀，同 uid 两平台互不顶掉
        assert_eq!(d.get("trae:abc").and_then(Value::as_str), Some("2026-11-02"));
        assert_eq!(d.get("qoder:abc").and_then(Value::as_str), Some("2026-11-03"));
    }

    #[test]
    fn variant_only_accepts_two_spellings() {
        let s = state("variant");
        assert_eq!(s.campaign_variant("cn"), "minimal");
        s.remember_campaign_variant("cn", "desktop");
        assert_eq!(s.campaign_variant("cn"), "desktop");
        s.remember_campaign_variant("global", "garbage");
        assert_eq!(s.campaign_variant("global"), "minimal");
    }

    #[test]
    fn auto_last_try_and_window_date() {
        let s = state("try");
        assert_eq!(s.auto_last_try(), 0);
        s.set_auto_last_try(123);
        assert_eq!(s.auto_last_try(), 123);
        assert_eq!(s.auto_window_date(), None);
        s.set_auto_window_date("2026-11-02");
        assert_eq!(s.auto_window_date().as_deref(), Some("2026-11-02"));
    }

    #[test]
    fn sgt_day_shifts_at_utc_16() {
        // 2026-01-02 15:59 UTC = SGT 23:59（同一天）；16:00 UTC = SGT 次日 00:00
        let before = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
            .unwrap()
            .and_hms_opt(15, 59, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let after = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
            .unwrap()
            .and_hms_opt(16, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(today_sgt(before), "2026-01-02");
        assert_eq!(today_sgt(after), "2026-01-03");
    }

    #[test]
    fn today_local_format() {
        let s = today_local(1_730_000_000_000);
        assert!(regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap().is_match(&s), "got {s}");
    }
}
