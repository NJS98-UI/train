// 逐行移植 GatewayBackend.kt —— 本地网关的转发后端选择（Trae / Qoder / 按模型自动路由）。
use crate::qoder::model_catalog;
use crate::state::AppState;
use serde_json::{json, Value};

pub const TRAE: &str = "trae";
pub const QODER: &str = "qoder";
pub const AUTO: &str = "auto";

const KEY: &str = "gateway_backend";

/// 用户显式选择（可能是 AUTO）
pub fn selection(app: &AppState) -> String {
    let v = app.pref_or(KEY, "");
    if v == TRAE || v == QODER { v } else { AUTO.to_string() }
}

pub fn set(app: &AppState, backend: &str) {
    let v = if backend == QODER { QODER } else if backend == TRAE { TRAE } else { AUTO };
    app.set_pref(KEY, v);
}

// ------------------------------------------------------------------
// 账号可用性
// ------------------------------------------------------------------

pub fn has_trae_account(app: &AppState) -> bool {
    crate::trae::auth_store::load(app).is_some()
}

pub fn has_qoder_account(app: &AppState) -> bool {
    crate::qoder::auth_store::load(app).is_some()
}

/// 该模型名是否属于 Qoder 目录（key / 展示名 / 别名任一命中）
pub fn is_qoder_model(model: &str) -> bool {
    let m = model.trim();
    if m.is_empty() {
        return false;
    }
    model_catalog::current().iter().any(|it| {
        it.key.eq_ignore_ascii_case(m)
            || it.name.eq_ignore_ascii_case(m)
            || it.aliases.iter().any(|a| a.eq_ignore_ascii_case(m))
    })
}

// ------------------------------------------------------------------
// 路由
// ------------------------------------------------------------------

/// 该模型名是否属于 Trae 目录（按 `id` 命中）
pub fn is_trae_model(model: &str) -> bool {
    let m = model.trim();
    if m.is_empty() {
        return false;
    }
    match crate::aux::models_repo::current().as_array() {
        Some(arr) => arr.iter().any(|o| {
            o.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case(m))
                .unwrap_or(false)
        }),
        None => false,
    }
}

/// 这个模型该走哪个上游。同名模型两边都有时 **Trae 优先**。
pub fn route_for(app: &AppState, model: &str) -> String {
    match selection(app).as_str() {
        QODER => QODER.to_string(),
        TRAE => TRAE.to_string(),
        _ => {
            let q = has_qoder_account(app);
            let t = has_trae_account(app);
            if q && !t {
                QODER.to_string()
            } else if !q && t {
                TRAE.to_string()
            } else if q && t {
                if is_trae_model(model) {
                    TRAE.to_string()
                } else if is_qoder_model(model) {
                    QODER.to_string()
                } else {
                    TRAE.to_string()
                }
            } else {
                TRAE.to_string()
            }
        }
    }
}

/// 展示用：当前"主"后端（auto 且两个都有时按 Trae 展示）
pub fn resolved(app: &AppState) -> String {
    match selection(app).as_str() {
        QODER => QODER.to_string(),
        TRAE => TRAE.to_string(),
        _ => {
            if has_qoder_account(app) && !has_trae_account(app) {
                QODER.to_string()
            } else {
                TRAE.to_string()
            }
        }
    }
}

pub fn is_qoder(app: &AppState) -> bool {
    resolved(app) == QODER
}

/// `auto` 且两个平台都有账号 = 模型表是并集、按模型路由
pub fn is_mixed(app: &AppState) -> bool {
    selection(app) == AUTO && has_trae_account(app) && has_qoder_account(app)
}

/// 后端选择的人话描述（UI 与日志共用）
pub fn describe(app: &AppState) -> String {
    if selection(app) == QODER {
        "Qoder（强制）".into()
    } else if selection(app) == TRAE {
        "Trae（强制）".into()
    } else if is_mixed(app) {
        "自动 · 按模型路由（Trae + Qoder 都在）".into()
    } else if has_qoder_account(app) {
        "自动 · Qoder".into()
    } else {
        "自动 · Trae".into()
    }
}

// ------------------------------------------------------------------
// 模型表
// ------------------------------------------------------------------

/// `/v1/models` 与模型页要用的模型表；auto 且双平台都有账号时返回并集
pub fn models_for(app: &AppState) -> Value {
    if selection(app) == QODER {
        return model_catalog::openai_models();
    }
    if selection(app) == TRAE {
        return crate::aux::models_repo::current();
    }
    let q = has_qoder_account(app);
    let t = has_trae_account(app);
    if q && !t {
        model_catalog::openai_models()
    } else if !q && t {
        crate::aux::models_repo::current()
    } else if q && t {
        merge(crate::aux::models_repo::current(), model_catalog::openai_models())
    } else {
        crate::aux::models_repo::current()
    }
}

/// 并集：Trae 表在前，Qoder 独有的按 id 去重后追加；每条补 `owned_by`
fn merge(trae: Value, qoder: Value) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(arr) = trae.as_array() {
        for o in arr {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !id.is_empty() && !seen.insert(id.clone()) {
                continue;
            }
            let mut obj = o.clone();
            if let Some(m) = obj.as_object_mut() {
                m.insert("owned_by".into(), json!("trae"));
            }
            out.push(obj);
        }
    }
    if let Some(arr) = qoder.as_array() {
        for o in arr {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !id.is_empty() && !seen.insert(id.clone()) {
                continue;
            }
            let mut obj = o.clone();
            if let Some(m) = obj.as_object_mut() {
                m.insert("owned_by".into(), json!("qoder"));
            }
            out.push(obj);
        }
    }
    Value::Array(out)
}

/// 模型页的"是否已加载"判定 —— 跟着当前选择的后端走
pub fn models_loaded(app: &AppState) -> bool {
    models_for(app)
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

pub fn label(backend: &str) -> String {
    match backend {
        QODER => "Qoder".to_string(),
        TRAE => "Trae".to_string(),
        _ => "自动".to_string(),
    }
}
