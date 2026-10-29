// Tauri 壳：命令层（PORT-TODO(main)：实现体由主线填充；下面的命令签名与 JSON 结构是
// 前后端契约，UI 移植以这里为准）。
use serde_json::{json, Value};
use tauri::State;
use traework2api_core::state::AppState;

pub type Cm = State<'_, std::sync::Arc<AppState>>;

#[tauri::command]
pub async fn overview(_s: Cm) -> Result<Value, String> {
    todo!("overview")
}

#[tauri::command]
pub async fn server_start(_s: Cm) -> Result<(), String> { todo!("server_start") }

#[tauri::command]
pub async fn server_stop(_s: Cm) -> Result<(), String> { todo!("server_stop") }

/// 账号页 - Trae：[{uid,nickname,regionId,regionName,isSupported,expiresAt,active,credit:{text,error}|null}]
#[tauri::command]
pub async fn trae_accounts(_s: Cm) -> Result<Value, String> { todo!("trae_accounts") }

/// 登录第 1 步：生成国服登录链接 → {url, regionId, regionName}
#[tauri::command]
pub async fn trae_login_prepare(_s: Cm) -> Result<Value, String> { todo!("trae_login_prepare") }

/// 登录第 2 步：粘贴浏览器回调链接 → {uid,nickname,regionId}
#[tauri::command]
pub async fn trae_login_finish(_s: Cm, callback_url: String) -> Result<Value, String> {
    let _ = callback_url;
    todo!("trae_login_finish")
}

#[tauri::command]
pub async fn trae_set_active(_s: Cm, uid: String) -> Result<(), String> { let _ = uid; todo!() }

#[tauri::command]
pub async fn trae_remove(_s: Cm, uid: String) -> Result<(), String> { let _ = uid; todo!() }

/// 当前区域：{id,name,isSupported,note}
#[tauri::command]
pub async fn trae_region_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn trae_region_set(_s: Cm, id: String) -> Result<(), String> { let _ = id; todo!() }

/// [{uid,name,email,mode,expiresAt,active,credit:{text,error}|null,fingerprintSet:bool}]
#[tauri::command]
pub async fn qoder_accounts(_s: Cm) -> Result<Value, String> { todo!() }

/// 粘贴 `pt-` PAT 导入 → {uid,name,email,mode}
#[tauri::command]
pub async fn qoder_add_pat(_s: Cm, pat: String) -> Result<Value, String> { let _ = pat; todo!() }

#[tauri::command]
pub async fn qoder_set_active(_s: Cm, uid: String) -> Result<(), String> { let _ = uid; todo!() }

#[tauri::command]
pub async fn qoder_remove(_s: Cm, uid: String) -> Result<(), String> { let _ = uid; todo!() }

/// {selection:"trae|qoder|auto", resolved, label, mixed, traeHas, qoderHas}
#[tauri::command]
pub async fn backend_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn backend_set(_s: Cm, b: String) -> Result<(), String> { let _ = b; todo!() }

/// {loaded, count, traeLoaded, qoderLoaded, items:[...OpenAI 模型对象含 owned_by]}
#[tauri::command]
pub async fn models_get(_s: Cm) -> Result<Value, String> { todo!() }

