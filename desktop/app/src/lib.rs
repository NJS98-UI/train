// Tauri 壳：命令层。
// 前后端契约：UI（ui/app.js）只经这些命令取数；命令体只做「取状态 / 调 core 模块 / 拼形状」，
// 业务逻辑全部在 traework2api-core（对齐 Android 的各 object 单例）。
use serde_json::{json, Value};
use tauri::{Manager, State};
use traework2api_core::gateway::{backend, server};
use traework2api_core::state::AppState;
use traework2api_core::{aux, log_i, log_e, log_w, logbus, qoder, trae};

pub type Cm = State<'_, std::sync::Arc<AppState>>;

/// State<Arc<AppState>> → &AppState（命令体统一从这里拿上下文）
fn st<'a>(s: &'a Cm) -> &'a AppState {
    s.inner().as_ref()
}

/// prefs 里积分快照的「更新时间」（ms）；Android 由 AccountCredits 快照自带，
/// 桌面版快照结构在 core/aux::account_credits，这里用独立键记录刷新时刻。
const KEY_CREDITS_AT: &str = "credits_updated_at";
/// 用户对服务的意图（对齐 GatewayService PREFS "traework2api_service" / KEY "should_run"；
/// 桌面版只有一份 prefs，键名沿用）。键存在 = 用户表达过意图。
const KEY_SHOULD_RUN: &str = "should_run";

// ==================================================================
// 概览页
// ==================================================================

/// 概览页服务卡：{running, port, lan, apiKey}
/// （对齐 MainActivity.overviewPage 的状态 chip + 服务地址/Key 强调卡；
/// Key 打码与否由 UI 决定，后端始终给全量）
#[tauri::command]
pub async fn overview(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "running": server::is_running(app),
        "port": server::PORT,
        "lan": lan_address(),
        "apiKey": server::api_key(app),
    }))
}

#[tauri::command]
pub async fn server_start(s: Cm) -> Result<(), String> {
    let app = s.inner().clone();
    // 对齐 GatewayService.start：先落「用户要开」的意图，再拉网关
    app.set_pref(KEY_SHOULD_RUN, "true");
    server::start(app).await
}

#[tauri::command]
pub async fn server_stop(s: Cm) -> Result<(), String> {
    let app = s.inner().clone();
    // 对齐 GatewayService.stop：意图落 false，「除非用户手动停止」的承诺靠它兑现
    app.set_pref(KEY_SHOULD_RUN, "false");
    server::stop(&app);
    Ok(())
}

/// 局域网地址（对齐 MainActivity.lanAddress：site-local IPv4 优先 → 链路本地兜底 → 剥 %scope）。
/// 桌面无 NetworkInterface 枚举：走「UDP 连公网地址看本地端点」拿默认路由口的 IPv4，
/// 与「site-local 优先」在同一张典型桌面网卡上等价；拿不到返回 None（UI 显示 127.0.0.1）。
fn lan_address() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let addr = sock.local_addr().ok()?;
    let ip = addr.ip();
    if !ip.is_ipv4() {
        return None;
    }
    let mut s = ip.to_string();
    if let Some(i) = s.find('%') {
        s.truncate(i); // 对齐 Kotlin 剥 %scope（v4 不会有，防御性保留）
    }
    Some(s)
}

// ==================================================================
// 账号页 - Trae
// ==================================================================

fn credit_json(
    app: &AppState,
    snap: &aux::account_credits::Snapshot,
    platform: &str,
    uid: &str,
) -> Value {
    let item = if platform == aux::account_credits::TRAE {
        aux::account_credits::trae_of(snap, uid)
    } else {
        aux::account_credits::qoder_of(snap, uid)
    };
    match item {
        Some(it) => json!({"text": aux::account_credits::label(Some(&it)), "error": it.error}),
        None => Value::Null,
    }
}

