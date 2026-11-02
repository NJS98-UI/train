//! Qoder 每日 Credits：活动列表（两档身份头 + 空列表升级阶梯）、单条领取（幂等 `{}`）、余量查询。
//! 对应 `QoderAuthApi.kt`，规格 = checkin-credits.md §1.2。
//!
//! 可测性设计（单测不发网络请求）：
//! - 头/URL 构造纯函数：[`get_identity_headers`] / [`post_claim_headers`] / [`campaigns_url`]；
//! - 解析纯函数：[`parse_campaigns`] / [`parse_claim`] / [`parse_credits`]（逐键对齐 org.json 容错语义）；
//! - 档位阶梯 [`load_campaigns_with`] 泛型化，注入 `fetch`/`force_refresh` 闭包即可覆盖全部分支，
//!   [`load_campaigns`] 是它的真实网络实例；
//! - 文案表纯函数：[`no_claimable_message`] / [`claim_summary_message`]。

use crate::auth::api::qoder::{force_refresh_job_token, open_api_base, valid_job_token_for};
use crate::auth::model::QoderAccount;
use crate::auth::store::QoderAuthStore;
use crate::checkin::state::CheckinState;
use crate::checkin::trae_api::PreparedRequest;
use crate::crypto::machine::{self, Identity, CLIENT_TYPE, COSY_VERSION};
use serde_json::{Map, Value};
use std::future::Future;
use std::sync::OnceLock;

/// `EP_CAMPAIGNS`（与聊天网关 `gateway.qoder.com.cn`/`api2.qoder.sh` 是两套域名，勿混）。
pub const EP_CAMPAIGNS: &str = "/sash/api/v1/me/campaigns";
/// `EP_USAGE`。
pub const EP_USAGE: &str = "/api/v2/quota/usage";

pub const VARIANT_MINIMAL: &str = "minimal";
pub const VARIANT_DESKTOP: &str = "desktop";

/// 最小档 UA（自识别身份）。
pub const UA_MINIMAL: &str = "traework2api";
/// 桌面档 UA。
pub const UA_DESKTOP: &str = "Qoder";
/// 最小档固定 Cosy-Version（Kotlin 字面量）。
pub const COSY_VERSION_MINIMAL: &str = "1.0.1";
/// `claimDailyForCred`/`claimedTodayFor` 的取 token 临期窗：**60 秒（毫秒域）**。
pub const CLAIM_JT_SKEW_MS: i64 = 60_000;

fn qoder_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest qoder client")
    })
}

// ---------------------------------------------------------------------------
// URL / 身份头（纯函数）
// ---------------------------------------------------------------------------

/// `openApiBase(mode) + EP_CAMPAIGNS`（`?forceRefresh=true` 由 fetchCampaigns 固定携带）。
pub fn campaigns_url(mode: &str, force_refresh: bool) -> String {
    let base = format!("{}{}", open_api_base(mode), EP_CAMPAIGNS);
    if force_refresh {
        format!("{base}?forceRefresh=true")
    } else {
        base
    }
}

/// `openApiBase(mode) + EP_CAMPAIGNS + "/" + campaignId + "/claim"`。
pub fn claim_url(mode: &str, campaign_id: &str) -> String {
    format!("{}/{}/claim", campaigns_url(mode, false), campaign_id)
}

/// `openApiBase(mode) + EP_USAGE`。
pub fn usage_url(mode: &str) -> String {
    format!("{}{}", open_api_base(mode), EP_USAGE)
}

fn is_desktop(variant: &str) -> bool {
    variant == VARIANT_DESKTOP
}

/// desktop 档且身份非空时追加的机器头；`Cosy-MachineType`/`Cosy-MachineHostname` 空白不发
/// （空串头会被某些网关判异常）。
fn machine_headers(id: Option<&Identity>) -> Vec<(String, String)> {
    let Some(id) = id else { return Vec::new() };
    let mut out = vec![
        ("Cosy-MachineId".to_string(), id.machine_id.clone()),
        // 官方无 umid 客户端约定：token 恒等于 machineId（§1.2 表）
        ("Cosy-MachineToken".to_string(), id.machine_token.clone()),
        ("Cosy-MachineOS".to_string(), id.machine_os.clone()),
    ];
    if !id.machine_type.trim().is_empty() {
        out.push(("Cosy-MachineType".to_string(), id.machine_type.clone()));
    }
    if !id.hostname.trim().is_empty() {
        out.push(("Cosy-MachineHostname".to_string(), id.hostname.clone()));
    }
    out
}

/// 逐档基础三项（UA / Cosy-Version / Cosy-ClientType）的净值。
fn variant_identity(variant: &str, id: Option<&Identity>) -> (String, String, String) {
    if is_desktop(variant) {
        (
            UA_DESKTOP.to_string(),
            id.map(|i| i.version.clone()).unwrap_or_else(|| COSY_VERSION.to_string()),
            id.map(|i| i.client_type.clone()).unwrap_or_else(|| CLIENT_TYPE.to_string()),
        )
    } else {
        (UA_MINIMAL.to_string(), COSY_VERSION_MINIMAL.to_string(), CLIENT_TYPE.to_string())
    }
}

/// `getWithIdentity` 的头（OkHttp 插入序，desktop 覆盖后的净值）。
pub fn get_identity_headers(jt: &str, variant: &str, id: Option<&Identity>) -> Vec<(String, String)> {
    let (ua, cosy_version, client_type) = variant_identity(variant, id);
    let mut h = vec![
        ("Authorization".to_string(), format!("Bearer {jt}")),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), ua),
        ("Cosy-Version".to_string(), cosy_version),
        ("Cosy-ClientType".to_string(), client_type),
    ];
    if is_desktop(variant) {
        h.extend(machine_headers(id));
    }
    h
}

