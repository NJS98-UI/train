//! auth API 层：`TraeAuthApi.kt` / `QoderAuthApi.kt` 的 Rust 对应体。
//! 权威依据 = Kotlin 源码（文档冲突以源码为准）。
//!
//! 结构：
//! - [`trae`]：登录回调解析 / ExchangeToken / GetUserInfo / 续期编排（refresh 轮换 + 防重放写回守卫）。
//! - [`qoder`]：PAT exchange / jobToken refresh / userinfo / valid+force 续期编排。
//! - 网络与解析拆开：`parse_*` / `build_*` 为纯函数（单测逐字节断言），`*_token` 等 async 才发请求。
//!
//! 区域纪律（源码注释强调的坑）：
//! - Trae：续期必须打**账号自己的** OAuth host（`TraeRegion::of(cred.region())`），绝不读全局翻面；
//! - `ensure_fresh_token_for` 只写 token（update_tokens+update_meta），**不动 active 账号**；
//!   `restart_with_valid_token` 走 save（Android 语义会 setActive，桌面保持一致）。
//! - Qoder：openapi（exchange/refresh/userinfo）与 infer（聊天 COSY，见 upstream::qoder）
//!   是**两套域名**，绝不混用。
//!
//! 单元测试全部是纯逻辑（URL/请求头/请求体构造、响应解析、阈值边界），**不触网**。

use crate::auth::model::{now_ms, now_sec};
use crate::auth::store::{QoderAuthStore, TraeAuthStore};
use crate::auth::model::{QoderAccount, TraeAccount};
use serde_json::Value;
use std::sync::OnceLock;

/// auth API 共享客户端：connect 20s / 总时限 30s（对应两侧 OkHttp `connectTimeout(20s).readTimeout(30s)`）。
pub fn api_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client")
    })
}

// ---------------------------------------------------------------------------
// URL 编解码（对齐 java.net.URLEncoder/URLDecoder，application/x-www-form-urlencoded 语义）
// ---------------------------------------------------------------------------

