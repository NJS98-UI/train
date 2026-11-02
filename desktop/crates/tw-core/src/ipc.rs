//! IPC 单一注册表：Tauri command 与 bridge HTTP 端点共用这一张路由表（design.md §IPC 契约）。
//! 命令名/出入参形状是前端的契约（frontend/src/ipc.ts + store 切片），改动必须两端同步。
//!
//! ## IPC 命令清单
//!
//! | 命令 | 入参 | 响应 data 形状 |
//! |---|---|---|
//! | `service_status` | — | `{running,port,shouldRun,hasUserIntent}` |
//! | `service_start` | — | bool |
//! | `service_stop` | — | bool(false) |
//! | `api_key` | — | String（`sk-`×35） |
//! | `lan_address` | — | String（前端也接受 String[]） |
//! | `config_get` / `config_update` | `{config:AppConfig 整份}` | AppConfig(camelCase) |
//! | `desktop_checks` | — | CheckItem[6] `{title,ok,why,autoFixable}` |
//! | `app_version` | — | String |
//! | `trae_state` | — | `{logged:[{uid,nickname,regionId,hasAccessToken,expiresAt}],activeUid,regionId,authFailedUid,hasAccessToken}`（绝不含 token） |
//! | `trae_login_url` | `{region}` | String（同时把 region 写入 config.trae_region，粘贴回调按同区域换 token） |
//! | `trae_login_submit` | `{url}` | `{ok,name?,error?}`（粘贴回调 URL → refreshToken 提取 → exchange → 入库；桌面简化版=「粘贴回调 URL」替代本地 8686 监听） |
//! | `trae_switch` | `{uid}` | bool |
//! | `trae_remove` | `{uid}` | bool（连额度缓存一起 forget） |
//! | `trae_region_set` | `{id}` | String（cn\|sg，未知回 cn） |
//! | `trae_diagnose` | — | String（逐账号摊开 token 形状 + 两个额度接口原文，对齐 Android 诊断弹窗） |
//! | `qoder_state` | — | `{logged:[{uid,name,email,mode,…}],activeUid,authFailedUid,hasJobToken}`（绝不含 token/PAT/指纹） |
//! | `qoder_login_pat` | `{pat,mode}` | `{ok,name?,error?}` |
//! | `qoder_switch` | `{uid}` | bool |
//! | `qoder_remove` | `{uid}` | bool |
//! | `qoder_diagnose` | — | String（领取链路逐步摊开；不实际 claim，避免与手动领取抢额度） |
//! | `qoder_fingerprint_get` | `{uid}` | Value\|null（camelCase QoderFingerprint） |
//! | `qoder_fingerprint_set` | `{uid,fingerprint}` | `{ok:true}`（machineId/machineToken 任一空白=中文 Err，同 Android 弹窗纪律） |
//! | `qoder_fingerprint_clear` | `{uid}` | bool（清除抓包指纹，回到本机派生身份） |
//! | `qoder_machine_id` | — | String（本机派生 sha256，crypto::machine） |
//! | `backend_get` | — | String（trae\|qoder\|auto） |
//! | `backend_set` | `{backend}` | String（非法值直接 Err） |
//! | `credits_snapshot` | — | CreditsSnapshot `{loading,at,trae,qoder,total,error,…}` |
//! | `credits_refresh_all` | `{force}` | CreditsSnapshot（查完即发 credits-snapshot 事件） |
//! | `checkin_claim_all` | — | `{date,total,okCount,allOk,results:[…]}`（BatchSummary） |
//! | `qoder_claim_all` | — | `{date,total,okCount,allOk,results:[…]}` |
//! | `qoder_claim_one` | `{uid}` | `{uid,status,amount,message}` |
//! | `checkin_schedule_get` | `{platform}` | `{platform,enabled,hhmm}` |
//! | `checkin_schedule_set` | `{platform,enabled?,hhmm?}` | `{platform,enabled,hhmm}`（写 config.checkin.* + scheduler::reschedule） |
//! | `auto_switch_get` | — | `{enabled,threshold}` |
//! | `auto_switch_set` | `{enabled?,threshold?}` | `{enabled,threshold}`（threshold<5 由 normalize 抬回 5） |
//! | `models_view` | — | `{loaded,backend,mixed,items:[{id,name,source,multiplier?,free,verified,context?,aliases?}],counts}` |
//! | `models_refresh_trae` | — | `{ok,count}`（拉 Trae 表；失败 Err=保持现表） |
//! | `models_sync_qoder` | — | `{source,total,free,dynamic}`（三级降级链产出） |
//! | `records_get` | — | `{items:[UsageRecord],agg:[ModelAgg],count}` |
//! | `backup_export` | — | `{path}`（0600 落 `<data>/backups/`） |
//! | `backup_import` | `{path}` 或 `{data}` | `{traeN,qoderN}` |
//! | `style_get` | — | `{styleId,mode}` |
//! | `style_set` | `{styleId?,modeId?}` | `{styleId,mode}`（modeId∈light\|dark\|system） |
//! | `close_to_tray_get/set` | `{on}`(set) | bool（tauri 层监听事件执行动作，core 只管值） |
//! | `autostart_get/set` | `{on}`(set) | bool |
//!
//! 各 wave 按 analysis 文档填实现；错误统一返回中文 toast 级文案。

use serde_json::{json, Value};

