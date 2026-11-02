//! Trae 签到/用量 API（checkin-credits.md §1.1，TraeUpstream.kt `ug()/fetchCheckin/claimCheckin/fetchUsage` 对应体）。
//!
//! - 请求构造与响应判定拆成**纯函数**（`build_ug_request` / `classify_ug_response` / `parse_*` /
//!   [`checkin_succeeded`]），单测逐字节断言 URL/头/body，不发网络请求；
//! - `last_auth_failed`（Kotlin `@Volatile`）→ 进程级 `AtomicBool`，每次 ug 调用前复位，
//!   供上层二分「token 被拒」vs「网络失败」文案；
//! - 设备三件套派生（seededStream，§1.1）在本模块私有实现——fingerprint.rs 只负责本地 API Key
//!   派生，签到设备标识不进那个文件（任务约束：不改 fingerprint.rs）。

use crate::auth::model::TraeAccount;
use crate::auth::store::TraeAuthStore;
use crate::upstream::trae::TraeRegion;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// `EP_CHECKIN_STATUS`。
pub const EP_CHECKIN_STATUS: &str = "/trae/api/v2/ug/checkin_credits/status";
/// `EP_CLAIM`。
pub const EP_CLAIM: &str = "/trae/api/v2/ug/checkin_credits/claim";
/// `EP_ENT_USAGE`。
pub const EP_ENT_USAGE: &str = "/trae/api/v2/pay/ide_user_ent_usage";

/// `lastAuthFailed`：区分鉴权失败与网络失败（每次 ug 调用前复位，§1.1 判定第 1 步）。
static LAST_AUTH_FAILED: AtomicBool = AtomicBool::new(false);

pub fn last_auth_failed() -> bool {
    LAST_AUTH_FAILED.load(Ordering::Relaxed)
}

/// 签到/用量专用客户端：connect 20s + read 180s（Kotlin `client` 逐字对齐；
/// reqwest 0.12.28 起有 `read_timeout`，无需再折中成总超时）。
fn ug_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(180))
            .build()
            .expect("reqwest ug client")
    })
}

// ---------------------------------------------------------------------------
// 设备三件套派生（§1.1 seededStream —— 逐字节对齐 Kotlin）
// ---------------------------------------------------------------------------

/// 三个配套设备标识（缺一即被上游判为共用设备 → 9074）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceIds {
    pub market_user_id: String,
    pub device_id: String,
    pub session_id: String,
}

/// 一次性派生三个标识（确定性：同 uid 恒等，异 uid 天然隔离）。
/// 派生本体复用 `upstream::trae`（`seeded_stream/device_id_for/session_id_for/market_user_id_for`
/// 已按 Kotlin `TraeUpstream` 逐字节实现并标注"供签到/用量链路复用"），此处只做打包。
pub fn derive_device_ids(uid: &str) -> DeviceIds {
    use crate::upstream::trae::{device_id_for, market_user_id_for, session_id_for};
    DeviceIds {
        market_user_id: market_user_id_for(uid),
        device_id: device_id_for(uid),
        session_id: session_id_for(uid),
    }
}

// ---------------------------------------------------------------------------
// 请求构造（纯函数，测试逐字节断言）
// ---------------------------------------------------------------------------

/// `TraeRegion.canQueryQuota`（Kotlin：`ugHost.isNotBlank()`）。
/// 只判账号自身区域，绝不读全局翻面（§1.1 区域快照纪律）。
pub fn can_query_quota(region: &TraeRegion) -> bool {
    !region.ug_host.trim().is_empty()
}

/// 随机头（每次调用变化）：X-Tt-Trace-Id / X-Request-Id。测试注入固定值即可全程逐字节断言。
#[derive(Clone, Debug)]
pub struct UgNonces {
    pub tt_trace_id: String,
    pub request_id: String,
}

impl UgNonces {
    /// `uuidNoDash().substring(0,16)` 包 `00-…-01` + 标准 UUID v4（Kotlin `java.util.UUID.randomUUID()`）。
    pub fn random() -> Self {
        let dashed = uuid::Uuid::new_v4().to_string();
        Self {
            tt_trace_id: format!("00-{}-01", dashed.replace('-', "").chars().take(16).collect::<String>()),
            request_id: dashed,
        }
    }
}