/// `postClaimRaw` 的头（最小档也带 `Cosy-ClientType: 10`，与列表同档）。
pub fn post_claim_headers(jt: &str, variant: &str, id: Option<&Identity>) -> Vec<(String, String)> {
    let (ua, cosy_version, client_type) = variant_identity(variant, id);
    let mut h = vec![
        ("Authorization".to_string(), format!("Bearer {jt}")),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), ua),
        ("Cosy-Version".to_string(), cosy_version),
        ("Cosy-ClientType".to_string(), client_type),
    ];
    if is_desktop(variant) {
        h.extend(machine_headers(id));
    }
    h
}

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// `QCampaign`。`startAt/endAt` 为 unix **秒**（§1.2 明确踩过的坑），不参与判定故不保留。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QCampaign {
    pub id: String,
    pub claim_status: String,
    pub action_type: String,
    pub benefit_kind: String,
    pub benefit_amount: i64,
}

impl QCampaign {
    /// `claimable`：id 非空白 && claimStatus 大小写不敏感等于 `CLAIMABLE`。
    /// 不按 actionType 过滤（VIEW_DETAILS 也领，否则"电脑能领、程序不能领"）。
    pub fn claimable(&self) -> bool {
        !self.id.trim().is_empty() && self.claim_status.eq_ignore_ascii_case("CLAIMABLE")
    }

    pub fn grants_credits(&self) -> bool {
        self.benefit_amount > 0
    }
}

/// `CampaignList`：`show_campaign=None` = 响应里没这个字段；**Some(false) 不能当"真没活动"**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignList {
    pub campaigns: Vec<QCampaign>,
    pub show_campaign: Option<bool>,
}

/// `CampaignLoad`：list=None 即整体失败（`error` 为人读消息，末档生效档位一并返回）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampaignLoad {
    pub list: Option<CampaignList>,
    pub variant: &'static str,
    pub error: Option<String>,
}

/// `QCredits`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QCredits {
    pub remaining: f64,
    pub exceeded: bool,
}

impl QCredits {
    /// 余 0 不算有额度；`exceeded=true` 一律不算。
    pub fn has_credit(&self) -> bool {
        !self.exceeded && self.remaining > 0.0
    }
}

/// `QClaimResult`：status ∈ `ok` / `already` / `fail`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QClaimResult {
    pub status: String,
    pub amount: i64,
    pub message: String,
}

impl QClaimResult {
    pub fn ok(&self) -> bool {
        self.status == "ok"
    }

    fn new(status: &str, amount: i64, message: impl Into<String>) -> Self {
        Self { status: status.to_string(), amount, message: message.into() }
    }
}

// ---------------------------------------------------------------------------
// org.json 风格取值helper（缺字段容错，字符串数字照旧强转）
// ---------------------------------------------------------------------------

fn opt_str(obj: &Map<String, Value>, key: &str) -> String {
    match obj.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn opt_bool(obj: &Map<String, Value>, key: &str, default: bool) -> bool {
    match obj.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(default),
        _ => default,
    }
}

fn opt_i64(obj: &Map<String, Value>, key: &str, default: i64) -> i64 {
    match obj.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().unwrap_or(default),
        _ => default,
    }
}

fn opt_f64(obj: &Map<String, Value>, key: &str, default: f64) -> f64 {
    match obj.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().unwrap_or(default),
        _ => default,
    }
}

fn sub<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    obj.get(key).and_then(Value::as_object)
}

// ---------------------------------------------------------------------------
// 解析（纯函数）
// ---------------------------------------------------------------------------

/// `parseCampaigns`：根无 `campaigns` 时下钻 `data`；`showCampaign` 先看下钻根、再看原始根、
/// 都没有 → None；id 键序 `campaignId → campaignKey → id`，全空白丢弃该条。
pub fn parse_campaigns(root_in: &Value) -> CampaignList {
    let in_root = root_in.as_object();
    let drilled = in_root.filter(|o| o.get("campaigns").is_none()).and_then(|o| sub(o, "data"));
    let work = drilled.or(in_root);

    let show = work
        .filter(|o| o.contains_key("showCampaign"))
        .map(|o| opt_bool(o, "showCampaign", false))
        .or_else(|| in_root.filter(|o| o.contains_key("showCampaign")).map(|o| opt_bool(o, "showCampaign", false)));

    let arr = match work.and_then(|o| o.get("campaigns")).and_then(Value::as_array) {
        Some(a) => a,
        None => return CampaignList { campaigns: Vec::new(), show_campaign: show },
    };

    let campaigns = arr
        .iter()
        .filter_map(|item| {
            let o = item.as_object()?;
            let mut id = opt_str(o, "campaignId");
            if id.trim().is_empty() {
                id = opt_str(o, "campaignKey");
            }
            if id.trim().is_empty() {
                id = opt_str(o, "id");
            }
            if id.trim().is_empty() {
                return None;
            }
            let b = sub(o, "benefit");
            Some(QCampaign {
                id,
                claim_status: opt_str(o, "claimStatus"),
                action_type: opt_str(o, "actionType"),
                benefit_kind: b.map(|x| opt_str(x, "kind")).unwrap_or_default(),
                benefit_amount: b.map(|x| opt_i64(x, "amount", 0)).unwrap_or(0),
            })
        })
        .collect();
    CampaignList { campaigns, show_campaign: show }
}

