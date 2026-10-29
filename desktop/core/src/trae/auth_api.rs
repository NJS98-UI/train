// 逐行移植 TraeAuthApi.kt —— Trae 官方认证 API。
//
// 对齐 reference/traework2api：登录指引 URL、ExchangeToken（refresh→access, token 轮换）、
// GetUserInfo。登录是"回调流"——登录成功后上游重定向到 127.0.0.1:18080/authorize，
// 把 refreshToken / userInfo / userJwt 放进 query；本模块只负责构造 URL 与换 token。
//
// 回调**由用户在浏览器里复制那条地址栏链接、再粘回 App**（finish_login）——
// 不依赖 WebView 或页面 JS（内置登录窗已整体移除，见 MainActivity.openLogin）。
use crate::log_i;
use crate::log_w;
use crate::state::AppState;
use crate::trae::auth_store::{self, cred_region};
use crate::trae::region::{self, TraeRegion};
use crate::types::TraeCred;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 脱敏日志：只打 uid / 是否过期 / 状态，绝不打 token 本身。（Kotlin TAG = "TraeAuth"）
const TAG: &str = "TraeAuth";

pub const IDE_VERSION: &str = region::IDE_VERSION;
pub const CALLBACK_HOST: &str = "127.0.0.1";
pub const CALLBACK_PORT: u16 = 18080;
pub const CALLBACK_PATH: &str = "/authorize";

const EP_EXCHANGE: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";
const EP_USER_INFO: &str = "/cloudide/api/v3/trae/GetUserInfo";
const JSON: &str = "application/json";

// 随区域变化的 host / ClientID 统一从 TraeRegion 取；CN 为已实测值。
// 登录入口走 require_login()，区域未接入时明确报错而不是拿 CN 的值去连。
fn region() -> Result<TraeRegion, String> {
    TraeRegion::require_login(region::current_global())
}

fn client() -> &'static reqwest::Client {
    // 对齐 Kotlin：connectTimeout 20s / readTimeout 30s（reqwest 用总超时近似 read 超时）
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ----------------------------------------------------------------------
// org.json optXxx 语义助手
// ----------------------------------------------------------------------