/// 请求入口：`cmd` = 命令名；`args` = 参数对象。返回 `Ok(data)` 或 `Err(中文 toast 级文案)`。
pub async fn call(cmd: &str, args: Value) -> Result<Value, String> {
    let args = if args.is_null() { json!({}) } else { args };
    match cmd {
        // ---- service ----
        "service_status" => Ok(service_status_json()),
        "service_start" => {
            let r = crate::appcore::gateway_start().await?;
            crate::appcore::intent::mark_should_run(true);
            Ok(Value::Bool(r))
        }
        "service_stop" => {
            crate::appcore::gateway_stop().await;
            crate::appcore::intent::stop_now();
            Ok(Value::Bool(false))
        }
        "api_key" => Ok(Value::String(crate::fingerprint::api_key())),
        "lan_address" => Ok(Value::String(crate::desktop::lan_address())),

        // ---- 配置/外观/桌面 ----
        "config_get" => Ok(serde_json::to_value(crate::config::get()).map_err(e2s)?),
        "config_update" => {
            let patch: crate::config::AppConfig = serde_json::from_value(args.get("config").cloned().unwrap_or_default()).map_err(e2s)?;
            crate::config::update(|c| *c = patch);
            Ok(serde_json::to_value(crate::config::get()).map_err(e2s)?)
        }
        "desktop_checks" => crate::desktop::checks().await,
        "app_version" => Ok(Value::String(crate::desktop::APP_VERSION.into())),

        // ---- trae ----
        "trae_state" => Ok(trae_state()),
        "trae_login_url" => trae_login_url(&args),
        "trae_login_submit" => trae_login_submit(&args).await,
        "trae_switch" => trae_switch(&args),
        "trae_remove" => trae_remove(&args),
        "trae_region_set" => trae_region_set(&args),
        "trae_diagnose" => Ok(Value::String(trae_diagnose().await)),

        // ---- qoder ----
        "qoder_state" => Ok(qoder_state()),
        "qoder_login_pat" => qoder_login_pat(&args).await,
        "qoder_switch" => qoder_switch(&args),
        "qoder_remove" => qoder_remove(&args),
        "qoder_diagnose" => Ok(Value::String(qoder_diagnose().await)),
        "qoder_fingerprint_get" => qoder_fingerprint_get(&args),
        "qoder_fingerprint_set" => qoder_fingerprint_set(&args),
        "qoder_fingerprint_clear" => qoder_fingerprint_clear(&args),
        "qoder_machine_id" => Ok(Value::String(crate::crypto::machine::machine_id_sha256())),

        // ---- backend ----
        "backend_get" => Ok(Value::String(crate::config::get().gateway_backend)),
        "backend_set" => backend_set(&args),

        // ---- credits / checkin ----
        "credits_snapshot" => Ok(crate::credits::monitor::snapshot_json_now()),
        "credits_refresh_all" => credits_refresh_all(&args).await,
        "checkin_claim_all" => checkin_claim_all().await,
        "qoder_claim_all" => qoder_claim_all().await,
        "qoder_claim_one" => qoder_claim_one(&args).await,
        "checkin_schedule_get" => checkin_schedule_get(&args),
        "checkin_schedule_set" => checkin_schedule_set(&args),
        "auto_switch_get" => Ok(auto_switch_json(crate::credits::auto_switch())),
        "auto_switch_set" => {
            let enabled = opt_bool(&args, "enabled");
            let threshold = opt_i64(&args, "threshold");
            Ok(auto_switch_json(crate::credits::set_auto_switch(enabled, threshold)))
        }

        // ---- models ----
        "models_view" => Ok(models_view()),
        "models_refresh_trae" => models_refresh_trae().await,
        "models_sync_qoder" => models_sync_qoder().await,

        // ---- records ----
        "records_get" => Ok(crate::usage::snapshot()),

        // ---- backup ----
        "backup_export" => Ok(json!({ "path": crate::backup::AccountBackup::global().export_file()? })),
        "backup_import" => backup_import(&args),

        // ---- 外观/托盘/自启（core 只管值，tauri 层执行动作） ----
        "style_get" => Ok(serde_json::to_value(crate::config::get().style).map_err(e2s)?),
        "style_set" => style_set(&args),
        "close_to_tray_get" => Ok(Value::Bool(crate::desktop::close_to_tray())),
        "close_to_tray_set" => {
            let on = need_bool(&args, "on")?;
            crate::desktop::set_close_to_tray(on);
            Ok(Value::Bool(on))
        }
        "autostart_get" => Ok(Value::Bool(crate::desktop::autostart())),
        "autostart_set" => {
            let on = need_bool(&args, "on")?;
            crate::desktop::set_autostart(on);
            Ok(Value::Bool(on))
        }

        _ => Err(format!("未知命令: {}", cmd)),
    }
}

fn e2s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// `{running, port, shouldRun, hasUserIntent}` — 概览页状态胶囊数据源。
fn service_status_json() -> Value {
    serde_json::json!({
        "running": crate::appcore::gateway_running(),
        "port": crate::gateway::PORT,
        "shouldRun": crate::appcore::intent::should_run(),
        "hasUserIntent": crate::appcore::intent::has_user_intent(),
    })
}

// ---------------------------------------------------------------------------
// 参数取值 helper（缺参/类型错→中文 Err，绝不在前端输入上 panic）
// ---------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn need_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    arg_str(args, key).ok_or_else(|| format!("缺少参数 {key}"))
}

fn need_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("参数 {key} 需要 true/false"))
}

fn opt_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_f64()).map(|f| f as i64)
}

/// 展示串截断（诊断摊原文用；按字符防切坏 UTF-8 中文）。
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn blank_to_fallback<'a>(primary: &'a str, fallback: &'a str) -> &'a str {
    if primary.trim().is_empty() { fallback } else { primary }
}

// ---------------------------------------------------------------------------
// trae 命令组
// ---------------------------------------------------------------------------

/// store.snapshot() 的 `accounts`（serde rename 落 `region` 键）→ 前端契约的 `logged`
/// （键改 `regionId`；token 红线：绝不透传 accessToken，`accessToken?` 字段留空即合法）。
fn trae_state() -> Value {
    let snap = crate::auth::store::TraeAuthStore::global().snapshot();
    let accounts = snap.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let logged: Vec<Value> = accounts
        .iter()
        .map(|a| {
            json!({
                "uid": a.get("uid").cloned().unwrap_or(Value::Null),
                "nickname": a.get("nickname").cloned().unwrap_or(Value::Null),
                "regionId": a.get("region").cloned().unwrap_or(Value::Null),
                "hasAccessToken": a.get("hasAccessToken").cloned().unwrap_or(Value::Bool(false)),
                "expiresAt": a.get("expiresAt").cloned().unwrap_or(Value::Number(0.into())),
            })
        })
        .collect();
    let active_uid = snap.get("activeUid").cloned().unwrap_or(Value::Null);
    let has_access_token = pick_active(&logged, active_uid.as_str()).get("hasAccessToken").and_then(|v| v.as_bool()).unwrap_or(false);
    json!({
        "logged": logged,
        "activeUid": active_uid,
        "regionId": snap.get("regionId").cloned().unwrap_or(Value::String(crate::types::region::CN.into())),
        "authFailedUid": snap.get("authFailedUid").cloned().unwrap_or(Value::Null),
        "hasAccessToken": has_access_token,
    })
}