/// `java.net.URLEncoder.encode(s, "UTF-8")` 等价：保留 `A-Za-z0-9` 与 `* - . _`，
/// 空格→`+`，其余字节→`%XX`（大写 hex，非 ASCII 先按 UTF-8 展开）。
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// `URLDecoder.decode(s, "UTF-8")` 等价（+→空格、%XX）；非法序列按原文处理。
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = &s[i + 1..i + 3];
                match u8::from_str_radix(h, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `hexStr(bytes)`：n 随机字节 → 小写 hex。
pub fn hex_str(n: usize) -> String {
    crate::crypto::cosy::random_bytes(n)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub mod trae {
    //! `TraeAuthApi.kt`：Trae 官方认证 API（登录回调流 + ExchangeToken 轮换 + GetUserInfo + 续期编排）。

    use super::*;
    use crate::upstream::trae::TraeRegion;

    pub const CALLBACK_HOST: &str = "127.0.0.1";
    pub const CALLBACK_PORT: u16 = 18080;
    pub const CALLBACK_PATH: &str = "/authorize";

    pub const EP_EXCHANGE: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";
    pub const EP_USER_INFO: &str = "/cloudide/api/v3/trae/GetUserInfo";

    /// `LocalFakeServer`/credProvider 使用的时间窗（临期即续）；`expiredWithin` 是唯一判据。
    pub const REFRESH_SKEW_SEC: i64 = 3600;

    /// 对应 `ExchangeResult`。`refresh_token`：轮换后的新 refresh（None=上游未返回，沿用旧值）；
    /// `expires_at`：unix **秒**（None=未返回有效期）。
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ExchangeResult {
        pub access_token: String,
        pub refresh_token: Option<String>,
        pub expires_at: Option<i64>,
    }

    /// 对应 `UserInfo`。
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct UserInfo {
        pub uid: String,
        pub nickname: String,
        pub enterprise_id: String,
    }

    /// 对应 `buildLoginUrl()`：参数**插入顺序**与 query 编码逐字节一致（region 由登录入口显式传入）。
    pub fn build_login_url(region: &TraeRegion) -> String {
        let device = hex_str(16);
        let machine = hex_str(16);
        let params: [(&str, std::borrow::Cow<str>); 18] = [
            ("login_version", "1".into()),
            ("auth_from", "solo".into()),
            ("login_channel", "native_ide".into()),
            ("plugin_version", "2.3.62834".into()),
            ("auth_type", "local".into()),
            ("client_id", region.client_id.into()),
            ("redirect", "0".into()),
            ("login_trace_id", hex_str(8).into()),
            (
                "auth_callback_url",
                format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}").into(),
            ),
            ("machine_id", machine.clone().into()),
            ("device_id", device.clone().into()),
            ("x_device_id", device.into()),
            ("x_machine_id", machine.into()),
            ("x_device_brand", "PC".into()),
            ("x_device_type", "PC".into()),
            ("x_os_version", "1.0".into()),
            ("x_app_version", TraeRegion::IDE_VERSION.into()),
            ("x_app_type", "stable".into()),
        ];
        let q = params
            .iter()
            .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{}/authorization?{}", region.console_host, q)
    }

    fn callback_prefix() -> String {
        format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}")
    }

    /// 对应 `isCallback`：前缀大小写无关匹配。
    pub fn is_callback(url: &str) -> bool {
        let p = callback_prefix();
        url.len() >= p.len() && url[..p.len()].eq_ignore_ascii_case(&p)
    }

    /// 回调 query → `(k,v)` 表（一次 URL 解码，`parse_qs` 语义；重复 key 后者覆盖前者）。
    fn query_map(url: &str) -> Vec<(String, String)> {
        let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut map: Vec<(String, String)> = Vec::new();
        for p in query.split('&').filter(|p| !p.is_empty()) {
            let mut kv = p.splitn(2, '=');
            let k = url_decode(kv.next().unwrap_or(""));
            let v = url_decode(kv.next().unwrap_or(""));
            map.push((k, v));
        }
        map
    }

    fn get<'a>(map: &'a [(String, String)], k: &str) -> Option<&'a str> {
        map.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.as_str())
    }

    /// 对应 `extractRefreshToken`：refreshToken → refresh_token → userJwt(.RefreshToken) 兜底。
    pub fn extract_refresh_token(callback: &str) -> Option<String> {
        let map = query_map(callback);
        let mut rt = get(&map, "refreshToken").and_then(|s| (!s.trim().is_empty()).then(|| s.to_string()));
        if rt.is_none() {
            rt = get(&map, "refresh_token").and_then(|s| (!s.trim().is_empty()).then(|| s.to_string()));
        }
        if rt.is_none() {
            let jwt = parse_json_param(get(&map, "userJwt"))
                .or_else(|| parse_json_param(get(&map, "user_jwt")));
            rt = jwt
                .and_then(|j| j.get("RefreshToken").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .filter(|s| !s.trim().is_empty());
        }
        rt
    }

    /// org.json `optString`：number/bool 也转字符串；其余取字符串或缺省。
    fn opt_str_longish(v: Option<&Value>) -> Option<String> {
        match v? {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            n => Some(n.to_string()),
        }
    }

    /// 对应 `parseJsonParam`：原串→解一次码，两次都试 JSON；失败 None。
    pub fn parse_json_param(raw: Option<&str>) -> Option<Value> {
        let raw = raw?.trim();
        if raw.is_empty() {
            return None;
        }
        for cand in [raw.to_string(), url_decode(raw)] {
            if let Ok(v) = serde_json::from_str::<Value>(&cand) {
                if v.is_object() {
                    return Some(v);
                }
            }
        }
        None
    }

    /// 对应 `normalizeMs`：**大于 1e12 判定毫秒→秒**，否则原样当秒。
    pub fn normalize_expiry(raw: i64) -> i64 {
        if raw > 1_000_000_000_000 {
            raw / 1000
        } else {
            raw
        }
    }

    /// 对应 exchangeToken 里 expiresAt 的三级取值（纯函数，now 注入便于测试）。
    pub fn parse_expires_at(result: &Value, now_sec: i64) -> Option<i64> {
        if let Some(s) = opt_str_longish(result.get("TokenExpireAt")) {
            if let Ok(v) = s.trim().parse::<i64>() {
                if v > 0 {
                    return Some(normalize_expiry(v));
                }
            }
        }
        let dur = result.get("TokenExpireDuration").and_then(coerce_i64)?;
        if dur > 0 {
            return Some(now_sec + dur);
        }
        None
    }

    fn coerce_i64(v: &Value) -> Option<i64> {
        match v {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    /// ExchangeToken 请求体（字段逐字对齐源码：`ClientID/RefreshToken/ClientSecret/UserID`）。
    pub fn exchange_body(region: &TraeRegion, refresh: &str) -> Value {
        serde_json::json!({
            "ClientID": region.client_id,
            "RefreshToken": refresh,
            "ClientSecret": "-",
            "UserID": "",
        })
    }

    /// GetUserInfo 请求体。
    pub fn user_info_body() -> Value {
        serde_json::json!({ "ReqSource": "IDE", "IDEVersion": TraeRegion::IDE_VERSION })
    }

    /// ExchangeToken 请求头（顺序无关的键值对，测试用断言逐键）。
    pub fn exchange_headers() -> Vec<(&'static str, String)> {
        vec![
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json".into()),
            ("User-Agent", format!("Trae/{}", TraeRegion::IDE_VERSION)),
        ]
    }

    pub fn user_info_headers(token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("Content-Type", "application/json".into()),
            ("Accept", "application/json".into()),
            ("User-Agent", format!("Trae/{}", TraeRegion::IDE_VERSION)),
            ("X-Cloudide-Token", token.to_string()),
        ]
    }

    /// 对应 `exchangeToken` 的响应解析（纯函数）。错误消息保持人话。
    pub fn parse_exchange_response(text: &str, now_sec: i64) -> Result<ExchangeResult, String> {
        let obj: Value =
            serde_json::from_str(text).map_err(|e| format!("ExchangeToken 响应非 JSON: {e}"))?;
        let result = obj
            .get("Result")
            .filter(|v| v.is_object())
            .ok_or_else(|| "ExchangeToken 缺 Result".to_string())?;
        let token = result
            .get("Token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if token.trim().is_empty() {
            return Err("ExchangeToken 未返回 Token".into());
        }
        let new_refresh = result
            .get("RefreshToken")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty());
        let expires_at = parse_expires_at(result, now_sec);
        Ok(ExchangeResult { access_token: token, refresh_token: new_refresh, expires_at })
    }

    /// 对应 `getUserInfo` 的响应解析（纯函数）。缺 Result → Err。
    pub fn parse_user_info(text: &str) -> Result<UserInfo, String> {
        let obj: Value = serde_json::from_str(text).map_err(|e| format!("GetUserInfo 响应非 JSON: {e}"))?;
        let result = obj
            .get("Result")
            .filter(|v| v.is_object())
            .ok_or_else(|| "GetUserInfo 缺 Result".to_string())?;
        let s = |k: &str| result.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok(UserInfo { uid: s("UserID"), nickname: s("ScreenName"), enterprise_id: s("EnterpriseID") })
    }

    /// 对应 `exchangeToken(refresh, region)`。⚠️ 续期/签到必须传**账号自己的** region。
    pub async fn exchange_token(region: &TraeRegion, refresh: &str) -> Result<ExchangeResult, String> {
        let url = format!("{}{}", region.oauth_host, EP_EXCHANGE);
        let mut req = api_client().post(&url).json(&exchange_body(region, refresh));
        for (k, v) in exchange_headers() {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| format!("ExchangeToken 网络错误: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "ExchangeToken http {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            ));
        }
        parse_exchange_response(&text, now_sec())
    }

    /// 对应 `getUserInfo(accessToken)`（走**当前配置区域**的 oauthHost；多账号请按参数版）。
    pub async fn get_user_info(region: &TraeRegion, access_token: &str) -> Result<UserInfo, String> {
        let url = format!("{}{}", region.oauth_host, EP_USER_INFO);
        let mut req = api_client().post(&url).json(&user_info_body());
        for (k, v) in user_info_headers(access_token) {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| format!("GetUserInfo 网络错误: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "GetUserInfo http {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            ));
        }
        parse_user_info(&text)
    }

    /// 对应 `reuseAfterRace`：库里 token 与调用方快照不同 → 别处已续，直接复用。
    pub fn reuse_after_race(stale_access: &str, stored_access: &str) -> bool {
        !stored_access.trim().is_empty() && stored_access != stale_access
    }

    /// 对应 `shouldWriteBack`：库里 refreshToken 还是我们用的那个（或为空）才允许覆盖。
    pub fn should_write_back(used_refresh: &str, stored_refresh: &str) -> bool {
        stored_refresh.trim().is_empty() || stored_refresh == used_refresh
    }

    /// 对应 `expiredWithin`（唯一"要不要续"判据，见 store::expired_within，单位=秒）。
    pub fn expired_within(expires_at: i64, skew_sec: i64) -> bool {
        TraeAuthStore::expired_within(expires_at, skew_sec)
    }

    /// 对应 TraeAuthApi 里的 `Gateway.locked`：refresh 轮换单飞，防同账号并发重放。
    /// （parking_lot 同步锁不可跨 await，故用 tokio Mutex；全进程共享一把。）
    fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// 对应 `loginWithCallbackUrl`：唯一的 Trae 登录方式（粘回调链接）。
    pub async fn login_with_callback_url(callback_url: &str, region: &TraeRegion) -> Result<TraeAccount, String> {
        let url = callback_url.trim();
        if url.is_empty() {
            return Err("请粘贴回调链接".into());
        }
        if !is_callback(url) {
            return Err(format!(
                "这不是 Trae 回调链接。应形如 http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}?refreshToken=…"
            ));
        }
        let refresh = extract_refresh_token(url)
            .ok_or("链接里没有 refreshToken，请确认复制的是登录后跳转的那条完整链接")?;
        let ex = exchange_token(region, &refresh).await?;
        let user = get_user_info(region, &ex.access_token).await?;
        let cred = TraeAccount {
            uid: user.uid,
            nickname: user.nickname,
            region_id: region.id.to_string(),
            expires_at: ex.expires_at.unwrap_or(0),
            access_token: ex.access_token,
            refresh_token: ex.refresh_token.unwrap_or(refresh),
        };
        TraeAuthStore::global().save(&cred);
        Ok(cred)
    }

    /// `refreshLocked`（内部唯一续期执行体）：进锁 → 读最新凭据 → 换 → 写回守卫。
    async fn refresh_locked(store: &TraeAuthStore, uid: &str, stale_access: &str) -> Option<String> {
        let _g = refresh_lock().lock().await;
        let base = store.load_by_uid(uid)?;
        if reuse_after_race(stale_access, &base.access_token) {
            return Some(base.access_token.clone());
        }
        if base.refresh_token.trim().is_empty() {
            // 无 refreshToken，无法续期
            return None;
        }
        let region = TraeRegion::of(&base.region_id);
        let ex = exchange_token(region, &base.refresh_token).await.ok()?;
        let stored = store.load_by_uid(uid);
        if let Some(stored) = &stored {
            if !should_write_back(&base.refresh_token, &stored.refresh_token) {
                // 续期期间凭据又被更新：放弃写回，采用库里那份
                let t = stored.access_token.clone();
                return if t.trim().is_empty() { Some(ex.access_token) } else { Some(t) };
            }
        }
        let mut new_cred = base.clone();
        new_cred.access_token = ex.access_token.clone();
        if ex.refresh_token.is_some() {
            new_cred.refresh_token = ex.refresh_token.unwrap();
        }
        if let Some(e) = ex.expires_at {
            new_cred.expires_at = e;
        }
        store.update_tokens(&new_cred);
        store.update_meta(&new_cred.to_meta());
        Some(ex.access_token)
    }

    /// 对应 `ensureFreshTokenFor`（唯一续期入口）：先读库内最新，别人续过就复用；
    /// **不改 active、不动全局区域**。返回 None=续不动。
    pub async fn ensure_fresh_token_for(store: &TraeAuthStore, cred: &TraeAccount, skew_sec: i64) -> Option<String> {
        let fresh = store.load_by_uid(&cred.uid).unwrap_or_else(|| cred.clone());
        if !fresh.access_token.trim().is_empty() && !expired_within(fresh.expires_at, skew_sec) {
            return Some(fresh.access_token);
        }
        if reuse_after_race(&cred.access_token, &fresh.access_token) {
            return Some(fresh.access_token);
        }
        refresh_locked(store, &cred.uid, &fresh.access_token).await
    }

    /// 对应 `restartWithValidTokenFor`：ensureFresh 失败时回落已存（可能过期的）access token。
    pub async fn restart_with_valid_token_for(
        store: &TraeAuthStore,
        cred: &TraeAccount,
        skew_sec: i64,
    ) -> Option<String> {
        match ensure_fresh_token_for(store, cred, skew_sec).await {
            Some(t) => Some(t),
            None => {
                if cred.access_token.trim().is_empty() {
                    None
                } else {
                    Some(cred.access_token.clone())
                }
            }
        }
    }

    /// 对应 `forceRefreshTokenFor`：401 救急强制续一次（不看临期）。
    pub async fn force_refresh_token_for(store: &TraeAuthStore, cred: &TraeAccount) -> Option<String> {
        let fresh = store.load_by_uid(&cred.uid).unwrap_or_else(|| cred.clone());
        if reuse_after_race(&cred.access_token, &fresh.access_token) {
            return Some(fresh.access_token);
        }
        refresh_locked(store, &cred.uid, &cred.access_token).await
    }

    /// 对应 `restartWithValidToken(context, skewSec)`（作用于**当前账号**）：
    /// 注意 Android 版会 `TraeRegion.current = cred.region` + `store.save`（=setActive+覆盖账号）；
    /// 桌面没有可变全局区域 —— 用账号自带区域续期，save 语义保持与 Android 一致。
    pub async fn restart_with_valid_token(skew_sec: i64) -> Option<String> {
        let store = TraeAuthStore::global();
        let cred = store.load()?;
        let region = TraeRegion::of(&cred.region_id);
        if !cred.access_token.trim().is_empty() && !expired_within(cred.expires_at, skew_sec) {
            return Some(cred.access_token);
        }
        if cred.refresh_token.trim().is_empty() {
            let t = cred.access_token.clone();
            return if t.trim().is_empty() { None } else { Some(t) };
        }
        let ex = exchange_token(region, &cred.refresh_token).await.ok()?;
        let mut new_cred = cred.clone();
        new_cred.access_token = ex.access_token.clone();
        if ex.refresh_token.is_some() {
            new_cred.refresh_token = ex.refresh_token.unwrap();
        }
        if let Some(e) = ex.expires_at {
            new_cred.expires_at = e;
        }
        store.save(&new_cred);
        Some(ex.access_token)
    }
}

