package com.example.traework2api

import org.json.JSONArray
import org.json.JSONObject

/**
 * Qoder 聊天网关的模型目录与解析器（对齐参考实现的 `modelLibrary`）。
 *
 * 与 Trae 的模型表有本质区别：Trae 是"上游拉回来什么就是什么"（[ModelsRepo]），
 * Qoder 这边**模型是协议的一部分** —— 请求里要带 `model_config` 的 key，上游按 key 路由。
 * 所以这里必须内置一份权威清单，且解析规则要能容忍客户端五花八门的写法
 * （`Ultimate`、`ultimate`、`ultimate[1m]` 都得认）。
 *
 * `[1m]` 是参考实现的一个约定后缀：客户端想显式要 1M 上下文时加上它，
 * 解析时剥掉；若模型本身上下文不足 900k 则**升级**到 `ultimate`（唯一能兜住 1M 的）。
 */
object QoderModelCatalog {

    /** 单条模型定义。 */
    data class QModel(
        val key: String,          // 请求里 `model_config` 用的 key
        val name: String,         // 展示名（也是客户端可用别名）
        val contextLength: Int,
        val reasoning: Boolean,   // 是否为推理模型（支持 reasoning_effort）
    )

    /**
     * 16 个模型。**顺序即 UI 顺序**：`auto` 在最前（默认），贵的推理模型靠后。
     * 与参考实现 `main.go` 的 `modelLibrary` 逐条一致。
     */
    val ALL: List<QModel> = listOf(
        QModel("auto", "Auto", 180_000, false),
        QModel("ultimate", "Ultimate", 1_000_000, true),
        QModel("performance", "Performance", 1_000_000, false),
        QModel("efficient", "Efficient", 180_000, false),
        QModel("lite", "Lite", 180_000, false),
        QModel("cmodel", "Cantus", 1_000_000, true),
        QModel("qmodel_38max", "Qwen3.8-Max", 1_000_000, true),
        QModel("qmodel_latest", "Qwen3.7-Max", 1_000_000, false),
        QModel("qmodel", "Qwen3.7-Plus", 1_000_000, false),
        QModel("kmodel_latest", "Kimi-K3", 1_000_000, false),
        QModel("kmodel", "Kimi-K2.7-Code", 256_000, false),
        QModel("gmodel", "GLM-5.3", 1_000_000, true),
        QModel("gm51model", "GLM-5.2", 1_000_000, true),
        QModel("dmodel", "DeepSeek-V4-Pro", 1_000_000, true),
        QModel("dfmodel", "DeepSeek-V4-Flash", 1_000_000, true),
        QModel("mmodel", "MiniMax-M3", 1_000_000, false),
    )

    /** 兜底模型：客户端没指定 / 指定了不认识的名字。 */
    const val DEFAULT_KEY = "auto"

    /** 1M 上下文的兜底模型（唯一能保证 1M 的那个）。 */
    private const val ONE_M_KEY = "ultimate"

    /** `[1m]` 后缀：客户端显式要求 1M 上下文。 */
    private const val ONE_M_SUFFIX = "[1m]"

    /** 解析结果。 */
    data class Resolved(
        val model: QModel,
        /** 客户端原始写法的 `[1m]` 标记（用于展示层区分同一模型的两个入口）。 */
        val oneMSuffix: Boolean,
    ) {
        /** 发给上游的模型 key。 */
        val key: String get() = model.key

        /** 对外展示的模型 id（`[1m]` 时带上后缀，与客户端请求保持一致）。 */
        val publicId: String get() = if (oneMSuffix) model.key + ONE_M_SUFFIX else model.key
    }

    /**
     * 解析客户端传的 model 字符串。
     *
     * 规则（对齐参考实现 `modelResolver.resolve`）：
     *  1. 去掉 `[1m]` 后缀，记下标记
     *  2. 先按 key 精确匹配（大小写不敏感）
     *  3. 再按展示名匹配（`Ultimate` → `ultimate`）
     *  4. `*` 通配符（参考实现里可映射到任意模型，这里保留但默认不启用）
     *  5. 都不中 → 默认 `auto`
     *  6. 带 `[1m]` 且目标模型上下文 < 900k → 升级到 `ultimate`
     */
    fun resolve(rawModel: String, maxInputTokens: Int = 0): Resolved {
        val trimmed = rawModel.trim()
        val oneM = trimmed.endsWith(ONE_M_SUFFIX, ignoreCase = true)
        val bare = if (oneM) trimmed.dropLast(ONE_M_SUFFIX.length).trim() else trimmed

        var found = if (bare.isEmpty()) null else findByIdentifier(bare)
        if (found == null) found = ALL.firstOrNull { it.key == DEFAULT_KEY }
        var model = found ?: ALL.first()

        if (oneM && maxInputTokens < 900_000 && model.contextLength < 900_000) {
            model = ALL.firstOrNull { it.key == ONE_M_KEY } ?: model
        }
        return Resolved(model, oneM)
    }

