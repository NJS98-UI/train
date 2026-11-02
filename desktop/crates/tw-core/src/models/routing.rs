//! auto 并集选路表（对照 Kotlin `GatewayBackend.kt` :58-:190，analysis §1.4/§2.7/§6）。
//!
//! 本文件是 models 层的「选路中枢」，**纯函数**：不读 config.json、不检测账号，
//! `selection/has_trae/has_qoder` 由调用方（gateway/IPC wave）传入。这样单元测试
//! 无需初始化全局 store，也避免 models 层反向依赖 config/auth（依赖方向保持向下）。
//!
//! 两条铁律（源码注释原话）：
//! - **同名模型 Trae 优先**（Qoder 链要 COSY 签名+排队，明显更慢；旧代码先看 Qoder
//!   把同名模型悄悄改派给 Qoder，正是「很难刷出来」的来源之一）。
//! - 并集时 Trae 表在前（保持老用户看到的顺序），Qoder 独有按 `id` 去重后追加，
//!   每条补 `owned_by` 来源标注（模型页据此显示 tag）；**不得污染共享缓存**——
//!   Rust 侧 `current()`/`openai_models()` 已是克隆出的 `Vec<Value>`，天然满足。

use serde_json::{json, Value};

use crate::types::{BACKEND_QODER, BACKEND_TRAE};

use super::catalog::QoderModelCatalog;
use super::repo::ModelsRepo;

// ------------------------------------------------------------------
// 成员判定（对应 isTraeModel / isQoderModel）
// ------------------------------------------------------------------

/// 该模型名是否属于 Trae 表（按条目 `id` 命中，大小写不敏感；空名恒 false）。
/// 入参为 [`ModelsRepo::current()`] 的产物，与 Kotlin 逐条 `optString("id")` 等价。
pub fn is_trae_model(model: &str, trae_entries: &[Value]) -> bool {
    let m = model.trim();
    if m.is_empty() {
        return false;
    }
    trae_entries.iter().any(|o| {
        o.get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .eq_ignore_ascii_case(m)
    })
}

/// 该模型名是否属于 Qoder 目录（key / 展示名 / 别名任一 icase 命中；空名恒 false）。
/// 对应 Kotlin :58-:66 —— 判定用 `current()`（内置+动态合并后的生效目录）。
pub fn is_qoder_model(model: &str, catalog: &QoderModelCatalog) -> bool {
    let m = model.trim();
    if m.is_empty() {
        return false;
    }
    catalog.current().iter().any(|it| {
        it.key.eq_ignore_ascii_case(m)
            || it.name.eq_ignore_ascii_case(m)
            || it.aliases.iter().any(|a| a.eq_ignore_ascii_case(m))
    })
}

// ------------------------------------------------------------------
// 路由（对应 routeFor / resolvedBackend）
// ------------------------------------------------------------------

/// 这个模型该走哪个上游：`BACKEND_TRAE` / `BACKEND_QODER`。
///
/// 与 Kotlin `routeFor`（:93-:110）逐分支一致：强制选择直接透传；`auto` 时
/// 单边账号直接定后端；双边账号按「Trae 命中→trae，Qoder 命中→qoder，都未命中→trae」，
/// 无账号兜底 trae（此时表多半为空，请求自然报错，与 Android 行为一致）。
pub fn route(
    model: &str,
    selection: &str,
    has_trae: bool,
    has_qoder: bool,
    repo: &ModelsRepo,
    catalog: &QoderModelCatalog,
) -> &'static str {
    if selection == BACKEND_QODER {
        return BACKEND_QODER;
    }
    if selection == BACKEND_TRAE {
        return BACKEND_TRAE;
    }
    if has_qoder && !has_trae {
        BACKEND_QODER
    } else if has_qoder && has_trae {
        // 同名两边都有 → Trae 优先（routeFor 的判定顺序就是修复点）
        if is_trae_model(model, &repo.current()) {
            BACKEND_TRAE
        } else if is_qoder_model(model, catalog) {
            BACKEND_QODER
        } else {
            BACKEND_TRAE
        }
    } else {
        BACKEND_TRAE
    }
}