pub mod qoder {
    //! `QoderAuthApi.kt`：Qoder OpenAPI（PAT exchange / jobToken refresh / userinfo）。
    //! ⚠️ openApiBase 与聊天 COSY 的 inferBase 是两套域名（`crate::upstream::qoder`）。

    use super::*;
    use crate::auth::model::qoder_normalize_mode;

    pub const MODE_CN: &str = crate::auth::model::QODER_MODE_CN;
    pub const MODE_GLOBAL: &str = crate::auth::model::QODER_MODE_GLOBAL;

    pub const EP_EXCHANGE: &str = "/api/v1/jobToken/exchange";
    pub const EP_REFRESH: &str = "/api/v1/jobToken/refresh";
    pub const EP_USERINFO: &str = "/api/v1/userinfo";

    /// `SIXTY_MINUTES_MS`：validJobToken 的临期窗（毫秒）。
    pub const JOB_SKEW_MS: i64 = 3_600_000;
    /// parseExpiry 缺省有效期 = 24h（毫秒）。
    pub const EXPIRE_FALLBACK_MS: i64 = 24 * 3600_000;

    /// 对应 `openApiBase(mode)`。
    pub fn open_api_base(mode: &str) -> &'static str {
        if qoder_normalize_mode(mode) == MODE_GLOBAL {
            "https://openapi.qoder.sh"
        } else {
            "https://openapi.qoder.com.cn"
        }
    }

    /// 对应 `inferBase(mode)`：聊天网关 host（供 upstream 用）。
    pub fn infer_base(mode: &str) -> &'static str {
        if qoder_normalize_mode(mode) == MODE_GLOBAL {
            "https://api2.qoder.sh"
        } else {
            "https://gateway.qoder.com.cn"
        }
    }

    /// 对应 `JobToken(token, refreshToken, expiresAtMillis)`。
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct JobToken {
        pub token: String,
        pub refresh_token: String,
        pub expires_at_ms: i64,
    }

    /// exchange/refresh 请求头（逐键可断言：Content-Type/Accept json + UA `traework2api`；无其他头）。
    pub fn post_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
            ("User-Agent", "traework2api"),
        ]
    }

    /// userinfo GET 请求头。
    pub fn bearer_headers(jt: &str) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("Bearer {jt}")),
            ("Accept", "application/json".into()),
            ("User-Agent", "traework2api".into()),
        ]
    }

    /// `parseExpiry`：优先 `expires_in`（**毫秒**，实测国内外 OpenAPI 都返回毫秒），缺省 24h。
    pub fn parse_expiry(now_ms: i64, o: &Value) -> i64 {
        let in_ms = o.get("expires_in").and_then(coerce_i64_q).unwrap_or(0);
        if in_ms > 0 {
            now_ms + in_ms
        } else {
            now_ms + EXPIRE_FALLBACK_MS
        }
    }

    fn coerce_i64_q(v: &Value) -> Option<i64> {
        match v {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    /// 对应 `exchangeJobToken` 的响应解析：token 空白→Err；refresh 缺省**空串**。
    pub fn parse_job_token(text: &str, default_refresh: &str, now_ms: i64) -> Result<JobToken, String> {
        let o: Value = serde_json::from_str(text).map_err(|e| format!("Qoder 响应非 JSON: {e}"))?;
        let token = o.get("token").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if token.trim().is_empty() {
            return Err("Qoder 未返回 token".into());
        }
        let refresh = o
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| default_refresh.to_string());
        Ok(JobToken { token, refresh_token: refresh, expires_at_ms: parse_expiry(now_ms, &o) })
    }

    /// exchange 请求体（源码只有 `personal_token` 一个字段）。
    pub fn exchange_body(pat: &str) -> Value {
        serde_json::json!({ "personal_token": pat })
    }

    /// refresh 请求体。
    pub fn refresh_body(jrt: &str) -> Value {
        serde_json::json!({ "refresh_token": jrt })
    }

    async fn post_json(url: &str, body: &Value) -> Result<String, String> {
        let mut req = api_client().post(url).json(body);
        for (k, v) in post_headers() {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| format!("Qoder 网络错误: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Qoder http {}: {}",
                status.as_u16(),
                text.chars().take(200).collect::<String>()
            ));
        }
        Ok(text)
    }

    /// 对应 `exchangeJobToken(pat, mode)`。
    pub async fn exchange_job_token(pat: &str, mode: &str) -> Result<JobToken, String> {
        let url = format!("{}{}", open_api_base(mode), EP_EXCHANGE);
        let text = post_json(&url, &exchange_body(pat)).await?;
        parse_job_token(&text, "", now_ms())
    }

    /// 对应 `refreshJobToken(jrt, mode)`：空 jrt 直接报错；refresh_token 缺省**沿用入参**。
    pub async fn refresh_job_token(jrt: &str, mode: &str) -> Result<JobToken, String> {
        if jrt.trim().is_empty() {
            return Err("缺少 jobRefreshToken，无法续期".into());
        }
        let url = format!("{}{}", open_api_base(mode), EP_REFRESH);
        let text = post_json(&url, &refresh_body(jrt)).await?;
        parse_job_token(&text, jrt, now_ms())
    }

    /// 对应 `QUserInfo`。
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct QUserInfo {
        pub uid: String,
        pub email: String,
        pub name: String,
        pub organization_id: String,
        pub organization_name: String,
        pub organization_tags: Vec<String>,
        pub data_policy_agreed: bool,
        /// 缺省 **true**（Kotlin `optBoolean(..., true)`）。
        pub is_data_policy_modifiable: bool,
    }

    /// 对应 `parseStringList`：数组或逗号分隔字符串都认。
    pub fn parse_string_list(raw: Option<&Value>) -> Vec<String> {
        match raw {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Some(Value::String(s)) => {
                s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// 对应 `fetchUserInfo` 的解析（失败返回默认值）。uid 兼容 `uid`/`id`/`userID` 三种写法。
    pub fn parse_user_info(text: &str) -> QUserInfo {
        let o: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return QUserInfo::default(),
        };
        let s = |k: &str| -> String {
            o.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
        };
        let b = |k: &str, d: bool| -> bool { o.get(k).and_then(|v| v.as_bool()).unwrap_or(d) };
        let uid = {
            let a = s("uid");
            if !a.trim().is_empty() {
                a
            } else {
                let c = s("id");
                if !c.trim().is_empty() {
                    c
                } else {
                    s("userID")
                }
            }
        };
        let name = {
            let n = s("name");
            if n.is_empty() {
                s("username")
            } else {
                n
            }
        };
        QUserInfo {
            uid,
            email: s("email"),
            name,
            organization_id: s("organization_id"),
            organization_name: s("organization_name"),
            organization_tags: parse_string_list(o.get("organization_tags")),
            data_policy_agreed: b("data_policy_agreed", false),
            is_data_policy_modifiable: b("is_data_policy_modifiable", true),
        }
    }

    /// 对应 `fetchUserInfo(jt, mode)`：**吞掉一切异常**，失败=全默认值（调用方判 uid 空）。
    pub async fn fetch_user_info(jt: &str, mode: &str) -> QUserInfo {
        let url = format!("{}{}", open_api_base(mode), EP_USERINFO);
        let mut req = api_client().get(&url);
        for (k, v) in bearer_headers(jt) {
            req = req.header(k, v);
        }
        let text = match req.send().await {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(_) => return QUserInfo::default(),
        };
        parse_user_info(&text)
    }

    /// 对应 `loginWithPat(context, pat, mode)`：exchange → userinfo → save（置为当前账号）。
    pub async fn login_with_pat(pat: &str, mode: &str) -> Result<QoderAccount, String> {
        let trimmed = pat.trim();
        if trimmed.is_empty() {
            return Err("请粘贴 PAT".into());
        }
        let jt = exchange_job_token(trimmed, mode).await?;
        let info = fetch_user_info(&jt.token, mode).await;
        if info.uid.trim().is_empty() {
            return Err("Qoder 未返回账号 ID，请确认 PAT 与区域（国内/国际）匹配".into());
        }
        let cred = QoderAccount {
            pat: trimmed.to_string(),
            job_token: jt.token,
            job_refresh_token: jt.refresh_token,
            expires_at: jt.expires_at_ms,
            uid: info.uid,
            name: info.name,
            email: info.email,
            mode: qoder_normalize_mode(mode).to_string(),
            organization_id: info.organization_id,
            organization_name: info.organization_name,
            organization_tags: info.organization_tags,
            data_policy_agreed: info.data_policy_agreed,
            is_data_policy_modifiable: info.is_data_policy_modifiable,
            fingerprint: None,
        };
        QoderAuthStore::global().save(&cred);
        Ok(cred)
    }

    /// `validJobTokenFor` 的新鲜判据（纯函数，now_ms 注入测试）：`jobToken 非空 && now+skew < expiresAt`。
    pub fn job_token_fresh_at(now_ms: i64, job_token: &str, expires_at_ms: i64, skew_ms: i64) -> bool {
        !job_token.trim().is_empty() && now_ms + skew_ms < expires_at_ms
    }

    /// jrt 优先续、续不动用 PAT 重换（两者都无→None）。exchange 可降级、refresh 失败不外抛（runCatching 语义）。
    async fn renew_job_token(cred: &QoderAccount) -> Option<JobToken> {
        let r = if !cred.job_refresh_token.trim().is_empty() {
            refresh_job_token(&cred.job_refresh_token, &cred.mode).await
        } else if !cred.pat.trim().is_empty() {
            exchange_job_token(&cred.pat, &cred.mode).await
        } else {
            return None;
        };
        r.ok()
    }

    /// 对应 `validJobTokenFor(context, cred, skewMillis)`：临期先用 jrt 续、续不动用 PAT 换、写回库
    /// （update_tokens 顺带 meta）；都失败时**回落已存 jobToken**（若有）。不改当前账号指针。
    pub async fn valid_job_token_for(store: &QoderAuthStore, cred: &QoderAccount, skew_ms: i64) -> Option<String> {
        let now = now_ms();
        if job_token_fresh_at(now, &cred.job_token, cred.expires_at, skew_ms) {
            return Some(cred.job_token.clone());
        }
        let renewed = match renew_job_token(cred).await {
            Some(r) => r,
            None => {
                let t = cred.job_token.clone();
                return if t.trim().is_empty() { None } else { Some(t) };
            }
        };
        let mut new_cred = cred.clone();
        new_cred.job_token = renewed.token.clone();
        if !renewed.refresh_token.trim().is_empty() {
            new_cred.job_refresh_token = renewed.refresh_token;
        } else {
            new_cred.job_refresh_token = cred.job_refresh_token.clone();
        }
        new_cred.expires_at = renewed.expires_at_ms;
        store.update_tokens(&new_cred);
        Some(renewed.token)
    }

    /// 对应 `forceRefreshJobToken`：被 401 拒了强制续一次；续不动 None（调用方据实报错，别无限重试）。
    pub async fn force_refresh_job_token(store: &QoderAuthStore, cred: &QoderAccount) -> Option<String> {
        let renewed = renew_job_token(cred).await?;
        let mut new_cred = cred.clone();
        new_cred.job_token = renewed.token.clone();
        if !renewed.refresh_token.trim().is_empty() {
            new_cred.job_refresh_token = renewed.refresh_token;
        }
        new_cred.expires_at = renewed.expires_at_ms;
        store.update_tokens(&new_cred);
        Some(renewed.token)
    }

    /// 当前账号版（供积分轮询等）：无账号 None。
    pub async fn valid_job_token(skew_ms: i64) -> Option<String> {
        let store = QoderAuthStore::global();
        let cred = store.load()?;
        valid_job_token_for(&store, &cred, skew_ms).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::trae::TraeRegion;

    // ---------- URL 编解码 ----------

    #[test]
    fn url_encode_matches_java_urlencoder() {
        assert_eq!(url_encode("1"), "1");
        assert_eq!(url_encode("native_ide"), "native_ide");
        assert_eq!(url_encode("2.3.62834"), "2.3.62834");
        assert_eq!(url_encode("*-_."), "*-_.");
        assert_eq!(url_encode("a b"), "a+b");
        assert_eq!(url_encode("http://127.0.0.1:18080/authorize"), "http%3A%2F%2F127.0.0.1%3A18080%2Fauthorize");
        assert_eq!(url_encode("中"), "%E4%B8%AD");
    }

    #[test]
    fn url_decode_roundtrip() {
        assert_eq!(url_decode("http%3A%2F%2F127.0.0.1%3A18080%2Fauthorize"), "http://127.0.0.1:18080/authorize");
        assert_eq!(url_decode("a+b"), "a b");
        assert_eq!(url_decode("%41%C2%B0"), "A°");
        // 非法 UTF-8 单字节：Java newString(UTF-8) 得到替换符（与 from_utf8_lossy 同形）
        assert!(url_decode("%41%B0").starts_with('A'));
        assert_eq!(url_decode("%41%B0").chars().count(), 2);
        // 非法 % 序列保持原样（org 场景极少，行为取宽松）
        assert_eq!(url_decode("%zz"), "%zz");
    }

    // ---------- 登录链接 / 回调解析 ----------

    #[test]
    fn login_url_is_byte_faithful_except_random_ids() {
        let url = trae::build_login_url(TraeRegion::of("cn"));
        let base = "https://www.trae.cn/authorization?".to_string();
        assert!(url.starts_with(&base), "{url}");
        let q = &url[base.len()..];
        let pairs: Vec<&str> = q.split('&').collect();
        // 参数条数与逐项键+编码值前缀逐字节断言（随机值只断言键名与静态值）
        assert_eq!(pairs[0], "login_version=1");
        assert_eq!(pairs[1], "auth_from=solo");
        assert_eq!(pairs[2], "login_channel=native_ide");
        assert_eq!(pairs[3], "plugin_version=2.3.62834");
        assert_eq!(pairs[4], "auth_type=local");
        assert_eq!(pairs[5], "client_id=en1oxy7wnw8j9n");
        assert_eq!(pairs[6], "redirect=0");
        assert!(pairs[7].starts_with("login_trace_id=") && pairs[7].len() == "login_trace_id=".len() + 16);
        assert!(pairs[8].starts_with("auth_callback_url=http%3A%2F%2F127.0.0.1%3A18080%2Fauthorize"));
        assert!(pairs[9].starts_with("machine_id=") && pairs[9].len() == "machine_id=".len() + 32);
        assert_eq!(pairs.len(), 18);
        assert_eq!(pairs[13], "x_device_brand=PC");
        assert_eq!(pairs[14], "x_device_type=PC");
        assert_eq!(pairs[15], "x_os_version=1.0");
        assert_eq!(pairs[16], format!("x_app_version={}", TraeRegion::IDE_VERSION));
        assert_eq!(pairs[17], "x_app_type=stable");
    }

    #[test]
    fn login_url_sg_client_id_switches() {
        let url = trae::build_login_url(TraeRegion::of("sg"));
        assert!(url.starts_with("https://www.trae.ai/authorization?"));
        assert!(url.contains("&client_id=ono9krqynydwx5&"));
    }

    #[test]
    fn callback_detection_and_token_extraction() {
        let base = "http://127.0.0.1:18080/authorize";
        assert!(trae::is_callback(base));
        assert!(trae::is_callback(&format!("{base}?refreshToken=abc123")));
        assert!(!trae::is_callback("http://evil.example.com/authorize?refreshToken=abc"));
        assert_eq!(trae::extract_refresh_token(base), None);
        assert_eq!(trae::extract_refresh_token(&format!("{base}?refreshToken=abc123")).as_deref(), Some("abc123"));
        // refresh_token 兜底 + refreshToken 优先；空值跳过
        assert_eq!(
            trae::extract_refresh_token(&format!("{base}?refresh_token=rt&refreshToken= ")).as_deref(),
            Some("rt")
        );
        // userJwt 兜底（双重 URL 编码的 JSON 串）
        let jwt = r#"{"RefreshToken":"from-jwt"}"#;
        let once = url_encode(jwt);
        let twice = url_encode(&once);
        let with_once = format!("{base}?userJwt={once}");
        let with_twice = format!("{base}?userJwt={twice}");
        assert_eq!(trae::extract_refresh_token(&with_once).as_deref(), Some("from-jwt"));
        assert_eq!(trae::extract_refresh_token(&with_twice).as_deref(), Some("from-jwt"));
    }

    #[test]
    fn parse_json_param_accepts_plain_and_double_encoded() {
        let v = trae::parse_json_param(Some(r#"{"a":1}"#)).unwrap();
        assert_eq!(v["a"], 1);
        let enc = url_encode(r#"{"a":2}"#);
        let v2 = trae::parse_json_param(Some(&enc)).unwrap();
        assert_eq!(v2["a"], 2);
        assert!(trae::parse_json_param(Some("")).is_none());
        assert!(trae::parse_json_param(None).is_none());
        assert!(trae::parse_json_param(Some("nope")).is_none());
    }

    // ---------- ExchangeToken / GetUserInfo 构造与解析 ----------

    #[test]
    fn exchange_request_is_faithful() {
        let r = TraeRegion::of("cn");
        assert_eq!(
            trae::exchange_body(r, "R1").to_string(),
            r#"{"ClientID":"en1oxy7wnw8j9n","ClientSecret":"-","RefreshToken":"R1","UserID":""}"#
        );
        let url = format!("{}{}", r.oauth_host, trae::EP_EXCHANGE);
        assert_eq!(url, "https://api.trae.com.cn/cloudide/api/v3/trae/oauth/ExchangeToken");
        let h = trae::exchange_headers();
        assert!(h.contains(&("Content-Type", "application/json".to_string())));
        assert!(h.contains(&("Accept", "application/json".to_string())));
        assert!(h.contains(&("User-Agent", "Trae/0.1.43".to_string())));
        assert_eq!(h.len(), 3);

        let uh = trae::user_info_headers("TK1");
        assert_eq!(uh.len(), 4);
        assert!(uh.contains(&("X-Cloudide-Token", "TK1".to_string())));
        assert_eq!(
            trae::user_info_body().to_string(),
            r#"{"IDEVersion":"0.1.43","ReqSource":"IDE"}"#
        );
    }

    #[test]
    fn parse_exchange_response_fields() {
        let text = r#"{"Result":{"Token":"T","RefreshToken":"R2","TokenExpireAt":"1780000000000"}}"#;
        let e = trae::parse_exchange_response(text, 1_700_000_000).unwrap();
        assert_eq!(e.access_token, "T");
        assert_eq!(e.refresh_token.as_deref(), Some("R2"));
        // >1e12 判毫秒 → 秒
        assert_eq!(e.expires_at, Some(1_780_000_000));

        // TokenExpireAt 缺失 → TokenExpireDuration 兜底
        let text2 = r#"{"Result":{"Token":"T","TokenExpireDuration":3600}}"#;
        let e2 = trae::parse_exchange_response(text2, 1_700_000_000).unwrap();
        assert_eq!(e2.expires_at, Some(1_700_003_600));
        assert_eq!(e2.refresh_token, None);

        // TokenExpireAt 是数字型也认（org.json optString 对 number 的转换）
        let text3 = r#"{"Result":{"Token":"T","TokenExpireAt":1780000000}}"#;
        let e3 = trae::parse_exchange_response(text3, 1_000).unwrap();
        assert_eq!(e3.expires_at, Some(1_780_000_000));

        // Token 空白 → Err；缺 Result → Err
        assert!(trae::parse_exchange_response(r#"{"Result":{"Token":"  "}}"#, 0).is_err());
        assert!(trae::parse_exchange_response("{}", 0).is_err());
    }

    #[test]
    fn parse_user_info_fields() {
        let text = r#"{"Result":{"UserID":"7612345678901234567","ScreenName":"小明","EnterpriseID":""}}"#;
        let u = trae::parse_user_info(text).unwrap();
        assert_eq!(u.uid, "7612345678901234567");
        assert_eq!(u.nickname, "小明");
        assert!(trae::parse_user_info("{}").is_err());
    }

    #[test]
    fn normalize_expiry_boundary() {
        assert_eq!(trae::normalize_expiry(1_000_000_000_000), 1_000_000_000_000);
        assert_eq!(trae::normalize_expiry(1_000_000_000_001), 1_000_000_000);
    }

    // ---------- 续期编排的纯守卫 ----------

    #[test]
    fn race_guards_pinned() {
        // Kotlin selfCheck 同款断言
        assert!(trae::reuse_after_race("old", "new"));
        assert!(!trae::reuse_after_race("same", "same"));
        assert!(!trae::reuse_after_race("old", ""));
        assert!(trae::should_write_back("r1", "r1"));
        assert!(trae::should_write_back("r1", ""));
        assert!(!trae::should_write_back("r1", "r2"));
    }

    #[test]
    fn expired_within_thresholds_sec() {
        let now = now_sec();
        let skew = trae::REFRESH_SKEW_SEC;
        assert!(trae::expired_within(0, skew));
        assert!(trae::expired_within(-5, skew));
        assert!(trae::expired_within(now - 10, skew));
        assert!(trae::expired_within(now + 60, skew));
        assert!(!trae::expired_within(now + skew + 60, skew));
    }

    // ---------- Qoder ----------

    #[test]
    fn qoder_bases_are_two_domains() {
        assert_eq!(qoder::open_api_base("cn"), "https://openapi.qoder.com.cn");
        assert_eq!(qoder::open_api_base("global"), "https://openapi.qoder.sh");
        assert_eq!(qoder::open_api_base("bogus"), "https://openapi.qoder.com.cn");
        assert_eq!(qoder::infer_base("cn"), "https://gateway.qoder.com.cn");
        assert_eq!(qoder::infer_base("global"), "https://api2.qoder.sh");
    }

    #[test]
    fn qoder_post_requests_are_minimal() {
        assert_eq!(qoder::exchange_body("pt-1").to_string(), r#"{"personal_token":"pt-1"}"#);
        assert_eq!(qoder::refresh_body("jrt-1").to_string(), r#"{"refresh_token":"jrt-1"}"#);
        let h = qoder::post_headers();
        assert_eq!(
            h,
            vec![
                ("Content-Type", "application/json"),
                ("Accept", "application/json"),
                ("User-Agent", "traework2api"),
            ]
        );
        let b = qoder::bearer_headers("jt");
        assert_eq!(b[0], ("Authorization", "Bearer jt".to_string()));
    }

    #[test]
    fn qoder_job_token_parsing() {
        let t = qoder::parse_job_token(r#"{"token":"T","refresh_token":"R","expires_in":3600000}"#, "", 1000).unwrap();
        assert_eq!(t.token, "T");
        assert_eq!(t.refresh_token, "R");
        assert_eq!(t.expires_at_ms, 1000 + 3600000);
        // 缺 expires_in → 24h；exchange 的 refresh 缺省空串
        let t2 = qoder::parse_job_token(r#"{"token":"T"}"#, "", 0).unwrap();
        assert_eq!(t2.expires_at_ms, qoder::EXPIRE_FALLBACK_MS);
        assert_eq!(t2.refresh_token, "");
        // refresh 的缺省=入参
        let t3 = qoder::parse_job_token(r#"{"token":"T"}"#, "old-jrt", 0).unwrap();
        assert_eq!(t3.refresh_token, "old-jrt");
        // token 空白 → Err
        assert!(qoder::parse_job_token(r#"{"token":" "}"#, "", 0).is_err());
        assert!(qoder::parse_job_token("not json", "", 0).is_err());
    }

    #[test]
    fn qoder_user_info_parsing() {
        let text = r#"{"uid":"","id":"u1","email":"e@x","name":"","organization_tags":["a"," ","b,c"],"data_policy_agreed":true}"#;
        let u = qoder::parse_user_info(text);
        assert_eq!(u.uid, "u1");
        assert_eq!(u.name, "");
        // name 空→username 兜底
        let u2 = qoder::parse_user_info(r#"{"userID":"u9","username":"un","organization_tags":"x, y"}"#);
        assert_eq!(u2.uid, "u9");
        assert_eq!(u2.name, "un");
        assert_eq!(u2.organization_tags, vec!["x", "y"]);
        assert!(u2.is_data_policy_modifiable);
        assert!(!u2.data_policy_agreed);
        // 非 JSON → 全默认
        assert_eq!(qoder::parse_user_info("oops"), QUserInfoProbe::default());
    }

    // 测试内联默认值参照（等价 QUserInfo::default()，断言可读性用）
    type QUserInfoProbe = qoder::QUserInfo;

    #[test]
    fn qoder_fresh_at_boundary_ms() {
        let now = 1_700_000_000_000i64;
        let skew = qoder::JOB_SKEW_MS;
        assert!(!qoder::job_token_fresh_at(now, "", now + 10 * skew, skew)); // 无 token
        assert!(!qoder::job_token_fresh_at(now, "T", 0, skew)); // 0=未知
        assert!(qoder::job_token_fresh_at(now, "T", now + skew + 1, skew));
        assert!(!qoder::job_token_fresh_at(now, "T", now + skew, skew)); // 恰在窗内=须续
        assert!(!qoder::job_token_fresh_at(now, "T", now - 1, skew));
    }
}