fn opt_str(o: &Value, k: &str, d: &str) -> String {
    let Some(obj) = o.as_object() else { return d.to_string() };
    match obj.get(k) {
        None | Some(Value::Null) => d.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn opt_i64(o: &Value, k: &str, d: i64) -> i64 {
    let Some(obj) = o.as_object() else { return d };
    match obj.get(k) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(d),
        Some(Value::String(s)) => s.parse::<i64>().unwrap_or(d),
        _ => d,
    }
}

/// 按字符截断（对齐 Kotlin `String.take(n)`）。
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ----------------------------------------------------------------------
// 登录：粘贴浏览器回调链接换取 token（**这是本 App 唯一的登录方式**）
// ----------------------------------------------------------------------

/// 登录：粘贴浏览器回调链接换取 token。
///
/// 对齐 dsh-trae-solo login.sh 的做法：用户用**系统浏览器**打开登录链接，
/// 登录成功后浏览器会跳到打不开的 `127.0.0.1:18080/authorize?...`（地址栏里带着
/// refreshToken），用户把整条链接粘回来即可。
///
/// @return 登录后的账号信息；失败返回 Err（消息可直接展示给用户）。
pub async fn finish_login(app: &AppState, callback_url: &str) -> Result<TraeCred, String> {
    let url = callback_url.trim();
    if url.is_empty() {
        return Err("请粘贴回调链接".to_string());
    }
    if !is_callback(url) {
        return Err(format!(
            "这不是 Trae 回调链接。应形如 http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}?refreshToken=…"
        ));
    }
    let refresh = extract_refresh_token(url).ok_or_else(|| {
        "链接里没有 refreshToken，请确认复制的是登录后跳转的那条完整链接".to_string()
    })?;
    let reg = region()?;
    let ex = exchange_token(&refresh, reg).await?;
    let user = get_user_info(&ex.access_token, reg).await?;
    let cred = TraeCred {
        access_token: ex.access_token,
        refresh_token: ex.refresh_token.unwrap_or(refresh),
        expires_at: ex.expires_at.unwrap_or(0),
        uid: user.uid,
        nickname: user.nickname,
        region_id: reg.id().to_string(), // 账号自带区域：切换账号时转发才知道该打哪个 host
    };
    auth_store::save(app, &cred);
    log_i!(TAG, "手动粘贴登录成功 (uid={}, region={})", cred.uid, cred.region_id);
    Ok(cred)
}

/// 构造登录指引 URL。`device`/`machine` 每次生成，兼容上游 cookie/设备绑定。
///
/// ⚠️ **别把它跟 ug 接口那三个设备标识**（device_id_for / session_id_for /
/// market_user_id_for）**混为一谈**：那三个是从 uid 派生、签到时必须配套携带的身份，
/// 而这里是**登录**参数 —— 登录时压根还没有 uid，也就无从派生，只能随机。
/// 曾经试图让两者复用同一个值（把两件事搞混），已回退。
pub fn build_login_url() -> String {
    let reg = match region() {
        Ok(r) => r,
        // Kotlin requireLogin 抛 IllegalStateException；两区域现均 canLogin=true，属死路径
        Err(m) => panic!("{m}"),
    };
    let device = hex_str(16);
    let machine = hex_str(16);
    let params: [(&str, String); 18] = [
        ("login_version", "1".into()),
        ("auth_from", "solo".into()),
        ("login_channel", "native_ide".into()),
        ("plugin_version", "2.3.62834".into()),
        ("auth_type", "local".into()),
        ("client_id", reg.client_id().into()),
        ("redirect", "0".into()),
        ("login_trace_id", hex_str(8)),
        ("auth_callback_url", format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}")),
        ("machine_id", machine.clone()),
        ("device_id", device.clone()),
        ("x_device_id", device.clone()),
        ("x_machine_id", machine.clone()),
        ("x_device_brand", "PC".into()),
        ("x_device_type", "PC".into()),
        ("x_os_version", "1.0".into()),
        ("x_app_version", IDE_VERSION.into()),
        ("x_app_type", "stable".into()),
    ];
    let q = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    // 只打 host 与 client_id（非敏感），便于确认区域没串号（曾出过恒为国服的 bug）
    log_i!(TAG, "生成登录链接: region={} host={} client_id={}", reg.id(), reg.console_host(), reg.client_id());
    format!("{}/authorization?{}", reg.console_host(), q)
}

/// 回调 query 是否是 Trae 登录回调地址。
pub fn is_callback(url: &str) -> bool {
    let prefix = format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}");
    url.get(..prefix.len()).map(|s| s.eq_ignore_ascii_case(&prefix)).unwrap_or(false)
}

/// 从回调 query 提取 refreshToken（一次 URL 解码，parse_qs 语义）。
pub fn extract_refresh_token(callback: &str) -> Option<String> {
    let query = callback.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut map: HashMap<String, String> = HashMap::new();
    for p in query.split('&').filter(|p| !p.is_empty()) {
        let mut it = p.splitn(2, '=');
        let k = decode(it.next().unwrap_or(""));
        let v = decode(it.next().unwrap_or(""));
        map.insert(k, v);
    }
    let mut rt: Option<String> = map.get("refreshToken").filter(|s| !s.trim().is_empty()).cloned();
    if rt.is_none() {
        rt = map.get("refresh_token").filter(|s| !s.trim().is_empty()).cloned();
    }
    if rt.is_none() {
        // 兜底：回调缺 refreshToken 时从 userJwt 里取（同 reference login.sh）
        let jwt = map
            .get("userJwt")
            .and_then(|v| parse_json(Some(v)))
            .or_else(|| map.get("user_jwt").and_then(|v| parse_json(Some(v))));
        rt = jwt.and_then(|j| {
            let s = opt_str(&j, "RefreshToken", "");
            if s.trim().is_empty() { None } else { Some(s) }
        });
    }
    rt.filter(|s| !s.trim().is_empty())
}

/// 解析回调里可能双重 URL 编码的 JSON 参数；解析失败返回 None。
pub fn parse_json_param(raw: Option<&str>) -> Option<Value> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }
    let decoded = decode(raw);
    for cand in [raw.to_string(), decoded] {
        if let Ok(v) = serde_json::from_str::<Value>(&cand) {
            if v.is_object() {
                return Some(v);
            }
        }
    }
    None
}

// ----------------------------------------------------------------------
// ExchangeToken / GetUserInfo
// ----------------------------------------------------------------------

