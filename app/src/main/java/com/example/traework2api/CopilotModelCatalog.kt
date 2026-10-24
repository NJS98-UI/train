package com.example.traework2api

import org.json.JSONArray
import org.json.JSONObject

/**
 * GitHub Copilot 的模型目录。
 *
 * 与 [QoderModelCatalog] 的分工一样：这里只管"上游给的 JSON 怎么变成本地模型表"，
 * 取数据在 [CopilotApi.fetchModels]。
 *
 * 为什么不用内置清单：Copilot 的模型**随账号套餐变化**（free 只有少数几个，
 * individual 有 GPT/Claude/Gemini 一大票），而且上游会随时增删。
 * 硬编码一份必然过期，所以这里**完全以上游为准**，解析失败就返回空表让 UI 提示。
 */
object CopilotModelCatalog {

    /** 一条模型。`id` 就是请求时要填的值（如 `gpt-4o`、`claude-sonnet-4`）。 */
    data class CModel(
        val id: String,
        val name: String,
        val vendor: String = "",
        val contextLength: Int = 0,
        val reasoning: Boolean = false,
        val vision: Boolean = false,
        /** 上游标记是否出现在模型选择器里（`false` 的一般是内部/隐藏模型）。 */
        val pickerEnabled: Boolean = true,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("id", id)
            .put("object", "model")
            .put("owned_by", "copilot")
            .put("name", name)
            .put("key", id)
            .put("context_length", contextLength)
            .put("is_free", false)
            .put("vendor", vendor)
            .put("reasoning", reasoning)
            .put("vision", vision)
    }

    @Volatile
    private var dynamic: List<CModel>? = null

    @Volatile
    private var fetchedAt: Long = 0L

    fun current(): List<CModel> = dynamic ?: emptyList()

    fun hasDynamic(): Boolean = dynamic != null

    fun fetchedAt(): Long = fetchedAt

    fun setDynamic(list: List<CModel>) {
        if (list.isEmpty()) return   // 空表一律拒绝覆盖（与 ModelsRepo 同一纪律）
        dynamic = list
        fetchedAt = System.currentTimeMillis()
    }

    fun clear() {
        dynamic = null
        fetchedAt = 0L
    }

    fun byId(id: String): CModel? = current().firstOrNull { it.id == id }

    /** 解析上游 `/models` 响应。认 `{"data":[…]}` / `{"models":[…]}` / 裸数组。 */
    fun parse(raw: String): List<CModel> {
        val arr = extractArray(raw) ?: return emptyList()
        val out = ArrayList<CModel>(arr.length())
        val seen = HashSet<String>()
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            val m = toModel(o) ?: continue
            if (seen.add(m.id)) out.add(m)
        }
        return out
    }

    private fun extractArray(raw: String): JSONArray? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty()) return null
        return try {
            when (val root = org.json.JSONTokener(trimmed).nextValue()) {
                is JSONArray -> root
                is JSONObject -> root.optJSONArray("data")
                    ?: root.optJSONArray("models")
                    ?: root.optJSONArray("items")
                else -> null
            }
        } catch (_: Exception) {
            null
        }
    }

    private fun toModel(o: JSONObject): CModel? {
        // id 可能叫 `id`，少数实现叫 `model` / `name`
        val id = o.optString("id", "").ifBlank { o.optString("model", "") }.trim()
        if (id.isEmpty()) return null
        val caps = o.optJSONObject("capabilities")
        val limits = caps?.optJSONObject("limits")
        val context = limits?.optInt("max_context_window_tokens", 0)
            ?: caps?.optInt("max_context_window_tokens", 0)
            ?: o.optInt("max_context_window_tokens", 0)
            ?: o.optInt("context_length", 0)
        val supports = caps?.optJSONObject("supports")
        val reasoning = supports?.optBoolean("reasoning", false)
            ?: caps?.optBoolean("supports_reasoning", false)
            ?: false
        val vision = supports?.optBoolean("vision", false)
            ?: caps?.optBoolean("supports_vision", false)
            ?: false
        // 只有显式 false 才隐藏；字段缺失一律当"可选"
        val picker = if (o.has("model_picker_enabled")) o.optBoolean("model_picker_enabled", true) else true
        return CModel(
            id = id,
            name = o.optString("name", "").ifBlank { id },
            vendor = o.optString("vendor", o.optString("publisher", "")),
            contextLength = context,
            reasoning = reasoning,
            vision = vision,
            pickerEnabled = picker,
        )
    }

    /**
     * `/v1/models` 的 `data` 数组。
     *
     * 与 Qoder 一致：`id` 用**官方展示名能认出来的 id**（这里上游 id 本身就是
     * `gpt-4o` 这种可读值，直接用它），额外带 `name` / `context_length` / `vendor`
     * 给本 App 的模型页用。
     */
    fun openAiModels(includeHidden: Boolean = false): JSONArray {
        val arr = JSONArray()
        val created = System.currentTimeMillis() / 1000
        for (m in current()) {
            if (!includeHidden && !m.pickerEnabled) continue
            arr.put(m.toJson().put("created", created))
        }
        return arr
    }

    fun selfCheck() {
        // 真实形状：{"data":[{id,name,vendor,capabilities:{limits:{...},supports:{...}},model_picker_enabled}]}
        val raw = """
            {"data":[
              {"id":"gpt-4o","name":"GPT-4o","vendor":"OpenAI","model_picker_enabled":true,
               "capabilities":{"limits":{"max_context_window_tokens":128000},
                               "supports":{"vision":true,"reasoning":false}}},
              {"id":"claude-sonnet-4","name":"Claude Sonnet 4","vendor":"Anthropic",
               "capabilities":{"limits":{"max_context_window_tokens":200000},
                               "supports":{"reasoning":true}}},
              {"id":"hidden-model","name":"Hidden","model_picker_enabled":false},
              {"name":"没有 id 的垃圾条目"}
            ]}
        """.trimIndent()
        val parsed = parse(raw)
        check(parsed.size == 3) { "自检失败：应解析出 3 条（无 id 的丢掉），实际 ${parsed.size}" }
        check(parsed[0].id == "gpt-4o" && parsed[0].contextLength == 128_000) { "自检失败：id/上下文解析失败" }
        check(parsed[0].vision && !parsed[0].reasoning) { "自检失败：能力位解析失败" }
        check(parsed[1].reasoning && parsed[1].contextLength == 200_000) { "自检失败：reasoning 解析失败" }
        check(!parsed[2].pickerEnabled) { "自检失败：model_picker_enabled=false 未识别" }

        // 隐藏模型默认不出现在 /v1/models
        setDynamic(parsed)
        check(openAiModels().length() == 2) { "自检失败：隐藏模型不该出现在 /v1/models" }
        check(openAiModels(includeHidden = true).length() == 3) { "自检失败：includeHidden 未生效" }
        check(byId("gpt-4o")?.name == "GPT-4o") { "自检失败：byId 失败" }

        // 空表不许覆盖已有目录
        setDynamic(emptyList())
        check(current().size == 3) { "自检失败：空表覆盖了已有目录" }
        clear()
        check(current().isEmpty()) { "自检失败：clear 未生效" }

        // 裸数组 / models 包装也要认
        check(parse("""[{"id":"a"}]""").size == 1) { "自检失败：裸数组解析失败" }
        check(parse("""{"models":[{"id":"b"}]}""").size == 1) { "自检失败：models 包装解析失败" }
        check(parse("not json").isEmpty()) { "自检失败：非 JSON 应返回空" }
        check(parse("").isEmpty()) { "自检失败：空串应返回空" }
    }
}