fn credits_snapshot_for(app: &AppState) -> Value {
    aux::account_credits::load_cache(app);
    let snap = aux::account_credits::current();
    let mut items = serde_json::Map::new();
    for m in trae::auth_store::list(app) {
        items.insert(
            format!("{}:{}", aux::account_credits::TRAE, m.uid),
            credit_json(app, &snap, aux::account_credits::TRAE, &m.uid),
        );
    }
    for m in qoder::auth_store::list(app) {
        items.insert(
            format!("{}:{}", aux::account_credits::QODER, m.uid),
            credit_json(app, &snap, aux::account_credits::QODER, &m.uid),
        );
    }
    let at = app.pref_or(KEY_CREDITS_AT, "0").parse::<i64>().unwrap_or(0);
    json!({
        "items": Value::Object(items),
        "totals": {
            "trae": aux::account_credits::total_label(&snap, aux::account_credits::TRAE),
            "qoder": aux::account_credits::total_label(&snap, aux::account_credits::QODER),
        },
        "at": at,
    })
}

/// Trae 账号列表：[{uid,nickname,regionId,regionName,isSupported,expiresAt,active,credit:{text,error}|null}]
#[tauri::command]
pub async fn trae_accounts(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let snap = {
        aux::account_credits::load_cache(app);
        aux::account_credits::current()
    };
    let active = trae::auth_store::active_uid(app);
    let arr: Vec<Value> = trae::auth_store::list(app)
        .iter()
        .map(|m| {
            let reg = trae::auth_store::meta_region(m);
            json!({
                "uid": m.uid,
                "nickname": m.nickname,
                "regionId": m.region_id,
                "regionName": reg.display_name(),
                "isSupported": reg.is_supported(),
                "expiresAt": m.expires_at,
                "active": active.as_deref() == Some(m.uid.as_str()),
                "credit": credit_json(app, &snap, aux::account_credits::TRAE, &m.uid),
            })
        })
        .collect();
    Ok(Value::Array(arr))
}

/// 登录第 1 步：生成当前区域的登录链接 → {url, regionId, regionName}
#[tauri::command]
pub async fn trae_login_prepare(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let reg = trae::region::TraeRegion::current(app);
    Ok(json!({
        "url": trae::auth_api::build_login_url(),
        "regionId": reg.id(),
        "regionName": reg.display_name(),
    }))
}

/// 登录第 2 步：粘贴浏览器回调链接（finish_login 内部落盘）→ {uid,nickname,regionId}
#[tauri::command]
pub async fn trae_login_finish(s: Cm, callback_url: String) -> Result<Value, String> {
    let app = s.inner().clone();
    let cred = trae::auth_api::finish_login(&app, &callback_url).await?;
    Ok(json!({"uid": cred.uid, "nickname": cred.nickname, "regionId": cred.region_id}))
}

/// 切号（对齐 doSwitchAccount）：setActive → 区域以账号为准 → 积分缓存作废。
/// 桌面积分快照由 credits_refresh(Force) 重建，这里只做前两步 + 记日志。
#[tauri::command]
pub async fn trae_set_active(s: Cm, uid: String) -> Result<(), String> {
    let app = st(&s);
    trae::auth_store::set_active(app, &uid);
    trae::auth_store::sync_region_to_active_account(app);
    log_i!("TraeAuthStore", "已切换到 {}", uid);
    Ok(())
}

#[tauri::command]
pub async fn trae_remove(s: Cm, uid: String) -> Result<(), String> {
    let app = st(&s);
    trae::auth_store::remove(app, &uid);
    log_i!("TraeAuthStore", "已删除账号 {}", uid);
    Ok(())
}

/// 当前区域：{id,name,isSupported,note}
#[tauri::command]
pub async fn trae_region_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let r = trae::region::TraeRegion::current(app);
    Ok(json!({
        "id": r.id(),
        "name": r.display_name(),
        "isSupported": r.is_supported(),
        "note": r.unsupported_note(),
    }))
}

#[tauri::command]
pub async fn trae_region_set(s: Cm, id: String) -> Result<(), String> {
    let app = st(&s);
    let r = trae::region::TraeRegion::from_id(&id);
    trae::region::TraeRegion::set_current(app, r);
    trae::auth_store::sync_region_to_active_account(app);
    log_i!("TraeRegion", "区域已切换为 {}", r.display_name());
    Ok(())
}

// ==================================================================
// 账号页 - Qoder
// ==================================================================