/// 展示用「当前主后端」（对应 Kotlin `resolved` :113-:117）：
/// `auto` 且两边都有账号时按 Trae 展示。
pub fn resolved_backend(selection: &str, has_trae: bool, has_qoder: bool) -> &'static str {
    if selection == BACKEND_QODER {
        BACKEND_QODER
    } else if selection == BACKEND_TRAE {
        BACKEND_TRAE
    } else if has_qoder && !has_trae {
        BACKEND_QODER
    } else {
        BACKEND_TRAE
    }
}

// ------------------------------------------------------------------
// 模型表（对应 modelsFor / merge）
// ------------------------------------------------------------------

/// `/v1/models` 与模型页要用的模型表（对应 Kotlin `modelsFor` :144-:157）。
/// `auto` 且两平台都有账号时返回**并集**，否则按实际生效后端返回单表。
pub fn models_for(
    selection: &str,
    has_trae: bool,
    has_qoder: bool,
    repo: &ModelsRepo,
    catalog: &QoderModelCatalog,
) -> Vec<Value> {
    if selection == BACKEND_QODER {
        return catalog.openai_models();
    }
    if selection == BACKEND_TRAE {
        return repo.current();
    }
    if has_qoder && !has_trae {
        catalog.openai_models()
    } else if has_qoder && has_trae {
        auto_union(&repo.current(), &catalog.openai_models())
    } else {
        repo.current()
    }
}

/// 并集（对应 Kotlin 私有 `merge` :165-:186）：Trae 在前、Qoder 独有追加；
/// `id` 非空才参与去重（空 id 条目原样保留，与 Kotlin `isNotBlank` 闸一致）；
/// 每条**克隆后**覆写 `owned_by` 为来源（trae/qoder）。
pub fn auto_union(trae: &[Value], qoder: &[Value]) -> Vec<Value> {
    let mut out = Vec::with_capacity(trae.len() + qoder.len());
    let mut seen: Vec<String> = Vec::new();
    for source in [(trae, BACKEND_TRAE), (qoder, BACKEND_QODER)] {
        for o in source.0 {
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if !id.is_empty() {
                if seen.iter().any(|s| s == id) {
                    continue;
                }
                seen.push(id.to_string());
            }
            let mut c = o.clone();
            if let Some(obj) = c.as_object_mut() {
                obj.insert("owned_by".to_string(), json!(source.1));
            }
            out.push(c);
        }
    }
    out
}

// ------------------------------------------------------------------
// 模型页快照（design.md §IPC `models_view` → {loaded, items:[…], counts}）
// ------------------------------------------------------------------

/// 单条转成模型页视图条目：`{id,name,source,multiplier?,free,verified,context?,aliases?}`。
/// - Trae 条目：trae-solo 表自带 `owned_by="trae-solo"`，来源按所在表判定（这里由
///   `backend` 参数给出），无倍率/免费/认证概念 → `free=false, verified=true`；
/// - Qoder 条目：`price_factor→multiplier`（null 省略）、`is_free→free`、
///   `unverified→verified`（取反）、`context_length→context`、`aliases` 原样。
fn view_item(o: &Value, source: &str) -> Value {
    let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let name = o
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(id);
    let mut item = json!({
        "id": id,
        "name": name,
        "source": source,
        "free": o.get("is_free").and_then(|v| v.as_bool()).unwrap_or(false),
        "verified": !o.get("unverified").and_then(|v| v.as_bool()).unwrap_or(false),
    });
    if let Some(p) = o.get("price_factor").and_then(|v| v.as_f64()) {
        item["multiplier"] = json!(p);
    }
    if let Some(c) = o.get("context_length").and_then(|v| v.as_i64()) {
        item["context"] = json!(c);
    }
    if let Some(a) = o.get("aliases") {
        item["aliases"] = a.clone();
    }
    item
}