/// `parseClaim` → `(是否真的新领到, 本次入账额度)`；`replayed=true` 判"未新领"。
pub fn parse_claim(root: &Value) -> (bool, i64) {
    let root_obj = root.as_object();
    let status_blank = root_obj.map(|o| opt_str(o, "status")).unwrap_or_default();
    let use_data = status_blank.trim().is_empty() && root_obj.and_then(|o| sub(o, "data")).is_some();
    let o: &Map<String, Value> = if use_data {
        root_obj.and_then(|r| sub(r, "data")).expect("use_data 已判存在")
    } else {
        root_obj.expect("parse_claim 只接受 JSON 对象")
    };
    let claimed = opt_str(o, "status").eq_ignore_ascii_case("CLAIMED");
    let replayed = opt_bool(o, "replayed", false);
    let amount = sub(o, "benefit").map(|b| opt_i64(b, "amount", 0)).unwrap_or(0);
    let fresh = claimed && !replayed;
    (fresh, if fresh { amount } else { 0 })
}

/// `fetchCredits` 的解析部分：可选下钻 `data` → 五键全累加 → 组织两键按 `available` 门控 →
/// `coerceAtLeast(0)`。国内外键名差异故"拿到的都算余量"。
pub fn parse_credits(root_in: &Value) -> QCredits {
    let root_obj = root_in.as_object();
    let has_known = root_obj
        .map(|o| o.contains_key("userQuota") || o.contains_key("personalQuota") || o.contains_key("isQuotaExceeded"))
        .unwrap_or(false);
    let drilled = if has_known { None } else { root_obj.and_then(|o| sub(o, "data")) };
    let empty: Map<String, Value> = Map::new();
    let root: &Map<String, Value> = drilled.or(root_obj).unwrap_or(&empty);

    let mut remaining = 0.0f64;
    for k in ["userQuota", "personalQuota", "addOnQuota", "addonQuota", "extraQuota"] {
        if let Some(o) = sub(root, k) {
            remaining += opt_f64(o, "remaining", 0.0);
        }
    }
    for k in ["orgResourcePackage", "organizationQuota"] {
        if let Some(o) = sub(root, k) {
            if opt_bool(o, "available", true) {
                remaining += opt_f64(o, "remaining", 0.0);
            }
        }
    }
    let exceeded = opt_bool(root, "isQuotaExceeded", false);
    QCredits { remaining: remaining.max(0.0), exceeded }
}

/// 无 claimable 时的四种 `already` 文案（§1.2，逐字）。
pub fn no_claimable_message(campaigns: &[QCampaign]) -> String {
    let all_claimed = !campaigns.is_empty() && campaigns.iter().all(|c| c.claim_status.eq_ignore_ascii_case("CLAIMED"));
    let credit_campaigns = campaigns.iter().filter(|c| c.grants_credits()).count();
    if credit_campaigns > 0 && all_claimed {
        format!("已领取 {credit_campaigns} 条")
    } else if credit_campaigns == 0 && !campaigns.is_empty() {
        format!("无积分活动（{} 条均为查看类，不发 Credits）", campaigns.len())
    } else if !campaigns.is_empty() {
        format!("无待领活动（{} 条均已处理）", campaigns.len())
    } else {
        "今日无可领活动（上游返回空列表）".to_string()
    }
}

/// 逐条 claim 之后的汇总三态（§1.2 表末三行）。
pub fn claim_summary_message(fresh: i64, total: i64, last_err: Option<&str>) -> QClaimResult {
    if fresh > 0 && total > 0 {
        QClaimResult::new("ok", total, "")
    } else if fresh > 0 {
        QClaimResult::new("ok", 0, format!("领取 {fresh} 条（无 Credits）"))
    } else {
        match last_err {
            Some(e) => QClaimResult::new("already", 0, format!("领取未生效：{e}")),
            None => QClaimResult::new("already", 0, "今日已领取"),
        }
    }
}

// ---------------------------------------------------------------------------
// 档位阶梯（注入 fetch/refresh，纯逻辑可测）
// ---------------------------------------------------------------------------

/// `campaignVariant` → 阶梯：记住 desktop 就只跑 desktop，否则 `[minimal, desktop]`。
pub fn ladder_of(variant: &str) -> &'static [&'static str] {
    if variant == VARIANT_DESKTOP {
        &[VARIANT_DESKTOP]
    } else {
        &[VARIANT_MINIMAL, VARIANT_DESKTOP]
    }
}