/// [{uid,name,email,mode,expiresAt,active,credit:{text,error}|null,fingerprintSet:bool}]
#[tauri::command]
pub async fn qoder_accounts(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let snap = {
        aux::account_credits::load_cache(app);
        aux::account_credits::current()
    };
    let active = qoder::auth_store::active_uid(app);
    let arr: Vec<Value> = qoder::auth_store::list(app)
        .iter()
        .map(|m| {
            json!({
                "uid": m.uid,
                "name": m.name,
                "email": m.email,
                "mode": m.mode,
                "expiresAt": m.expires_at,
                "active": active.as_deref() == Some(m.uid.as_str()),
                "credit": credit_json(app, &snap, aux::account_credits::QODER, &m.uid),
                "fingerprintSet": qoder::auth_store::fingerprint(app, &m.uid).is_some(),
            })
        })
        .collect();
    Ok(Value::Array(arr))
}

/// 粘贴 `pt-` PAT 导入（login_with_pat 走 PAT 换 jobToken 全流程；落盘由 save 做）
#[tauri::command]
pub async fn qoder_add_pat(s: Cm, pat: String) -> Result<Value, String> {
    let app = s.inner().clone();
    let cred = qoder::auth_api::login_with_pat(&app, &pat).await?;
    qoder::auth_store::save(&app, &cred);
    log_i!("QoderAuthStore", "PAT 导入成功 uid={} mode={}", cred.uid, cred.mode);
    Ok(json!({"uid": cred.uid, "name": cred.name, "email": cred.email, "mode": cred.mode}))
}

#[tauri::command]
pub async fn qoder_set_active(s: Cm, uid: String) -> Result<(), String> {
    let app = st(&s);
    qoder::auth_store::set_active(app, &uid);
    log_i!("QoderAuthStore", "已切换到 {}", uid);
    Ok(())
}

#[tauri::command]
pub async fn qoder_remove(s: Cm, uid: String) -> Result<(), String> {
    let app = st(&s);
    qoder::auth_store::remove(app, &uid);
    log_i!("QoderAuthStore", "已删除账号 {}", uid);
    Ok(())
}

// ==================================================================
// 后端选择（GatewayBackend）
// ==================================================================

/// {selection:"trae|qoder|auto", resolved, label, mixed, traeHas, qoderHas}
#[tauri::command]
pub async fn backend_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "selection": backend::selection(app),
        "resolved": backend::resolved(app),
        "label": backend::describe(app),
        "mixed": backend::is_mixed(app),
        "traeHas": backend::has_trae_account(app),
        "qoderHas": backend::has_qoder_account(app),
    }))
}

#[tauri::command]
pub async fn backend_set(s: Cm, b: String) -> Result<(), String> {
    let app = st(&s);
    backend::set(app, &b);
    Ok(())
}

// ==================================================================
// 模型页
// ==================================================================

/// {loaded, count, traeLoaded, qoderLoaded, items:[...OpenAI 模型对象含 owned_by]}
#[tauri::command]
pub async fn models_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let arr = backend::models_for(app);
    let count = arr.as_array().map(|a| a.len()).unwrap_or(0);
    let trae_loaded = aux::models_repo::current()
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let qoder_loaded = qoder::model_catalog::openai_models()
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    Ok(json!({
        "loaded": backend::models_loaded(app),
        "count": count,
        "traeLoaded": trae_loaded,
        "qoderLoaded": qoder_loaded,
        "items": arr,
    }))
}

fn trae_model_count() -> usize {
    aux::models_repo::current().as_array().map(|a| a.len()).unwrap_or(0)
}

