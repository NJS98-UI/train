package com.example.traework2api

import android.content.Context
import org.json.JSONArray

/**
 * 本地网关的**转发后端选择**：Trae 还是 Qoder。
 *
 * 为什么需要它：本 App 原本只有 Trae 一条链路，模型表、凭证、换号全是 Trae 的。
 * 现在 Qoder 账号也能当聊天后端（COSY 签名链路，见 [QoderUpstream]），
 * 两条链路的上游协议、模型表、区域 host 完全不同，必须显式区分，
 * 否则会出现"拿 Trae 的模型名去打 Qoder 网关"这种必然 4xx 的组合。
 *
 * 三档：
 *  - `trae`  强制 Trae（老行为，默认兜底）
 *  - `qoder` 强制 Qoder
 *  - `auto`  有 Trae 账号就走 Trae，否则有 Qoder 账号就走 Qoder，都没有回 Trae
 *
 * `auto` 是默认值：这样"只登了 Qoder 账号"的用户开箱就能用聊天转发，
 * 而"只登了 Trae"的老用户行为完全不变 —— 不需要任何人先去点一下开关。
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

    /** 实际生效的后端（把 [AUTO] 解析成具体值）。 */
    fun resolved(ctx: Context): String = when (selection(ctx)) {
        QODER -> QODER
        TRAE -> TRAE
        else -> {
            val hasTrae = TraeAuthStore.load(ctx) != null
            val hasQoder = QoderAuthStore.load(ctx) != null
            if (!hasTrae && hasQoder) QODER else TRAE
        }
    }

    fun isQoder(ctx: Context): Boolean = resolved(ctx) == QODER

    /** `/v1/models` 与模型页要用的模型表（按后端切换）。 */
    fun modelsFor(ctx: Context): JSONArray =
        if (isQoder(ctx)) QoderModelCatalog.openAiModels() else ModelsRepo.current()

    /** 模型页的"是否已加载"判定：Qoder 目录是内置的，永远算已加载。 */
    fun modelsLoaded(ctx: Context): Boolean =
        if (isQoder(ctx)) true else ModelsRepo.isDynamic

    fun label(backend: String): String = when (backend) {
        QODER -> "Qoder"
        TRAE -> "Trae"
        else -> "自动"
    }

    fun selfCheck(ctx: Context) {
        check(label(QODER) == "Qoder" && label(TRAE) == "Trae") { "自检失败：后端标签不对" }
        // auto 解析：只登 Qoder 时必须是 Qoder，只登 Trae 时必须是 Trae
        val hasTrae = TraeAuthStore.load(ctx) != null
        val hasQoder = QoderAuthStore.load(ctx) != null
        val expected = if (!hasTrae && hasQoder) QODER else TRAE
        if (selection(ctx) == AUTO) {
            check(resolved(ctx) == expected) {
                "自检失败：auto 应解析为 $expected，实际 ${resolved(ctx)}"
            }
        }
    }
}