/// active 优先、缺失回第一条（与概览页 `activeTraeAccount` 的 JS 兜底同构）。
fn pick_active<'a>(logged: &'a [Value], active_uid: Option<&'a str>) -> &'a Value {
    if let Some(uid) = active_uid.filter(|s| !s.is_empty()) {
        if let Some(a) = logged.iter().find(|a| a.get("uid").and_then(|v| v.as_str()) == Some(uid)) {
            return a;
        }
    }
    logged.first().unwrap_or(&Value::Null)
}

/// `{region}` → 登录链接（复制即跳系统浏览器）。同 Kotlin `copyLoginUrl`：
/// 先把 config.trae_region 切到该区域——粘贴回调换 token 按同一区域走。
fn trae_login_url(args: &Value) -> Result<Value, String> {
    let id = arg_str(args, "region").map(str::to_string).unwrap_or_else(|| crate::config::get().trae_region);
    let region = crate::upstream::trae::TraeRegion::of(&id);
    if !region.can_login() {
        return Err(format!("{} 区域常量未配置完整", region.id));
    }
    crate::config::update(|c| c.trae_region = region.id.to_string());
    Ok(Value::String(crate::auth::api::trae::build_login_url(region)))
}

/// `{url}` → `LoginResult{ok,name?,error?}`。Kotlin 的本地 8686 回调监听在桌面简化为
/// 「浏览器登录后把跳转地址整条粘回来」：提取 refreshToken → ExchangeToken → GetUserInfo → 入库。
/// 业务失败进 `data.error`（LoginResult 语义），不抛 IPC 级错误。
async fn trae_login_submit(args: &Value) -> Result<Value, String> {
    let url = need_str(args, "url")?;
    let region = crate::upstream::trae::TraeRegion::of(&crate::config::get().trae_region);
    match crate::auth::api::trae::login_with_callback_url(url, region).await {
        Ok(cred) => Ok(json!({ "ok": true, "name": blank_to_fallback(&cred.nickname, &cred.uid) })),
        Err(e) => Ok(json!({ "ok": false, "error": e })),
    }
}

fn need_trae_uid(args: &Value) -> Result<String, String> {
    let uid = need_str(args, "uid")?.trim().to_string();
    if uid.is_empty() {
        return Err("缺少账号标识 uid".into());
    }
    Ok(uid)
}

fn trae_switch(args: &Value) -> Result<Value, String> {
    let uid = need_trae_uid(args)?;
    let store = crate::auth::store::TraeAuthStore::global();
    if store.load_by_uid(&uid).is_none() {
        return Err("未找到该 Trae 账号，可能已被删除".into());
    }
    store.set_active(&uid);
    // 区域以账号为准并持久化（同 doSwitchAccount：切号后手动选的区域是残留错值）
    store.sync_region_to_active_account();
    Ok(Value::Bool(true))
}

fn trae_remove(args: &Value) -> Result<Value, String> {
    let uid = need_trae_uid(args)?;
    let store = crate::auth::store::TraeAuthStore::global();
    if store.load_by_uid(&uid).is_none() {
        return Err("未找到该 Trae 账号，可能已被删除".into());
    }
    store.remove(&uid);
    // 删号连额度缓存一起清（同 Kotlin：AccountCredits.forget 在删除确认里调用）
    crate::credits::account::forget(crate::credits::account::TRAE, &uid);
    Ok(Value::Bool(true))
}

fn trae_region_set(args: &Value) -> Result<Value, String> {
    let raw = need_str(args, "id")?;
    let id = crate::auth::model::region_from_id(Some(raw)).to_string();
    crate::config::update(|c| c.trae_region = id.clone());
    Ok(Value::String(id))
}

/// Trae 单账号诊断（对齐 TraeUpstream.diagnose）：token 形状/有效期 + 两个额度接口原文。
/// 用**库内现 token** 直连（不续期、不改状态），401 恰恰是要给人看见的信息。
async fn trae_diagnose() -> String {
    use crate::checkin::trae_api as ta;
    use chrono::{Local, TimeZone};
    let store = crate::auth::store::TraeAuthStore::global();
    let metas = store.list();
    let mut sb = format!("账号数：{}\n", metas.len());
    if metas.is_empty() {
        sb.push_str("没有 Trae 账号\n");
        return sb;
    }
    let sep = format!("\n{}\n", "─".repeat(30));
    let now = crate::auth::model::now_sec();
    for (i, m) in metas.iter().enumerate() {
        if i > 0 {
            sb.push_str(&sep);
        }
        let cred = match store.load_by_uid(&m.uid) {
            Some(c) => c,
            None => {
                sb.push_str(&format!("uid={} 凭据读取失败\n", m.uid));
                continue;
            }
        };
        let region = crate::upstream::trae::TraeRegion::of(&cred.region_id);
        sb.push_str(&format!("uid={}  nick={}\n", cred.uid, cred.nickname));
        sb.push_str(&format!("region={}  supported={}  canQueryQuota={}\n", region.id, region.is_supported(), ta::can_query_quota(region)));
        sb.push_str(&format!("access={}字  refresh={}字\n", cred.access_token.chars().count(), cred.refresh_token.chars().count()));
        if cred.expires_at > 0 {
            let stamp = Local.timestamp_opt(cred.expires_at, 0).single().map(|d| d.format("%m-%d %H:%M").to_string());
            sb.push_str(&format!("expiresAt={}  剩余={}分钟{}\n", cred.expires_at, (cred.expires_at - now) / 60, stamp.map(|s| format!("  ({s})")).unwrap_or_default()));
        } else {
            sb.push_str("expiresAt=0 (未知)\n");
        }
        for (name, path) in [("status", ta::EP_CHECKIN_STATUS), ("usage", ta::EP_ENT_USAGE)] {
            if !ta::can_query_quota(region) {
                sb.push_str(&format!("\n[{name}] 跳过：{} 区域不支持额度查询\n", region.id));
                continue;
            }
            match ta::ug_post(region, path, &cred.access_token, &cred.uid).await {
                Ok(v) => sb.push_str(&format!("\n[{name}] OK\n{}\n", clip(&v.to_string(), 400))),
                Err(ta::UgFailure::Auth) => sb.push_str(&format!("\n[{name}] 鉴权失败（401/403/1001）：token 已失效，需重新登录\n")),
                Err(ta::UgFailure::Network) => sb.push_str(&format!("\n[{name}] 网络异常/非 2xx/响应解析失败\n")),
            }
        }
    }
    sb
}