/// 刷新两个平台的模型表 → {trae:{ok,count,msg}, qoder:{ok,count,msg,free,dynamic}}
#[tauri::command]
pub async fn models_refresh(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn qoder_models_import_blob(_s: Cm, text: String, uid: String) -> Result<Value, String> {
    let _ = (text, uid);
    todo!()
}

#[tauri::command]
pub async fn qoder_models_cli_hint(_s: Cm, uid: String) -> Result<Value, String> { let _ = uid; todo!() }

/// {items:{"<platform>:<uid>":{text,error}}, totals:{trae:String, qoder:String}}
#[tauri::command]
pub async fn credits_snapshot(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn credits_refresh(_s: Cm, force: bool) -> Result<Value, String> { let _ = force; todo!() }

#[tauri::command]
pub async fn credits_forget(_s: Cm, platform: String, uid: String) -> Result<(), String> {
    let _ = (platform, uid);
    todo!()
}

/// 一键签到全部账号 → {results:[{platform,uid,name,ok,msg}]}
#[tauri::command]
pub async fn checkin_run_all(_s: Cm) -> Result<Value, String> { todo!() }

/// 每日定时签到（Trae 侧）：{enabled,timeText}
#[tauri::command]
pub async fn auto_checkin_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn auto_checkin_set(_s: Cm, enabled: bool) -> Result<(), String> { let _ = enabled; todo!() }

/// Qoder 每日领取：{enabled,timeText,lastResults:[...]}
#[tauri::command]
pub async fn qoder_daily_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn qoder_daily_set(_s: Cm, enabled: bool, time_text: String) -> Result<(), String> {
    let _ = (enabled, time_text);
    todo!()
}

/// 按模型用量统计
#[tauri::command]
pub async fn usage_summary(_s: Cm) -> Result<Value, String> { todo!() }

/// {theme:"light|dark", colorStyle:String}
#[tauri::command]
pub async fn settings_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn settings_set(_s: Cm, theme: String, color_style: String) -> Result<(), String> {
    let _ = (theme, color_style);
    todo!()
}

/// 积分监测：{autoSwitch, threshold, interval}
#[tauri::command]
pub async fn credit_monitor_get(_s: Cm) -> Result<Value, String> { todo!() }

#[tauri::command]
pub async fn credit_monitor_set(_s: Cm, auto_switch: bool, threshold: i64, interval: i64) -> Result<(), String> {
    let _ = (auto_switch, threshold, interval);
    todo!()
}

/// {lines:[...最近日志]}
#[tauri::command]
pub async fn logs_recent(_s: Cm) -> Result<Value, String> {
    Ok(json!({ "lines": traework2api_core::logbus::recent() }))
}

/// 导出账号 JSON（明文 token）→ {trae:String, qoder:String}
#[tauri::command]
pub async fn export_accounts(_s: Cm) -> Result<Value, String> { todo!() }

/// 导入账号 JSON → {trae:n, qoder:m}
#[tauri::command]
pub async fn import_accounts(_s: Cm, trae: String, qoder: String) -> Result<Value, String> {
    let _ = (trae, qoder);
    todo!()
}

/// 保活体检 → [{name,ok,detail}]
#[tauri::command]
pub async fn keepalive_check(_s: Cm) -> Result<Value, String> { todo!() }

/// 用系统浏览器打开链接（登录用）
#[tauri::command]
pub async fn open_browser(_app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    _app.opener().open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

/// 自检面板 → [{name,ok,msg}]
#[tauri::command]
pub async fn self_check_run(_s: Cm) -> Result<Value, String> { todo!() }

pub fn run() {
    let app_data = dirs_data_dir();
    let state = AppState::new(app_data);
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            overview, server_start, server_stop,
            trae_accounts, trae_login_prepare, trae_login_finish, trae_set_active, trae_remove,
            trae_region_get, trae_region_set,
            qoder_accounts, qoder_add_pat, qoder_set_active, qoder_remove,
            backend_get, backend_set,
            models_get, models_refresh, qoder_models_import_blob, qoder_models_cli_hint,
            credits_snapshot, credits_refresh, credits_forget,
            checkin_run_all, auto_checkin_get, auto_checkin_set,
            qoder_daily_get, qoder_daily_set,
            usage_summary, settings_get, settings_set,
            credit_monitor_get, credit_monitor_set,
            logs_recent, export_accounts, import_accounts,
            keepalive_check, open_browser, self_check_run,
        ])
        .setup(|_app| Ok(()))
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn dirs_data_dir() -> std::path::PathBuf {
    // 与 Android 端独立：桌面用系统应用数据目录
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join(".traework2api")
}