/// `loadCampaigns` 的完全对应体：空列表升档复核、401 同档续 token 重试一次（只一次）、
/// 出活动即记住档位。`fetch`/`force_refresh` 由调用方注入（网络实例见 [`load_campaigns`]）。
pub async fn load_campaigns_with<F, Fut, R, RFut>(
    state: &CheckinState,
    mode: &str,
    jt: String,
    mut fetch: F,
    mut force_refresh: R,
) -> CampaignLoad
where
    F: FnMut(&'static str, String) -> Fut,
    Fut: Future<Output = Result<CampaignList, String>>,
    R: FnMut() -> RFut,
    RFut: Future<Output = Option<String>>,
{
    let first: &'static str = state.campaign_variant(mode);
    let ladder = ladder_of(first);
    let mut cur = jt;
    let mut retried_401 = false;
    let mut last_error: Option<String> = None;
    let mut i = 0usize;
    while i < ladder.len() {
        let v = ladder[i];
        match fetch(v, cur.clone()).await {
            Ok(got) => {
                last_error = None;
                if !got.campaigns.is_empty() {
                    if v != first {
                        state.remember_campaign_variant(mode, v);
                    }
                    return CampaignLoad { list: Some(got), variant: v, error: None };
                }
                if i == ladder.len() - 1 {
                    return CampaignLoad { list: Some(got), variant: v, error: None };
                }
            }
            Err(msg) => {
                last_error = Some(msg.clone());
                if msg.contains("401") && !retried_401 {
                    retried_401 = true;
                    // 续不动也照样同档再试一次（Kotlin `refreshJt()?.let{cur=it}; continue`）
                    if let Some(nt) = force_refresh().await {
                        cur = nt;
                    }
                    continue;
                }
                if i == ladder.len() - 1 {
                    return CampaignLoad { list: None, variant: v, error: Some(msg) };
                }
            }
        }
        i += 1;
    }
    CampaignLoad {
        list: None,
        variant: first,
        error: Some(last_error.unwrap_or_else(|| "未知错误".to_string())),
    }
}

// ---------------------------------------------------------------------------
// 网络实例 + 领取编排（claimDailyForCred / claimedTodayFor / fetchCredits）
// ---------------------------------------------------------------------------

fn to_machine_fp(fp: &crate::auth::model::QoderFingerprint) -> machine::QFingerprint {
    machine::QFingerprint {
        machine_id: fp.machine_id.clone(),
        machine_token: fp.machine_token.clone(),
        machine_type: fp.machine_type.clone(),
        machine_os: fp.machine_os.clone(),
        hostname: fp.hostname.clone(),
        client_type: fp.client_type.clone(),
        version: fp.version.clone(),
    }
}

/// `campaignIdentity`：最小档恒 None；桌面档优先抓包回填指纹，否则本机派生（官方"无 umid"形态）。
pub fn campaign_identity(store: &QoderAuthStore, uid: &str, variant: &str) -> Option<Identity> {
    if !is_desktop(variant) {
        return None;
    }
    let captured = store.fingerprint(uid).map(|fp| to_machine_fp(&fp));
    Some(machine::identity(captured.as_ref()))
}

/// 非 2xx → `IOException("Qoder {code}: {body 前 200 字}")` 的对应文本。
async fn read_body(resp: reqwest::Response) -> Result<String, String> {
    let code = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if (200..=299).contains(&code) {
        Ok(text)
    } else {
        let cut: String = text.chars().take(200).collect();
        Err(format!("Qoder {code}: {cut}"))
    }
}

/// 复用 [`trae_api::PreparedRequest`]（顺序可断言的显式请求，§1.2 断言用）。
fn build_request(method: reqwest::Method, url: &str, body: Vec<u8>, headers: Vec<(String, String)>) -> PreparedRequest {
    PreparedRequest { method, url: url.to_string(), body, headers }
}

fn build_request_e(method: reqwest::Method, url: &str, body: &[u8], headers: Vec<(String, String)>) -> Result<reqwest::Request, String> {
    build_request(method, url, body.to_vec(), headers)
        .execute_with(qoder_client())
        .map_err(|e| e.to_string())
}

/// `getWithIdentity`。
pub async fn get_with_identity(url: &str, bearer: &str, variant: &str, id: Option<&Identity>) -> Result<String, String> {
    let req = build_request_e(reqwest::Method::GET, url, &[], get_identity_headers(bearer, variant, id))?;
    let resp = qoder_client().execute(req).await.map_err(|e| e.to_string())?;
    read_body(resp).await
}

/// `postClaimRaw`（body 固定 `{}`）。
pub async fn post_claim_raw(url: &str, bearer: &str, variant: &str, id: Option<&Identity>) -> Result<String, String> {
    let req = build_request_e(reqwest::Method::POST, url, b"{}", post_claim_headers(bearer, variant, id))?;
    let resp = qoder_client().execute(req).await.map_err(|e| e.to_string())?;
    read_body(resp).await
}

fn parse_json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("JSONException: {e}"))
}

/// `fetchCampaigns(jt, mode, variant, id)`。
pub async fn fetch_campaigns(store: &QoderAuthStore, cred: &QoderAccount, jt: &str, variant: &'static str) -> Result<CampaignList, String> {
    let id = campaign_identity(store, &cred.uid, variant);
    let url = campaigns_url(&cred.mode, true);
    let text = get_with_identity(&url, jt, variant, id.as_ref()).await?;
    Ok(parse_campaigns(&parse_json(&text)?))
}

/// `loadCampaigns`（真实网络实例）。
pub async fn load_campaigns(state: &CheckinState, store: &QoderAuthStore, cred: &QoderAccount, jt: String) -> CampaignLoad {
    // 先落成局部值再交给 async move：闭包只需按 Copy 借出两个 &，Future 不依赖闭包捕获状态。
    let st: &QoderAuthStore = store;
    let cd: &QoderAccount = cred;
    load_campaigns_with(
        state,
        &cred.mode,
        jt,
        move |variant: &'static str, token: String| {
            let s: &QoderAuthStore = st;
            let c: &QoderAccount = cd;
            async move { fetch_campaigns(s, c, &token, variant).await }
        },
        move || {
            let s: &QoderAuthStore = st;
            let c: &QoderAccount = cd;
            async move { force_refresh_job_token(s, c).await }
        },
    )
    .await
}

/// `claimOne(jt, mode, campaignId, variant, id)`。
pub async fn claim_one(_store: &QoderAuthStore, cred: &QoderAccount, jt: &str, variant: &'static str, id: Option<&Identity>, campaign_id: &str) -> Result<(bool, i64), String> {
    let url = claim_url(&cred.mode, campaign_id);
    let text = post_claim_raw(&url, jt, variant, id).await?;
    Ok(parse_claim(&parse_json(&text)?))
}