// ---------------------------------------------------------------------------
// qoder 命令组
// ---------------------------------------------------------------------------

/// store.snapshot() 的 `accounts`（条目本身 camelCase，多余字段前端 interface 容忍）→
/// 前端契约 `logged` + 顶层 `hasJobToken`（active/首条的按账号标志）。token/PAT/指纹红线不变。
fn qoder_state() -> Value {
    let snap = crate::auth::store::QoderAuthStore::global().snapshot();
    let accounts = snap.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let logged: Vec<Value> = accounts
        .iter()
        .map(|a| {
            json!({
                "uid": a.get("uid").cloned().unwrap_or(Value::Null),
                "name": a.get("name").cloned().unwrap_or(Value::Null),
                "email": a.get("email").cloned().unwrap_or(Value::Null),
                "mode": a.get("mode").cloned().unwrap_or(Value::Null),
                "expiresAt": a.get("expiresAt").cloned().unwrap_or(Value::Number(0.into())),
                "hasJobToken": a.get("hasJobToken").cloned().unwrap_or(Value::Bool(false)),
            })
        })
        .collect();
    let active_uid = snap.get("activeUid").cloned().unwrap_or(Value::Null);
    let has_job_token = pick_active(&logged, active_uid.as_str()).get("hasJobToken").and_then(|v| v.as_bool()).unwrap_or(false);
    json!({
        "logged": logged,
        "activeUid": active_uid,
        "authFailedUid": snap.get("authFailedUid").cloned().unwrap_or(Value::Null),
        "hasJobToken": has_job_token,
    })
}

/// `{pat,mode}` → `LoginResult{ok,name?,error?}`（业务失败进 data.error）。
async fn qoder_login_pat(args: &Value) -> Result<Value, String> {
    let pat = need_str(args, "pat")?;
    let mode = crate::auth::model::qoder_normalize_mode(arg_str(args, "mode").unwrap_or("cn"));
    match crate::auth::api::qoder::login_with_pat(pat, mode).await {
        Ok(acc) => {
            let name = if !acc.name.trim().is_empty() { acc.name.clone() } else if !acc.email.trim().is_empty() { acc.email.clone() } else { acc.uid.clone() };
            Ok(json!({ "ok": true, "name": name }))
        }
        Err(e) => Ok(json!({ "ok": false, "error": e })),
    }
}

fn need_qoder_uid(args: &Value) -> Result<String, String> {
    let uid = need_str(args, "uid")?.trim().to_string();
    if uid.is_empty() {
        return Err("缺少账号标识 uid".into());
    }
    Ok(uid)
}

fn qoder_switch(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    let store = crate::auth::store::QoderAuthStore::global();
    if store.load_by_uid(&uid).is_none() {
        return Err("未找到该 Qoder 账号，可能已被删除".into());
    }
    store.set_active(&uid);
    Ok(Value::Bool(true))
}

fn qoder_remove(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    let store = crate::auth::store::QoderAuthStore::global();
    if store.load_by_uid(&uid).is_none() {
        return Err("未找到该 Qoder 账号，可能已被删除".into());
    }
    store.remove(&uid);
    crate::credits::account::forget(crate::credits::account::QODER, &uid);
    Ok(Value::Bool(true))
}

/// 领取链路诊断（对齐 QoderAuthApi.diagnose 文案）。桌面版**不做实际 claim**：
/// 领取是用户点「领取全部」的显式动作，诊断只摊活动列表与额度，避免诊断顺手消耗掉当日额度。
async fn qoder_diagnose() -> String {
    use crate::checkin::qoder_api as qa;
    let store = crate::auth::store::QoderAuthStore::global();
    let state = crate::checkin::CheckinState::global();
    let metas = store.list();
    let mut sb = format!("账号数：{}\n", metas.len());
    if metas.is_empty() {
        sb.push_str("没有 Qoder 账号\n");
        return sb;
    }
    for m in &metas {
        sb.push_str(&format!("\n== {} · {} · {} ==\n", blank_to_fallback(&m.name, &m.uid), clip(&m.uid, 8), m.mode));
        let cred = match store.load_by_uid(&m.uid) {
            Some(c) => c,
            None => {
                sb.push_str("凭据缺失\n");
                continue;
            }
        };
        let jt = crate::auth::api::qoder::valid_job_token_for(&store, &cred, crate::credits::monitor::MONITOR_JT_SKEW_MS).await;
        let jt = match jt.filter(|s| !s.trim().is_empty()) {
            Some(t) => t,
            None => {
                sb.push_str("jobToken 失败（PAT 无效或网络不通）\n");
                continue;
            }
        };
        sb.push_str(&format!("jobToken ok {}…\n", clip(&jt, 12)));
        sb.push_str(&format!("记忆档位：{}\n", state.campaign_variant(&cred.mode)));
        let load = qa::load_campaigns(&state, &store, &cred, jt.clone()).await;
        match &load.list {
            Some(list) => {
                let claimable = list.campaigns.iter().filter(|c| c.claimable()).count();
                sb.push_str(&format!("[{}] 解析 {} 条，可领 {} 条，showCampaign={:?}\n", load.variant, list.campaigns.len(), claimable, list.show_campaign));
                for c in &list.campaigns {
                    sb.push_str(&format!("  · {} {} {} benefit={}/{}{}\n", clip(&c.id, 8), c.action_type, c.claim_status, c.benefit_kind, c.benefit_amount, if c.claimable() { "  ← 可领" } else { "" }));
                }
                if list.campaigns.is_empty() {
                    sb.push_str("  解析成功但活动列表为空\n");
                }
            }
            None => sb.push_str(&format!("活动列表拉取失败：{}\n", load.error.unwrap_or_else(|| "未知原因".into()))),
        }
        match qa::fetch_credits(&jt, &cred.mode).await {
            Some(c) => sb.push_str(&format!("[usage] 余={}  exceeded={}\n", c.remaining, c.exceeded)),
            None => sb.push_str("[usage] 查询失败\n"),
        }
    }
    sb
}

