package com.example.traework2api

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * Qoder 聊天网关的模型目录与解析器（对齐参考实现的 `modelLibrary`）。
 *
 * 与 Trae 的模型表有本质区别：Trae 是"上游拉回来什么就是什么"（[ModelsRepo]），
 * Qoder 这边**模型是协议的一部分** —— 请求里要带 `model_config` 的 key，上游按 key 路由。
 * 所以必须内置一份权威清单兜底，且解析规则要能容忍客户端五花八门的写法
 * （`Ultimate`、`ultimate`、`ultimate[1m]` 都得认）。
 *
 * ## 为什么还要"动态目录"
 *
 * 内置的 16 条是参考实现抄下来的**旧快照**：官方会新增模型（尤其**免费/0 倍率**的那些），
 * 也会调整倍率。硬编码的结果就是「官方有、这里没有」+「倍率全显示 1.0」——
 * 后者更糟，会让人以为免费模型也扣费。
 *
 * 所以这里支持用**上游真实目录**覆盖内置清单：
 *  - [fetchFromUpstream] 拿 `openapi` 的 `/api/v1/models`（未鉴权时返回 401，说明路由存在）
 *  - [parseCatalogJson] 容错解析：既认 Qoder CLI 的 `{chat:[...], byok_*:[...]}` 分组格式，
 *    也认任何嵌套里"长得像模型"的对象数组（字段名对齐 `catalogEntry`）
 *  - 结果按 key 合并进内置清单（内置顺序不变、同 key 覆盖字段、新 key 追加到末尾）
 *  - 落 prefs，重启不丢；拉不到就继续用内置清单（绝不因为网络失败把清单清空）
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
        /** 官方标注的**倍率**（`price_factor`）。0.0 = 免费。缺省按 1.0（中性，不夸大成免费）。 */
        val priceFactor: Double = 1.0,
    ) {
        /** 免费模型（官方倍率 0）。 */
        val free: Boolean get() = priceFactor <= 0.0

        /** 倍率展示文本：`0` / `1.0` / `2.5`（去掉无意义的小数尾巴）。 */
        val priceLabel: String
            get() = if (priceFactor == priceFactor.toLong().toDouble()) {
                priceFactor.toLong().toString()
            } else {
                priceFactor.toString()
            }
    }

    /**
     * 16 个**内置**模型。**顺序即 UI 顺序**：`auto` 在最前（默认），贵的推理模型靠后。
     * 与参考实现 `main.go` 的 `builtinCatalog` 逐条一致（倍率也照抄 1.0 ——
     * 参考实现自己也没有真实倍率，真实倍率靠 [setDynamic] 从上游目录覆盖）。
     */
    val BUILTIN: List<QModel> = listOf(
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

    /** 兼容旧引用名。 */
    val ALL: List<QModel> get() = current()

    /** 兜底模型：客户端没指定 / 指定了不认识的名字。 */
    const val DEFAULT_KEY = "auto"

    /** 1M 上下文的兜底模型（唯一能保证 1M 的那个）。 */
    private const val ONE_M_KEY = "ultimate"

    /** `[1m]` 后缀：客户端显式要求 1M 上下文。 */
    private const val ONE_M_SUFFIX = "[1m]"

    private const val PREF = "traework2api_qoder_models"
    private const val KEY_CATALOG = "catalog_json"
    private const val KEY_FETCHED_AT = "fetched_at"

    /** 上游目录覆盖层；null = 用内置清单。 */
    @Volatile
    private var dynamic: List<QModel>? = null

    @Volatile
    private var fetchedAt: Long = 0L

    /** 当前生效的模型清单（动态优先，否则内置）。 */
    fun current(): List<QModel> = dynamic ?: BUILTIN

    /** 是否已经在用上游目录。 */
    fun hasDynamic(): Boolean = dynamic != null

    /** 上游目录的拉取时间（0 = 没有）。 */
    fun dynamicFetchedAt(): Long = fetchedAt

    /**
     * 设置动态目录。
     *
     * @param entries null 或空 = 退回内置清单（**不是**清空目录 —— 空目录会让聊天全废）
     */
    fun setDynamic(entries: List<QModel>?) {
        dynamic = if (entries.isNullOrEmpty()) null else merge(entries)
    }

    /**
     * 合并：内置顺序不变，同 key 用上游字段覆盖（倍率/上下文/推理位以官方为准），
     * 新 key 追加到末尾。这样 `auto` 永远排第一，客户端默认值不会漂。
     */
    fun merge(entries: List<QModel>): List<QModel> {
        if (entries.isEmpty()) return BUILTIN
        val byKey = entries.associateBy { it.key }
        val out = ArrayList<QModel>(BUILTIN.size + entries.size)
        val used = HashSet<String>(BUILTIN.size + entries.size)
        for (b in BUILTIN) {
            val d = byKey[b.key]
            out.add(
                if (d == null) b else b.copy(
                    name = d.name.ifBlank { b.name },
                    contextLength = if (d.contextLength > 0) d.contextLength else b.contextLength,
                    reasoning = d.reasoning,
                    priceFactor = d.priceFactor,
                ),
            )
            used.add(b.key)
        }
        for (e in entries) if (used.add(e.key)) out.add(e)
        return out
    }

    // ------------------------------------------------------------------
    // 解析
    // ------------------------------------------------------------------

    /** Qoder CLI 目录里的场景分组，**按此顺序**消费（`chat` 优先，与参考实现一致）。 */
    private val SCENES = listOf("chat", "byok_enterprise", "byok_teams")

    /**
     * 容错解析上游目录 JSON。认三种形状：
     *  1. `{"chat":[{...}], "byok_teams":[{...}]}`（CLI 缓存格式）
     *  2. `[{...}, {...}]`（扁平数组）
     *  3. `{"data":[...]}` / 任意嵌套（OpenAI 风格或未知包装）
     *
     * 识别"长得像模型"的对象：有 `key`/`model_key`，且带任一模型元字段。
     * 解析不出任何条目时返回空列表（调用方应保留原清单，绝不能清空）。
     */
    fun parseCatalogJson(raw: String): List<QModel> {
        val root = try {
            val t = raw.trim()
            when {
                t.startsWith("[") -> JSONArray(t)
                t.startsWith("{") -> JSONObject(t)
                else -> return emptyList()
            }
        } catch (_: Exception) {
            return emptyList()
        }

        val out = ArrayList<QModel>()
        val seen = HashSet<String>()

        // 1) 分组格式：只消费已知场景，且按固定顺序
        if (root is JSONObject) {
            var grouped = false
            for (scene in SCENES) {
                val arr = root.optJSONArray(scene) ?: continue
                grouped = true
                for (i in 0 until arr.length()) {
                    val o = arr.optJSONObject(i) ?: continue
                    val m = toModel(o) ?: continue
                    if (seen.add(m.key)) out.add(m)
                }
            }
            if (grouped && out.isNotEmpty()) return out
        }

        // 2) 通用递归扫描
        collect(root, out, seen)
        return out
    }

    private fun collect(node: Any?, out: MutableList<QModel>, seen: MutableSet<String>) {
        when (node) {
            is JSONObject -> {
                if (looksLikeModel(node)) {
                    toModel(node)?.let { if (seen.add(it.key)) out.add(it) }
                } else {
                    for (k in node.keys()) collect(node.opt(k), out, seen)
                }
            }
            is JSONArray -> for (i in 0 until node.length()) collect(node.opt(i), out, seen)
        }
    }

    private fun looksLikeModel(o: JSONObject): Boolean {
        val hasKey = o.has("key") || o.has("model_key")
        val hasMeta = o.has("price_factor") || o.has("display_name") || o.has("max_input_tokens") ||
            o.has("context_config") || o.has("is_reasoning") || o.has("format")
        return hasKey && hasMeta
    }

    private fun toModel(o: JSONObject): QModel? {
        val key = o.optString("key", "").ifBlank { o.optString("model_key", "") }.trim()
        if (key.isEmpty()) return null

        // enable：显式 false 或数字 0 才排除（缺省视为启用，与参考实现一致）
        val en = o.opt("enable")
        if (en is Boolean && !en) return null
        if (en is Number && en.toInt() == 0) return null

        val name = o.optString("display_name", "").ifBlank { o.optString("name", "") }.ifBlank { key }

        var ctx = o.optInt("max_input_tokens", 0)
        if (ctx <= 0) ctx = maxContextFrom(o.optJSONObject("context_config"))
        if (ctx <= 0) ctx = 180_000

        val pf = o.optDouble("price_factor", 1.0)
        return QModel(
            key = key,
            name = name,
            contextLength = ctx,
            reasoning = o.optBoolean("is_reasoning", false),
            priceFactor = if (pf.isNaN() || pf.isInfinite()) 1.0 else pf,
        )
    }

    /** `{"default":{"token_count":N},"1m":{"token_count":M}}` → 取最大。 */
    private fun maxContextFrom(cc: JSONObject?): Int {
        if (cc == null) return 0
        var max = 0
        for (k in cc.keys()) {
            val v = cc.optJSONObject(k) ?: continue
            val n = v.optInt("token_count", 0)
            if (n > max) max = n
        }
        return max
    }

    // ------------------------------------------------------------------
    // 持久化
    // ------------------------------------------------------------------

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREF, Context.MODE_PRIVATE)

    /** 启动时装载（无缓存时保持内置清单）。 */
    fun load(ctx: Context) {
        val raw = prefs(ctx).getString(KEY_CATALOG, null) ?: return
        val parsed = try { parseCatalogJson(raw) } catch (_: Exception) { emptyList() }
        if (parsed.isEmpty()) return
        fetchedAt = prefs(ctx).getLong(KEY_FETCHED_AT, 0L)
        setDynamic(parsed)
    }

    /** 保存原始目录 JSON（存原文而不是解析结果，方便以后改解析规则时重解）。 */
    fun save(ctx: Context, rawCatalogJson: String, at: Long = System.currentTimeMillis()) {
        prefs(ctx).edit().putString(KEY_CATALOG, rawCatalogJson).putLong(KEY_FETCHED_AT, at).apply()
        fetchedAt = at
    }

    /** 清掉动态目录，退回内置清单。 */
    fun clear(ctx: Context) {
        prefs(ctx).edit().remove(KEY_CATALOG).remove(KEY_FETCHED_AT).apply()
        dynamic = null
        fetchedAt = 0L
    }

    // ------------------------------------------------------------------
    // 解析客户端请求的模型名
    // ------------------------------------------------------------------

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
     *  4. 都不中 → 默认 `auto`
     *  5. 带 `[1m]` 且目标模型上下文 < 900k → 升级到 `ultimate`
     */
    fun resolve(rawModel: String, maxInputTokens: Int = 0): Resolved {
        val list = current()
        val trimmed = rawModel.trim()
        val oneM = trimmed.endsWith(ONE_M_SUFFIX, ignoreCase = true)
        val bare = if (oneM) trimmed.dropLast(ONE_M_SUFFIX.length).trim() else trimmed

        var found = if (bare.isEmpty()) null else findByIdentifier(list, bare)
        if (found == null) found = list.firstOrNull { it.key == DEFAULT_KEY }
        var model = found ?: list.first()

        if (oneM && maxInputTokens < 900_000 && model.contextLength < 900_000) {
            model = list.firstOrNull { it.key == ONE_M_KEY } ?: model
        }
        return Resolved(model, oneM)
    }

    /** 先按 key、再按展示名匹配（都大小写不敏感）。 */
    private fun findByIdentifier(list: List<QModel>, identifier: String): QModel? =
        list.firstOrNull { it.key.equals(identifier, ignoreCase = true) }
            ?: list.firstOrNull { it.name.equals(identifier, ignoreCase = true) }

    /** 按 key 找（供内部使用，不做别名解析）。 */
    fun byKey(key: String): QModel? = current().firstOrNull { it.key == key }

    // ------------------------------------------------------------------
    // /v1/models 输出 & UI 列表
    // ------------------------------------------------------------------

    /**
     * OpenAI `/v1/models` 的 `data` 数组。
     *
     * 同时带 `name`/`context_length`/`price_factor` 供本 App 的模型页复用
     * （[modelCard] 读这几个字段），标准 OpenAI 字段（`id`/`object`/`created`/`owned_by`）
     * 保持齐全，免得严格客户端因为缺字段解析失败。
     */
    fun openAiModels(): JSONArray {
        val arr = JSONArray()
        val created = System.currentTimeMillis() / 1000
        for (m in current()) {
            arr.put(
                JSONObject()
                    .put("id", m.key)
                    .put("object", "model")
                    .put("created", created)
                    .put("owned_by", "qoder")
                    .put("name", m.name)
                    .put("context_length", m.contextLength)
                    .put("price_factor", m.priceFactor)
                    .put("is_free", m.free),
            )
        }
        return arr
    }

    /** 平台总览用：免费模型数 / 总数。 */
    fun freeCount(): Int = current().count { it.free }

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
        // ---- 内置清单 ----
        check(BUILTIN.size == 16) { "自检失败：Qoder 内置模型目录应为 16 条，实际 ${BUILTIN.size}" }
        check(BUILTIN.map { it.key }.toSet().size == 16) { "自检失败：内置模型 key 有重复" }
        check(BUILTIN.first().key == DEFAULT_KEY) { "自检失败：默认模型应排在最前" }

        // ---- 解析（用内置清单，不依赖是否已加载动态目录）----
        setDynamic(null)
        check(resolve("qmodel").key == "qmodel") { "自检失败：key 精确匹配失败" }
        check(resolve("Ultimate").key == "ultimate") { "自检失败：展示名匹配失败" }
        check(resolve("GLM-5.3").key == "gmodel") { "自检失败：展示名（含点号）匹配失败" }
        check(resolve("nonexistent").key == DEFAULT_KEY) { "自检失败：未知模型应回落默认" }
        check(resolve("").key == DEFAULT_KEY) { "自检失败：空模型应回落默认" }

        val lm = resolve("lite[1m]", maxInputTokens = 200_000)
        check(lm.oneMSuffix) { "自检失败：[1m] 标记未识别" }
        check(lm.key == ONE_M_KEY) { "自检失败：上下文不足 900k 时应升级到 $ONE_M_KEY，得到 ${lm.key}" }
        val lm2 = resolve("qmodel[1m]", maxInputTokens = 1_000_000)
        check(lm2.key == "qmodel") { "自检失败：已够 1M 时不该升级，得到 ${lm2.key}" }
        check(lm.publicId.endsWith(ONE_M_SUFFIX)) { "自检失败：publicId 应保留 [1m] 后缀" }

        // ---- 倍率展示 ----
        check(QModel("k", "n", 1, false, 0.0).priceLabel == "0") { "自检失败：0 倍率应显示 0" }
        check(QModel("k", "n", 1, false, 1.0).priceLabel == "1") { "自检失败：1.0 倍率应显示 1" }
        check(QModel("k", "n", 1, false, 2.5).priceLabel == "2.5") { "自检失败：2.5 倍率应显示 2.5" }
        check(QModel("k", "n", 1, false, 0.0).free) { "自检失败：0 倍率应算免费" }
        check(!QModel("k", "n", 1, false, 0.5).free) { "自检失败：0.5 倍率不该算免费" }

        // ---- 上游目录解析：CLI 分组格式 ----
        val cli = """
            {"chat":[
              {"key":"auto","display_name":"Auto","price_factor":0.0,"max_input_tokens":180000},
              {"model_key":"free-model","name":"Free Model","price_factor":0.0,
               "context_config":{"default":{"token_count":256000}}},
              {"key":"paid","display_name":"Paid","price_factor":3.0,"is_reasoning":true,
               "max_input_tokens":1000000}
            ],"byok_teams":[{"key":"team-only","display_name":"Team","price_factor":1.0}]}
        """.trimIndent()
        val parsed = parseCatalogJson(cli)
        check(parsed.size == 4) { "自检失败：分组目录应解析出 4 条（含 byok_teams），实际 ${parsed.size}" }
        check(parsed[0].key == "auto" && parsed[0].priceFactor == 0.0) { "自检失败：0 倍率未解析出来" }
        check(parsed[1].key == "free-model" && parsed[1].contextLength == 256_000) {
            "自检失败：model_key 别名 / context_config 回退未生效"
        }
        check(parsed[2].reasoning && parsed[2].priceFactor == 3.0) { "自检失败：推理位/倍率未解析" }

        // 合并语义：内置顺序不变、同 key 覆盖、新 key 追加
        val merged = merge(parsed)
        // 解析出 4 条：auto（内置已有，覆盖）+ free-model/paid/team-only（新增 3）→ 16 + 3
        check(merged.size == 16 + 3) { "自检失败：合并后应为内置16 + 新3，实际 ${merged.size}" }
        check(merged.first().key == DEFAULT_KEY) { "自检失败：合并后默认模型必须仍在最前" }
        check(merged.first { it.key == "auto" }.priceFactor == 0.0) {
            "自检失败：同 key 应被上游倍率覆盖（auto 0.0）"
        }
        check(merged.last().key == "team-only") { "自检失败：新 key 应追加到末尾" }

        // 应用动态目录后：新模型可解析、倍率生效、免费数正确
        setDynamic(parsed)
        check(current().size == 19) { "自检失败：动态目录条数不对，实际 ${current().size}" }
        check(resolve("Free Model").key == "free-model") { "自检失败：新模型按展示名解析失败" }
        check(byKey("free-model")?.free == true) { "自检失败：新免费模型未标记免费" }
        check(freeCount() == 2) { "自检失败：免费模型数应为 2，实际 ${freeCount()}" }

        // 空/垃圾输入绝不能把目录清空
        check(parseCatalogJson("").isEmpty()) { "自检失败：空串应解析为空" }
        check(parseCatalogJson("not json").isEmpty()) { "自检失败：非 JSON 应解析为空" }
        check(parseCatalogJson("""{"foo":"bar"}""").isEmpty()) { "自检失败：无模型文档应解析为空" }
        setDynamic(parseCatalogJson("not json"))
        check(current().size == 16) { "自检失败：解析失败必须退回内置清单，不能清空" }

        // ---- /v1/models 形状 ----
        setDynamic(null)
        val models = openAiModels()
        check(models.length() == 16) { "自检失败：/v1/models 条数不对" }
        val first = models.getJSONObject(0)
        check(first.getString("object") == "model") { "自检失败：object 字段必须是 model" }
        check(first.getString("id") == DEFAULT_KEY) { "自检失败：首个模型 id 不对" }
        check(first.optInt("context_length", 0) > 0) { "自检失败：context_length 缺失（模型页要用）" }
        check(first.has("price_factor")) { "自检失败：price_factor 缺失（模型页要显示倍率）" }

        // ---- 会话 id：同模型稳定、不同模型不同 ----
        val s1 = sessionFor("auto")
        check(s1 == sessionFor("auto")) { "自检失败：同模型 session 必须稳定" }
        check(s1 != sessionFor("ultimate")) { "自检失败：不同模型 session 必须不同" }
        check(s1.startsWith("q2a-") && s1.length == 4 + 36) { "自检失败：session 格式不对: $s1" }
    }
}