/// ExchangeToken 的返回（refresh token 轮换）。
#[derive(Clone, Debug)]
pub struct ExchangeResult {
    pub access_token: String,
    /// 轮换后的新 refresh；None=上游未返回（沿用旧值）
    pub refresh_token: Option<String>,
    /// unix 秒；None=未返回有效期
    pub expires_at: Option<i64>,
}

/// refresh token 换 access token（refreshToken 轮换）。网络/结构错误返回 Err。
///
/// [region] 必须传**该账号自己的区域**，否则会拿 A 区域的 token 打到 B 区域的
/// OAuth host（区域串号，实测踩过）。
pub async fn exchange_token(refresh_token: &str, region: TraeRegion) -> Result<ExchangeResult, String> {
    let body = json!({
        "ClientID": region.client_id(),
        "RefreshToken": refresh_token,
        "ClientSecret": "-",
        "UserID": "",
    })
    .to_string();
    let url = format!("{}{}", region.oauth_host(), EP_EXCHANGE);
    let resp = client()
        .post(url)
        .body(body)
        .header("Content-Type", JSON)
        .header("Accept", JSON)
        .header("User-Agent", format!("Trae/{IDE_VERSION}"))
        .send()
        .await
        .map_err(|e| format!("ExchangeToken 请求失败: {e}"))?;
    let code = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&code) {
        return Err(format!("ExchangeToken http {code}: {}", take_chars(&text, 200)));
    }
    let obj: Value = serde_json::from_str(&text).map_err(|e| format!("ExchangeToken 返回解析失败: {e}"))?;
    let result = obj.get("Result").filter(|v| v.is_object()).ok_or("ExchangeToken 缺 Result")?;
    let token = opt_str(result, "Token", "");
    if token.trim().is_empty() {
        return Err("ExchangeToken 未返回 Token".to_string());
    }
    let new_refresh = {
        let s = opt_str(result, "RefreshToken", "");
        if s.trim().is_empty() { None } else { Some(s) }
    };
    let expire_at_raw = opt_str(result, "TokenExpireAt", "");
    // 到期时间：优先 TokenExpireAt（上游可能是毫秒）；缺失时用 TokenExpireDuration 兜底
    // （reference client.go:214-219 同款）。缺了兜底会回落成"旧的可能已过期的值"，
    // 导致每次启动都重复轮换 ExchangeToken。注意上游可能返回数字或字符串。
    let expires_at = expire_at_raw
        .parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .map(normalize_ms)
        .or_else(|| {
            let d = opt_i64(result, "TokenExpireDuration", 0);
            if d > 0 {
                Some(now_secs() + d)
            } else {
                None
            }
        });
    Ok(ExchangeResult { access_token: token, refresh_token: new_refresh, expires_at })
}

/// GetUserInfo 的返回。
#[derive(Clone, Debug)]
pub struct UserInfo {
    pub uid: String,
    pub nickname: String,
    pub enterprise_id: String,
}

/// 用 access token 拉取账号信息。
pub async fn get_user_info(access_token: &str, region: TraeRegion) -> Result<UserInfo, String> {
    let body = json!({ "ReqSource": "IDE", "IDEVersion": IDE_VERSION }).to_string();
    let url = format!("{}{}", region.oauth_host(), EP_USER_INFO);
    let resp = client()
        .post(url)
        .body(body)
        .header("Content-Type", JSON)
        .header("Accept", JSON)
        .header("User-Agent", format!("Trae/{IDE_VERSION}"))
        .header("X-Cloudide-Token", access_token)
        .send()
        .await
        .map_err(|e| format!("GetUserInfo 请求失败: {e}"))?;
    let code = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&code) {
        return Err(format!("GetUserInfo http {code}: {}", take_chars(&text, 200)));
    }
    let obj: Value = serde_json::from_str(&text).map_err(|e| format!("GetUserInfo 返回解析失败: {e}"))?;
    let result = obj.get("Result").filter(|v| v.is_object()).ok_or("GetUserInfo 缺 Result")?;
    Ok(UserInfo {
        uid: opt_str(result, "UserID", ""),
        nickname: opt_str(result, "ScreenName", ""),
        enterprise_id: opt_str(result, "EnterpriseID", ""),
    })
}

// ----------------------------------------------------------------------
// 续期编排
// ----------------------------------------------------------------------