/// 供 IPC `models_view`：当前选择下的模型表快照（对应 Kotlin
/// `modelsLoaded = modelsFor(...).length() > 0` :196，「表里到底有没有模型」）。
pub fn snapshot(
    repo: &ModelsRepo,
    catalog: &QoderModelCatalog,
    selection: &str,
    has_trae: bool,
    has_qoder: bool,
) -> Value {
    // 分支与 models_for 一一对应；每条带来源 tag（并集分支用 auto_union 已覆写的 owned_by）。
    let union_case = selection != BACKEND_QODER && selection != BACKEND_TRAE && has_trae && has_qoder;
    let qoder_single =
        selection == BACKEND_QODER || (selection != BACKEND_TRAE && has_qoder && !has_trae);

    let mut items: Vec<Value> = Vec::new();
    let mut n_trae = 0usize;
    let mut n_qoder = 0usize;
    if union_case {
        for o in auto_union(&repo.current(), &catalog.openai_models()) {
            let src = o
                .get("owned_by")
                .and_then(|v| v.as_str())
                .unwrap_or(BACKEND_TRAE);
            items.push(view_item(&o, src));
            if src == BACKEND_QODER {
                n_qoder += 1;
            } else {
                n_trae += 1;
            }
        }
    } else if qoder_single {
        for o in catalog.openai_models() {
            items.push(view_item(&o, BACKEND_QODER));
            n_qoder += 1;
        }
    } else {
        // 强制 Trae / auto 只有 Trae / auto 无账号 → Trae 表（Kotlin 兜底分支）
        for o in repo.current() {
            items.push(view_item(&o, BACKEND_TRAE));
            n_trae += 1;
        }
    }
    let total = items.len();
    json!({
        "loaded": total > 0,
        "backend": resolved_backend(selection, has_trae, has_qoder),
        "mixed": union_case,
        "items": items,
        "counts": { "total": total, "trae": n_trae, "qoder": n_qoder },
    })
}

