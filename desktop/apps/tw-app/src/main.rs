//! tw-app: Tauri v2 壳。业务全在 tw-core；本文件只做 IPC 转发 + 生命周期。
//! 桌面集成项（托盘菜单/体检/自启接线）在 wave-desktop 填充（design.md §桌面体检）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter as _;
use tauri::Manager as _;

static EXITING: AtomicBool = AtomicBool::new(false);

#[derive(serde::Serialize)]
struct IpcResp {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[tauri::command]
async fn ipc(cmd: String, args: serde_json::Value) -> Result<IpcResp, String> {
    match tw_core::ipc::call(&cmd, args).await {
        Ok(data) => Ok(IpcResp { ok: true, data: Some(data), error: None }),
        Err(error) => Ok(IpcResp { ok: false, data: None, error: Some(error) }),
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            tw_core::appcore::init(dir);

            let handle = app.handle().clone();
            tw_core::emitter::set_emitter(Arc::new(move |topic: &str, payload: serde_json::Value| {
                let h = handle.clone();
                let topic = topic.to_string();
                tauri::async_runtime::spawn(async move {
                    let _ = h.emit(&topic, payload);
                });
            }));

            // should_run 三态意图 → 打开即拉起网关（除非用户主动停过）
            if tw_core::appcore::intent::should_autostart_on_open() {
                tauri::async_runtime::spawn(async {
                    if let Err(e) = tw_core::appcore::gateway_start().await {
                        eprintln!("[tw-app] 自动启动网关失败: {}", e);
                    } else {
                        tw_core::appcore::intent::mark_should_run(true);
                    }
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if tw_core::config::get().close_to_tray && !EXITING.load(Ordering::SeqCst) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![ipc])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[allow(dead_code)]
fn request_exit(app: &tauri::AppHandle) {
    EXITING.store(true, Ordering::SeqCst);
    app.exit(0);
}
