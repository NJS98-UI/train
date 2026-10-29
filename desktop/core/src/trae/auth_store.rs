// 逐行移植 TraeAuthStore.kt —— Trae 登录凭证的**多账号**加密存取。
//
// 存储布局（桌面版全部在 AppState 里）：
//   prefs["trae_accounts"]          → 账号索引 JSON 数组（非敏感元数据：uid/nickname/region/expiresAt）
//   secrets["trae_acc_<uid>_access"] → access token 密文
//   secrets["trae_acc_<uid>_refresh"] → refresh token 密文
//   prefs["trae_active_uid"]        → 当前选中账号的 uid
//
// 安全红线：token 只以密文落盘（AES-256-GCM，等价 AndroidKeyStore），索引里**绝不**出现 token。
//
// 兼容：旧版单账号的四条 key（trae_access/trae_refresh/trae_uid/trae_nick/trae_exp）
// 会在首次读取时自动迁移成一个账号条目，用户不需要重新登录。
use crate::log_i;
use crate::state::AppState;
use crate::trae::region::TraeRegion;
use crate::types::{TraeAccountMeta, TraeCred};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

// （Kotlin 的 SEP=":" 密文格式由桌面 Storage.encrypt/decrypt 直接沿用同形格式，无需在此定义）

const KEY_INDEX: &str = "trae_accounts"; // JSON 数组
const KEY_ACTIVE: &str = "trae_active_uid";
/// 导出 JSON 的格式标识，导入时用来辨认是不是本 App 导出的文件。
pub const EXPORT_FORMAT: &str = "traework2api-accounts";

// 旧版单账号 key（仅用于迁移）
const LEGACY_ACCESS: &str = "trae_access";
const LEGACY_REFRESH: &str = "trae_refresh";
const LEGACY_UID: &str = "trae_uid";
const LEGACY_NICK: &str = "trae_nick";
const LEGACY_EXP: &str = "trae_exp";

/// 对齐 Kotlin `private var migrated = false`（进程级一次性迁移标记）。
static MIGRATED: AtomicBool = AtomicBool::new(false);

// ----------------------------------------------------------------------
// 小助手：org.json optXxx 语义（缺失/类型不符 → 默认值）
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

// ----------------------------------------------------------------------
// 账号区域（对齐 Kotlin AccountMeta.region / Cred.region 属性）
// ----------------------------------------------------------------------

/// 账号元数据所属区域。
pub fn meta_region(m: &TraeAccountMeta) -> TraeRegion {
    TraeRegion::from_id(&m.region_id)
}

/// 凭证所属区域（转发/续期都要用它选 host）。
pub fn cred_region(c: &TraeCred) -> TraeRegion {
    TraeRegion::from_id(&c.region_id)
}

// ----------------------------------------------------------------------
// 账号列表 / 当前账号
// ----------------------------------------------------------------------

/// 全部账号元数据（按索引顺序）。会先做一次旧数据迁移 + 修补。
pub fn list(app: &AppState) -> Vec<TraeAccountMeta> {
    migrate_legacy_if_needed(app);
    repair_mislabeled_regions(app);
    read_index(app)
}

/// 当前选中的账号 uid；无账号返回 None。
pub fn active_uid(app: &AppState) -> Option<String> {
    migrate_legacy_if_needed(app);
    repair_mislabeled_regions(app);
    let saved = app.pref(KEY_ACTIVE);
    let metas = read_index(app);
    if metas.is_empty() {
        return None;
    }
    // 选中的账号可能已被删除 → 回退到第一个
    match saved {
        Some(s) if metas.iter().any(|m| m.uid == s) => Some(s),
        _ => Some(metas.first().unwrap().uid.clone()),
    }
}

/// 让**当前账号的区域**成为生效区域。
///
/// 账号自带的 region 才是权威——`trae_region` 只是"上次手动选的区域"，
/// 它可能和当前账号不一致（例：切到国际服账号后 prefs 仍是 cn），
/// 那样转发就会拿国际服 token 打国内服 host。启动与切账号时都应调用本方法。
/// 无账号时不动（保留用户手动选的区域，登录时还要用）。
pub fn sync_region_to_active_account(app: &AppState) {
    let Some(uid) = active_uid(app) else { return };
    let Some(meta) = read_index(app).into_iter().find(|m| m.uid == uid) else { return };
    crate::trae::region::set_current_global(meta_region(&meta));
    app.set_pref("trae_region", &meta.region_id);
}