/// 启动即用的续期编排：读**当前账号**的加密凭证 → 若 token 在 [skew_sec] 内将过期则换新 → 写回。
/// 返回当前有效 access token（无可用 token 时 None）。旧 refresh 失败时不破坏已存数据。
///
/// 关键：账号自带 region，续期必须打到**该账号自己的** OAuth host。
/// 这里临时把当前区域切到账号的区域再换 token，
/// 否则国际服账号会被拿去国内服换（或反过来）——那是实打实踩过的坑。
pub async fn restart_with_valid_token(app: &AppState, skew_sec: i64) -> Option<String> {
    let cred = auth_store::load(app)?;
    // 让后续 exchange_token/get_user_info 走该账号自己的区域
    region::set_current_global(cred_region(&cred));
    if !cred.access_token.is_empty() && !expired_within(Some(cred.expires_at), skew_sec) {
        log_i!(
            TAG,
            "token 有效，无需续期 (uid={}, region={}, exp={})",
            cred.uid,
            cred.region_id,
            cred.expires_at
        );
        return Some(cred.access_token);
    }
    // 临近/已过期 → 用 refresh 换新的
    log_i!(
        TAG,
        "token 临近/已过期，触发续期 (uid={}, region={}, exp={})",
        cred.uid,
        cred.region_id,
        cred.expires_at
    );
    if cred.refresh_token.is_empty() {
        log_w!(TAG, "无 refreshToken，无法续期");
        return if cred.access_token.is_empty() { None } else { Some(cred.access_token) };
    }
    let reg = cred_region(&cred);
    let ex = match exchange_token(&cred.refresh_token, reg).await {
        Ok(e) => e,
        Err(_) => return None,
    };
    auth_store::save(
        app,
        &TraeCred {
            access_token: ex.access_token.clone(),
            refresh_token: ex.refresh_token.clone().unwrap_or_else(|| cred.refresh_token.clone()),
            expires_at: ex.expires_at.unwrap_or(cred.expires_at),
            uid: cred.uid.clone(),
            nickname: cred.nickname.clone(),
            region_id: cred.region_id.clone(),
        },
    );
    log_i!(
        TAG,
        "续期成功 (新exp={}, refresh轮换={})",
        ex.expires_at.unwrap_or(cred.expires_at),
        ex.refresh_token.is_some()
    );
    Some(ex.access_token)
}

/// 对**指定账号**取有效 token（临期/过期就用它自己的 refreshToken 续），**不改当前账号、不翻全局区域**。
///
/// 与 [restart_with_valid_token] 的区别：那个作用于"当前账号"并会临时切换全局区域；
/// 这里给积分监测器扫描**其它候选账号**用——必须按各自区域续、按 cred 写回，
/// 绝不能因为探测别的号就把用户正在用的当前账号切走或污染全局区域。
/// 续期失败时回落到已存的（可能过期的）access token，行为与原逻辑一致。
pub async fn restart_with_valid_token_for(app: &AppState, cred: &TraeCred, skew_sec: i64) -> Option<String> {
    ensure_fresh_token_for(app, cred, skew_sec)
        .await
        .or_else(|| if cred.access_token.is_empty() { None } else { Some(cred.access_token.clone()) })
}

/// **唯一续期入口**（所有"要不要续/怎么续"的判定都收到这里）。
///
/// 两条纪律，缺一条就会把账号弄废：
///
/// 1. **先读库里的最新凭据**，不拿调用方手里的快照去续。调用方的 `cred` 可能是几秒前
///    `load_by_uid` 拿的，期间别处（转发/积分监测/另一个账号的签到）已经续过一次 ——
///    再拿**旧的 refreshToken** 去换，服务端会把它当成"旧令牌重放"，按轮换安全策略
///    把整条令牌族作废。表现就是：签到明明成功了，过一会儿全部 401。
/// 2. **别人续过就复用，不再轮换**：库里 access token 已经和调用方手里的不一样了，
///    说明新鲜度由别人保证了，直接拿来用，一次网络请求都不发。
///
/// 返回 None 表示"续不动"（无 refreshToken / 上游拒绝），调用方据此如实报错。
pub async fn ensure_fresh_token_for(app: &AppState, cred: &TraeCred, skew_sec: i64) -> Option<String> {
    let fresh = auth_store::load_by_uid(app, &cred.uid).unwrap_or_else(|| cred.clone());
    if !fresh.access_token.is_empty() && !expired_within(Some(fresh.expires_at), skew_sec) {
        return Some(fresh.access_token);
    }
    // 调用方快照里的 token 和库里的不一样 → 期间已被别处刷新，直接用库里的
    if reuse_after_race(&cred.access_token, &fresh.access_token) {
        log_i!(TAG, "{} 已被别处续期，复用库里的 token（不轮换）", cred.uid);
        return Some(fresh.access_token);
    }
    refresh_locked(app, &cred.uid, &fresh.access_token).await
}

