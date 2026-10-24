package com.example.traework2api

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * 本地网关的**转发后端选择**：Trae 还是 Qoder，或者按模型自动路由。
 *
 * 为什么需要它：本 App 原本只有 Trae 一条链路，模型表、凭证、换号全是 Trae 的。
 * 现在 Qoder 账号也能当聊天后端（COSY 签名链路，见 [QoderUpstream]），
 * 两条链路的上游协议、模型表、区域 host 完全不同，必须显式区分，
 * 否则会出现"拿 Trae 的模型名去打 Qoder 网关"这种必然 4xx 的组合。
 *
 * 三档：
 *  - `trae`  强制 Trae
 *  - `qoder` 强制 Qoder
 *  - `auto`  **按模型路由**（默认）
 *
 * ## `auto` 为什么是"按模型"而不是"挑一个"
 *
 * 早期实现是"有 Trae 账号就走 Trae，否则走 Qoder"——结果同时登了两个平台的用户，
 * 本地 `/v1/models` 只吐出 Trae 的表，**刚导入的 Qoder 模型一个都看不到**，
 * 而用户根本没意识到"后端"这个概念被静默决定了。
 *
 * 现在两个平台都有账号时：模型表是**并集**，客户端请求哪个模型就发给对应上游。
 * 模型名同时存在于两边时按 Trae 优先（老用户行为不变）。
 */
object GatewayBackend {

    private const val PREF = "traework2api_prefs"
    private const val KEY = "gateway_backend"

    const val TRAE = "trae"
    const val QODER = "qoder"
    const val AUTO = "auto"

    /** 用户显式选择（可能是 [AUTO]）。 */
    fun selection(ctx: Context): String {
        val v = ctx.getSharedPreferences(PREF, Context.MODE_PRIVATE).getString(KEY, null)
        return if (v == TRAE || v == QODER) v else AUTO
    }

    fun set(ctx: Context, backend: String) {
        val v = if (backend == QODER) QODER else if (backend == TRAE) TRAE else AUTO
        ctx.getSharedPreferences(PREF, Context.MODE_PRIVATE).edit().putString(KEY, v).apply()
    }

    // ------------------------------------------------------------------
    // 账号可用性
    // ------------------------------------------------------------------

    fun hasTraeAccount(ctx: Context): Boolean = TraeAuthStore.load(ctx) != null

    fun hasQoderAccount(ctx: Context): Boolean = QoderAuthStore.load(ctx) != null

    /** 该模型名是否属于 Qoder 目录（key / 展示名 / 别名任一命中）。 */
    fun isQoderModel(model: String): Boolean {
        val m = model.trim()
        if (m.isEmpty()) return false
        return QoderModelCatalog.current().any {
            it.key.equals(m, ignoreCase = true) ||
                it.name.equals(m, ignoreCase = true) ||
                it.aliases.any { a -> a.equals(m, ignoreCase = true) }
        }
    }

    // ------------------------------------------------------------------
    // 路由
    // ------------------------------------------------------------------

    /**
     * 这个模型该走哪个上游。
     *
     * @param model 客户端请求里的 `model` 字段（空串 = 没指定，按 Trae 兜底）
     */
    fun routeFor(ctx: Context, model: String): String = when (selection(ctx)) {
        QODER -> QODER
        TRAE -> TRAE
        else -> {
            val q = hasQoderAccount(ctx)
            val t = hasTraeAccount(ctx)
            when {
                q && !t -> QODER
                !q && t -> TRAE
                q && t -> if (isQoderModel(model)) QODER else TRAE
                else -> TRAE
            }
        }
    }

    /** 展示用：当前"主"后端（`auto` 且两个都有时按 Trae 展示）。 */
    fun resolved(ctx: Context): String = when (selection(ctx)) {
        QODER -> QODER
        TRAE -> TRAE
        else -> if (hasQoderAccount(ctx) && !hasTraeAccount(ctx)) QODER else TRAE
    }

    fun isQoder(ctx: Context): Boolean = resolved(ctx) == QODER

    /** `auto` 且两个平台都有账号 = 模型表是并集、按模型路由。 */
    fun isMixed(ctx: Context): Boolean =
        selection(ctx) == AUTO && hasTraeAccount(ctx) && hasQoderAccount(ctx)

    /** 后端选择的**人话**描述（UI 与日志共用）。 */
    fun describe(ctx: Context): String = when {
        selection(ctx) == QODER -> "Qoder（强制）"
        selection(ctx) == TRAE -> "Trae（强制）"
        isMixed(ctx) -> "自动 · 按模型路由（Trae + Qoder 都在）"
        hasQoderAccount(ctx) -> "自动 · Qoder"
        else -> "自动 · Trae"
    }