/// 切换当前账号。uid 不存在则忽略。
pub fn set_active(app: &AppState, uid: &str) {
    if !read_index(app).iter().any(|m| m.uid == uid) {
        return;
    }
    app.set_pref(KEY_ACTIVE, uid);
}

/// 故障转移候选：**换一个别的账号**。
/// 优先选**受支持区域**的账号（国服）—— 国际服已冻结为未开发（聊天半残、额度极少），
/// 把它当兜底只会让用户从"国服 401"变成"国际服答不上来"，是更差的体验。
/// 只有确实没有受支持区域的账号时，才退而求其次换任何别的账号。
/// 只有一个账号时返回 None（没人可换，原错误照报）。
pub fn pick_failover_candidate(metas: &[TraeAccountMeta], failed_uid: &str) -> Option<TraeAccountMeta> {
    let others: Vec<TraeAccountMeta> = metas.iter().filter(|m| m.uid != failed_uid).cloned().collect();
    others
        .iter()
        .find(|m| TraeRegion::from_id(&m.region_id).is_supported())
        .cloned()
        .or_else(|| others.first().cloned())
}

/// 读取**当前账号**的凭证；无账号或解密失败返回 None。
pub fn load(app: &AppState) -> Option<TraeCred> {
    active_uid(app).and_then(|uid| load_by_uid(app, &uid))
}

/// 读取指定账号的凭证；不存在或解密失败返回 None。
pub fn load_by_uid(app: &AppState, uid: &str) -> Option<TraeCred> {
    let meta = read_index(app).into_iter().find(|m| m.uid == uid)?;
    let access = app
        .secret(&access_key(uid))
        .filter(|s| !s.is_empty())?;
    let refresh = app.secret(&refresh_key(uid)).unwrap_or_default();
    Some(TraeCred {
        access_token: access,
        refresh_token: refresh,
        expires_at: meta.expires_at,
        uid: meta.uid,
        nickname: meta.nickname,
        region_id: meta.region_id,
    })
}

/// 保存（新增或更新）一个账号并设为当前账号。
/// 用 uid 作为键，所以**同一账号重登不会产生重复条目**，也不会覆盖别的账号。
pub fn save(app: &AppState, cred: &TraeCred) {
    app.set_secret(&access_key(&cred.uid), &cred.access_token);
    app.set_secret(&refresh_key(&cred.uid), &cred.refresh_token);
    write_meta(app, &cred.to_meta());
    set_active(app, &cred.uid);
}

/// 只更新元数据（不改 token）；账号不存在则忽略。
pub fn update_meta(app: &AppState, meta: &TraeAccountMeta) {
    if !read_index(app).iter().any(|m| m.uid == meta.uid) {
        return;
    }
    write_meta(app, meta);
}

/// 只更新**某个账号的 token**（access/refresh），**不改当前选中账号**。
///
/// 与 [save] 的区别：[save] 会把传入账号设为当前账号，后台批量续期/签到绝不能偷偷切号，
/// 所以那条路径用本方法。账号不存在则忽略（不新建）。
pub fn update_tokens(app: &AppState, cred: &TraeCred) {
    if !read_index(app).iter().any(|m| m.uid == cred.uid) {
        return;
    }
    app.set_secret(&access_key(&cred.uid), &cred.access_token);
    app.set_secret(&refresh_key(&cred.uid), &cred.refresh_token);
}

/// 删除账号（连密文一起删）。若删的是当前账号，自动切到剩下的第一个。
pub fn remove(app: &AppState, uid: &str) {
    let metas: Vec<TraeAccountMeta> = read_index(app).into_iter().filter(|m| m.uid != uid).collect();
    app.del_secret(&access_key(uid));
    app.del_secret(&refresh_key(uid));
    app.set_pref(KEY_INDEX, &to_json(&metas).to_string());
    if app.pref(KEY_ACTIVE).as_deref() == Some(uid) {
        app.set_pref(KEY_ACTIVE, metas.first().map(|m| m.uid.as_str()).unwrap_or(""));
    }
}

// ----------------------------------------------------------------------
// 导出 / 导入（JSON 明文，换设备一键登录用）
// ----------------------------------------------------------------------