/// 按 Kotlin `ug()` 的**声明顺序**生成头列表（trace/request-id 两个随机项由 nonces 注入）。
/// 静态项 17 条 + 随机 2 条 = 19 个显式头（任务描述里的「22 头」= OkHttp 另隐式注入
/// Host/Content-Length/Connection 三条，见最终报告「不一致裁决」）。
pub fn ug_headers_ordered(token: &str, uid: &str, user_region_header: &str, nonces: &UgNonces) -> Vec<(String, String)> {
    let d = derive_device_ids(uid);
    vec![
        ("Authorization".into(), format!("Cloud-IDE-JWT {token}")),
        ("Accept".into(), "*/*".into()),
        ("Content-Type".into(), "application/json".into()),
        ("User-Agent".into(), "VSCode 1.107.1 (TRAE SOLO CN)".into()),
        ("X-Market-Client-Id".into(), "VSCode 1.107.1".into()),
        ("X-Market-User-Id".into(), d.market_user_id),
        ("X-Device-Id".into(), d.device_id),
        ("Vscode-Sessionid".into(), d.session_id),
        ("X-Lgw-Req-Sdk-Type".into(), "3".into()),
        ("Package-Type".into(), "stable_cn".into()),
        ("X-Lscbd-Aid".into(), "787976".into()),
        ("X-Lscbd-Platform".into(), "windows".into()),
        ("App-Version".into(), "0.1.45".into()),
        ("X-Tt-Trace-Id".into(), nonces.tt_trace_id.clone()),
        ("X-Request-Id".into(), nonces.request_id.clone()),
        ("Sec-Fetch-Dest".into(), "empty".into()),
        ("Sec-Fetch-Mode".into(), "no-cors".into()),
        ("Sec-Fetch-Site".into(), "none".into()),
        // region.userRegionHeader，空白时填 "CN"
        ("X-User-Region".into(), {
            let r = user_region_header.trim();
            if r.is_empty() { "CN".to_string() } else { r.to_string() }
        }),
    ]
}

/// 构造完整请求（POST + 固定体 `{}`）。纯函数：`build()` 不发网络。
/// 显式构造的请求（顺序可控、可逐字节断言；`reqwest::Request` 无法直接构造）。
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub body: Vec<u8>,
    /// 有序显式头（与 `ug_headers_ordered` 一一对应，不去重、不排序）。
    pub headers: Vec<(String, String)>,
}

impl PreparedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// 交给 reqwest 实发（进程唯一 client，HeaderMap 装载显式头）。
    pub fn execute(self) -> Result<reqwest::Request, reqwest::Error> {
        self.execute_with(ug_client())
    }

    /// 指定 client 版（qoder 侧共用同一构造器，§1.2/§4 头表断言同此结构）。
    pub fn execute_with(self, client: &reqwest::Client) -> Result<reqwest::Request, reqwest::Error> {
        let mut map = reqwest::header::HeaderMap::new();
        for (k, v) in &self.headers {
            if let (Ok(n), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                map.insert(n, val);
            }
        }
        client
            .request(self.method.clone(), &self.url)
            .headers(map)
            .body(self.body)
            .build()
    }
}

pub fn build_ug_request(
    region: &TraeRegion,
    path: &str,
    token: &str,
    uid: &str,
    nonces: &UgNonces,
) -> PreparedRequest {
    let url = format!("{}{}", region.ug_host, path);
    PreparedRequest {
        method: reqwest::Method::POST,
        url,
        body: b"{}".to_vec(),
        headers: ug_headers_ordered(token, uid, region.user_region_header, nonces),
    }
}

// ---------------------------------------------------------------------------
// 响应判定（§1.1 六步，纯函数）
// ---------------------------------------------------------------------------

/// `ug()` 的失败二分：Auth=token 被拒（401/403/业务码 1001）；Network=非 2xx/空体/解析失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UgFailure {
    Network,
    Auth,
}

/// 输入 HTTP 状态码 + 响应文本，输出根对象或失败分类（步骤 2~5）。
pub fn classify_ug_response(status: u16, text: &str) -> Result<Value, UgFailure> {
    if status == 401 || status == 403 {
        return Err(UgFailure::Auth);
    }
    let ok = (200..=299).contains(&status);
    if !ok || text.trim().is_empty() {
        return Err(UgFailure::Network);
    }
    let v: Value = serde_json::from_str(text).map_err(|_| UgFailure::Network)?;
    // org.json optInt("code", 0)：非整数（字符串数字也认 org 行为？org.json 会强转；此处按 JSON 数值/可解析字符串）
    if code_field(&v) == Some(1001) {
        return Err(UgFailure::Auth);
    }
    Ok(v)
}