/// `claimedTodayFor`：true=已领完 / false=还有可领 / None=查不到（**只读**，不改任何状态）。
pub async fn claimed_today_for(state: &CheckinState, store: &QoderAuthStore, cred: &QoderAccount) -> Option<bool> {
    let jt = valid_job_token_for(store, cred, CLAIM_JT_SKEW_MS).await?;
    let load = load_campaigns(state, store, cred, jt).await;
    let campaigns = load.list?.campaigns;
    if campaigns.is_empty() {
        return None;
    }
    if campaigns.iter().any(QCampaign::claimable) {
        return Some(false);
    }
    if campaigns.iter().any(|c| c.claim_status.eq_ignore_ascii_case("CLAIMED")) {
        return Some(true);
    }
    None
}

/// `claimDailyForCred`：完整状态机 + 全部文案（失败不抛，一人失败不拖垮整批）。
pub async fn claim_daily_for_cred(state: &CheckinState, store: &QoderAuthStore, cred: &QoderAccount) -> QClaimResult {
    let jt = match valid_job_token_for(store, cred, CLAIM_JT_SKEW_MS).await {
        Some(t) if !t.trim().is_empty() => t,
        _ => return QClaimResult::new("fail", 0, "没有有效 token，请重新粘贴 PAT 登录"),
    };
    let load = load_campaigns(state, store, cred, jt.clone()).await;
    let list = match load.list {
        Some(l) => l,
        None => {
            let err = load.error.unwrap_or_else(|| "未知错误".to_string());
            return QClaimResult::new("fail", 0, format!("拉取活动失败：{err}"));
        }
    };
    let claimable: Vec<&QCampaign> = list.campaigns.iter().filter(|c| c.claimable()).collect();
    if claimable.is_empty() {
        return QClaimResult::new("already", 0, no_claimable_message(&list.campaigns));
    }
    let identity = campaign_identity(store, &cred.uid, load.variant);
    let mut total = 0i64;
    let mut fresh = 0i64;
    let mut last_err: Option<String> = None;
    for c in claimable {
        match claim_one(store, cred, &jt, load.variant, identity.as_ref(), &c.id).await {
            Ok((newly, amt)) => {
                if newly {
                    total += amt;
                    fresh += 1;
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    claim_summary_message(fresh, total, last_err.as_deref())
}

/// `claimDailyForUid`（含"凭据读取失败"分支）。
pub async fn claim_daily_for_uid(state: &CheckinState, store: &QoderAuthStore, uid: &str) -> QClaimResult {
    match store.load_by_uid(uid) {
        Some(cred) => claim_daily_for_cred(state, store, &cred).await,
        None => QClaimResult::new("fail", 0, "凭据读取失败"),
    }
}

/// `fetchCredits(jt, mode)`：空白 jt → None；固定最小档（用量接口不需要机器身份）。
pub async fn fetch_credits(jt: &str, mode: &str) -> Option<QCredits> {
    if jt.trim().is_empty() {
        return None;
    }
    let text = get_with_identity(&usage_url(mode), jt, VARIANT_MINIMAL, None).await.ok()?;
    let root = parse_json(&text).ok()?;
    Some(parse_credits(&root))
}

/// `QoderAuthApi.selfCheck` 的离线部分。
pub fn self_check() -> Result<(), String> {
    if ladder_of(VARIANT_MINIMAL) != [VARIANT_MINIMAL, VARIANT_DESKTOP] {
        return Err("自检失败：阶梯起点应为最小档".into());
    }
    if ladder_of(VARIANT_DESKTOP) != [VARIANT_DESKTOP] {
        return Err("自检失败：记住桌面档后不该白跑最小档".into());
    }
    let hidden = serde_json::json!({"showCampaign": false, "campaigns": []});
    let p = parse_campaigns(&hidden);
    if p.show_campaign != Some(false) || !p.campaigns.is_empty() {
        return Err("自检失败：静默隐藏形态解析错位".into());
    }
    let rp = serde_json::json!({"status": "CLAIMED", "replayed": true, "benefit": {"amount": 100}});
    if parse_claim(&rp).0 {
        return Err("自检失败：replayed=true 不得算新领".into());
    }
    let view = QCampaign {
        id: "c1".into(),
        claim_status: "CLAIMABLE".into(),
        action_type: "VIEW_DETAILS".into(),
        benefit_kind: String::new(),
        benefit_amount: 0,
    };
    if view.grants_credits() || no_claimable_message(&[view]).contains("已领取") {
        return Err("自检失败：查看类活动文案错位".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DataStore;
    use std::sync::Arc;

    fn ident() -> Identity {
        Identity {
            machine_id: "mid-abc".into(),
            machine_token: "mid-abc".into(),
            machine_type: "5".into(),
            machine_os: "aarch64_darwin".into(),
            hostname: "host-1".into(),
            client_type: "10".into(),
            version: "1.1.69".into(),
            from_capture: false,
        }
    }

    fn names(hs: &[(String, String)]) -> Vec<&str> {
        hs.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn urls_bytes() {
        assert_eq!(campaigns_url("cn", true), "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns?forceRefresh=true");
        assert_eq!(campaigns_url("global", true), "https://openapi.qoder.sh/sash/api/v1/me/campaigns?forceRefresh=true");
        assert_eq!(campaigns_url("cn", false), "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns");
        assert_eq!(claim_url("cn", "camp-1"), "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns/camp-1/claim");
        assert_eq!(usage_url("global"), "https://openapi.qoder.sh/api/v2/quota/usage");
        // 未知 mode 一律按国内（两套 host 绝不混）
        assert_eq!(campaigns_url("bogus", false), "https://openapi.qoder.com.cn/sash/api/v1/me/campaigns");
    }

    #[test]
    fn minimal_headers_bytes() {
        let hs = get_identity_headers("JT-1", VARIANT_MINIMAL, Some(&ident()));
        assert_eq!(
            hs,
            vec![
                ("Authorization".to_string(), "Bearer JT-1".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
                ("User-Agent".to_string(), "traework2api".to_string()),
                ("Cosy-Version".to_string(), "1.0.1".to_string()),
                ("Cosy-ClientType".to_string(), "10".to_string()),
            ],
            "最小档即使握着身份也不发任何 Cosy-Machine*（少一个风控点）"
        );
        let p = post_claim_headers("JT-1", VARIANT_MINIMAL, None);
        assert_eq!(names(&p), ["Authorization", "Content-Type", "Accept", "User-Agent", "Cosy-Version", "Cosy-ClientType"]);
        assert_eq!(p[1].1, "application/json");
    }

    #[test]
    fn desktop_headers_bytes() {
        let hs = get_identity_headers("JT-2", VARIANT_DESKTOP, Some(&ident()));
        assert_eq!(
            hs,
            vec![
                ("Authorization".to_string(), "Bearer JT-2".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
                ("User-Agent".to_string(), "Qoder".to_string()),
                ("Cosy-Version".to_string(), "1.1.69".to_string()),
                ("Cosy-ClientType".to_string(), "10".to_string()),
                ("Cosy-MachineId".to_string(), "mid-abc".to_string()),
                ("Cosy-MachineToken".to_string(), "mid-abc".to_string()),
                ("Cosy-MachineOS".to_string(), "aarch64_darwin".to_string()),
                ("Cosy-MachineType".to_string(), "5".to_string()),
                ("Cosy-MachineHostname".to_string(), "host-1".to_string()),
            ],
            "身份头必须与列表同档（claim 用另一档会被静默拒）"
        );
        // 本机派生身份：type/hostname 空白 → 两条不发；空白串头一律规避
        let derived = Identity {
            machine_id: "sha".into(),
            machine_token: "sha".into(),
            machine_type: String::new(),
            machine_os: "aarch64_darwin".into(),
            hostname: "   ".into(),
            client_type: "10".into(),
            version: String::new(),
            from_capture: false,
        };
        let hs2 = get_identity_headers("JT-3", VARIANT_DESKTOP, Some(&derived));
        assert_eq!(
            names(&hs2),
            ["Authorization", "Accept", "User-Agent", "Cosy-Version", "Cosy-ClientType", "Cosy-MachineId", "Cosy-MachineToken", "Cosy-MachineOS"]
        );
        // Kotlin 是 `id?.version ?: COSY_VERSION`：只有 null 回落，空串原样发出（不 trim 不回退）
        assert_eq!(hs2[3].1, "", "空白 version 原样透传（?: 语义）");
        // 桌面档但身份缺失：仍补 UA/版本，不带机器头（Kotlin id==null 分支）
        let hs3 = post_claim_headers("JT-4", VARIANT_DESKTOP, None);
        assert_eq!(names(&hs3), ["Authorization", "Content-Type", "Accept", "User-Agent", "Cosy-Version", "Cosy-ClientType"]);
        assert_eq!(hs3[4].1, COSY_VERSION);
    }

    #[test]
    fn parse_campaigns_dump_shapes() {
        // 实测形态：内容裹 data、id 键名三种、benefit 可为 null、字符串数字 amount
        let root = serde_json::json!({
            "data": {"showCampaign": true, "campaigns": [
                {"campaignId":"c1","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":100},"startAt":1764000000,"endAt":1764100000},
                {"campaignKey":"c2","claimStatus":"ClaimABLE","actionType":"VIEW_DETAILS","benefit":null},
                {"id":"  ","claimStatus":"CLAIMABLE"},
                {"campaignId":"c4","claimStatus":"CLAIMED","benefit":{"kind":"CREDITS","amount":"150"}}
            ]}
        });
        let p = parse_campaigns(&root);
        assert_eq!(p.show_campaign, Some(true));
        assert_eq!(p.campaigns.len(), 3, "空白 id 条目丢弃");
        assert_eq!(p.campaigns[0].id, "c1");
        assert_eq!(p.campaigns[0].benefit_kind, "CREDITS");
        assert!(p.campaigns[0].claimable() && p.campaigns[0].grants_credits());
        assert_eq!(p.campaigns[1].id, "c2");
        assert!(p.campaigns[1].claimable(), "claimStatus 大小写不敏感；actionType=VIEW_DETAILS 照领（判严会漏）");
        assert!(!p.campaigns[1].grants_credits(), "benefit=null → amount 0");
        assert!(p.campaigns[2].id == "c4" && !p.campaigns[2].claimable(), "claimed 不可领");
        assert_eq!(p.campaigns[2].benefit_amount, 150, "字符串数字照 org.json 强转");
        assert!(!p.campaigns[2].claimable());

        // 根上直接放 campaigns；showCampaign 只在原始 root → 仍取到
        let flat = serde_json::json!({"campaigns": [], "showCampaign": false});
        let f = parse_campaigns(&flat);
        assert_eq!(f.show_campaign, Some(false));
        assert!(f.campaigns.is_empty());

        let none = serde_json::json!({"foo": 1});
        assert_eq!(parse_campaigns(&none), CampaignList { campaigns: vec![], show_campaign: None });

        // 非对象活动条目（数组里的字符串）被 filter_map 丢弃而非 panic
        let dirty = serde_json::json!({"campaigns": ["x", {"campaignId":"ok"}]});
        assert_eq!(parse_campaigns(&dirty).campaigns.len(), 1);

        // 有 campaigns 时**不**下钻 data（两处都有 campaigns 优先看根）
        let both = serde_json::json!({"campaigns":[{"campaignId":"root1"}], "data":{"campaigns":[{"campaignId":"deep"}]}});
        assert_eq!(parse_campaigns(&both).campaigns[0].id, "root1");
    }

    #[test]
    fn parse_claim_matrix() {
        let cases: Vec<(&str, (bool, i64))> = vec![
            (r#"{"status":"CLAIMED","benefit":{"amount":100}}"#, (true, 100)),
            (r#"{"status":"CLAIMED","replayed":true,"benefit":{"amount":100}}"#, (false, 0)),
            (r#"{"status":"claimed","benefit":{}}"#, (true, 0)),
            (r#"{"status":"FAILED","benefit":{"amount":100}}"#, (false, 0)),
            (r#"{"status":""}"#, (false, 0)),
            (r#"{"data":{"status":"CLAIMED","benefit":{"amount":"100"}}}"#, (true, 100)),
            (r#"{"status":"  ","data":{"status":"CLAIMED"}}"#, (true, 0)),
            (r#"{"status":"CLAIMED"}"#, (true, 0)),
        ];
        for (raw, want) in cases {
            let v: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(parse_claim(&v), want, "{raw}");
        }
    }

    #[test]
    fn parse_credits_sums_all_known_keys() {
        let root = serde_json::json!({
            "userQuota": {"remaining": 100.0},
            "personalQuota": {"remaining": 20.5},
            "addOnQuota": {"remaining": 9.0},
            "addonQuota": {"remaining": 0.5},
            "extraQuota": {"remaining": 0.0},
            "orgResourcePackage": {"available": true, "remaining": 700.0},
            "organizationQuota": {"available": false, "remaining": 999.0},
            "isQuotaExceeded": false
        });
        let c = parse_credits(&root);
        assert!((c.remaining - 830.0).abs() < 1e-9, "{}", c.remaining);
        assert!(!c.exceeded && c.has_credit());

        // data 下钻只在缺三个已知键时生效
        let wrapped = serde_json::json!({"data": {"userQuota": {"remaining": 5.0}, "isQuotaExceeded": true}});
        let w = parse_credits(&wrapped);
        assert!((w.remaining - 5.0).abs() < 1e-9);
        assert!(w.exceeded && !w.has_credit(), "exceeded=true 一律不算有额度");

        // 负累加被抬到 0；available 缺省 true；空对象 → 0 且无额度
        let neg = serde_json::json!({"userQuota": {"remaining": -3.0}, "organizationQuota": {"remaining": 4.0}});
        assert!((parse_credits(&neg).remaining - 1.0).abs() < 1e-9, "−3+4=1（coerceAtLeast 作用于总和）");
        let all_neg = serde_json::json!({"userQuota": {"remaining": -9.0}});
        assert_eq!(parse_credits(&all_neg).remaining, 0.0);
        let empty = serde_json::json!({});
        assert_eq!(parse_credits(&empty), QCredits { remaining: 0.0, exceeded: false });
        assert!(!parse_credits(&empty).has_credit(), "余 0 不算有额度");
    }

    #[test]
    fn no_claimable_message_four_branches() {
        let c = |status: &str, amount: i64| QCampaign {
            id: "x".into(),
            claim_status: status.into(),
            action_type: "CLAIM_BENEFIT".into(),
            benefit_kind: "CREDITS".into(),
            benefit_amount: amount,
        };
        assert_eq!(no_claimable_message(&[c("CLAIMED", 100), c("CLAIMED", 100)]), "已领取 2 条");
        assert_eq!(no_claimable_message(&[c("CLAIMABLE", 0), c("CLAIMED", 0)]), "无积分活动（2 条均为查看类，不发 Credits）");
        assert_eq!(no_claimable_message(&[c("EXPIRED", 100)]), "无待领活动（1 条均已处理）");
        assert_eq!(no_claimable_message(&[]), "今日无可领活动（上游返回空列表）");
    }

    #[test]
    fn claim_summary_message_branches() {
        assert_eq!(claim_summary_message(2, 150, None), QClaimResult::new("ok", 150, ""));
        assert_eq!(claim_summary_message(1, 0, None), QClaimResult::new("ok", 0, "领取 1 条（无 Credits）"));
        assert_eq!(claim_summary_message(0, 0, Some("Qoder 500: x")), QClaimResult::new("already", 0, "领取未生效：Qoder 500: x"));
        assert_eq!(claim_summary_message(0, 0, None), QClaimResult::new("already", 0, "今日已领取"));
    }

    // ---- 阶梯：假 fetch（无网络） ----

    fn tmp_state(tag: &str) -> CheckinState {
        let dir = std::env::temp_dir().join(format!("tw-qoder-api-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        CheckinState::new(Arc::new(DataStore::new(dir)))
    }

    fn list_of(n: usize) -> CampaignList {
        CampaignList {
            campaigns: (0..n)
                .map(|i| QCampaign {
                    id: format!("c{i}"),
                    claim_status: "CLAIMABLE".into(),
                    action_type: "CLAIM_BENEFIT".into(),
                    benefit_kind: "CREDITS".into(),
                    benefit_amount: 100,
                })
                .collect(),
            show_campaign: Some(true),
        }
    }

    async fn ladder_with(
        state: &CheckinState,
        script: Vec<Result<CampaignList, String>>,
        refreshed: Vec<Option<String>>,
    ) -> (CampaignLoad, Vec<String>) {
        let mut seen: Vec<String> = Vec::new();
        let mut it = script.into_iter();
        let mut rit = refreshed.into_iter();
        let load = load_campaigns_with(
            state,
            "cn",
            "JT".to_string(),
            |variant, jt| {
                seen.push(format!("{variant}|{jt}"));
                let r = it.next().unwrap_or_else(|| Err("script exhausted".to_string()));
                async move { r }
            },
            || {
                let r = rit.next().unwrap_or(None);
                async move { r }
            },
        )
        .await;
        (load, seen)
    }

    #[tokio::test]
    async fn ladder_empty_minimal_upgrades_desktop_and_remembers() {
        let st = tmp_state("up");
        let (load, seen) = ladder_with(&st, vec![Ok(list_of(0)), Ok(list_of(2))], vec![]).await;
        assert_eq!(seen, vec!["minimal|JT", "desktop|JT"]);
        assert_eq!(load.variant, "desktop");
        assert_eq!(load.list.as_ref().unwrap().campaigns.len(), 2);
        assert_eq!(st.campaign_variant("cn"), "desktop", "哪档出活动就记住哪档（省一次白跑）");
    }

    #[tokio::test]
    async fn ladder_nonempty_first_variant_not_rewritten() {
        let st = tmp_state("keep");
        st.remember_campaign_variant("cn", "desktop");
        let (load, seen) = ladder_with(&st, vec![Ok(list_of(1))], vec![]).await;
        assert_eq!(seen, vec!["desktop|JT"], "记住桌面档就不再白跑最小档");
        assert_eq!(load.variant, "desktop");
        assert_eq!(st.campaign_variant("cn"), "desktop");
    }

    #[tokio::test]
    async fn ladder_both_empty_returns_last_variant_with_empty_list() {
        let st = tmp_state("both-empty");
        let hidden = Ok(CampaignList { campaigns: vec![], show_campaign: Some(false) });
        let (load, seen) = ladder_with(&st, vec![hidden, Ok(list_of(0))], vec![]).await;
        assert_eq!(seen, vec!["minimal|JT", "desktop|JT"]);
        assert_eq!(load.variant, "desktop");
        assert!(load.list.as_ref().unwrap().campaigns.is_empty(), "桌面档问出来的空才是真空");
        assert_eq!(st.campaign_variant("cn"), "minimal", "全程空列表不改档位");
    }

    #[tokio::test]
    async fn ladder_401_refreshes_and_retries_same_variant() {
        let st = tmp_state("retry401");
        let (load, seen) = ladder_with(
            &st,
            vec![Err("Qoder 401: unauthorized".to_string()), Ok(list_of(1))],
            vec![Some("JT2".to_string())],
        )
        .await;
        assert_eq!(seen, vec!["minimal|JT", "minimal|JT2"], "同一档用新 token 再来一次");
        assert_eq!(load.variant, "minimal");
        assert_eq!(load.list.unwrap().campaigns.len(), 1);
        assert_eq!(st.campaign_variant("cn"), "minimal");
    }

    #[tokio::test]
    async fn ladder_401_refresh_failure_still_retries_then_upgrades() {
        let st = tmp_state("retry-fail");
        // Kotlin 的 `retried401` 是**整条阶梯一份**（循环外声明），不是每档一份：
        // minimal 撞 401 → 同档重打一次；再 401 → 升 desktop；desktop -first-401 已无重试额度 → 直接末档返回。
        let (load, seen) = ladder_with(
            &st,
            vec![
                Err("Qoder 401: a".to_string()),
                Err("Qoder 401: b".to_string()),
                Err("Qoder 401: c".to_string()),
            ],
            vec![None],
        )
        .await;
        assert_eq!(seen, vec!["minimal|JT", "minimal|JT", "desktop|JT"]);
        assert!(load.list.is_none());
        assert_eq!(load.variant, "desktop", "末档失败时报末档");
        assert_eq!(load.error.as_deref(), Some("Qoder 401: c"));
    }

    #[tokio::test]
    async fn ladder_non401_error_upgrades_without_retry() {
        let st = tmp_state("no-retry");
        let (load, seen) = ladder_with(&st, vec![Err("Qoder 500: boom".to_string()), Ok(list_of(1))], vec![]).await;
        assert_eq!(seen, vec!["minimal|JT", "desktop|JT"]);
        assert_eq!(load.variant, "desktop");
        assert_eq!(st.campaign_variant("cn"), "desktop");
    }

    #[tokio::test]
    async fn ladder_shape_and_error_defaults() {
        let st = tmp_state("final-err");
        let (load, _) = ladder_with(&st, vec![Err("x".to_string()), Err("y".to_string())], vec![]).await;
        assert_eq!(load, CampaignLoad { list: None, variant: "desktop", error: Some("y".to_string()) });

        // 成功清 lastError：先错后成不应带 error
        let st2 = tmp_state("clear-err");
        let (load2, _) = ladder_with(&st2, vec![Err("Qoder 500: x".to_string()), Ok(list_of(1))], vec![]).await;
        assert_eq!(load2.error, None);
        // 401 文案只在同档重试一次；"未知错误" 兜底走 claim_daily（非此处），这里覆盖 empty 的 variant 归属
        let st3 = tmp_state("empty-min");
        st3.remember_campaign_variant("cn", "desktop");
        let (load3, seen3) = ladder_with(&st3, vec![Ok(list_of(0))], vec![]).await;
        assert_eq!(seen3, vec!["desktop|JT"]);
        assert!(load3.list.unwrap().campaigns.is_empty() && load3.error.is_none());
    }

    #[test]
    fn self_check_passes() {
        self_check().unwrap();
    }
}