/// 把全部账号导出成 JSON 字符串。⚠️ **含明文 access/refresh token**。
///
/// 为什么是明文：token 平时加密落盘，主密钥**绑死这台设备**，导出的密文到另一台机器
/// 根本解不开。要"换设备一键登录"，导出的就只能是明文 token，导入方再重新加密。
/// 因此导出的文件等价于"账号钥匙"，只走本地文件/剪贴板，绝不联网、绝不写日志。
///
/// 结构：`{"format":"traework2api-accounts","version":1,"activeUid":"…","accounts":[{…}]}`
pub fn export_json(app: &AppState) -> String {
    let metas = list(app);
    let mut arr: Vec<Value> = Vec::new();
    for m in metas {
        let Some(cred) = load_by_uid(app, &m.uid) else { continue };
        arr.push(json!({
            "uid": cred.uid,
            "nickname": cred.nickname,
            "region": cred.region_id,
            "expiresAt": cred.expires_at,
            "accessToken": cred.access_token,
            "refreshToken": cred.refresh_token,
        }));
    }
    let root = json!({
        "format": EXPORT_FORMAT,
        "version": 1,
        "activeUid": active_uid(app).unwrap_or_default(),
        "accounts": arr,
    });
    serde_json::to_string_pretty(&root).unwrap_or_default()
}