/// 调用方手里的 token 与库里不同 → 说明期间别处已经续过，该复用而不是再续一次。
pub fn reuse_after_race(stale_access: &str, stored_access: &str) -> bool {
    !stored_access.is_empty() && stored_access != stale_access
}

/// 写回前的守卫：库里 refreshToken 还是我们用的那个才覆盖（防止覆盖更新的凭据）。
pub fn should_write_back(used_refresh: &str, stored_refresh: &str) -> bool {
    stored_refresh.is_empty() || stored_refresh == used_refresh
}

/// 真续期：必须在 trae_lock 内调用（Mutex 不可重入，调用方已持锁时不可再包）。
async fn refresh_locked(app: &AppState, uid: &str, stale_access: &str) -> Option<String> {
    let base = auth_store::load_by_uid(app, uid)?;
    if reuse_after_race(stale_access, &base.access_token) {
        return Some(base.access_token);
    }
    if base.refresh_token.is_empty() {
        log_w!(TAG, "{uid} 无 refreshToken，无法续期");
        return None;
    }
    let ex = match exchange_token(&base.refresh_token, cred_region(&base)).await {
        Ok(e) => e,
        Err(e) => {
            log_w!(TAG, "{uid} 续期失败: {e}");
            return None;
        }
    };
    let stored = auth_store::load_by_uid(app, uid);
    if let Some(s) = &stored {
        if !should_write_back(&base.refresh_token, &s.refresh_token) {
            // 我们续期的这段时间里，别处又把 refreshToken 换掉了 —— 我们的结果已经过期，
            // 写回去会把更新的凭据盖掉（下一个请求就拿旧令牌去撞墙）。
            log_w!(TAG, "{uid} 续期期间凭据又被更新，放弃写回，采用库里那份");
            return Some(if s.access_token.is_empty() { ex.access_token } else { s.access_token.clone() });
        }
    }
    let new_cred = TraeCred {
        access_token: ex.access_token.clone(),
        refresh_token: ex.refresh_token.clone().unwrap_or_else(|| base.refresh_token.clone()),
        expires_at: ex.expires_at.unwrap_or(base.expires_at),
        ..base.clone()
    };
    auth_store::update_tokens(app, &new_cred); // 不碰 active
    auth_store::update_meta(app, &new_cred.to_meta());
    log_i!(TAG, "{uid} 续期成功 (refresh轮换={})", ex.refresh_token.is_some());
    Some(ex.access_token)
}

/// **强制**续一次（不看是否临期）：积分查询撞到 401 时救急。
///
/// 与 [restart_with_valid_token_for] 的区别：那个是"快过期才续"，这个是"已经被拒了就续"。
/// 续不动返回 None（调用方据此把失败如实标出来，别无限重试）。
pub async fn force_refresh_token_for(app: &AppState, cred: &TraeCred) -> Option<String> {
    // 走同一入口：先看库里是否已被别处续过（那就直接复用，绝不再轮换一次）。
    let fresh = auth_store::load_by_uid(app, &cred.uid).unwrap_or_else(|| cred.clone());
    if reuse_after_race(&cred.access_token, &fresh.access_token) {
        return Some(fresh.access_token);
    }
    refresh_locked(app, &cred.uid, &cred.access_token).await
}

/// 该 token 是否会在 `skew_sec` 秒内过期（0/负数=未知 → 必须当成已过期）。
fn expired_within(expires_at: Option<i64>, skew_sec: i64) -> bool {
    match expires_at {
        None => true,
        Some(e) if e <= 0 => true,
        Some(e) => now_secs() + skew_sec >= e,
    }
}

/// TokenExpireAt 毫秒 → Unix 秒（>1e12 判定毫秒）。
fn normalize_ms(v: i64) -> i64 {
    if v > 1_000_000_000_000 {
        v / 1000
    } else {
        v
    }
}

// ----------------------------------------------------------------------
// 工具
// ----------------------------------------------------------------------

