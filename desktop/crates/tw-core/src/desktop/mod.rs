//! 桌面集成：局域网地址、体检面板、托盘/自启配置、版本。
//! Android 保活等值决策见 `.monkeycode/analysis/ui-keepalive.md` §5、design.md §桌面体检。
//!
//! title 用词纪律：前端 `keepalive.ts` 用 `new RegExp(HINT).test(title)` 定位"一键修复"目标，
//! HINT 是含空格斜杠的整段短语（子串匹配），网关/关窗/自启三项标题必须字面命中：
//! `网关 / 运行 / 端口 / 监听 / 健康`、`关窗 / 后台`、`自启`。

use crate::types::CheckItem;
use serde_json::Value;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 局域网 IPv4（概览页展示"同 WiFi 设备可连"地址）。
/// UDP connect 技巧：不发包，只让内核选路，取出站接口 IP；无路由/离线返回空串（合法空态）。
pub fn lan_address() -> String {
    let socket = match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    if socket.connect("8.8.8.8:53").is_err() {
        return String::new();
    }
    socket.local_addr().map(|a| a.ip().to_string()).unwrap_or_default()
}

/// TCP 探测本机端口（体检第 1 项的"监听"判据；400ms 足够回环，超时视为没在听）。
fn port_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400)).is_ok()
}

/// 6 项体检（替代 Android 保活面板；顺序与一键配置引导序一致）。
pub async fn checks() -> Result<Value, String> {
    let cfg = crate::config::get();
    let port = crate::gateway::PORT;
    let items = vec![
        {
            let running = crate::appcore::gateway_running();
            let listening = running && port_listening(port);
            CheckItem {
                title: "网关 / 运行 / 端口 / 监听 / 健康".into(),
                ok: listening,
                why: if running && !listening {
                    format!("网关状态显示运行但端口 {} 探测不通：可能被其他程序占用或正在退出，建议停止后重新启动。", port)
                } else {
                    format!("本地网关未运行：端口 {} 无监听，客户端连不上。可点一键配置直接启动。", port)
                },
                auto_fixable: true,
            }
        },
        CheckItem {
            title: "本地 API Key 可读且稳定".into(),
            ok: crate::fingerprint::self_check().is_ok(),
            why: "派生 Key 格式或确定性自检失败：客户端会全部 401。重启应用可恢复；持续失败请反馈。".into(),
            auto_fixable: false,
        },
        CheckItem {
            title: "数据目录可写".into(),
            ok: write_probe(),
            why: "数据目录写入失败：账号/用量/配置都存不进去。检查磁盘空间与目录权限后重试。".into(),
            auto_fixable: false,
        },
        CheckItem {
            title: "系统时钟正常（无明显偏移）".into(),
            ok: clock_sane(),
            why: "系统年份明显异常（早于 2016 或晚于 2100）：token 有效期与签到窗口全部依赖本机时钟，请先校准。".into(),
            auto_fixable: false,
        },
        CheckItem {
            title: "关窗 / 后台常驻（最小化到托盘）".into(),
            ok: cfg.close_to_tray,
            why: "关闭主窗口即退出会让网关断服：桌面等值方案是最小化到托盘继续常驻。可点一键配置直接打开该设置。".into(),
            auto_fixable: true,
        },
        CheckItem {
            title: "开机自启（可选，默认关闭）".into(),
            ok: true,
            why: if cfg.autostart {
                "已开启：开机后自动拉起常驻服务。".to_string()
            } else {
                "未开启（默认）：属可选项，开机后手动打开应用即可，不算异常。".to_string()
            },
            auto_fixable: false,
        },
    ];
    serde_json::to_value(items).map_err(|e| e.to_string())
}

/// 探针写入：save_json 原子写（tmp+rename），成功即目录可写。
fn write_probe() -> bool {
    let store = crate::storage::store();
    let body = serde_json::json!({ "probe": crate::auth::model::now_ms() });
    store.save_json("checks_probe.json", &body).is_ok()
}