    // ------------------------------------------------------------------
    // 模型表
    // ------------------------------------------------------------------

    /**
     * `/v1/models` 与模型页要用的模型表。
     *
     * `auto` 且两个平台都有账号时返回**并集**（Qoder 独有模型追加在 Trae 表后面），
     * 否则按实际生效的后端返回单表。
     */
    fun modelsFor(ctx: Context): JSONArray = when {
        selection(ctx) == QODER -> QoderModelCatalog.openAiModels()
        selection(ctx) == TRAE -> ModelsRepo.current()
        else -> {
            val q = hasQoderAccount(ctx)
            val t = hasTraeAccount(ctx)
            when {
                q && !t -> QoderModelCatalog.openAiModels()
                !q && t -> ModelsRepo.current()
                q && t -> merge(ModelsRepo.current(), QoderModelCatalog.openAiModels())
                else -> ModelsRepo.current()
            }
        }
    }

    /**
     * 并集：Trae 表在前（保持老用户看到的顺序），Qoder 独有的按 id 去重后追加。
     *
     * 每个条目补 `owned_by` 标注来源（`trae` / `qoder`），模型页据此显示 tag。
     * **必须复制对象**：`ModelsRepo.current()` 返回的是共享缓存，直接改会污染它。
     */
    private fun merge(trae: JSONArray, qoder: JSONArray): JSONArray {
        val out = JSONArray()
        val seen = HashSet<String>()
        for (i in 0 until trae.length()) {
            val o = trae.optJSONObject(i) ?: continue
            val id = o.optString("id", "")
            if (id.isNotBlank() && !seen.add(id)) continue
            out.put(JSONObject(o.toString()).put("owned_by", "trae"))
        }
        for (i in 0 until qoder.length()) {
            val o = qoder.optJSONObject(i) ?: continue
            val id = o.optString("id", "")
            if (id.isNotBlank() && !seen.add(id)) continue
            // 也补来源标注（模型页据此显示 qoder tag），并保持"不改入参"的一致纪律
            out.put(JSONObject(o.toString()).put("owned_by", "qoder"))
        }
        return out
    }

    /** 模型页的"是否已加载"判定。 */
    fun modelsLoaded(ctx: Context): Boolean = when {
        hasQoderAccount(ctx) -> true
        else -> ModelsRepo.isDynamic
    }

    fun label(backend: String): String = when (backend) {
        QODER -> "Qoder"
        TRAE -> "Trae"
        else -> "自动"
    }

    fun selfCheck(ctx: Context) {
        check(label(QODER) == "Qoder" && label(TRAE) == "Trae") { "自检失败：后端标签不对" }

        // Qoder 目录里的模型必须被认成 Qoder 模型（含别名）
        check(isQoderModel("qmodel_38max")) { "自检失败：qmodel_38max 应属于 Qoder 目录" }
        check(isQoderModel("DeepSeek-Flash")) { "自检失败：DeepSeek-Flash 应属于 Qoder 目录" }
        check(isQoderModel("DeepSeek-V4-Flash")) { "自检失败：别名 DeepSeek-V4-Flash 应属于 Qoder 目录" }
        check(!isQoderModel("")) { "自检失败：空模型名不该算 Qoder 模型" }

        // auto 解析：只登 Qoder 时必须是 Qoder，只登 Trae 时必须是 Trae
        val hasTrae = hasTraeAccount(ctx)
        val hasQoder = hasQoderAccount(ctx)
        val expected = if (!hasTrae && hasQoder) QODER else TRAE
        if (selection(ctx) == AUTO) {
            check(resolved(ctx) == expected) {
                "自检失败：auto 应解析为 $expected，实际 ${resolved(ctx)}"
            }
            // 只登 Qoder 时，任何模型都必须路由到 Qoder（否则聊天必失败）
            if (hasQoder && !hasTrae) {
                check(routeFor(ctx, "whatever") == QODER) { "自检失败：只登 Qoder 时应全部路由到 Qoder" }
            }
            // 只登 Trae 时，Qoder 模型名也只能走 Trae（没有 Qoder 账号）
            if (hasTrae && !hasQoder) {
                check(routeFor(ctx, "qmodel_38max") == TRAE) {
                    "自检失败：没有 Qoder 账号时不该路由到 Qoder"
                }
            }
        }
        // 显式选择必须压倒一切
        val saved = selection(ctx)
        set(ctx, QODER)
        check(routeFor(ctx, "anything") == QODER) { "自检失败：强制 Qoder 时应无视模型名" }
        set(ctx, TRAE)
        check(routeFor(ctx, "qmodel_38max") == TRAE) { "自检失败：强制 Trae 时应无视模型名" }
        set(ctx, saved)
    }
}