    /** 先按 key、再按展示名匹配（都大小写不敏感）。 */
    private fun findByIdentifier(identifier: String): QModel? =
        ALL.firstOrNull { it.key.equals(identifier, ignoreCase = true) }
            ?: ALL.firstOrNull { it.name.equals(identifier, ignoreCase = true) }

    /** 按 key 找（供内部使用，不做别名解析）。 */
    fun byKey(key: String): QModel? = ALL.firstOrNull { it.key == key }

    // ------------------------------------------------------------------
    // /v1/models 输出 & UI 列表
    // ------------------------------------------------------------------

    /**
     * OpenAI `/v1/models` 的 `data` 数组。
     *
     * 同时带 `name`/`context_length` 供本 App 的模型页复用（[modelCard] 读这两个字段），
     * 标准 OpenAI 字段（`id`/`object`/`created`/`owned_by`）保持齐全，
     * 免得严格客户端因为缺字段解析失败。
     */
    fun openAiModels(): JSONArray {
        val arr = JSONArray()
        val created = System.currentTimeMillis() / 1000
        for (m in ALL) {
            arr.put(
                JSONObject()
                    .put("id", m.key)
                    .put("object", "model")
                    .put("created", created)
                    .put("owned_by", "qoder")
                    .put("name", m.name)
                    .put("context_length", m.contextLength),
            )
        }
        return arr
    }

    /**
     * 每个模型一个**持久**会话 id（`session_id`）。
     *
     * 为什么按模型分开：Qoder 的会话历史绑在 `session_id` 上，所有模型共用一个 id
     * 会让上游把不同模型的上下文混在一起。参考实现 `sessionFor` 就是这么做的。
     * 进程内缓存即可 —— 重启换新 id 只是丢历史，不影响功能。
     */
    private val sessions = HashMap<String, String>()

    @Synchronized
    fun sessionFor(modelKey: String): String = sessions.getOrPut(modelKey) {
        "q2a-" + QoderCosyCrypto.formatLowerUUID(
            QoderCosyCrypto.reverseMaskUUID(ByteArray(16).also { java.security.SecureRandom().nextBytes(it) }),
        )
    }

    /** 新的一次请求 id（`request_id` / `chat_record_id` 共用）。 */
    fun newRequestId(): String = QoderCosyCrypto.newRequestId()

    // ------------------------------------------------------------------
    // 自检
    // ------------------------------------------------------------------

    fun selfCheck() {
        check(ALL.size == 16) { "自检失败：Qoder 模型目录应为 16 条，实际 ${ALL.size}" }
        check(ALL.map { it.key }.toSet().size == 16) { "自检失败：模型 key 有重复" }
        check(ALL.first().key == DEFAULT_KEY) { "自检失败：默认模型应排在最前" }

        // 精确 key / 展示名 / 大小写 / 未知值
        check(resolve("qmodel").key == "qmodel") { "自检失败：key 精确匹配失败" }
        check(resolve("Ultimate").key == "ultimate") { "自检失败：展示名匹配失败" }
        check(resolve("GLM-5.3").key == "gmodel") { "自检失败：展示名（含点号）匹配失败" }
        check(resolve("nonexistent").key == DEFAULT_KEY) { "自检失败：未知模型应回落默认" }
        check(resolve("").key == DEFAULT_KEY) { "自检失败：空模型应回落默认" }

        // [1m] 语义：上下文不足 900k 的模型要升级到 ultimate；本来就 1M 的不动
        val lm = resolve("lite[1m]", maxInputTokens = 200_000)
        check(lm.oneMSuffix) { "自检失败：[1m] 标记未识别" }
        check(lm.key == ONE_M_KEY) { "自检失败：上下文不足 900k 时应升级到 $ONE_M_KEY，得到 ${lm.key}" }
        val lm2 = resolve("qmodel[1m]", maxInputTokens = 1_000_000)
        check(lm2.key == "qmodel") { "自检失败：已够 1M 时不该升级，得到 ${lm2.key}" }
        check(lm.publicId.endsWith(ONE_M_SUFFIX)) { "自检失败：publicId 应保留 [1m] 后缀" }

        // /v1/models 形状
        val models = openAiModels()
        check(models.length() == 16) { "自检失败：/v1/models 条数不对" }
        val first = models.getJSONObject(0)
        check(first.getString("object") == "model") { "自检失败：object 字段必须是 model" }
        check(first.getString("id") == DEFAULT_KEY) { "自检失败：首个模型 id 不对" }
        check(first.optInt("context_length", 0) > 0) { "自检失败：context_length 缺失（模型页要用）" }

        // 会话 id：同模型稳定、不同模型不同
        val s1 = sessionFor("auto")
        check(s1 == sessionFor("auto")) { "自检失败：同模型 session 必须稳定" }
        check(s1 != sessionFor("ultimate")) { "自检失败：不同模型 session 必须不同" }
        check(s1.startsWith("q2a-") && s1.length == 4 + 36) { "自检失败：session 格式不对: $s1" }
    }
}