fn code_field(v: &Value) -> Option<i64> {
    match v.get("code") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// classify 结果 → 是否鉴权失败（纯函数，测试用；`record` 写全局位）。
pub fn is_auth_failure(result: &Result<Value, UgFailure>) -> bool {
    matches!(result, Err(UgFailure::Auth))
}

/// 把 classify 结果落到 LAST_AUTH_FAILED（async 层用）。
fn record(result: &Result<Value, UgFailure>) {
    LAST_AUTH_FAILED.store(is_auth_failure(result), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// 解析（纯函数）
// ---------------------------------------------------------------------------

/// `CheckinStatus`（§1.1 parseCheckin）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckinStatus {
    pub checked_in: bool,
    pub credits: i64,
    pub enable: bool,
}

/// `parseCheckin`：code==1001 → None（鉴权失败伪装成"未开放"）；缺字段按 false/0 容错。
pub fn parse_checkin(root: &Value) -> Option<CheckinStatus> {
    if code_field(root) == Some(1001) {
        return None;
    }
    Some(CheckinStatus {
        checked_in: bool_field(root, "checked_in"),
        credits: i64_field(root, "credits", 0),
        enable: bool_field(root, "enable"),
    })
}

fn bool_field(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn i64_field(v: &Value, key: &str, default: i64) -> i64 {
    match v.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().unwrap_or(default),
        _ => default,
    }
}

/// `claimCheckin` 的业务码：`root.optInt("code", -1)`；-1 = 响应缺 code 字段。
pub fn claim_code_of(root: &Value) -> i32 {
    code_field(root).map(|c| c as i32).unwrap_or(-1)
}

/// `fetchUsage`：`usage_summary.total_amount` 缺省 -1，<0 → None；
/// 返回 `total − consumed` 截断取整（Kotlin `.toLong()` 同为向零截断）。
pub fn parse_usage(root: &Value) -> Option<i64> {
    let sum = root.get("usage_summary")?.as_object()?;
    let total = f64_of(sum, "total_amount", -1.0);
    if total < 0.0 {
        return None;
    }
    let consumed = f64_of(sum, "consumed_amount", 0.0);
    Some((total - consumed) as i64)
}

fn f64_of(obj: &serde_json::Map<String, Value>, key: &str, default: f64) -> f64 {
    match obj.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().unwrap_or(default),
        _ => default,
    }
}

/// 成功的**唯一**判据：成功码 ∧ 重查得到状态 ∧ 现在已签 ∧ 之前未签（§1.1 checkinSucceeded）。
pub fn checkin_succeeded(before: Option<&CheckinStatus>, after: Option<&CheckinStatus>, code: Option<i32>) -> bool {
    code == Some(0) && after.map(|a| a.checked_in).unwrap_or(false) && before.map(|b| b.checked_in) != Some(true)
}

// ---------------------------------------------------------------------------
// async 层（真实网络；单测不调用）
// ---------------------------------------------------------------------------

/// `ug(token, url, uid, region)`：构造 + 发送 + 判定；复位/记录 last_auth_failed。
pub async fn ug_post(region: &TraeRegion, path: &str, token: &str, uid: &str) -> Result<Value, UgFailure> {
    let result = send_raw(build_ug_request(region, path, token, uid, &UgNonces::random())).await;
    record(&result);
    result
}

/// Kotlin `ug()` 的响应读取部分：异常（DNS/超时/连接重置）一律 Network（lastAuthFailed=false）。
async fn send_raw(p: PreparedRequest) -> Result<Value, UgFailure> {
    let req = p.execute().map_err(|_| UgFailure::Network)?;
    let resp = ug_client().execute(req).await.map_err(|_| UgFailure::Network)?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    classify_ug_response(status, &text)
}

/// `fetchCheckin`：区域不可查 → None（§1.1 canQueryQuota）。
pub async fn fetch_checkin(region: &TraeRegion, token: &str, uid: &str) -> Option<CheckinStatus> {
    if !can_query_quota(region) {
        return None;
    }
    let root = ug_post(region, EP_CHECKIN_STATUS, token, uid).await.ok()?;
    parse_checkin(&root)
}

/// `claimCheckin`：返回业务 code；None = 网络失败（调用方**必须**用 last_auth_failed 二分文案）。
pub async fn claim_checkin(region: &TraeRegion, token: &str, uid: &str) -> Option<i32> {
    if !can_query_quota(region) {
        return None;
    }
    let root = ug_post(region, EP_CLAIM, token, uid).await.ok()?;
    Some(claim_code_of(&root))
}

/// `fetchUsage`。
pub async fn fetch_usage(region: &TraeRegion, token: &str, uid: &str) -> Option<i64> {
    if !can_query_quota(region) {
        return None;
    }
    let root = ug_post(region, EP_ENT_USAGE, token, uid).await.ok()?;
    parse_usage(&root)
}

/// `fetchCheckinHealing`：token 被判废 → `force_refresh_token_for` 强制续一次再查（§1.1 自愈封装）。
pub async fn fetch_checkin_healing(store: &TraeAuthStore, cred: &TraeAccount) -> Option<CheckinStatus> {
    let region = TraeRegion::of(&cred.region_id);
    if let Some(s) = fetch_checkin(region, &cred.access_token, &cred.uid).await {
        return Some(s);
    }
    if !last_auth_failed() {
        return None;
    }
    let t2 = crate::auth::api::trae::force_refresh_token_for(store, cred).await?;
    fetch_checkin(region, &t2, &cred.uid).await
}

/// `claimCheckinHealing`（同构自愈）。
pub async fn claim_checkin_healing(store: &TraeAuthStore, cred: &TraeAccount) -> Option<i32> {
    let region = TraeRegion::of(&cred.region_id);
    if let Some(c) = claim_checkin(region, &cred.access_token, &cred.uid).await {
        return Some(c);
    }
    if !last_auth_failed() {
        return None;
    }
    let t2 = crate::auth::api::trae::force_refresh_token_for(store, cred).await?;
    claim_checkin(region, &t2, &cred.uid).await
}

// ---------------------------------------------------------------------------
// 自检（§2 selfCheck 的 Trae 判据部分 + §1.1 状态解析钉死）
// ---------------------------------------------------------------------------

pub fn self_check() -> Result<(), String> {
    // 合取判据：只看 code / 只看跃迁都会"没签到却说成功"
    let signed = CheckinStatus { checked_in: true, credits: 150, enable: true };
    let unsigned = CheckinStatus { checked_in: false, credits: 150, enable: true };
    if !checkin_succeeded(Some(&unsigned), Some(&signed), Some(0)) {
        return Err("自检失败：合取判据应认可真跃迁".into());
    }
    if checkin_succeeded(Some(&signed), Some(&signed), Some(0)) {
        return Err("自检失败：before 已签不算本次成功".into());
    }
    if checkin_succeeded(Some(&unsigned), Some(&signed), Some(9004)) {
        return Err("自检失败：9004 不算成功".into());
    }
    if checkin_succeeded(Some(&unsigned), None, Some(0)) {
        return Err("自检失败：after=null 一律判未成功".into());
    }
    if checkin_succeeded(Some(&unsigned), Some(&signed), None) || checkin_succeeded(Some(&unsigned), Some(&signed), Some(-1)) {
        return Err("自检失败：code=null/-1 不算成功".into());
    }
    // 1001 = 鉴权失败被伪装成"未开放"
    let fake = serde_json::json!({"code": 1001, "enable": false, "checked_in": false});
    if parse_checkin(&fake).is_some() {
        return Err("自检失败：code=1001 应解析为 None".into());
    }
    if classify_ug_response(200, "{\"code\":1001}").is_ok() {
        return Err("自检失败：HTTP 200 + code=1001 必须判鉴权失败".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID1: &str = "3695871032827027";
    const UID2: &str = "7000000000000001";

    fn fixed_nonces() -> UgNonces {
        UgNonces {
            tt_trace_id: "00-0123456789abcdef-01".into(),
            request_id: "11111111-2222-4333-8444-555555555555".into(),
        }
    }

    // ---- 设备三件套：黄金向量（与 Kotlin seededStream 规格逐字节对齐，独立实现交叉验证） ----

    #[test]
    fn device_ids_golden_vectors() {
        let d = derive_device_ids(UID1);
        assert_eq!(d.device_id, "398959912212659");
        assert_eq!(
            d.session_id,
            "d10970a4b0ea61fe314363cc1e3cfaa3a70c784becc1efc970e43da7541bb694"
        );
        assert_eq!(d.market_user_id, "99a67e0e-9d1c-4906-919f-90af16d1efc0");

        let d2 = derive_device_ids(UID2);
        assert_eq!(d2.device_id, "764195421295293");
        assert_eq!(d2.market_user_id, "8c4e1815-1273-4a3d-a29c-1ee03a6f9acc");
    }

    #[test]
    fn device_ids_shape_and_determinism() {
        let a = derive_device_ids(UID1);
        let b = derive_device_ids(UID1);
        assert_eq!(a, b, "同 uid 恒等（不漂移）");
        assert_ne!(a.device_id, derive_device_ids(UID2).device_id, "异 uid 必须隔离（9074 根因）");

        assert_eq!(a.device_id.len(), 15);
        assert!(a.device_id.bytes().all(|c| c.is_ascii_digit()), "deviceId 必须是纯数字");
        assert_eq!(a.session_id.len(), 64);
        assert!(a.session_id.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // UUID v4：版本位 '4'（第 13 个 hex 字符）+ 变体位 8/9/a/b
        let h: String = a.market_user_id.chars().filter(|c| *c != '-').collect();
        assert_eq!(h.len(), 32);
        assert_eq!(&h[12..13], "4");
        assert!("89ab".contains(&h[16..17]));
    }

    // ---- 请求逐字节断言 ----

    #[test]
    fn ug_request_bytes() {
        let cn: &'static TraeRegion = &crate::upstream::trae::CN;
        let req = build_ug_request(cn, EP_CHECKIN_STATUS, "TOK-9", UID1, &fixed_nonces());
        assert_eq!(req.method, reqwest::Method::POST);
        assert_eq!(req.url, "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status");
        assert_eq!(req.body, b"{}".to_vec());

        let hs = ug_headers_ordered("TOK-9", UID1, cn.user_region_header, &fixed_nonces());
        assert_eq!(hs.len(), 19, "显式头 19 条（§1.1 全集实数，task 里写的 22 含 OkHttp 隐式头）");
        assert_eq!(req.headers, hs, "构造请求与头表逐条同序");
        let named = |k: &str| req.header(k).unwrap_or("<缺>").to_string();
        assert_eq!(named("Authorization"), "Cloud-IDE-JWT TOK-9", "自定义 scheme，非 Bearer");
        assert_eq!(named("Accept"), "*/*");
        assert_eq!(named("Content-Type"), "application/json");
        assert_eq!(named("User-Agent"), "VSCode 1.107.1 (TRAE SOLO CN)");
        assert_eq!(named("X-Market-Client-Id"), "VSCode 1.107.1");
        assert_eq!(named("X-Market-User-Id"), "99a67e0e-9d1c-4906-919f-90af16d1efc0");
        assert_eq!(named("X-Device-Id"), "398959912212659");
        assert_eq!(named("Vscode-Sessionid"), "d10970a4b0ea61fe314363cc1e3cfaa3a70c784becc1efc970e43da7541bb694");
        assert_eq!(named("X-Lgw-Req-Sdk-Type"), "3");
        assert_eq!(named("Package-Type"), "stable_cn");
        assert_eq!(named("X-Lscbd-Aid"), "787976");
        assert_eq!(named("X-Lscbd-Platform"), "windows");
        assert_eq!(named("App-Version"), "0.1.45");
        assert_eq!(named("X-Tt-Trace-Id"), "00-0123456789abcdef-01");
        assert_eq!(named("X-Request-Id"), "11111111-2222-4333-8444-555555555555");
        assert_eq!(named("Sec-Fetch-Dest"), "empty");
        assert_eq!(named("Sec-Fetch-Mode"), "no-cors");
        assert_eq!(named("Sec-Fetch-Site"), "none");
        assert_eq!(named("X-User-Region"), "CN");
    }

    #[test]
    fn claim_and_usage_paths_and_sg_region_header() {
        let n = fixed_nonces();
        let cn: &'static TraeRegion = &crate::upstream::trae::CN;
        let sg: &'static TraeRegion = &crate::upstream::trae::SG;
        let r1 = build_ug_request(cn, EP_CLAIM, "t", UID1, &n);
        assert_eq!(r1.url, "https://api.trae.cn/trae/api/v2/ug/checkin_credits/claim");
        let r2 = build_ug_request(cn, EP_ENT_USAGE, "t", UID1, &n);
        assert_eq!(r2.url, "https://api.trae.cn/trae/api/v2/pay/ide_user_ent_usage");
        // SG userRegionHeader 为空 → 头填 "CN"（ifBlank 语义）
        let hs = ug_headers_ordered("t", UID1, sg.user_region_header, &n);
        assert_eq!(hs.last().unwrap(), &("X-User-Region".to_string(), "CN".to_string()));
    }

    #[test]
    fn random_nonces_shapes() {
        let n = UgNonces::random();
        assert!(regex::Regex::new(r"^00-[0-9a-f]{16}-01$").unwrap().is_match(&n.tt_trace_id), "{}", n.tt_trace_id);
        assert!(regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
            .unwrap()
            .is_match(&n.request_id), "{}", n.request_id);
    }

    // ---- 判定表 ----

    #[test]
    fn classify_ug_response_table() {
        assert_eq!(classify_ug_response(401, "{}"), Err(UgFailure::Auth));
        assert_eq!(classify_ug_response(403, "{}"), Err(UgFailure::Auth));
        assert_eq!(classify_ug_response(500, "{}"), Err(UgFailure::Network));
        assert_eq!(classify_ug_response(200, "   "), Err(UgFailure::Network));
        assert_eq!(classify_ug_response(200, "not json"), Err(UgFailure::Network));
        assert_eq!(classify_ug_response(200, "{\"code\":1001}"), Err(UgFailure::Auth));
        let ok = classify_ug_response(200, "{\"code\":0,\"a\":1}").unwrap();
        assert_eq!(ok["a"], serde_json::json!(1));
        // last_auth_failed 的语义 = 仅 Auth 类置 true（record 写全局位，纯判据单独测）
        assert!(!is_auth_failure(&Err(UgFailure::Network)));
        assert!(is_auth_failure(&Err(UgFailure::Auth)));
        assert!(!is_auth_failure(&Ok(serde_json::json!({}))));
    }

    #[test]
    fn parse_checkin_dump_and_defaults() {
        // 2026-09-25 实测 dump：多余字段忽略、缺字段不抛
        let root = serde_json::json!({
            "checked_in": true, "code": 0, "credits": 150, "did_checked_in": false,
            "enable": true, "extra_credits": 50, "message": "success"
        });
        assert_eq!(
            parse_checkin(&root),
            Some(CheckinStatus { checked_in: true, credits: 150, enable: true })
        );
        assert_eq!(parse_checkin(&serde_json::json!({})), Some(CheckinStatus { checked_in: false, credits: 0, enable: false }));
        assert_eq!(parse_checkin(&serde_json::json!({"code": 1001, "enable": true})), None);
        assert_eq!(claim_code_of(&serde_json::json!({"message": "x"})), -1, "缺 code 字段 = -1");
        assert_eq!(claim_code_of(&serde_json::json!({"code": 9074})), 9074);
    }

    #[test]
    fn parse_usage_truncates_toward_zero() {
        let root = serde_json::json!({"usage_summary": {"total_amount": 5100.0, "consumed_amount": 1931.46}});
        assert_eq!(parse_usage(&root), Some(3168), "5100−1931.46=3168.54 → 截断 3168");
        assert_eq!(parse_usage(&serde_json::json!({"usage_summary": {}})), None, "total_amount 缺省 -1 → None");
        assert_eq!(parse_usage(&serde_json::json!({})), None);
        let neg = serde_json::json!({"usage_summary": {"total_amount": -2.0}});
        assert_eq!(parse_usage(&neg), None);
    }

    #[test]
    fn checkin_succeeded_truth_table() {
        let signed = CheckinStatus { checked_in: true, credits: 1, enable: true };
        let unsigned = CheckinStatus { checked_in: false, credits: 1, enable: true };
        let cases: [(Option<&CheckinStatus>, Option<&CheckinStatus>, Option<i32>, bool); 8] = [
            (Some(&unsigned), Some(&signed), Some(0), true),
            (Some(&signed), Some(&signed), Some(0), false),
            (None, Some(&signed), Some(0), true),
            (Some(&unsigned), Some(&signed), Some(9004), false),
            (Some(&unsigned), Some(&signed), Some(9074), false),
            (Some(&unsigned), None, Some(0), false),
            (Some(&unsigned), Some(&signed), None, false),
            (Some(&unsigned), Some(&signed), Some(-1), false),
        ];
        for (b, a, c, want) in cases {
            assert_eq!(checkin_succeeded(b, a, c), want, "{b:?} {a:?} {c:?}");
        }
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }

    #[test]
    fn clock_units_not_crossed_uses_now_sec_for_trae() {
        // Trae 侧一律秒域（analysis §2.1 单位坑）：这里只钉住两个取时函数不自相矛盾
        use crate::auth::model::{now_ms, now_sec};
        let (s, ms) = (now_sec(), now_ms());
        assert!((ms / 1000 - s).abs() <= 1);
    }
}
