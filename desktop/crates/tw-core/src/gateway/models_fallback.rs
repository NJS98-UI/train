//! `/v1/models` 静态兜底快照（内置 21 条 Qoder BUILTIN，转成 OpenAI 模型对象）。
//! 数据逐条转写自 `.monkeycode/analysis/models-usage-backup.md` §2.2（源码 QoderModelCatalog 第 74–96 行）；
//! 输出形状对齐 `QoderModelCatalog.openAiModels()`（id 用官方显示名、owned_by=qoder、
//! price_factor 为 null 时**不写键**、aliases 带上内部 key 并滤掉与 id 同名项）。
//!
//! ⚠️ models 层（`crate::models`）完成后由集成方把 `fallback_models()` 替换为
//! `models::qoder_catalog_builtin()` 之类的权威来源；本表仅是网关层内置兜底。

use serde_json::{json, Value};

/// 一条内置模型（字段与 Kotlin QModel 对齐）。
struct BuiltinModel {
    key: &'static str,
    name: &'static str,
    context_length: i64,
    reasoning: bool,
    /// `None` = 倍率未知（输出不写 price_factor，展示层显示「倍率未知」）
    price_factor: Option<f64>,
    aliases: &'static [&'static str],
}

/// 内置快照（顺序即 UI 顺序；21 条，首条 qwen3-coder-plus——以源码清单为准，
/// 分析文档 §2.2 红字矛盾已按 "21 条现状" 基线裁决，见 models-usage-backup.md）。
const BUILTIN: &[BuiltinModel] = &[
    BuiltinModel { key: "qwen3-coder-plus", name: "Qwen3 Coder Plus", context_length: 180_000, reasoning: false, price_factor: None, aliases: &[] },
    BuiltinModel { key: "auto", name: "Auto", context_length: 180_000, reasoning: false, price_factor: Some(0.5), aliases: &[] },
    BuiltinModel { key: "ultimate", name: "极致", context_length: 1_000_000, reasoning: true, price_factor: Some(0.8), aliases: &["Ultimate"] },
    BuiltinModel { key: "performance", name: "快速", context_length: 1_000_000, reasoning: false, price_factor: None, aliases: &["Performance"] },
    BuiltinModel { key: "efficient", name: "经济", context_length: 180_000, reasoning: false, price_factor: None, aliases: &["Efficient"] },
    BuiltinModel { key: "smodel", name: "Sonus", context_length: 180_000, reasoning: true, price_factor: Some(8.0), aliases: &["smodel"] },
    BuiltinModel { key: "cmodel", name: "Cantus", context_length: 200_000, reasoning: true, price_factor: Some(4.0), aliases: &[] },
    BuiltinModel { key: "qmodel_38max", name: "Qwen3.8-Max 2", context_length: 1_000_000, reasoning: true, price_factor: Some(0.5), aliases: &["Qwen3.8-Max"] },
    BuiltinModel { key: "qfmodel", name: "Qwen3.8-Flash", context_length: 1_000_000, reasoning: true, price_factor: Some(0.0), aliases: &["Qwen3.8-Flash-1M"] },
    BuiltinModel { key: "kmodel_latest", name: "Kimi-K3.1-Pro", context_length: 1_000_000, reasoning: false, price_factor: Some(1.0), aliases: &["Kimi-K3"] },
    BuiltinModel { key: "kmodel", name: "Kimi-K2.8-Preview", context_length: 256_000, reasoning: true, price_factor: Some(0.8), aliases: &["Kimi-K2.7-Code"] },
    BuiltinModel { key: "gmodel", name: "GLM-5.3", context_length: 1_000_000, reasoning: true, price_factor: Some(0.8), aliases: &[] },
    BuiltinModel { key: "gfmodel", name: "GLM-5.3-Flash", context_length: 1_000_000, reasoning: true, price_factor: Some(0.1), aliases: &[] },
    BuiltinModel { key: "dmodel", name: "DeepSeek-V4-Pro", context_length: 1_000_000, reasoning: true, price_factor: Some(0.5), aliases: &[] },
    BuiltinModel { key: "dfmodel", name: "DeepSeek-Flash", context_length: 1_000_000, reasoning: true, price_factor: Some(0.0), aliases: &["DeepSeek-V4-Flash"] },
    BuiltinModel { key: "mmodel", name: "MiniMax-M3.1-Flash", context_length: 1_000_000, reasoning: false, price_factor: Some(0.1), aliases: &["MiniMax-M3"] },
    BuiltinModel { key: "lite", name: "Lite", context_length: 180_000, reasoning: false, price_factor: None, aliases: &[] },
    BuiltinModel { key: "qmodel_latest", name: "Qwen3.7-Max", context_length: 1_000_000, reasoning: true, price_factor: Some(0.1), aliases: &[] },
    BuiltinModel { key: "qmodel", name: "Qwen3.7-Plus", context_length: 1_000_000, reasoning: true, price_factor: Some(0.04), aliases: &[] },
    BuiltinModel { key: "q37fmodel", name: "Qwen3.7-Flash", context_length: 1_000_000, reasoning: true, price_factor: Some(0.1), aliases: &[] },
    BuiltinModel { key: "gm51model", name: "GLM-5.2", context_length: 1_000_000, reasoning: true, price_factor: Some(0.6), aliases: &[] },
];