fn qoder_fingerprint_get(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    let store = crate::auth::store::QoderAuthStore::global();
    match store.fingerprint(&uid) {
        Some(fp) => Ok(serde_json::to_value(fp).map_err(e2s)?),
        None => Ok(Value::Null),
    }
}

/// 抓包指纹容错解析：长键（machineId/machineToken/machineOS…）与短键（id/tk/ty…）都收，
/// 缺省语义同 [`crate::auth::model`] 的 QoderFingerprint::default()。
fn fingerprint_from_value(v: &Value) -> crate::auth::model::QoderFingerprint {
    let take = |keys: &[&str]| -> Option<String> { keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str())).map(|s| s.to_string()) };
    let def = |keys: &[&str], d: &str| -> String { take(keys).unwrap_or_else(|| d.to_string()) };
    crate::auth::model::QoderFingerprint {
        machine_id: take(&["machineId", "id", "cosy-machineid"]).unwrap_or_default(),
        machine_token: take(&["machineToken", "tk", "cosy-machinetoken"]).unwrap_or_default(),
        machine_type: {
            let s = def(&["machineType", "ty"], "5");
            if s.trim().is_empty() { "5".into() } else { s }
        },
        machine_os: def(&["machineOs", "machineOS", "os"], "aarch64_darwin"),
        hostname: {
            let s = def(&["hostname", "hn"], "localhost");
            if s.trim().is_empty() { "localhost".into() } else { s }
        },
        client_type: def(&["clientType", "ct"], "10"),
        version: {
            let s = def(&["version", "ver"], "1.1.69");
            if s.trim().is_empty() { "1.1.69".into() } else { s }
        },
    }
}

/// `{uid,fingerprint}`：同 Kotlin 指纹弹窗纪律「两项都留空=本机派生；要覆盖就两项都填」。
fn qoder_fingerprint_set(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    let store = crate::auth::store::QoderAuthStore::global();
    if store.load_by_uid(&uid).is_none() {
        return Err("未找到该 Qoder 账号，可能已被删除".into());
    }
    let raw = args.get("fingerprint").cloned().unwrap_or(Value::Null);
    if !raw.is_object() {
        return Err("指纹需要 JSON 对象（machineId/machineToken…）".into());
    }
    let fp = fingerprint_from_value(&raw);
    if fp.machine_id.trim().is_empty() || fp.machine_token.trim().is_empty() {
        return Err("两项都留空 = 用本机派生；要覆盖就两项都填".into());
    }
    store.set_fingerprint(&uid, &fp);
    Ok(json!({ "ok": true }))
}

fn qoder_fingerprint_clear(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    crate::auth::store::QoderAuthStore::global().clear_fingerprint(&uid);
    Ok(Value::Bool(true))
}

// ---------------------------------------------------------------------------
// backend 选路
// ---------------------------------------------------------------------------

fn backend_set(args: &Value) -> Result<Value, String> {
    let raw = need_str(args, "backend")?.trim().to_lowercase();
    if !matches!(raw.as_str(), crate::types::BACKEND_TRAE | crate::types::BACKEND_QODER | crate::types::BACKEND_AUTO) {
        return Err("后端选择仅支持 trae / qoder / auto".into());
    }
    crate::config::update(|c| c.gateway_backend = raw.clone());
    Ok(Value::String(raw))
}

// ---------------------------------------------------------------------------
// credits / checkin 命令组
// ---------------------------------------------------------------------------

async fn credits_refresh_all(args: &Value) -> Result<Value, String> {
    let force = opt_bool(args, "force").unwrap_or(true);
    let _snap = crate::credits::account::refresh_all(force, None).await;
    // 查询结果已落进 account 缓存；发出/返回合并快照（monitor 无内存态时 snapshot_json(None) 走按号合计）
    let v = crate::credits::monitor::snapshot_json(crate::credits::monitor::current_snapshot());
    crate::emitter::emit(crate::emitter::topics::CREDITS_SNAPSHOT, v.clone());
    Ok(v)
}

fn batch_summary_json(results: &Value, date: &str, total: usize, ok_count: usize, all_ok: bool) -> Value {
    json!({ "date": date, "total": total, "okCount": ok_count, "allOk": all_ok, "results": results })
}

/// Trae 手动「签到全部」：busy 互斥（同 Android 连点守卫）；跑完落 checkin.state。
async fn checkin_claim_all() -> Result<Value, String> {
    if crate::checkin::batch::is_busy() {
        return Err("签到正在进行中，请稍候再试".into());
    }
    let state = crate::checkin::CheckinState::global();
    let store = crate::auth::store::TraeAuthStore::global();
    let r = crate::checkin::batch::run_all(&state, &store).await;
    Ok(batch_summary_json(&r.to_json(), &r.date, r.results.len(), r.ok_count(), r.all_ok()))
}

async fn qoder_claim_all() -> Result<Value, String> {
    if crate::checkin::qoder_batch::is_busy() {
        return Err("领取正在进行中，请稍候再试".into());
    }
    let state = crate::checkin::CheckinState::global();
    let store = crate::auth::store::QoderAuthStore::global();
    let r = crate::checkin::qoder_batch::run_all(&state, &store).await;
    Ok(batch_summary_json(&r.to_json(), &r.date, r.results.len(), r.ok_count(), r.all_ok()))
}

async fn qoder_claim_one(args: &Value) -> Result<Value, String> {
    let uid = need_qoder_uid(args)?;
    let state = crate::checkin::CheckinState::global();
    let store = crate::auth::store::QoderAuthStore::global();
    let r = crate::checkin::catchup::claim_one_qoder(&state, &store, &uid).await;
    Ok(json!({ "uid": uid, "status": r.status, "amount": r.amount, "message": r.message }))
}

/// `(platform) → (enabled,hhmm)` 取值/校验共用口；非法平台 Err。
fn checkin_platform_slot(platform: &str) -> Result<&'static str, String> {
    match platform.trim().to_lowercase().as_str() {
        "trae" => Ok("trae"),
        "qoder" => Ok("qoder"),
        _ => Err("平台仅支持 trae / qoder".into()),
    }
}

