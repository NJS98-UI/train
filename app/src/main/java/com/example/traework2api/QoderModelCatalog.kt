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
        /**
         * 官方标注的**倍率**（`price_factor`）。`0.0` = 免费，`null` = **不知道**。
         *
         * 为什么允许 null：官方 UI 上「快速 / 经济」这类档位没显示倍率，
         * 而瞎填 1.0 会让人以为免费模型也扣费 —— 显示「倍率未知」才是诚实的。
         */
        val priceFactor: Double? = null,
        /** 旧展示名（客户端可能还在用旧名字请求，得能认）。 */
        val aliases: List<String> = emptyList(),
        /**
         * key 是**推断**出来的、未经官方目录验证。
         * 官方新模型的 key 没有公开映射表，只能按命名规律猜；猜错上游会报错。
         * 用官方目录（[QoderModelCache] 导入 / 上游同步）覆盖后该标记自然消失。
         */
        val unverified: Boolean = false,
    ) {
        /** 免费模型（官方倍率 0）。 */
        val free: Boolean get() = priceFactor != null && priceFactor <= 0.0

        /** 倍率展示文本：`0` / `1` / `2.5` / `?`（未知，去掉无意义的小数尾巴）。 */
        val priceLabel: String
            get() {
                val v = priceFactor ?: return "?"
                return if (v == v.toLong().toDouble()) v.toLong().toString() else v.toString()
            }
    }

    /**
     * 16 个**内置**模型。**顺序即 UI 顺序**：`auto` 在最前（默认），贵的推理模型靠后。
     * 与参考实现 `main.go` 的 `builtinCatalog` 逐条一致（倍率也照抄 1.0 ——
     * 参考实现自己也没有真实倍率，真实倍率靠 [setDynamic] 从上游目录覆盖）。
     */
    val BUILTIN: List<QModel> = listOf(
        // ---- key 全部来自官方目录快照（catalog-v6 解密导出），不再靠命名规律猜 ----
        // 首项必须是 DEFAULT_KEY（自检 check(BUILTIN.first().key == DEFAULT_KEY)）
        QModel("qwen3-coder-plus", "Qwen3 Coder Plus", 180_000, false, 1.0),
        QModel("auto", "Auto（容易排队）", 180_000, false, 0.5),
        QModel("ultimate", "极致", 1_000_000, true, 0.8, aliases = listOf("Ultimate")),
        QModel("performance", "快速", 1_000_000, false, null, aliases = listOf("Performance")),
        QModel("efficient", "经济", 180_000, false, null, aliases = listOf("Efficient")),
        QModel("smodel", "Sonus", 180_000, true, 8.0, aliases = listOf("smodel")),
        QModel("cmodel", "Cantus", 200_000, true, 4.0),
        QModel("qmodel_38max", "Qwen3.8-Max 2", 1_000_000, true, 0.5, aliases = listOf("Qwen3.8-Max")),
        QModel("qfmodel", "Qwen3.8-Flash", 1_000_000, true, 0.0, aliases = listOf("Qwen3.8-Flash-1M")),
        QModel("kmodel_latest", "Kimi-K3.1-Pro", 1_000_000, false, 1.0, aliases = listOf("Kimi-K3")),
        QModel("kmodel", "Kimi-K2.8-Preview", 256_000, true, 0.8, aliases = listOf("Kimi-K2.7-Code")),
        QModel("gmodel", "GLM-5.3", 1_000_000, true, 0.8),
        QModel("gfmodel", "GLM-5.3-Flash", 1_000_000, true, 0.1),
        QModel("dmodel", "DeepSeek-V4-Pro", 1_000_000, true, 0.5),
        QModel("dfmodel", "DeepSeek-Flash", 1_000_000, true, 0.0, aliases = listOf("DeepSeek-V4-Flash")),
        QModel("mmodel", "MiniMax-M3.1-Flash", 1_000_000, false, 0.1, aliases = listOf("MiniMax-M3")),

        // ---- 官方 UI 上已看不到、但 key 仍然有效的老模型（留着兼容旧客户端）----
        QModel("lite", "Lite", 180_000, false),
        QModel("qmodel_latest", "Qwen3.7-Max", 1_000_000, true, 0.1),
        QModel("qmodel", "Qwen3.7-Plus", 1_000_000, true, 0.04),
        QModel("q37fmodel", "Qwen3.7-Flash", 1_000_000, true, 0.1),
        QModel("gm51model", "GLM-5.2", 1_000_000, true, 0.6),
    )

    /** 兼容旧引用名。 */
    val ALL: List<QModel> get() = current()

    /**
     * 兜底模型：客户端没指定 / 指定了不认识的名字。
     *
     * ⚠️ 曾经是 `auto`，实测（2026-10-26）它**默认排队**：
     * 上游返回 `403 / 10605 / RESOURCE_LIMIT / isQueued:true / retryAfterSeconds:30`。
     * 客户端不指定模型是很常见的情形（很多客户端只填 base_url），一律撞上排队就成了
     * "一次都用不了"。改成实测直接可用的 `qwen3-coder-plus`。
     */
    const val DEFAULT_KEY = "qwen3-coder-plus"

    /**
     * 排队时的降级候选（按顺序试，全部实测可直接用、不排队）。
     *
     * 只在**确认是排队/资源受限**时才降级 —— 别的错误（鉴权、参数）降级没有意义。
     */
    val QUEUE_FALLBACK_KEYS = listOf("qwen3-coder-plus", "claude-sonnet-4", "deepseek-v3")

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
                    // ⚠️ 名称与倍率**以内置清单为准**（那是官方 APP 里显示的值）。
                    // 上游目录返回的是英文名 + 基础价（Ultimate / Performance / Efficient，
                    // 倍率也不含促销与套餐调整），拿它覆盖会变成"和官方 APP 对不上" ——
                    // 这正是用户反复报的那个问题。
                    name = b.name,
                    // 上下文只在"内置没写"或"上游更大"时更新：这是纯增益，不会显示成错的值。
                    contextLength = when {
                        b.contextLength <= 0 -> d.contextLength
                        d.contextLength > b.contextLength -> d.contextLength
                        else -> b.contextLength
                    },
                    // 推理位同理：内置是官方快照值，上游缺失(false)时不能把 true 冲掉
                    reasoning = b.reasoning || d.reasoning,
                    priceFactor = b.priceFactor ?: d.priceFactor,
                    // 上游出现的名字当别名留着，客户端按旧名/英文名请求也能命中
                    aliases = (b.aliases + d.name).distinct().filter { it.isNotBlank() && it != b.name },
                    unverified = false,
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

        // 目录里**没写** price_factor = 未知；写了 0 才是免费。默认 1.0 会把免费模型洗成收费。
        val pf = if (o.has("price_factor") && !o.isNull("price_factor")) {
            o.optDouble("price_factor", 1.0).let { if (it.isNaN() || it.isInfinite()) null else it }
        } else {
            null
        }
        return QModel(
            key = key,
            name = name,
            contextLength = ctx,
            reasoning = o.optBoolean("is_reasoning", false),
            priceFactor = pf,
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
            ?: list.firstOrNull { m -> m.aliases.any { it.equals(identifier, ignoreCase = true) } }

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
            // `id` 用**官方显示名**，不用内部 key。
            // 为什么：客户端模型下拉框只显示 id，给 `qfmodel` / `smodel` 这种缩写，
            // 用户根本认不出是哪个模型（"名字变了，不知道谁对应谁"）。
            // 转发不受影响：resolve() 先按 key 再按展示名再按别名匹配，三种都能命中。
            val publicId = m.name.ifBlank { m.key }
            val o = JSONObject()
                .put("id", publicId)
                .put("object", "model")
                .put("created", created)
                .put("owned_by", "qoder")
                .put("name", m.name)
                .put("key", m.key)
                .put("context_length", m.contextLength)
                .put("is_free", m.free)
            // 倍率未知时**不写字段**：让展示层显示「倍率未知」，而不是编一个 ×1
            m.priceFactor?.let { o.put("price_factor", it) }
            if (m.unverified) o.put("unverified", true)
            // 别名里带上内部 key，兼容"直接填 key"的老客户端
            val aliases = (listOf(m.key) + m.aliases).distinct().filter { it != publicId }
            if (aliases.isNotEmpty()) o.put("aliases", JSONArray(aliases))
            arr.put(o)
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
        check(BUILTIN.size == 20) { "自检失败：Qoder 内置模型目录应为 20 条，实际 ${BUILTIN.size}" }
        check(BUILTIN.map { it.key }.toSet().size == 20) { "自检失败：内置模型 key 有重复" }
        // key 全部来自官方目录快照，不该再有"推断"的
        check(BUILTIN.none { it.unverified }) { "自检失败：内置清单不该再有 unverified 的 key" }
        // 官方 APP 清单里两个 0 倍率模型必须在
        check(BUILTIN.count { it.free } == 2) { "自检失败：内置清单应有 2 个免费模型（DeepSeek-Flash / Qwen3.8-Flash）" }
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
        // 未知倍率：显示 ?，且**不能**算免费也不能算 1.0
        check(QModel("k", "n", 1, false, null).priceLabel == "?") { "自检失败：未知倍率应显示 ?" }
        check(!QModel("k", "n", 1, false, null).free) { "自检失败：未知倍率不该算免费" }

        // 别名：官方改名后，旧名字必须还能解析到同一个 key
        check(resolve("Ultimate").key == "ultimate") { "自检失败：别名 Ultimate 解析失败" }
        check(resolve("Kimi-K3").key == "kmodel_latest") { "自检失败：旧名 Kimi-K3 应解析到 kmodel_latest" }
        check(resolve("DeepSeek-V4-Flash").key == "dfmodel") { "自检失败：旧名 DeepSeek-V4-Flash 应解析到 dfmodel" }
        check(resolve("Qwen3.8-Max").key == "qmodel_38max") { "自检失败：旧名 Qwen3.8-Max 应解析到 qmodel_38max" }
        // 官方新名字（Qoder.txt 里抄的）必须能解析
        check(resolve("Qwen3.8-Flash").key == "qfmodel") { "自检失败：Qwen3.8-Flash 应解析到 qfmodel" }
        check(resolve("DeepSeek-Flash").key == "dfmodel") { "自检失败：DeepSeek-Flash 解析失败" }
        check(resolve("MiniMax-M3.1-Flash").key == "mmodel") { "自检失败：MiniMax-M3.1-Flash 解析失败" }
        check(resolve("Kimi-K3.1-Pro").key == "kmodel_latest") { "自检失败：Kimi-K3.1-Pro 解析失败" }
        check(resolve("Sonus").key == "smodel") { "自检失败：Sonus 应解析到 smodel" }
        check(resolve("Cantus").key == "cmodel") { "自检失败：Cantus 应解析到 cmodel" }
        check(resolve("GLM-5.3-Flash").key == "gfmodel") { "自检失败：GLM-5.3-Flash 应解析到 gfmodel" }
        // 官方 key 已确认：不该再有 unverified 标记
        check(byKey("smodel")?.unverified == false) { "自检失败：Sonus 的 key 已确认，不该标 unverified" }
        check(byKey("gmodel")?.unverified == false) { "自检失败：GLM-5.3 的 key 是确定的，不该标记 unverified" }

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
        // 解析出 4 条：auto（内置已有，覆盖）+ free-model/paid/team-only（新增 3）→ 20 + 3
        check(merged.size == 20 + 3) { "自检失败：合并后应为内置20 + 新3，实际 ${merged.size}" }
        check(merged.first().key == DEFAULT_KEY) { "自检失败：合并后默认模型必须仍在最前" }
        // 同 key：名称/倍率**保留内置值**（官方 APP 显示值），不被上游英文名/基础价覆盖
        check(merged.first { it.key == "auto" }.priceFactor == 0.5) {
            "自检失败：同 key 的倍率应保留内置值（auto 0.5），实际 ${merged.first { it.key == "auto" }.priceFactor}"
        }
        check(merged.first { it.key == "auto" }.name == "Auto") { "自检失败：内置名称被覆盖了" }
        // 上游英文名进别名：内置叫「极致」，客户端按 Ultimate 请求也要命中
        check(resolve("Ultimate").key == "ultimate") { "自检失败：英文名 Ultimate 应能解析到 ultimate" }
        check(merged.last().key == "team-only") { "自检失败：新 key 应追加到末尾" }

        // 应用动态目录后：新模型可解析、倍率生效、免费数正确
        setDynamic(parsed)
        check(current().size == 23) { "自检失败：动态目录条数不对，实际 ${current().size}" }
        check(resolve("Free Model").key == "free-model") { "自检失败：新模型按展示名解析失败" }
        check(byKey("free-model")?.free == true) { "自检失败：新免费模型未标记免费" }
        // 内置免费的 qfmodel / dfmodel（2）+ 目录新增的 free-model（1）= 3；
        // auto 上游说 0.0，但内置（官方 APP 显示）是 0.5，按内置算 —— 所以不额外加 1
        check(freeCount() == 3) { "自检失败：免费模型数应为 3（内置2 + 目录1），实际 ${freeCount()}" }

        // 空/垃圾输入绝不能把目录清空
        check(parseCatalogJson("").isEmpty()) { "自检失败：空串应解析为空" }
        check(parseCatalogJson("not json").isEmpty()) { "自检失败：非 JSON 应解析为空" }
        check(parseCatalogJson("""{"foo":"bar"}""").isEmpty()) { "自检失败：无模型文档应解析为空" }
        setDynamic(parseCatalogJson("not json"))
        check(current().size == 20) { "自检失败：解析失败必须退回内置清单，不能清空" }

        // ---- /v1/models 形状 ----
        setDynamic(null)
        val models = openAiModels()
        check(models.length() == 20) { "自检失败：/v1/models 条数不对" }
        val first = models.getJSONObject(0)
        check(first.getString("object") == "model") { "自检失败：object 字段必须是 model" }
        // id 现在是**官方显示名**（客户端下拉框只显示 id，给内部 key 用户认不出）
        check(first.getString("id") == "Auto") { "自检失败：首个模型 id 应为官方名 Auto，实际 ${first.optString("id")}" }
        check(first.getString("key") == DEFAULT_KEY) { "自检失败：key 字段应保留内部 key" }
        // 官方名必须能解析回 key（客户端拿 id 直接请求）
        check(resolve(first.getString("id")).key == DEFAULT_KEY) { "自检失败：按官方名解析不回默认 key" }
        check(resolve("极致").key == "ultimate") { "自检失败：按官方中文名解析失败" }
        check(resolve("Qwen3.8-Flash").key == "qfmodel") { "自检失败：按官方英文名解析失败" }
        check(resolve("qfmodel").key == "qfmodel") { "自检失败：按内部 key 仍须能解析" }
        check(first.optInt("context_length", 0) > 0) { "自检失败：context_length 缺失（模型页要用）" }
        check(first.has("price_factor")) { "自检失败：price_factor 缺失（模型页要显示倍率）" }

        // ---- 会话 id：同模型稳定、不同模型不同 ----
        val s1 = sessionFor("auto")
        check(s1 == sessionFor("auto")) { "自检失败：同模型 session 必须稳定" }
        check(s1 != sessionFor("ultimate")) { "自检失败：不同模型 session 必须不同" }
        check(s1.startsWith("q2a-") && s1.length == 4 + 36) { "自检失败：session 格式不对: $s1" }
    }
}