/// 生成兜底 data 数组（`created` 取当前 unix 秒，与 Kotlin openAiModels 的运行时行为一致）。
pub fn fallback_models() -> Vec<Value> {
    let created = chrono::Utc::now().timestamp();
    BUILTIN
        .iter()
        .map(|m| {
            // `id` 用官方显示名（空则退回 key），不用内部缩写 key（客户端下拉只认得名字）
            let public_id = if m.name.trim().is_empty() { m.key } else { m.name };
            let free = m.price_factor.map(|p| p <= 0.0).unwrap_or(false);
            let mut o = json!({
                "id": public_id,
                "object": "model",
                "created": created,
                "owned_by": "qoder",
                "name": m.name,
                "key": m.key,
                "context_length": m.context_length,
                "is_free": free,
            });
            // 倍率未知时**不写字段**（展示层显示「倍率未知」，不编 ×1）
            if let Some(p) = m.price_factor {
                o["price_factor"] = json!(p);
            }
            // 别名带上内部 key，兼容"直接填 key"的老客户端；滤掉与 id 同名项、去重
            let mut aliases: Vec<&str> = Vec::new();
            for a in std::iter::once(m.key).chain(m.aliases.iter().copied()) {
                if a != public_id && !aliases.contains(&a) {
                    aliases.push(a);
                }
            }
            if !aliases.is_empty() {
                o["aliases"] = json!(aliases);
            }
            o
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_count_and_order() {
        let models = fallback_models();
        // 分析文档 §2.2 裁决基线：以源码清单为准 = 21 条，首条 qwen3-coder-plus
        assert_eq!(models.len(), 21);
        assert_eq!(models[0]["key"], "qwen3-coder-plus");
        assert_eq!(models[0]["id"], "Qwen3 Coder Plus");
        assert_eq!(models[1]["key"], "auto");
        assert_eq!(models[1]["id"], "Auto");
    }

    #[test]
    fn openai_shape_fields() {
        let models = fallback_models();
        for m in &models {
            assert_eq!(m["object"], "model");
            assert_eq!(m["owned_by"], "qoder");
            assert!(m["created"].as_i64().unwrap() > 0);
            assert!(m["id"].as_str().unwrap().starts_with(char::is_alphabetic));
            assert!(m["name"].as_str().unwrap().len() > 0);
            assert!(m["context_length"].as_i64().unwrap() > 0);
            assert!(m["is_free"].is_boolean());
        }
        // price_factor null → 不写键；0.0 → 写且 is_free=true
        let by_key = |k: &str| models.iter().find(|m| m["key"] == k).unwrap().clone();
        assert!(by_key("qwen3-coder-plus").get("price_factor").is_none());
        assert!(by_key("qfmodel").get("price_factor").is_some());
        assert_eq!(by_key("qfmodel")["is_free"], json!(true));
        assert_eq!(by_key("auto")["is_free"], json!(false));
        // aliases：首项是内部 key、不含展示名本身
        let ultimate = by_key("ultimate");
        let aliases = ultimate["aliases"].as_array().unwrap();
        assert_eq!(aliases[0], json!("ultimate"));
        assert_eq!(aliases[1], json!("Ultimate"));
        let auto = by_key("auto");
        let auto_aliases = auto["aliases"].as_array().unwrap();
        assert_eq!(auto_aliases, &vec![json!("auto")]);
    }
}