fn checkin_schedule_read(platform: &str) -> Value {
    let c = crate::config::get();
    let (enabled, hhmm) = if platform == "trae" { (c.checkin.trae_enabled, c.checkin.trae_time) } else { (c.checkin.qoder_enabled, c.checkin.qoder_time) };
    json!({ "platform": platform, "enabled": enabled, "hhmm": hhmm })
}

fn checkin_schedule_get(args: &Value) -> Result<Value, String> {
    let p = checkin_platform_slot(need_str(args, "platform")?)?;
    Ok(checkin_schedule_read(p))
}

fn checkin_schedule_set(args: &Value) -> Result<Value, String> {
    let p = checkin_platform_slot(need_str(args, "platform")?)?;
    let enabled = opt_bool(args, "enabled");
    let hhmm = arg_str(args, "hhmm").map(str::trim).map(str::to_string);
    if let Some(h) = hhmm.as_deref() {
        if !crate::config::is_hhmm(h) {
            return Err("时间格式应为 HH:mm（如 08:00）".into());
        }
    }
    crate::config::update(|c| {
        let (e, t) = if p == "trae" { (&mut c.checkin.trae_enabled, &mut c.checkin.trae_time) } else { (&mut c.checkin.qoder_enabled, &mut c.checkin.qoder_time) };
        if let Some(v) = enabled {
            *e = v;
        }
        if let Some(v) = hhmm {
            *t = v;
        }
    });
    // 改开关/时刻后立刻唤醒 tick（同 Android setEnabled/setTime 重挂闹钟）；调度器未挂时 no-op。
    crate::checkin::scheduler::reschedule();
    Ok(checkin_schedule_read(p))
}

fn auto_switch_json(pair: (bool, i64)) -> Value {
    json!({ "enabled": pair.0, "threshold": pair.1 })
}

// ---------------------------------------------------------------------------
// models 命令组
// ---------------------------------------------------------------------------

fn models_view() -> Value {
    let repo = crate::models::repo::ModelsRepo::global();
    let catalog = crate::models::catalog::QoderModelCatalog::global();
    repo.load();
    catalog.load();
    let cfg = crate::config::get();
    let has_trae = crate::auth::store::TraeAuthStore::global().is_logged_in();
    let has_qoder = crate::auth::store::QoderAuthStore::global().is_logged_in();
    crate::models::routing::snapshot(&repo, &catalog, &cfg.gateway_backend, has_trae, has_qoder)
}

/// Trae 表刷新：当前账号 → 新鲜 token → 拉表 → 非空即装表（空/失败保持现表，
/// fetch_trae_models 的 Err 已是中文文案直接透传）。成功事件广播 models-updated。
async fn models_refresh_trae() -> Result<Value, String> {
    let store = crate::auth::store::TraeAuthStore::global();
    let cred = store.load().ok_or("还没有可用的 Trae 账号，请先登录后再拉取模型")?;
    let token = crate::auth::api::trae::ensure_fresh_token_for(&store, &cred, crate::auth::api::trae::REFRESH_SKEW_SEC)
        .await
        .filter(|t| !t.trim().is_empty())
        .ok_or("token 续期失败，请重新登录后再试")?;
    let entries = crate::models::repo::fetch_trae_models(&token, &cred.uid, &cred.region_id).await?;
    let count = entries.len();
    crate::models::repo::ModelsRepo::global().apply_entries(entries);
    crate::emitter::emit(crate::emitter::topics::MODELS_UPDATED, json!({ "source": "trae", "count": count }));
    Ok(json!({ "ok": true, "count": count }))
}

/// Qoder 目录同步：三级降级链产出 `{source,total,free,dynamic}`；
/// 全链塌到 builtin 且带错误 = 前端要 toast「拉取失败，继续用内置模型」→ Err 透传。
async fn models_sync_qoder() -> Result<Value, String> {
    let store = crate::auth::store::QoderAuthStore::global();
    let catalog = crate::models::catalog::QoderModelCatalog::global();
    catalog.load();
    let cred = store.load().ok_or("还没有可用的 Qoder 账号，请先登录后再同步目录")?;
    let jt = crate::auth::api::qoder::valid_job_token_for(&store, &cred, crate::auth::api::qoder::JOB_SKEW_MS)
        .await
        .unwrap_or_default();
    let outcome = catalog.sync_qoder_catalog(&cred, &jt).await;
    if outcome.source == "builtin" {
        if let Some(e) = &outcome.error {
            return Err(e.clone());
        }
    }
    crate::emitter::emit(crate::emitter::topics::MODELS_UPDATED, json!({ "source": "qoder", "total": outcome.total }));
    Ok(serde_json::to_value(&outcome).map_err(e2s)?)
}

// ---------------------------------------------------------------------------
// backup 命令组
// ---------------------------------------------------------------------------

/// `backup_import` 双形态入参：`{data:<对象|字符串>}`（未来的对话框直传）或 `{path:"..."}`。
fn backup_import(args: &Value) -> Result<Value, String> {
    let text = match args.get("data") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => std::fs::read_to_string(p).map_err(|e| format!("读取备份文件失败: {e}"))?,
            None => return Err("请选择备份文件或传入备份内容".into()),
        },
        Some(v) => serde_json::to_string(v).map_err(e2s)?,
    };
    let summary = crate::backup::AccountBackup::global().import_json(&text)?;
    Ok(summary.to_value())
}

// ---------------------------------------------------------------------------
// 外观（style_get/set）——设计契约 modeId∈light|dark|system；风格 id 白名单对齐前端 theme
// ---------------------------------------------------------------------------

const KNOWN_STYLES: [&str; 4] = ["indigo", "graphite", "moss", "sunset"];
const KNOWN_MODES: [&str; 3] = ["light", "dark", "system"];