/// 从 Qoder 官方拉模型目录（对齐 syncQoderCatalog：COSY 动态目录优先 → openapi 兜底；
/// 失败不清空现有清单，内置清单永远兜底）→ {ok,count,msg,free,dynamic}
async fn sync_qoder_catalog_impl(app: &AppState) -> Value {
    let cred = match qoder::auth_store::load(app) {
        Some(c) => c,
        None => {
            return json!({
                "ok": false, "count": qoder::model_catalog::current().len(),
                "msg": "先添加一个 Qoder 账号，才能拉官方模型目录",
                "free": qoder::model_catalog::free_count(),
                "dynamic": qoder::model_catalog::has_dynamic(),
            })
        }
    };
    let jt = qoder::auth_api::valid_job_token_for(app, &cred, 60 * 60 * 1000)
        .await
        .or_else(|| {
            if cred.job_token.is_empty() {
                None
            } else {
                Some(cred.job_token.clone())
            }
        });
    let mut raw: Option<String> = None;
    let mut parsed_ok = false;
    if let Some(jt) = jt {
        // ① 首选：COSY 签名的动态目录（与客户端模型选择器同源，「此刻」最准）
        raw = qoder::upstream::fetch_model_list_raw(app, &cred, &jt).await;
        if let Some(r) = &raw {
            parsed_ok = !qoder::model_catalog::parse_catalog_json(r).is_empty();
        }
        if !parsed_ok {
            // ② 退一步：openapi 的 /api/v1/models（响应形状没实测过，能出就用）
            let alt = qoder::auth_api::fetch_model_catalog(&jt, &cred.mode).await;
            let alt_ok = alt
                .as_deref()
                .map(|a| !qoder::model_catalog::parse_catalog_json(a).is_empty())
                .unwrap_or(false);
            if alt_ok {
                raw = alt.clone();
                parsed_ok = true;
            } else if raw.is_none() {
                raw = alt;
            }
        }
    }
    if parsed_ok {
        if let Some(r) = &raw {
            qoder::model_catalog::set_dynamic(qoder::model_catalog::parse_catalog_json(r));
            qoder::model_catalog::save(app, r);
            log_i!("QoderModelCatalog", "官方目录已同步并落盘");
        }
    }
    let total = qoder::model_catalog::current().len();
    let free = qoder::model_catalog::free_count();
    let dynamic = qoder::model_catalog::has_dynamic();
    let msg = if dynamic {
        format!("官方目录已同步：{} 个模型（{} 个免费）", total, free)
    } else {
        let why = qoder::auth_api::last_catalog_error()
            .map(|e| e.chars().take(120).collect::<String>())
            .unwrap_or_default();
        format!("拉取失败，继续用内置 {} 个模型{}", total, if why.is_empty() { String::new() } else { format!("：{}", why) })
    };
    json!({"ok": dynamic, "count": total, "msg": msg, "free": free, "dynamic": dynamic})
}

/// Trae 侧刷新（对齐 refreshModels 的 Trae 分支：拉不到保持现有状态）
async fn trae_refresh_impl(app: &AppState) -> Value {
    let cred = trae::auth_store::load(app).filter(|c| !c.access_token.is_empty());
    match cred {
        None => json!({"ok": false, "count": trae_model_count(), "msg": "未登录 Trae，请先登录"}),
        Some(c) => match aux::models_repo::refresh(app, &c.access_token).await {
            Ok(n) => json!({"ok": true, "count": n, "msg": ""}),
            Err(e) => json!({"ok": false, "count": trae_model_count(), "msg": e}),
        },
    }
}

/// 刷新两个平台的模型表（对齐 refreshModels 的平台分派）→ {trae:{ok,count,msg}, qoder:{...}}
#[tauri::command]
pub async fn models_refresh(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    let (trae_res, qoder_res);
    if backend::is_qoder(app) {
        // Qoder 后端：只拉官方目录
        qoder_res = sync_qoder_catalog_impl(app).await;
        trae_res = json!({"ok": false, "count": 0, "msg": ""});
    } else {
        // 并集模式：Qoder 目录也顺手刷一次（失败不影响 Trae 刷新）
        if backend::is_mixed(app) {
            qoder_res = sync_qoder_catalog_impl(app).await;
        } else {
            qoder_res = json!({"ok": false, "count": 0, "msg": "", "free": 0, "dynamic": false});
        }
        trae_res = trae_refresh_impl(app).await;
    }
    Ok(json!({"trae": trae_res, "qoder": qoder_res}))
}

/// 导入 Qoder CLI 模型目录缓存 → {ok, models, decrypted, total}
/// 落盘必须存明文（密文下次 parse 不出来，等于白导）——对齐 MainActivity 注释。
#[tauri::command]
pub async fn qoder_models_import_blob(s: Cm, text: String, uid: String) -> Result<Value, String> {
    let app = st(&s);
    let r = qoder::model_cache::import_blob(app, &text, &uid);
    if !r.ok {
        let msg = if r.error.is_empty() { r.msg.clone() } else { r.error.clone() };
        return Err(msg);
    }
    if let Some(plain) = &r.plain {
        qoder::model_catalog::save(app, plain);
    }
    Ok(json!({
        "ok": true,
        "models": r.models,
        "decrypted": r.decrypted,
        "total": qoder::model_catalog::current().len(),
    }))
}