/// java.net.URLDecoder.decode(s, "UTF-8") 等价：'+'→空格、%XX 解码；失败原样返回。
fn decode(s: &str) -> String {
    let replaced = s.replace('+', " ");
    let b = replaced.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return s.to_string();
            }
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => {
                    out.push(((h * 16) + l) as u8);
                    i += 3;
                }
                _ => return s.to_string(),
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// java.net.URLEncoder.encode(s, "UTF-8") 等价：字母数字与 `.-*_` 保留、空格→'+'、其余 %XX。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'*' | b'_' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn parse_json(raw: Option<&str>) -> Option<Value> {
    let r = raw?;
    serde_json::from_str::<Value>(r).ok().filter(|v| v.is_object())
}

/// 每次生成的随机 hex（登录参数 device/machine/trace 用；对齐 Kotlin SecureRandom 版）。
fn hex_str(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

// ----------------------------------------------------------------------
// 自检
// ----------------------------------------------------------------------

/// 自检 [expired_within] 的边界。
///
/// 为什么要它：这是"要不要续期"的唯一判据，而服务现在**每个转发请求**都靠它决定。
/// 判错的方向后果不对称：
///   - 该续期却判"有效" → 拿过期 token 打上游，持续 401（用户看到请求全废）
///   - 不该续期却判"过期" → 反复续期，且 refresh 是轮换的，白白多消耗
/// 所以两边都钉住，并覆盖"0/负数=未知"这个必须当成已过期的约定。
/// 纯计算，不碰密钥/网络，可安全在启动时调用。
pub fn self_check() {
    let now = now_secs();
    let skew = 3600i64;
    // 未知到期时间（0 / null / 负数）必须判为过期 → 才会走续期，而不是盲用一个未知的 token
    assert!(expired_within(Some(0), skew), "自检失败：exp=0（未知）应判为已过期");
    // 防令牌重放的两条守卫（"签到把 token 弄废"的结构性修复，别删）
    assert!(reuse_after_race("old", "new"), "自检失败：库里 token 更新过就应复用");
    assert!(!reuse_after_race("same", "same"), "自检失败：token 相同不算被别处续过");
    assert!(!reuse_after_race("old", ""), "自检失败：库里为空不能当已续期");
    assert!(should_write_back("r1", "r1"), "自检失败：refreshToken 没变应允许写回");
    assert!(should_write_back("r1", ""), "自检失败：库里没有 refreshToken 应允许写回");
    assert!(!should_write_back("r1", "r2"), "自检失败：refreshToken 变了必须放弃写回");
    assert!(expired_within(None, skew), "自检失败：exp=null 应判为已过期");
    assert!(expired_within(Some(-5), skew), "自检失败：负数 exp 应判为已过期");
    // 已经过去 → 过期
    assert!(expired_within(Some(now - 10), skew), "自检失败：已过去的 exp 应判为已过期");
    // 落在 skew 窗口内（即将过期）→ 也必须续期，不能等真的 401
    assert!(expired_within(Some(now + 60), skew), "自检失败：skew 内的 exp 应判为需续期");
    // 远离 skew → 不该续期（否则每请求都轮换 refresh）
    assert!(!expired_within(Some(now + skew + 60), skew), "自检失败：skew 外的 exp 不应判为续期");

    // 登录链接是**现在唯一的登录方式**（内置 WebView 窗已移除），
    // 它坏了 = 用户彻底进不去，所以钉住关键字段。
    let base = format!("http://{CALLBACK_HOST}:{CALLBACK_PORT}{CALLBACK_PATH}");
    assert!(is_callback(&base), "自检失败：回调地址本身必须被 is_callback 认作合法");
    // 缺 refreshToken 的裸回调不该被当成有效链接（否则会拿一个空 refresh 去换 token）
    assert!(extract_refresh_token(&base).is_none(), "自检失败：无 refreshToken 的回调不应解析出 token");
    assert!(is_callback(&format!("{base}?refreshToken=abc123")), "自检失败：带 refreshToken 的回调应合法");
    assert_eq!(
        extract_refresh_token(&format!("{base}?refreshToken=abc123")).as_deref(),
        Some("abc123"),
        "自检失败：refreshToken 解析错误"
    );
    // 别把别的站点的链接误判成回调（用户可能粘错东西）
    assert!(
        !is_callback("http://evil.example.com/authorize?refreshToken=abc"),
        "自检失败：非本机回调地址不该被认作合法"
    );
}
