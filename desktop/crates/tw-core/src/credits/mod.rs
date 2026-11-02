//! credits 层：转发后余额监测与自动换号（§5）+ 按账号积分查询/缓存/展示（§6）。
//!
//! - [`monitor`]：唯一刷新入口 `note_forwarded`（每平台 60s 节流、urgent 跳节流仍盖时间戳、**禁一切定时轮询**）、
//!   见底保守判定、换号优先级，并把结果写穿到 [`account`] 的按账号缓存；
//! - [`account`]：账号页"每个号各剩多少"的事件驱动查询、`credits.cache.json` 双态落盘、
//!   `— / 数字 / —（原因）` 三态读数与合计规则。
//!
//! 快照 JSON（`credits-snapshot` 事件 + `credits_snapshot` 命令共同载荷）由
//! [`monitor::snapshot_json`] 统一合并两侧生成，字段名对齐
//! 当前工作区 desktop/frontend/src/store/credits.ts 的 `CreditsSnapshot`
//! （必出：`loading/at/trae/qoder/total/error`；桌面扩展：`items/traeTotal/qoderTotal/`
//! `traeLabel/qoderLabel/totalAt/authFailed/monitor`）。

pub mod account;
pub mod monitor;

pub use account::{
    format_value, is_fresh, item_key, label_of, load_cache, save_cache, snapshot, Snapshot as AccountSnapshot, Item, TTL_MS,
};
pub use account::{fetch_all, fetch_one, refresh_all};
pub use monitor::{
    current_snapshot, emit_snapshot, note_forwarded, note_forwarded_at, should_poll, snapshot_json, snapshot_json_now,
    MonitorSnapshot, DEFAULT_INTERVAL_SEC, FORWARD_POLL_MIN_MS, MIN_THRESHOLD,
};

/// `auto_switch_get`：开关 + 阈值（低于下限的值由 `config::normalize` 抬回 5）。
pub fn auto_switch() -> (bool, i64) {
    let c = crate::config::get();
    (c.auto_switch_enabled, c.auto_switch_threshold)
}

/// `auto_switch_set`：写回 config 并回读。
pub fn set_auto_switch(enabled: Option<bool>, threshold: Option<i64>) -> (bool, i64) {
    crate::config::update(|c| {
        if let Some(v) = enabled {
            c.auto_switch_enabled = v;
        }
        if let Some(v) = threshold {
            c.auto_switch_threshold = v;
        }
    });
    auto_switch()
}

/// §5 + §6 全部不变量自检。
pub fn self_check() -> Result<(), String> {
    account::self_check()?;
    monitor::self_check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_snapshot_always_has_frontend_keys() {
        let v = snapshot_json_now();
        let o = v.as_object().expect("快照必为对象");
        for k in ["loading", "at", "trae", "qoder", "total", "error"] {
            assert!(o.contains_key(k), "缺前端字段 {k}");
        }
    }

    #[test]
    fn threshold_floor_is_normalized() {
        // 用纯函数 normalize 验证下限规则（避免并发测试互相踩全局 config）
        let mut c = crate::config::get();
        c.auto_switch_threshold = 0;
        c.auto_switch_enabled = true;
        let n = crate::config::normalize(c);
        assert!(n.auto_switch_threshold >= MIN_THRESHOLD, "旧版本存的 0 必须抬回 5");
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