#[tauri::command]
pub async fn qoder_models_cli_hint(s: Cm, uid: String) -> Result<Value, String> {
    let _ = st(&s);
    Ok(json!(qoder::model_cache::cli_hint(&uid)))
}

// ==================================================================
// 积分 / 签到
// ==================================================================

/// {items:{"<platform>:<uid>":{text,error}}, totals:{trae,qoder}, at}
#[tauri::command]
pub async fn credits_snapshot(s: Cm) -> Result<Value, String> {
    Ok(credits_snapshot_for(st(&s)))
}

/// 刷新全部账号积分（force=true 跳过缓存 TTL）→ 同 credits_snapshot 形状
#[tauri::command]
pub async fn credits_refresh(s: Cm, force: bool) -> Result<Value, String> {
    let app = st(&s);
    let _snap = aux::account_credits::refresh_all(app, force).await;
    app.set_pref(KEY_CREDITS_AT, &chrono::Utc::now().timestamp_millis().to_string());
    Ok(credits_snapshot_for(app))
}

#[tauri::command]
pub async fn credits_forget(s: Cm, platform: String, uid: String) -> Result<(), String> {
    let app = st(&s);
    aux::account_credits::forget(app, &platform, &uid);
    Ok(())
}

/// 一键签到全部账号（逐个签，不只当前那个）→ {results:[{platform,uid,name,ok,msg}]}
#[tauri::command]
pub async fn checkin_run_all(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(aux::batch_checkin::checkin_all(app).await)
}

/// Trae 每日定时签到：{enabled,timeText}
#[tauri::command]
pub async fn auto_checkin_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "enabled": aux::auto_checkin::is_enabled(app),
        "timeText": aux::auto_checkin::time_text(app),
    }))
}

#[tauri::command]
pub async fn auto_checkin_set(s: Cm, enabled: bool) -> Result<(), String> {
    let app = st(&s);
    aux::auto_checkin::set_enabled(app, enabled);
    Ok(())
}

/// Qoder 每日领取：{enabled,timeText,lastResults:[...]}
#[tauri::command]
pub async fn qoder_daily_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "enabled": qoder::checkin::is_enabled(app),
        "timeText": qoder::checkin::time_text(app),
        "lastResults": qoder::checkin::last_results(app),
    }))
}

#[tauri::command]
pub async fn qoder_daily_set(s: Cm, enabled: bool, time_text: String) -> Result<(), String> {
    let app = st(&s);
    if enabled {
        qoder::checkin::enable(app, &time_text);
        log_i!("QoderCheckin", "定时领取已开启，每天 {}", time_text);
    } else {
        qoder::checkin::disable(app);
        log_i!("QoderCheckin", "定时领取已关闭");
    }
    Ok(())
}

// ==================================================================
// 记录页 / 设置 / 监测
// ==================================================================

/// 按模型用量统计：{records:[{ts,model,prompt,completion,total,ms,ok,err}], groups:[{model,total,output}]}
#[tauri::command]
pub async fn usage_summary(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(aux::usage_store::summary(app))
}

/// {theme:"system|light|dark", colorStyle:"indigo|graphite|moss|sunset"}
/// （键名对齐 AppStyle：PREFS ui_appearance / style_id / mode_id；UI 只展示 light|dark，
/// 存储保留原值 —— 跟 Android「跟随系统」语义一致）
#[tauri::command]
pub async fn settings_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "theme": app.pref_or("mode_id", "system"),
        "colorStyle": app.pref_or("style_id", "indigo"),
    }))
}

#[tauri::command]
pub async fn settings_set(s: Cm, theme: String, color_style: String) -> Result<(), String> {
    let app = st(&s);
    let mode_ok = matches!(theme.as_str(), "system" | "light" | "dark");
    let style_ok = matches!(color_style.as_str(), "indigo" | "graphite" | "moss" | "sunset");
    if mode_ok {
        app.set_pref("mode_id", &theme);
    }
    if style_ok {
        app.set_pref("style_id", &color_style);
    }
    Ok(())
}

/// 积分监测：{autoSwitch, threshold, interval}
#[tauri::command]
pub async fn credit_monitor_get(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(json!({
        "autoSwitch": aux::credit_monitor::is_auto_switch_enabled(app),
        "threshold": aux::credit_monitor::threshold(app),
        "interval": aux::credit_monitor::scan_interval(app),
    }))
}