/// 从 [export_json] 那种 JSON 导入账号：按 uid 合并写回（新设备重新加密），
/// 同名 uid 覆盖、新 uid 追加。导入后**不**自动改当前账号，除非文件里带 `activeUid` 且它存在。
/// 兼容直接给出顶层数组（无外壳）的老格式。
///
/// 返回成功导入的账号数。解析失败返回 Err（消息给人话）。
pub fn import_json(app: &AppState, json: &str) -> Result<i64, String> {
    let root: Option<Value> = serde_json::from_str(json).ok();
    let accounts: Vec<Value> = match &root {
        // 标准外壳：{format,version,activeUid,accounts:[…]}
        Some(v) if v.is_object() => v
            .get("accounts")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default(),
        // 兼容老格式：顶层直接是账号数组
        _ => match root {
            Some(v) if v.is_array() => v.as_array().cloned().unwrap_or_default(),
            _ => return Err("不是有效的账号 JSON".to_string()),
        },
    };
    let mut imported: i64 = 0;
    for o in &accounts {
        let Some(o) = o.as_object() else { continue };
        let uid = opt_str(o, "uid", "");
        if uid.trim().is_empty() {
            continue;
        }
        let access = opt_str(o, "accessToken", "");
        if access.trim().is_empty() {
            continue;
        }
        let cred = TraeCred {
            access_token: access,
            refresh_token: opt_str(o, "refreshToken", ""),
            expires_at: opt_i64(o, "expiresAt", 0),
            uid,
            nickname: opt_str(o, "nickname", ""),
            region_id: opt_str(o, "region", TraeRegion::Cn.id()),
        };
        // 走 import_one，避免每个账号都把当前号切成它
        import_one(app, &cred);
        imported += 1;
    }
    if imported <= 0 {
        return Err("JSON 里没有可导入的账号（缺 uid 或 accessToken）".to_string());
    }
    // 若文件带了 activeUid 且该账号在导入结果里，则把当前账号设成它（换设备保持同一个号）
    let active: Option<String> = root
        .as_ref()
        .and_then(|v| v.get("activeUid"))
        .and_then(|a| match a {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .filter(|s| !s.trim().is_empty());
    if let Some(active) = active {
        if read_index(app).iter().any(|m| m.uid == active) {
            set_active(app, &active);
        }
    }
    Ok(imported)
}

/// 导入单个账号：写 token + 元数据，但不动当前选中账号。
fn import_one(app: &AppState, cred: &TraeCred) {
    app.set_secret(&access_key(&cred.uid), &cred.access_token);
    app.set_secret(&refresh_key(&cred.uid), &cred.refresh_token);
    write_meta(app, &cred.to_meta());
}

// ----------------------------------------------------------------------
// 索引读写
// ----------------------------------------------------------------------

fn access_key(uid: &str) -> String {
    format!("trae_acc_{uid}_access")
}

fn refresh_key(uid: &str) -> String {
    format!("trae_acc_{uid}_refresh")
}

fn read_index(app: &AppState) -> Vec<TraeAccountMeta> {
    let Some(raw) = app.pref(KEY_INDEX) else {
        return Vec::new();
    };
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in &arr {
        let Some(o) = item.as_object() else { continue };
        let uid = opt_str(o, "uid", "");
        if uid.trim().is_empty() {
            continue;
        }
        out.push(TraeAccountMeta {
            uid,
            nickname: opt_str(o, "nickname", ""),
            region_id: opt_str(o, "region", TraeRegion::Cn.id()),
            expires_at: opt_i64(o, "expiresAt", 0),
        });
    }
    out
}

fn to_json(metas: &[TraeAccountMeta]) -> Value {
    Value::Array(
        metas
            .iter()
            .map(|m| {
                json!({
                    "uid": m.uid,
                    "nickname": m.nickname,
                    "region": m.region_id,
                    "expiresAt": m.expires_at,
                })
            })
            .collect(),
    )
}

/// 写入/替换一条元数据，保持其余条目与顺序。
fn write_meta(app: &AppState, meta: &TraeAccountMeta) {
    let mut metas = read_index(app);
    match metas.iter().position(|m| m.uid == meta.uid) {
        Some(i) => metas[i] = meta.clone(),
        None => metas.push(meta.clone()),
    }
    app.set_pref(KEY_INDEX, &to_json(&metas).to_string());
}

// ----------------------------------------------------------------------
// 旧版单账号 → 多账号 迁移
// ----------------------------------------------------------------------

/// 修补区域标注。
///
/// 背景：迁移逻辑第一版把两个 uid 的长度记反了，已把国际服账号错标成 `cn`。
/// 这里按 uid 长度重算，不一致就改回来（幂等，改对了就不动）。
/// 只碰**区域**，不碰 token；区域错了会导致转发打到错误的 host，必须修。
fn repair_mislabeled_regions(app: &AppState) {
    let metas = read_index(app);
    if metas.is_empty() {
        return;
    }
    let mut changed = false;
    let fixed: Vec<TraeAccountMeta> = metas
        .into_iter()
        .map(|m| {
            let expect = infer_region_from_uid(&m.uid).id().to_string();
            if m.region_id != expect {
                log_i!("TraeAuthStore", "修正区域标注 uid={}: {} → {}", m.uid, m.region_id, expect);
                changed = true;
                TraeAccountMeta { region_id: expect, ..m }
            } else {
                m
            }
        })
        .collect();
    if changed {
        app.set_pref(KEY_INDEX, &to_json(&fixed).to_string());
    }
}

/// 旧版数据没有区域字段。用 uid **长度**推断（实测样本，勿凭印象改）：
///   国内服 `3695871032827027`       = 16 位
///   国际服 `7687189514112844821`    = 19 位
/// 即**国际服更长**。≥19 位判国际服，否则国内服。
/// 认不出的一律按 CN（历史上只实测过这两类）。
pub fn infer_region_from_uid(uid: &str) -> TraeRegion {
    let digits = uid.chars().filter(|c| c.is_numeric()).count();
    if digits >= 19 {
        TraeRegion::Sg
    } else {
        TraeRegion::Cn
    }
}

/// 把旧版单账号的四条 key 迁移成一个账号条目，然后删掉旧 key（避免明文/重复来源）。
/// 幂等：迁移过一次或没有旧数据时什么都不做。
fn migrate_legacy_if_needed(app: &AppState) {
    if MIGRATED.swap(true, Ordering::SeqCst) {
        return;
    }
    if app.pref(LEGACY_ACCESS).is_none() {
        return;
    }
    let access = app.pref(LEGACY_ACCESS).and_then(|blob| app.store.decrypt(&blob));
    let uid = app.pref_or(LEGACY_UID, "");
    let access = match access {
        Some(a) if !a.trim().is_empty() => a,
        _ => {
            // 旧数据不完整：只清掉，避免留下半截状态
            for k in [LEGACY_ACCESS, LEGACY_REFRESH, LEGACY_UID, LEGACY_NICK, LEGACY_EXP] {
                app.del_pref(k);
            }
            return;
        }
    };
    let refresh = app.pref(LEGACY_REFRESH).and_then(|blob| app.store.decrypt(&blob)).unwrap_or_default();
    let meta = TraeAccountMeta {
        uid: uid.clone(),
        nickname: app.pref_or(LEGACY_NICK, ""),
        region_id: infer_region_from_uid(&uid).id().to_string(),
        expires_at: app.pref_or(LEGACY_EXP, "0").parse().unwrap_or(0),
    };
    // 密文原样搬过去（重新加密一次更干净，但会多一次主密钥调用）
    app.set_secret(&access_key(&uid), &access);
    app.set_secret(&refresh_key(&uid), &refresh);
    app.set_pref(KEY_INDEX, &to_json(&[meta]).to_string());
    app.set_pref(KEY_ACTIVE, &uid);
    for k in [LEGACY_ACCESS, LEGACY_REFRESH, LEGACY_UID, LEGACY_NICK, LEGACY_EXP] {
        app.del_pref(k);
    }
    log_i!("TraeAuthStore", "已迁移旧版单账号 → 多账号 (uid={uid})");
}

// ----------------------------------------------------------------------
// 自检
// ----------------------------------------------------------------------

/// 自检：索引 JSON 序列化/反序列化往返必须无损（uid 是主键，错了会串账号）。
/// 纯字符串逻辑，不碰密钥，可在启动时安全调用。
pub fn self_check() {
    let src = vec![
        TraeAccountMeta {
            uid: "7687189514112844821".into(),
            nickname: "intl-user".into(),
            region_id: TraeRegion::Sg.id().into(),
            expires_at: 1791341100,
        },
        TraeAccountMeta {
            uid: "3695871032827027".into(),
            nickname: "cn-user".into(),
            region_id: TraeRegion::Cn.id().into(),
            expires_at: 0,
        },
    ];
    let back: Vec<TraeAccountMeta> = match serde_json::from_value(to_json(&src)) {
        Ok(v) => v,
        Err(e) => panic!("自检失败：账号索引序列化异常 {e}"),
    };
    assert!(
        serde_json::to_value(&back).unwrap_or(Value::Null) == serde_json::to_value(&src).unwrap_or(Value::Null),
        "自检失败：账号索引往返不一致"
    );
    // 区域解析必须按 id 命中，否则账号会挂到错的 host
    assert!(meta_region(&back[0]) == TraeRegion::Sg, "自检失败：国际服账号应解析为 SG");
    assert!(meta_region(&back[1]) == TraeRegion::Cn, "自检失败：国内服账号应解析为 CN");
    // 旧数据迁移靠 uid 长度推断区域，这里把实测样本钉住（16 位=CN，19 位=SG）
    assert!(
        infer_region_from_uid("3695871032827027") == TraeRegion::Cn,
        "自检失败：16 位 uid 应判为 CN"
    );
    assert!(
        infer_region_from_uid("7687189514112844821") == TraeRegion::Sg,
        "自检失败：19 位 uid 应判为 SG"
    );

    // 故障转移必须**优先选受支持区域**：国际服已冻结为未开发，
    // 拿它当兜底会把"国服 401"变成"国际服答不上来"，是更差的体验。
    let sg = TraeAccountMeta { uid: "7687189514112844821".into(), nickname: "intl".into(), region_id: TraeRegion::Sg.id().into(), expires_at: 0 };
    let cn = TraeAccountMeta { uid: "3695871032827027".into(), nickname: "cn".into(), region_id: TraeRegion::Cn.id().into(), expires_at: 0 };
    let got = pick_failover_candidate(&[sg.clone(), cn.clone()], &sg.uid);
    assert!(got.map(|m| m.uid == cn.uid).unwrap_or(false), "自检失败：国际服挂掉时应换到国服账号");
    let got = pick_failover_candidate(&[cn.clone(), sg.clone()], &cn.uid);
    assert!(got.map(|m| m.uid == sg.uid).unwrap_or(false), "自检失败：只剩国际服可换时仍应给出它（聊胜于无）");
    assert!(pick_failover_candidate(std::slice::from_ref(&cn), &cn.uid).is_none(), "自检失败：只有一个账号时不应换号");
    // 两个国服账号 → 换到另一个国服（不要跳去国际服）
    let cn2 = TraeAccountMeta { uid: "3695871032827028".into(), nickname: "cn2".into(), region_id: TraeRegion::Cn.id().into(), expires_at: 0 };
    let got = pick_failover_candidate(&[cn.clone(), cn2.clone(), sg], &cn.uid);
    assert!(got.map(|m| m.uid == cn2.uid).unwrap_or(false), "自检失败：有另一个国服账号时应优先换它，而不是国际服");
}