// ------------------------------------------------------------------
// 测试（临时目录注入；路由/并集/成员判定/快照形态）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BACKEND_AUTO;
    use crate::models::catalog::QModel;
    use crate::storage::DataStore;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmpdir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tw-models-routing-test-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn repo_with(dir: &str, ids: &[&str]) -> ModelsRepo {
        let repo = ModelsRepo::with_store(Arc::new(DataStore::new(dir)));
        let arr: Vec<Value> = ids
            .iter()
            .map(|id| {
                json!({
                    "id": id, "name": id, "config_name": id,
                    "context_length": 131072, "object": "model",
                    "owned_by": "trae-solo", "created": 1753600000
                })
            })
            .collect();
        repo.apply_fetched(arr);
        repo
    }

    fn catalog_with(dir: &str) -> QoderModelCatalog {
        // 默认目录 = 内置 21 条（含 key=auto/ultimate、DeepSeek-Flash 别名等）
        QoderModelCatalog::with_store(Arc::new(DataStore::new(dir)))
    }

    // ---- 成员判定 ----

    #[test]
    fn trae_member_icase_and_empty() {
        let d = tmpdir("trae");
        let repo = repo_with(d.to_str().unwrap(), &["GPT-4o", "deepseek-v3"]);
        let cur = repo.current();
        assert!(is_trae_model("gpt-4o", &cur));
        assert!(is_trae_model("  deepseek-v3  ", &cur));
        assert!(!is_trae_model("", &cur));
        assert!(!is_trae_model("   ", &cur));
        assert!(!is_trae_model("glm-4.6", &cur));
    }

    #[test]
    fn qoder_member_by_key_name_alias() {
        let d = tmpdir("qoder");
        let catalog = catalog_with(d.to_str().unwrap());
        assert!(is_qoder_model("auto", &catalog), "按 key 命中");
        assert!(is_qoder_model("QWEN3-CODER-PLUS", &catalog), "按展示名 icase 命中");
        assert!(
            is_qoder_model("DeepSeek-V4-Flash", &catalog),
            "内置 deepseek 别名链应命中"
        );
        assert!(!is_qoder_model("", &catalog));
        assert!(!is_qoder_model("definitely-not-a-model", &catalog));
    }

    // ---- 并集 ----

    #[test]
    fn union_keeps_trae_first_dedups_and_tags() {
        let trae = vec![
            json!({"id": "dup", "name": "T", "owned_by": "trae-solo"}),
            json!({"id": "only-trae", "owned_by": "trae-solo"}),
            json!({"id": "", "name": "blank1"}),
            json!({"id": "", "name": "blank2"}),
        ];
        let qoder = vec![
            json!({"id": "dup", "name": "Q", "key": "dup"}),
            json!({"id": "only-qoder", "name": "Q2"}),
        ];
        let u = auto_union(&trae, &qoder);
        let ids: Vec<&str> = u.iter().map(|o| o["id"].as_str().unwrap_or("")).collect();
        assert_eq!(ids, vec!["dup", "only-trae", "", "", "only-qoder"], "trae 在前、qoder 独有追加、空 id 不去重");
        assert_eq!(u[0]["owned_by"], "trae", "重复 id 以 trae 条目为准并覆写来源");
        assert_eq!(u[0]["name"], "T");
        assert_eq!(u[4]["owned_by"], "qoder");
        // 入参未被污染（Kotlin「必须复制对象」纪律）
        assert_eq!(trae[0]["owned_by"], "trae-solo");
        assert!(qoder[0].get("owned_by").is_none());
    }

    // ---- models_for 分支 ----

    #[test]
    fn models_for_branches() {
        let d = tmpdir("for");
        let dir = d.to_str().unwrap();
        let repo = repo_with(dir, &["gpt-4o"]);
        let catalog = catalog_with(dir);

        let q = models_for(BACKEND_QODER, true, true, &repo, &catalog);
        assert_eq!(q.len(), catalog.current().len(), "强制 qoder → 目录全量");
        assert_eq!(q[0]["owned_by"], "qoder");

        let t = models_for(BACKEND_TRAE, true, true, &repo, &catalog);
        assert_eq!(t.len(), 1, "强制 trae → 仅 trae 表");
        assert_eq!(t[0]["id"], "gpt-4o");

        let auto_q = models_for(BACKEND_AUTO, false, true, &repo, &catalog);
        assert_eq!(auto_q.len(), catalog.current().len());
        let auto_t = models_for(BACKEND_AUTO, true, false, &repo, &catalog);
        assert_eq!(auto_t.len(), 1);
        let auto_none = models_for(BACKEND_AUTO, false, false, &repo, &catalog);
        assert_eq!(auto_none.len(), 1, "无账号兜底 trae 表（同 Kotlin 最后分支）");

        let mixed = models_for(BACKEND_AUTO, true, true, &repo, &catalog);
        assert_eq!(mixed.len(), 1 + catalog.current().len(), "并集：gpt-4o 不与内置重名");
        assert_eq!(mixed[0]["id"], "gpt-4o");
        assert_eq!(mixed[0]["owned_by"], "trae");
        assert_eq!(mixed[1]["owned_by"], "qoder");
    }

    #[test]
    fn union_dedups_same_public_id() {
        let d = tmpdir("dedup");
        let dir = d.to_str().unwrap();
        // 内置目录展示名首条 = "Qwen3-Coder-Plus"；用同名 config 制造重名
        let catalog = catalog_with(dir);
        let first_name = catalog.current()[0].name.clone();
        let repo = repo_with(dir, &[&first_name]);
        let mixed = models_for(BACKEND_AUTO, true, true, &repo, &catalog);
        assert_eq!(
            mixed.iter().filter(|o| o["id"].as_str() == Some(first_name.as_str())).count(),
            1,
            "同 id 只留 trae 条目"
        );
        assert_eq!(mixed[0]["owned_by"], "trae");
        assert_eq!(mixed.len(), catalog.current().len(), "总数 = trae1 + qoder21 - 重名1");
    }

    // ---- route / resolved ----

    #[test]
    fn route_same_name_prefers_trae() {
        let d = tmpdir("route");
        let dir = d.to_str().unwrap();
        let catalog = catalog_with(dir);
        let dup = catalog.current()[0].name.clone(); // "Qwen3-Coder-Plus"
        let repo = repo_with(dir, &[&dup, "gpt-4o"]);

        assert_eq!(
            route(&dup, BACKEND_AUTO, true, true, &repo, &catalog),
            BACKEND_TRAE,
            "同名模型 Trae 优先（源码注释铁律）"
        );
        assert_eq!(
            route("auto", BACKEND_AUTO, true, true, &repo, &catalog),
            BACKEND_QODER,
            "仅 qoder 命中（key）→ Qoder"
        );
        assert_eq!(
            route("DeepSeek-V4-Flash", BACKEND_AUTO, true, true, &repo, &catalog),
            BACKEND_QODER,
            "别名命中 → Qoder"
        );
        assert_eq!(
            route("no-such-model", BACKEND_AUTO, true, true, &repo, &catalog),
            BACKEND_TRAE,
            "都未命中 → Trae 兜底"
        );
        assert_eq!(route("auto", BACKEND_QODER, true, true, &repo, &catalog), BACKEND_QODER);
        assert_eq!(route(&dup, BACKEND_TRAE, true, true, &repo, &catalog), BACKEND_TRAE);
        assert_eq!(route("no-such-model", BACKEND_AUTO, true, false, &repo, &catalog), BACKEND_TRAE);
        assert_eq!(route("no-such-model", BACKEND_AUTO, false, true, &repo, &catalog), BACKEND_QODER);
        assert_eq!(route("no-such-model", BACKEND_AUTO, false, false, &repo, &catalog), BACKEND_TRAE);
    }

    #[test]
    fn resolved_backend_matrix() {
        assert_eq!(resolved_backend(BACKEND_QODER, true, true), BACKEND_QODER);
        assert_eq!(resolved_backend(BACKEND_TRAE, true, true), BACKEND_TRAE);
        assert_eq!(resolved_backend(BACKEND_AUTO, true, true), BACKEND_TRAE, "auto 双边按 Trae 展示");
        assert_eq!(resolved_backend(BACKEND_AUTO, false, true), BACKEND_QODER);
        assert_eq!(resolved_backend(BACKEND_AUTO, true, false), BACKEND_TRAE);
        assert_eq!(resolved_backend(BACKEND_AUTO, false, false), BACKEND_TRAE);
    }

    // ---- 动态目录参与判定（回归：set_dynamic 后 is_qoder_model 立即生效）----

    #[test]
    fn qoder_member_follows_dynamic_catalog() {
        let d = tmpdir("dyn");
        let catalog = catalog_with(d.to_str().unwrap());
        catalog.set_dynamic(Some(vec![QModel {
            key: "brand-new".into(),
            name: "Brand New".into(),
            context_length: 128_000,
            reasoning: false,
            price_factor: Some(0.0),
            aliases: vec!["bn".into()],
            unverified: true,
        }]));
        assert!(is_qoder_model("brand new", &catalog));
        assert!(is_qoder_model("bn", &catalog));
    }

    // ---- snapshot ----

    #[test]
    fn snapshot_shape_forced_trae_union_and_empty() {
        let d = tmpdir("snap");
        let dir = d.to_str().unwrap();
        let catalog = catalog_with(dir);
        let dup = catalog.current()[0].name.clone();
        let repo = repo_with(dir, &[&dup, "gpt-4o"]);

        let s = snapshot(&repo, &catalog, BACKEND_TRAE, true, true);
        assert_eq!(s["loaded"], json!(true));
        assert_eq!(s["backend"], json!("trae"));
        assert_eq!(s["mixed"], json!(false));
        assert_eq!(s["counts"]["total"], json!(2));
        assert_eq!(s["counts"]["trae"], json!(2));
        assert_eq!(s["items"][0]["source"], json!("trae"));
        assert_eq!(s["items"][0]["free"], json!(false));
        assert_eq!(s["items"][0]["verified"], json!(true));
        assert_eq!(s["items"][0]["context"], json!(131072));
        assert!(s["items"][0].get("multiplier").is_none(), "trae 条目无倍率");

        let u = snapshot(&repo, &catalog, BACKEND_AUTO, true, true);
        assert_eq!(u["mixed"], json!(true));
        assert_eq!(u["counts"]["trae"], json!(2));
        assert_eq!(u["counts"]["qoder"], json!(20), "21 内置去重 1 条");
        assert_eq!(u["counts"]["total"], json!(22));
        // Qoder 条目的对客 id = 官方显示名（Kotlin openAiModels :391），条目内 key 字段才是内部键。
        let auto_item = u["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == json!("Auto"))
            .expect("并集里应有 Auto（display name 作 id）");
        assert_eq!(auto_item["source"], json!("qoder"));
        assert_eq!(auto_item["verified"], json!(true));
        assert!(auto_item.get("multiplier").is_some(), "qoder 条目带倍率");

        let empty_repo = ModelsRepo::with_store(Arc::new(DataStore::new(dir)));
        let e = snapshot(&empty_repo, &catalog, BACKEND_AUTO, true, false);
        assert_eq!(e["loaded"], json!(false), "强制/兜底 trae 空表 → 未加载（表里没模型就是没加载）");
        assert_eq!(e["items"].as_array().unwrap().len(), 0);

        let qonly = snapshot(&empty_repo, &catalog, BACKEND_AUTO, false, true);
        assert_eq!(qonly["backend"], json!("qoder"));
        assert_eq!(qonly["counts"]["qoder"], json!(21));
        assert!(qonly["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["source"] == json!("qoder") && o["id"] == json!(dup)));
    }
}