#[tauri::command]
pub async fn credit_monitor_set(s: Cm, auto_switch: bool, threshold: i64, interval: i64) -> Result<(), String> {
    let app = st(&s);
    aux::credit_monitor::set_auto_switch_enabled(app, auto_switch);
    aux::credit_monitor::set_threshold(app, threshold);
    aux::credit_monitor::set_scan_interval(app, interval);
    Ok(())
}

/// {lines:[...最近日志]}（与 setup 里注册的 "log" 事件同源）
#[tauri::command]
pub async fn logs_recent(_s: Cm) -> Result<Value, String> {
    Ok(json!({ "lines": logbus::recent() }))
}

// ==================================================================
// 备份 / 保活 / 自检
// ==================================================================

/// 导出账号 JSON（明文 token）→ {trae:String, qoder:String}
#[tauri::command]
pub async fn export_accounts(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(aux::account_backup::export_all(app))
}

/// 导入账号 JSON → {trae:n, qoder:m}
#[tauri::command]
pub async fn import_accounts(s: Cm, trae: String, qoder: String) -> Result<Value, String> {
    let app = st(&s);
    Ok(aux::account_backup::import_all(app, &trae, &qoder))
}

/// 保活体检 → [{name,ok,detail}]
#[tauri::command]
pub async fn keepalive_check(s: Cm) -> Result<Value, String> {
    let app = st(&s);
    Ok(aux::keepalive::check(app).await)
}

/// 用系统浏览器打开链接（登录用）
#[tauri::command]
pub async fn open_browser(_app: tauri::AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    _app.opener().open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "自检失败".into())
}

/// 单模块自检（self_check 失败即 panic —— Android 惯例；这里捕获成结果）
fn chk(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) -> Value {
    match std::panic::catch_unwind(f) {
        Ok(()) => json!({"name": name, "ok": true, "msg": ""}),
        Err(p) => json!({"name": name, "ok": false, "msg": panic_msg(&p)}),
    }
}

/// 自检面板 → [{name,ok,msg}]（对齐 Android onCreate 的 selfCheck 清单）
#[tauri::command]
pub async fn self_check_run(s: Cm) -> Result<Value, String> {
    let app = s.inner().clone();
    let mut out: Vec<Value> = Vec::new();
    out.push(chk("TraeRegion", || trae::region::TraeRegion::self_check()));
    out.push(chk("TraeAuthStore", || trae::auth_store::self_check()));
    out.push(chk("TraeAuthApi", || trae::auth_api::self_check()));
    out.push(chk("TraeUpstream", || trae::upstream::self_check()));
    out.push(chk("QoderMachine", || qoder::machine::self_check()));
    out.push(chk("QoderAuthStore", || qoder::auth_store::self_check()));
    out.push(chk("QoderCosyCrypto", || qoder::cosy_crypto::self_check()));
    out.push(chk("QoderAuthApi", || qoder::auth_api::self_check()));
    out.push(chk("QoderUpstream", || qoder::upstream::self_check()));
    out.push(chk("QoderModelCatalog", || qoder::model_catalog::self_check()));
    out.push(chk("QoderModelCache", || qoder::model_cache::self_check()));
    out.push(chk("QoderCheckin", || qoder::checkin::self_check()));
    out.push(chk("ModelsRepo", || aux::models_repo::self_check()));
    out.push(chk("AutoCheckin", || aux::auto_checkin::self_check()));
    out.push(chk("CreditMonitor", || aux::credit_monitor::self_check()));
    out.push(chk("AccountCredits", || aux::account_credits::self_check()));
    out.push(chk("AccountBackup", || aux::account_backup::self_check()));
    out.push(chk("KeepAlive", || aux::keepalive::self_check()));
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server::self_check(&app))) {
        Ok(Ok(())) => out.push(json!({"name": "Gateway", "ok": true, "msg": ""})),
        Ok(Err(e)) => out.push(json!({"name": "Gateway", "ok": false, "msg": e})),
        Err(p) => out.push(json!({"name": "Gateway", "ok": false, "msg": panic_msg(&p)})),
    }
    Ok(Value::Array(out))
}