fn clock_sane() -> bool {
    use chrono::Datelike;
    let y = chrono::Utc::now().year();
    (2016..=2100).contains(&y)
}

// ---------------------------------------------------------------------
// 托盘 / 自启配置（IPC `close_to_tray_get/set`、`autostart_get/set` 的数据源；
// 平台侧注册动作由 apps/tauri 层执行，本层只存用户意图）
// ---------------------------------------------------------------------

pub fn close_to_tray() -> bool {
    crate::config::get().close_to_tray
}

pub fn set_close_to_tray(on: bool) {
    crate::config::update(|c| c.close_to_tray = on);
}

pub fn autostart() -> bool {
    crate::config::get().autostart
}

pub fn set_autostart(on: bool) {
    crate::config::update(|c| c.autostart = on);
}

/// 自检：LAN 地址可空（离线环境合法），版本非空。
pub fn self_check() -> Result<(), String> {
    if APP_VERSION.is_empty() {
        return Err("自检失败：版本号为空".into());
    }
    let s = lan_address();
    if !s.is_empty() && s.parse::<std::net::IpAddr>().is_err() {
        return Err(format!("自检失败：LAN 地址形态非法: {}", s));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 与 frontend/src/store/keepalive.ts 逐字一致的三段 HINT（RegExp 子串语义）。
    const CLOSE_TO_TRAY_HINT: &str = "关窗 / 后台";
    const AUTOSTART_HINT: &str = "自启";
    const SERVICE_HINT: &str = "网关 / 运行 / 端口 / 监听 / 健康";

    #[tokio::test]
    async fn checks_shape_and_hint_titles() {
        crate::appcore::init(std::env::temp_dir().join(format!("tw-desktop-shape-{}", std::process::id())));
        let raw = checks().await.unwrap();
        let list: Vec<CheckItem> = serde_json::from_value(raw).unwrap();
        assert_eq!(list.len(), 6, "体检必须恰好 6 项");

        let titles: Vec<&str> = list.iter().map(|c| c.title.as_str()).collect();
        // 前端 RegExp.test 语义（HINT 无正则元字符）：等价 title 整段子串包含。
        assert!(titles[0].contains(SERVICE_HINT), "第 1 项 title 必须命中网关 HINT: {}", titles[0]);
        assert!(list.iter().any(|c| c.title.contains(CLOSE_TO_TRAY_HINT)), "须有关窗/后台项");
        assert!(list.iter().any(|c| c.title.contains(AUTOSTART_HINT)), "须有自启项");

        // 一键修复注册表：仅第 1、5 项 autoFixable（其余项点了也只会走"手动设置"文案）。
        let fixable: Vec<usize> = list.iter().enumerate().filter(|(_, c)| c.auto_fixable).map(|(i, _)| i).collect();
        assert_eq!(fixable, vec![0, 4], "autoFixable 只允许网关与关窗两项");
    }

    #[tokio::test]
    async fn close_to_tray_toggle_updates_check() {
        crate::appcore::init(std::env::temp_dir().join(format!("tw-desktop-tray-{}", std::process::id())));
        set_close_to_tray(false);
        let list: Vec<CheckItem> = serde_json::from_value(checks().await.unwrap()).unwrap();
        let tray = list.iter().find(|c| c.title.contains(CLOSE_TO_TRAY_HINT)).unwrap();
        assert!(!tray.ok, "关闭后体检项必须显示未就绪");

        set_close_to_tray(true);
        let list: Vec<CheckItem> = serde_json::from_value(checks().await.unwrap()).unwrap();
        let tray = list.iter().find(|c| c.title.contains(CLOSE_TO_TRAY_HINT)).unwrap();
        assert!(tray.ok);

        // 回写默认值（close_to_tray 默认开），避免污染并行用例的观感。
        set_close_to_tray(true);
    }

    #[test]
    fn lan_address_is_empty_or_valid_ip() {
        let s = lan_address();
        assert!(s.is_empty() || s.parse::<std::net::IpAddr>().is_ok(), "LAN 地址形态非法: {:?}", s);
        assert!(self_check().is_ok());
    }
}