fn style_set(args: &Value) -> Result<Value, String> {
    if let Some(s) = arg_str(args, "styleId") {
        if !KNOWN_STYLES.contains(&s) {
            return Err(format!("未知外观风格：仅支持 {}", KNOWN_STYLES.join(" / ")));
        }
        let s = s.to_string();
        crate::config::update(|c| c.style.style_id = s);
    }
    if let Some(m) = arg_str(args, "modeId") {
        if !KNOWN_MODES.contains(&m) {
            return Err(format!("亮暗模式仅支持 {}", KNOWN_MODES.join(" / ")));
        }
        let m = m.to_string();
        crate::config::update(|c| c.style.mode = m);
    }
    Ok(serde_json::to_value(crate::config::get().style).map_err(e2s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_tmp() {
        crate::appcore::init(std::env::temp_dir().join(format!("tw-ipc-{}", std::process::id())));
    }

    fn has_keys(v: &Value, keys: &[&str]) {
        for k in keys {
            assert!(v.get(*k).is_some(), "缺少关键键 {k}，实际: {v}");
        }
    }

    /// 全部「无网络依赖」命令各调一次：不 panic、形状含关键键。串在同一测试里顺序执行，
    /// 避免并行用例互相踩进程级 config 缓存。
    #[tokio::test]
    async fn smoke_all_commands_shapes() {
        init_tmp();

        let v = call("service_status", json!({})).await.unwrap();
        has_keys(&v, &["running", "port", "shouldRun", "hasUserIntent"]);

        let v = call("api_key", json!({})).await.unwrap();
        let key = v.as_str().unwrap();
        assert!(key.starts_with("sk-") && key.chars().count() == 35, "API Key 形态异常");

        let v = call("lan_address", json!({})).await.unwrap();
        assert!(v.is_string() || v.is_array());

        let v = call("app_version", json!({})).await.unwrap();
        assert!(!v.as_str().unwrap().is_empty());

        let cfg = call("config_get", json!({})).await.unwrap();
        has_keys(&cfg, &["gatewayBackend", "traeRegion", "checkin", "style", "closeToTray", "autostart"]);
        let v = call("config_update", json!({ "config": cfg })).await.unwrap();
        // 其他模块的桌面开关测试可能并发改写 closeToTray/autostart，只回验结构不深比
        has_keys(&v, &["gatewayBackend", "style", "checkin"]);

        let v = call("desktop_checks", json!({})).await.unwrap();
        assert_eq!(v.as_array().unwrap().len(), 6);
        has_keys(&v[0], &["title", "ok", "why", "autoFixable"]);

        // ---- trae / qoder 状态（空账号库合法态） ----
        let v = call("trae_state", json!({})).await.unwrap();
        has_keys(&v, &["logged", "activeUid", "regionId", "authFailedUid", "hasAccessToken"]);
        assert!(v["logged"].is_array());

        let v = call("qoder_state", json!({})).await.unwrap();
        has_keys(&v, &["logged", "activeUid", "hasJobToken"]);
        assert!(v["logged"].is_array());

        // ---- credits（空账号：refresh 只走缓存装载，不打上游） ----
        let v = call("credits_snapshot", json!({})).await.unwrap();
        has_keys(&v, &["loading", "at", "trae", "qoder", "total", "error"]);

        let v = call("credits_refresh_all", json!({ "force": true })).await.unwrap();
        has_keys(&v, &["loading", "at", "trae", "qoder", "total", "error"]);

        // ---- backend（set 的返回值自证；get 值可能被其他模块并发改写，只验类型） ----
        let v = call("backend_get", json!({})).await.unwrap();
        assert!(v.is_string());
        let v = call("backend_set", json!({ "backend": "auto" })).await.unwrap();
        assert_eq!(v, json!("auto"));
        assert!(call("backend_set", json!({ "backend": "bogus" })).await.is_err());

        // ---- checkin 调度 / 自动换号 ----
        let v = call("checkin_schedule_get", json!({ "platform": "trae" })).await.unwrap();
        has_keys(&v, &["platform", "enabled", "hhmm"]);
        // enabled=false：避免进程内其他测试已挂载的闹钟读到 true 后意外触发真实签到
        for (p, t) in [("trae", "08:30"), ("qoder", "09:15")] {
            let v = call("checkin_schedule_set", json!({ "platform": p, "enabled": false, "hhmm": t })).await.unwrap();
            assert_eq!(v["hhmm"].as_str(), Some(t));
            assert_eq!(v["enabled"], json!(false));
        }
        let v = call("checkin_schedule_get", json!({ "platform": "qoder" })).await.unwrap();
        assert_eq!(v["hhmm"].as_str(), Some("09:15"));
        assert!(call("checkin_schedule_get", json!({ "platform": "xx" })).await.is_err());
        assert!(call("checkin_schedule_set", json!({ "platform": "trae", "hhmm": "25:00" })).await.is_err());

        let v = call("auto_switch_get", json!({})).await.unwrap();
        has_keys(&v, &["enabled", "threshold"]);
        let v = call("auto_switch_set", json!({ "enabled": false, "threshold": 1 })).await.unwrap();
        has_keys(&v, &["enabled", "threshold"]);
        assert!(v["threshold"].as_i64().unwrap_or(0) >= 5, "threshold<5 应抬回 5，实际: {v}");
        let v = call("auto_switch_set", json!({ "enabled": true, "threshold": 5 })).await.unwrap();
        assert_eq!(v["threshold"], json!(5));

        // ---- 桌面开关 ----
        for key in ["close_to_tray", "autostart"] {
            let g = call(&format!("{key}_get"), json!({})).await.unwrap();
            assert!(g.is_boolean());
            for on in [true, false] {
                let v = call(&format!("{key}_set"), json!({ "on": on })).await.unwrap();
                assert_eq!(v, json!(on));
            }
        }

        // ---- 外观 ----
        let v = call("style_set", json!({ "styleId": "moss", "modeId": "dark" })).await.unwrap();
        assert_eq!(v["mode"], json!("dark"));
        let v = call("style_get", json!({})).await.unwrap();
        has_keys(&v, &["styleId", "mode"]);
        assert!(call("style_set", json!({ "modeId": "neon" })).await.is_err());

        // ---- models（空账号/空表：view 空态合法；refresh 快速 Err 不打上游） ----
        let v = call("models_view", json!({})).await.unwrap();
        has_keys(&v, &["loaded", "items", "counts"]);
        assert!(v["items"].is_array());
        // 空账号库：refresh/sync 应立即中文 Err（不打上游；本测试此前未入库任何账号）
        let e = call("models_refresh_trae", json!({})).await.unwrap_err();
        assert!(e.contains("Trae"), "错误文案应可读，实际: {e}");
        let e = call("models_sync_qoder", json!({})).await.unwrap_err();
        assert!(e.contains("Qoder"), "错误文案应可读，实际: {e}");

        // ---- records ----
        let v = call("records_get", json!({})).await.unwrap();
        has_keys(&v, &["items", "agg"]);
        assert!(v["items"].is_array() && v["agg"].is_array());

        // ---- backup（空账号库：导出合法落盘；导入无账号→中文 Err） ----
        let v = call("backup_export", json!({})).await.unwrap();
        let path = v["path"].as_str().unwrap().to_string();
        assert!(std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false), "导出文件应落盘: {path}");
        let e = call("backup_import", json!({ "path": path })).await.unwrap_err();
        assert!(e.contains("备份"), "导入错误文案应中文可读，实际: {e}");
        assert!(call("backup_import", json!({})).await.unwrap_err().contains("请选择"));

        // ---- trae 登录（不触网两分支）+ 切号删号错误路径 ----
        let v = call("trae_login_url", json!({ "region": "cn" })).await.unwrap();
        assert!(v.as_str().unwrap().starts_with("https://www.trae.cn/authorization?"), "国服登录链接 host 不符: {v}");
        let v = call("trae_login_submit", json!({ "url": "https://evil.example/nobody" })).await.unwrap();
        assert_eq!(v["ok"], json!(false));
        assert!(v["error"].as_str().unwrap().contains("回调链接"));
        assert!(call("trae_login_submit", json!({})).await.is_err());
        assert!(call("trae_switch", json!({ "uid": "ghost" })).await.unwrap_err().contains("未找到"));
        assert!(call("trae_remove", json!({ "uid": "ghost" })).await.unwrap_err().contains("未找到"));
        let v = call("trae_region_set", json!({ "id": "sg" })).await.unwrap();
        assert_eq!(v, json!("sg"));
        let v = call("trae_region_set", json!({ "id": "jp" })).await.unwrap();
        assert_eq!(v, json!("cn"), "未知区域回 cn");

        // ---- qoder：fingerprint / machine id / 错误路径 ----
        let v = call("qoder_machine_id", json!({})).await.unwrap();
        assert_eq!(v.as_str().unwrap().chars().count(), 64, "machine id 应为 sha256 hex");
        let v = call("qoder_fingerprint_get", json!({ "uid": "ghost" })).await.unwrap();
        assert_eq!(v, Value::Null);
        assert!(call("qoder_fingerprint_set", json!({ "uid": "ghost", "fingerprint": { "machineId": "a", "machineToken": "b" } })).await.unwrap_err().contains("未找到"));
        assert_eq!(call("qoder_fingerprint_clear", json!({ "uid": "ghost" })).await.unwrap(), json!(true));
        let store = crate::auth::store::QoderAuthStore::global();
        // load_by_uid 对 pat/jt 双空白账号返回 None（Kotlin 硬条件），夹具须带 pat
        store.save(&crate::auth::model::QoderAccount { uid: "ipc-fp-test".into(), name: "t".into(), pat: "pt-fixture".into(), ..Default::default() });
        assert!(call("qoder_fingerprint_set", json!({ "uid": "ipc-fp-test", "fingerprint": { "machineId": " ", "machineToken": " " } })).await.unwrap_err().contains("两项都留空"));
        let v = call("qoder_fingerprint_set", json!({ "uid": "ipc-fp-test", "fingerprint": { "machineId": "mid", "machineToken": "mtk", "machineOS": "x86_64_windows" } })).await.unwrap();
        assert_eq!(v["ok"], json!(true));
        let v = call("qoder_fingerprint_get", json!({ "uid": "ipc-fp-test" })).await.unwrap();
        assert_eq!(v["machineId"].as_str(), Some("mid"));
        store.remove("ipc-fp-test");

        // ---- 签到批量（空账号库 → 零次上游请求，纯形状） ----
        let v = call("checkin_claim_all", json!({})).await.unwrap();
        has_keys(&v, &["date", "total", "okCount", "allOk", "results"]);
        assert_eq!(v["total"], json!(0));
        let v = call("qoder_claim_all", json!({})).await.unwrap();
        has_keys(&v, &["date", "total", "okCount", "allOk", "results"]);
        let v = call("qoder_claim_one", json!({ "uid": "ghost" })).await.unwrap();
        assert_eq!(v["status"], json!("fail"));
        assert_eq!(v["message"], json!("凭据读取失败"));

        // ---- 诊断（空账号快速回落；有账号才会打上游） ----
        let v = call("trae_diagnose", json!({})).await.unwrap();
        assert!(v.as_str().unwrap().contains("账号数"));
        let v = call("qoder_diagnose", json!({})).await.unwrap();
        assert!(v.as_str().unwrap().contains("账号数"));

        // ---- 兜底 ----
        assert!(call("no_such_cmd", json!({})).await.unwrap_err().contains("未知命令"));
    }

    /// 触网命令的错误路径快速失败钉形状：假 PAT 走 exchange（DNS/401 都应在数十秒内失败），
    /// 离线 CI 里这条会慢，默认 ignore；需要时用 `cargo test -- --ignored` 跑。
    #[tokio::test]
    #[ignore = "触网：假 PAT 打 Qoder exchange（快速 401/断网），仅手工验证错误路径形状"]
    async fn qoder_login_pat_fake_pat_network_shape() {
        init_tmp();
        let v = call("qoder_login_pat", json!({ "pat": "pt-dummy-not-a-real-token", "mode": "cn" })).await.unwrap();
        assert_eq!(v["ok"], json!(false), "LoginResult 形状：失败也应 Ok 且带 error 文案");
        assert!(!v["error"].as_str().unwrap_or("").is_empty());
    }

    #[tokio::test]
    #[ignore = "触网：假回调 URL（前缀合法、refreshToken 假）打 Trae exchange，仅手工验证错误路径形状"]
    async fn trae_login_submit_fake_refresh_network_shape() {
        init_tmp();
        call("trae_region_set", json!({ "id": "cn" })).await.unwrap();
        let url = format!("http://{}:{}/authorize?refreshToken=rt-dummy", crate::auth::api::trae::CALLBACK_HOST, crate::auth::api::trae::CALLBACK_PORT);
        let v = call("trae_login_submit", json!({ "url": url })).await.unwrap();
        assert_eq!(v["ok"], json!(false));
        assert!(!v["error"].as_str().unwrap_or("").is_empty());
    }
}