// ==================================================================
// 后台调度（setup 时拉起；对齐 Android 的 AlarmManager / 定时线程）
// ==================================================================

fn parse_hm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

/// 每日定时：Trae 自动签到（run_due 自带「到点判断 + 没跑成补签」）
/// + Qoder 定时领取（到点跑一次，当天只跑一次）。
async fn daily_loop(app: std::sync::Arc<AppState>) {
    use chrono::Timelike;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        // Trae 自动签到：到点/补签判断在 core 内
        let r = aux::auto_checkin::run_due(&app).await;
        if r.get("ran").and_then(|v| v.as_bool()) == Some(true) {
            log_i!("AutoCheckin", "定时签到执行完成");
        }
        // Qoder 每日领取
        if qoder::checkin::is_enabled(&app) {
            let t = qoder::checkin::time_text(&app);
            if let Some((h, m)) = parse_hm(&t) {
                let now = chrono::Local::now();
                let today = now.format("%Y-%m-%d").to_string();
                if now.hour() == h && now.minute() == m && app.pref_or("qoder_daily_last", "") != today {
                    app.set_pref("qoder_daily_last", &today);
                    let r = qoder::checkin::run_once(&app).await;
                    log_i!("QoderCheckin", "定时领取完成: {}", r);
                }
            }
        }
    }
}

/// 积分监测循环（对齐 CreditMonitor：用户开了检测才扫；间隔可调，内部另有 60s 节流）
async fn monitor_loop(app: std::sync::Arc<AppState>) {
    if aux::credit_monitor::scan_interval(&app) > 0 {
        aux::credit_monitor::scan_once(&app).await;
    }
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let iv = aux::credit_monitor::scan_interval(&app);
        if iv <= 0 {
            continue;
        }
        if last.elapsed().as_secs() >= iv as u64 {
            last = tokio::time::Instant::now();
            aux::credit_monitor::scan_once(&app).await;
        }
    }
}

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
        .setup(|app| {
            // 日志总线先行：之后的每条日志都同步推给前端「记录页」
            let handle = app.handle().clone();
            logbus::init(Box::new(move |line| {
                use tauri::Emitter;
                let _ = handle.emit("log", line.to_string());
            }));
            let st = app.state::<std::sync::Arc<AppState>>().inner().clone();

            // 启动初始化（对齐 MainActivity.onCreate 的各 object init）
            trae::region::TraeRegion::load(&st);
            aux::models_repo::init(&st);
            qoder::model_catalog::load(&st);
            aux::account_credits::load_cache(&st);
            match server::self_check(&st) {
                Ok(()) => log_i!("SelfCheck", "网关自检通过"),
                Err(e) => log_e!("SelfCheck", "网关自检失败: {}", e),
            }

            // 进 App 默认拉起网关（对齐 autoStartServiceIfWanted：
            // 用户没主动停过就开 —— 「关了又自己开」靠 should_run 键兑现承诺）
            let st2 = st.clone();
            tauri::async_runtime::spawn(async move {
                if st2.pref(KEY_SHOULD_RUN).is_some() && st2.pref_or(KEY_SHOULD_RUN, "true") != "true" {
                    log_w!("Gateway", "用户曾主动停止服务，本次不自动启动");
                    return;
                }
                if let Err(e) = server::start(st2).await {
                    log_e!("Gateway", "自动启动失败: {}", e);
                }
            });

            // 进 App 静默补新模型表（maybeAutoRefreshModels：每进程一次，失败无感）
            let st3 = st.clone();
            tauri::async_runtime::spawn(async move {
                let _ = refresh_models_impl(&st3).await;
            });

            // 启动即补签一次（AutoCheckin 的「没跑成则下次打开 App 自动补签」）
            let st4 = st.clone();
            tauri::async_runtime::spawn(async move {
                let _ = aux::auto_checkin::run_due(&st4).await;
            });

            // 每日定时（签到/领取）+ 积分监测
            let st5 = st.clone();
            tauri::async_runtime::spawn(async move { daily_loop(st5).await; });
            let st6 = st;
            tauri::async_runtime::spawn(async move { monitor_loop(st6).await; });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn dirs_data_dir() -> std::path::PathBuf {
    // 与 Android 端独立：桌面用系统应用数据目录
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&base).join(".traework2api")
}
