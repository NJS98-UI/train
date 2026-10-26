package com.example.traework2api

import android.content.Context
import android.util.Log
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject
import java.util.concurrent.TimeUnit

/**
 * Qoder 官方认证 / 积分接口（对齐 Qoder 桌面/CLI 版实际使用的 `qodercli` / `qoderclicn` 协议）。
 *
 * 关键结论（来自多个独立社区实现交叉印证）：
 * - Qoder 对外可用凭据是 **PAT（`pt-`）**，不能直接调 API，要先换短时 **jobToken（`jt-`）**。
 * - `jobToken/exchange`、`jobToken/refresh`、`quota/usage` **都不需要 COSY 签名**。
 *   只有聊天网关 `/algo/...` 要签名 —— 那部分在 [QoderUpstream] / [QoderCosyCrypto]。
 * - 区域不同 host 不同：
 *     openapi 国内 `openapi.qoder.com.cn` / 国际 `openapi.qoder.sh`
 *     infer   国内 `gateway.qoder.com.cn`  / 国际 `api2.qoder.sh`
 *   两套 host **不能混用**：拿国内的 jobToken 打国际 infer 一律 401。
 *
 * token 只进内存与请求头，绝不落日志。
 */
object QoderAuthApi {

    private const val TAG = "QoderAuth"
    const val MODE_CN = "cn"
    const val MODE_GLOBAL = "global"

    private const val EP_EXCHANGE = "/api/v1/jobToken/exchange"
    private const val EP_REFRESH = "/api/v1/jobToken/refresh"
    private const val EP_USERINFO = "/api/v1/userinfo"
    private const val EP_USAGE = "/api/v2/quota/usage"

    // 每日可领 Credits（campaigns）。来自多个独立社区实现交叉印证：
    // 列表 GET /sash/api/v1/me/campaigns?forceRefresh=true；领取 POST .../campaigns/{id}/claim（body {}，幂等）。
    private const val EP_CAMPAIGNS = "/sash/api/v1/me/campaigns"

    /**
     * 活动列表的请求身份档位。**这是「领取不了」的关键开关**。
     *
     * 两组互相矛盾的现场证据，所以做成**运行时升降级**而不是二选一写死：
     *  - 2026-10-24 本机对照：同一 jobToken，`ClientType=10` + 普通头 → 活动正常下发；
     *  - 2026-09-30 qoder2api-hub 用官方桌面端 `main.log` 对照：**缺桌面端身份头时服务端
     *    不报错、直接返回空活动列表**（`showCampaign=false, campaigns=[]`），
     *    带齐 `User-Agent: Qoder` + `Cosy-ClientType: 10` + `Cosy-Machine*` 才下发。
     *
     * 空列表是**静默失败**（HTTP 200），所以只能先按普通身份问一次，拿到空列表且
     * `showCampaign` 不为 true 时再升到桌面端身份问一次；哪一档真的出活动就记住哪一档
     * （按国内/国际分别记），下次直接用，不再白白多打一次上游。
     */
    private const val VARIANT_MINIMAL = "minimal"
    private const val VARIANT_DESKTOP = "desktop"

    private const val PREFS = "traework2api_prefs"
    private const val KEY_VARIANT = "qoder_campaign_variant_"

    // 官方模型目录（带真实 price_factor）。实测：这两个路径在所有 openapi 主机上
    // 未鉴权时都返回 **401**（=路由存在，只是缺 token），而 /api/v2/models、
    // /algo/api/v1/model/list 之类一律 404。两个都试，谁先回出可解析的目录用谁。
    private const val EP_MODELS = "/api/v1/models"
    private const val EP_MODEL_LIST = "/api/v1/model/list"

    /**
     * 最近一次模型目录拉取的失败摘要（给 UI 显示，便于排查"为什么同步不了"）。
     * 形如 `GET /api/v1/models → HTTP 401`。成功时清空。
     */
    @Volatile
    var lastCatalogError: String? = null
        private set

    /**
     * 最近一次模型目录请求**拿到的响应原文**（哪怕解析失败也留着）。
     *
     * 用途：用户说"拉出来的对不上"时，得能看见上游到底回了什么 ——
     * 没有原文就只能瞎猜是接口路径错、鉴权错还是字段名变了。
     */
    @Volatile
    var lastCatalogRaw: String? = null
        private set

    /**
     * 记录一次目录请求的结果（供 [QoderUpstream] 的签名接口复用同一套诊断）。
     *
     * @param raw   响应原文；null = 连响应都没有（传输异常）
     * @param error 失败原因；null = 成功
     */
    fun noteCatalogAttempt(raw: String?, error: String?) {
        if (raw != null) lastCatalogRaw = raw
        if (error != null) lastCatalogError = error
    }

    private const val JSON = "application/json"

    private val client = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .build()

    /** 按 mode 取 OpenAPI host。未知 mode 一律按国内（主链路）。 */
    private fun openApiBase(mode: String): String =
        if (mode == MODE_GLOBAL) "https://openapi.qoder.sh" else "https://openapi.qoder.com.cn"

    /**
     * 按 mode 取**聊天转发** host（COSY `/algo/` 专用）。
     *
     * ⚠️ 与 [openApiBase] 是两套域名，绝不能拿错：聊天网关不在 openapi 上。
     * 参考实现里 `infer` 国内 `gateway.qoder.com.cn`、国际 `api2.qoder.sh`。
     */
    fun inferBase(mode: String): String =
        if (mode == MODE_GLOBAL) "https://api2.qoder.sh" else "https://gateway.qoder.com.cn"

    private fun post(url: String, body: JSONObject): String {
        val req = Request.Builder().url(url)
            .post(body.toString().toRequestBody(JSON.toMediaType()))
            .header("Content-Type", JSON)
            .header("Accept", JSON)
            .header("User-Agent", "traework2api")
            .header("Cosy-Version", "1.0.1")
            .header("Cosy-ClientType", "5")
            .build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    private fun getBearer(url: String, bearer: String): String {
        val req = Request.Builder().url(url).get()
            .header("Authorization", "Bearer $bearer")
            .header("Accept", JSON)
            .header("User-Agent", "traework2api")
            .header("Cosy-Version", "1.0.1")
            .header("Cosy-ClientType", "5")
            .build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    /**
     * 拉**官方模型目录**（含真实 `price_factor`）。拿不到返回 null，调用方保留原清单。
     *
     * 为什么用"能解析出模型"当成功判据而不是 HTTP 200：官方可能回 200 但内容是
     * `{"code":..,"message":..}` 这种壳；那种东西交给 [QoderModelCatalog.parseCatalogJson]
     * 会解析出 0 条，正好用来判定"这次不算成功"，继续试下一个候选。
     *
     * 候选顺序：`/api/v1/models`（GET → POST）→ `/api/v1/model/list`（GET → POST）。
     */
    fun fetchModelCatalog(jt: String, mode: String): String? {
        if (jt.isBlank()) {
            lastCatalogError = "没有可用的 jobToken"
            return null
        }
        lastCatalogError = null
        lastCatalogRaw = null
        val base = openApiBase(mode)
        val fails = ArrayList<String>()
        for (path in listOf(EP_MODELS, EP_MODEL_LIST)) {
            for (method in listOf("GET", "POST")) {
                val text = try {
                    rawRequest(base + path, jt, method, fails)
                } catch (e: Exception) {
                    fails.add("$method $path → ${e.javaClass.simpleName}")
                    Log.w(TAG, "模型目录 $method $path 异常: ${e.javaClass.simpleName}")
                    null
                } ?: continue
                if (lastCatalogRaw == null) lastCatalogRaw = text
                if (QoderModelCatalog.parseCatalogJson(text).isNotEmpty()) {
                    Log.i(TAG, "模型目录已拉取（$method $path，${text.length} 字节）")
                    return text
                }
                fails.add("$method $path → 内容不像目录")
                Log.i(TAG, "模型目录 $method $path 内容不像目录，继续试下一个候选")
            }
        }
        lastCatalogError = fails.joinToString("；").ifBlank { "全部候选都失败" }
        return null
    }

    /** 返回 2xx 的响应体；非 2xx 返回 null（**不抛**，让调用方继续试下一个候选）。 */
    private fun rawRequest(url: String, bearer: String, method: String, fails: MutableList<String>): String? {
        val builder = Request.Builder().url(url)
            .header("Authorization", "Bearer $bearer")
            .header("Accept", JSON)
            .header("User-Agent", "traework2api")
            .header("Cosy-Version", "1.0.1")
            .header("Cosy-ClientType", "5")
        if (method == "POST") {
            builder.post("{}".toRequestBody(JSON.toMediaType()))
        } else {
            builder.get()
        }
        client.newCall(builder.build()).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) {
                val short = url.substringAfter("//")
                Log.i(TAG, "模型目录 $method $short → HTTP ${r.code}")
                fails.add("$method /${url.substringAfterLast("/api/")} → HTTP ${r.code}")
                return null
            }
            return text
        }
    }

    data class JobToken(val token: String, val refreshToken: String, val expiresAtMillis: Long)

    /** PAT 换 jobToken。缺 token 抛异常。 */
    fun exchangeJobToken(pat: String, mode: String = MODE_CN): JobToken {
        val text = post(openApiBase(mode) + EP_EXCHANGE, JSONObject().put("personal_token", pat))
        val o = JSONObject(text)
        val token = o.optString("token", "").takeIf { it.isNotBlank() }
            ?: throw java.io.IOException("Qoder exchange 未返回 token")
        val expires = parseExpiry(o)
        return JobToken(token, o.optString("refresh_token", ""), expires)
    }

    /** jobRefreshToken 续期。 */
    fun refreshJobToken(jrt: String, mode: String = MODE_CN): JobToken {
        if (jrt.isBlank()) throw IllegalArgumentException("缺少 jobRefreshToken，无法续期")
        val text = post(openApiBase(mode) + EP_REFRESH, JSONObject().put("refresh_token", jrt))
        val o = JSONObject(text)
        val token = o.optString("token", "").takeIf { it.isNotBlank() }
            ?: throw java.io.IOException("Qoder refresh 未返回 token")
        val expires = parseExpiry(o)
        return JobToken(token, o.optString("refresh_token", jrt), expires)
    }

    /**
     * 到期时刻（unix **毫秒**）。优先 `expires_in`（实测国内/国际 OpenAPI 返回毫秒），缺省 24h。
     * 不解析 `expires_at` 字符串：minSdk 24 下 `java.time` 未开脱敏，避免 API 兼容问题。
     */
    private fun parseExpiry(o: JSONObject): Long {
        val inMs = o.optLong("expires_in", 0L)
        if (inMs > 0) return System.currentTimeMillis() + inMs
        return System.currentTimeMillis() + 24L * 3600_000L
    }

    /**
     * 账号身份 + **COSY 运行时字段所需的组织信息**。
     *
     * 为什么要把组织字段一起抓回来：`/algo/` 聊天签名里的 AES 明文（`runtimeFieldInput`）
     * 必须带 `organization_id` / `organization_tags` / `data_policy_agreed`，
     * 它们只能在登录时从 userinfo 拿到。漏抓就得每次聊天前多打一次 userinfo（还容易过期）。
     */
    data class QUserInfo(
        val uid: String,
        val email: String,
        val name: String,
        val organizationId: String = "",
        val organizationName: String = "",
        val organizationTags: List<String> = emptyList(),
        val dataPolicyAgreed: Boolean = false,
        val isDataPolicyModifiable: Boolean = true,
    )

    /**
     * 用 jobToken 拉账号身份；失败返回空字段（调用方 login 需判 uid 非空）。
     *
     * uid 取值兼容三种写法：`uid`（参考实现）、`id`、`userID`（历史 OpenAPI 返回过）。
     */
    fun fetchUserInfo(jt: String, mode: String): QUserInfo = try {
        val o = JSONObject(getBearer(openApiBase(mode) + EP_USERINFO, jt))
        QUserInfo(
            uid = o.optString("uid", "").ifBlank { o.optString("id", "") }
                .ifBlank { o.optString("userID", "") },
            email = o.optString("email", ""),
            name = o.optString("name", o.optString("username", "")),
            organizationId = o.optString("organization_id", ""),
            organizationName = o.optString("organization_name", ""),
            organizationTags = parseStringList(o.opt("organization_tags")),
            dataPolicyAgreed = o.optBoolean("data_policy_agreed", false),
            isDataPolicyModifiable = o.optBoolean("is_data_policy_modifiable", true),
        )
    } catch (e: Exception) {
        Log.w(TAG, "userinfo 失败: ${e.javaClass.simpleName}")
        QUserInfo("", "", "")
    }

    /** 组织标签可能是字符串数组，也可能是单个逗号分隔字符串（两种都实测见过）。 */
    private fun parseStringList(raw: Any?): List<String> = when (raw) {
        is org.json.JSONArray -> (0 until raw.length()).mapNotNull { raw.optString(it, "").takeIf { s -> s.isNotBlank() } }
        is String -> raw.split(',').map { it.trim() }.filter { it.isNotEmpty() }
        else -> emptyList()
    }

    data class QCredits(val remaining: Double, val exceeded: Boolean) {
        val hasCredit: Boolean get() = !exceeded && remaining > 0.0
    }

    /** 查积分余量（`quota/usage`）。失败/无 token 返回 null。用 Cosy-ClientType:10 与桌面端一致。 */
    fun fetchCredits(jt: String, mode: String): QCredits? {
        if (jt.isBlank()) return null
        return try {
            // 用量查询是普通鉴权接口，不需要机器身份（传 null 即不带头，走最小档）
            var root = JSONObject(getWithIdentity(openApiBase(mode) + EP_USAGE, jt, VARIANT_MINIMAL, null))
            // 部分返回把内容裹在 data 下；探测不到已知键时再解一层。
            if (!root.has("userQuota") && !root.has("personalQuota") &&
                !root.has("isQuotaExceeded") && root.has("data")
            ) root.optJSONObject("data")?.let { root = it }
            var remaining = 0.0
            // 个人 / 加购 / 组织额度分开返回，键名国内外有差异，逐个累加（拿到的都算进余量）。
            for (k in listOf("userQuota", "personalQuota", "addOnQuota", "addonQuota", "extraQuota")) {
                root.optJSONObject(k)?.let { remaining += it.optDouble("remaining", 0.0) }
            }
            for (k in listOf("orgResourcePackage", "organizationQuota")) {
                root.optJSONObject(k)?.let { org ->
                    if (org.optBoolean("available", true)) remaining += org.optDouble("remaining", 0.0)
                }
            }
            val exceeded = root.optBoolean("isQuotaExceeded", false)
            QCredits(remaining = remaining.coerceAtLeast(0.0), exceeded = exceeded)
        } catch (e: Exception) {
            Log.w(TAG, "credits 查询失败: ${e.javaClass.simpleName}")
            null
        }
    }

    /**
     * 用 PAT 登录并保存为当前账号：exchange → userinfo → 存库。uid 取 userinfo.id。
     * 失败抛异常（消息给人话）。
     */
    fun loginWithPat(context: Context, pat: String, mode: String): QoderAuthStore.QCred {
        val trimmed = pat.trim()
        if (trimmed.isBlank()) throw IllegalArgumentException("请粘贴 PAT")
        val jt = exchangeJobToken(trimmed, mode)
        val info = fetchUserInfo(jt.token, mode)
        val uid = info.uid.takeIf { it.isNotBlank() }
            ?: throw IllegalStateException("Qoder 未返回账号 ID，请确认 PAT 与区域（国内/国际）匹配")
        val cred = QoderAuthStore.QCred(
            pat = trimmed, jobToken = jt.token, jobRefreshToken = jt.refreshToken,
            expiresAt = jt.expiresAtMillis, uid = uid, name = info.name, email = info.email, mode = mode,
            organizationId = info.organizationId, organizationName = info.organizationName,
            organizationTags = info.organizationTags, dataPolicyAgreed = info.dataPolicyAgreed,
            isDataPolicyModifiable = info.isDataPolicyModifiable,
        )
        QoderAuthStore.save(context, cred)
        Log.i(TAG, "Qoder PAT 登录成功 uid=$uid mode=$mode")
        return cred
    }

    /**
     * 取当前 Qoder 账号一个**有效** jobToken：临期先用 jrt 续、续不动再用 PAT 重新换，写回库。
     * 无账号返回 null。供积分轮询用（不碰 Trae 的锁/区域，独立链路）。
     */
    fun validJobToken(context: Context, skewMillis: Long): String? {
        val cred = QoderAuthStore.load(context) ?: return null
        return validJobTokenFor(context, cred, skewMillis)
    }

    /**
     * 为**指定账号**取一个有效 jobToken（不改变当前选中账号）：临期先用 jrt 续、续不动再用 PAT 重新换，写回库。
     * 供批量领取 / 换号探测用（[QoderAuthStore.updateTokens] 只写 token，绝不 setActive）。
     */
    fun validJobTokenFor(context: Context, cred: QoderAuthStore.QCred, skewMillis: Long): String? {
        val now = System.currentTimeMillis()
        if (cred.jobToken.isNotBlank() && now + skewMillis < cred.expiresAt) return cred.jobToken
        val renewed: JobToken = runCatching {
            if (cred.jobRefreshToken.isNotBlank()) refreshJobToken(cred.jobRefreshToken, cred.mode)
            else if (cred.pat.isNotBlank()) exchangeJobToken(cred.pat, cred.mode)
            else null
        }.getOrNull() ?: return cred.jobToken.takeIf { it.isNotBlank() }
        QoderAuthStore.updateTokens(
            context,
            cred.copy(jobToken = renewed.token, jobRefreshToken = renewed.refreshToken.ifBlank { cred.jobRefreshToken },
                expiresAt = renewed.expiresAtMillis),
        )
        return renewed.token
    }

    /**
     * **强制**续一次 jobToken（不看是否临期），供转发链路撞到 401 时救急。
     *
     * 与 [validJobTokenFor] 的区别：那个是"快过期才续"，这个是"已经被拒了就续"。
     * 续不动返回 null（调用方据此把 401 报给客户端，别无限重试）。
     */
    fun forceRefreshJobToken(context: Context, cred: QoderAuthStore.QCred): String? {
        val renewed: JobToken = runCatching {
            if (cred.jobRefreshToken.isNotBlank()) refreshJobToken(cred.jobRefreshToken, cred.mode)
            else if (cred.pat.isNotBlank()) exchangeJobToken(cred.pat, cred.mode)
            else null
        }.getOrNull() ?: return null
        QoderAuthStore.updateTokens(
            context,
            cred.copy(
                jobToken = renewed.token,
                jobRefreshToken = renewed.refreshToken.ifBlank { cred.jobRefreshToken },
                expiresAt = renewed.expiresAtMillis,
            ),
        )
        return renewed.token
    }

    // ------------------------------------------------------------------
    // 每日可领 Credits（campaigns list + claim）
    // ------------------------------------------------------------------

    /** 单条活动（campaign）关键字段。startAt/endAt 是 unix **秒**（不是毫秒，社区踩过的坑）。 */
    data class QCampaign(
        val id: String,
        val claimStatus: String,   // CLAIMABLE | CLAIMED | ...
        val actionType: String,    // CLAIM_BENEFIT | ...
        val benefitKind: String,   // CREDITS | ...
        val benefitAmount: Long,
    ) {
        /** 有额度就记额度（VIEW_DETAILS 的 benefit 是 null → 0）。 */
        val grantsCredits: Boolean get() = benefitAmount > 0L

        /**
         * 只要服务端说 CLAIMABLE 就去领 —— **不再只认 CLAIM_BENEFIT**。
         *
         * 实测（2026-10-25，号 A）：桌面端对 `VIEW_DETAILS` 横幅也显示「领取」按钮，
         * 点了就变 CLAIMED；我们以前把这类过滤掉，用户看到的就是「电脑能领、App 不能领」。
         * 领它不发积分，但状态要和桌面端一致，否则永远对不上。
         */
        val claimable: Boolean
            get() = id.isNotBlank() && claimStatus.equals("CLAIMABLE", true)
    }

    /** 领取结果。status ∈ ok / already / fail（国际与国内同一套判定，不再区分 unsupported）。 */
    data class QClaimResult(val status: String, val amount: Long, val message: String) {
        val ok: Boolean get() = status == "ok"
    }

    /**
     * 活动接口的身份头。**普通档与桌面档的区别就在这几个头**（见 [VARIANT_DESKTOP]）。
     *
     * 官方桌面端（`main.log` 实测）用的是：
     * `User-Agent: Qoder` + `Cosy-Version: 0.4.x` + `Cosy-ClientType: 10` + `Cosy-Machine*`。
     * 缺这些头时服务端**不报错、直接返回空活动列表**，这就是「桌面端能领、App 领不了」的由来。
     */
    private fun applyMachineHeaders(
        b: Request.Builder,
        variant: String,
        id: QoderMachine.Identity?,
    ): Request.Builder {
        if (variant != VARIANT_DESKTOP) return b
        var out = b
            .header("User-Agent", "Qoder")
            // 桌面档统一用 CLI 的版本号（与 [QoderMachine] 一致）：实测 1.0.1 / 1.1.69 都能下发，
            // 真正起作用的是 ClientType=10 + 机器身份，版本号只要不是明显的老古董即可。
            .header("Cosy-Version", id?.version ?: QoderMachine.COSY_VERSION)
            .header("Cosy-ClientType", id?.clientType ?: QoderMachine.CLIENT_TYPE)
        if (id == null) return out
        out = out
            .header("Cosy-MachineId", id.machineId)
            .header("Cosy-MachineToken", id.machineToken)
            .header("Cosy-MachineOS", id.machineOS)
        // 官方无 umid 时这两项留空，空串头会被某些网关当成异常，干脆不发
        if (id.machineType.isNotBlank()) out = out.header("Cosy-MachineType", id.machineType)
        if (id.hostname.isNotBlank()) out = out.header("Cosy-MachineHostname", id.hostname)
        return out
    }

    /** GET 带 Bearer。普通档走最小头；桌面档补齐官方桌面端的身份头。 */
    private fun getWithIdentity(
        url: String,
        bearer: String,
        variant: String,
        id: QoderMachine.Identity?,
    ): String {
        val desktop = variant == VARIANT_DESKTOP
        val base = Request.Builder().url(url).get()
            .header("Authorization", "Bearer $bearer")
            .header("Accept", JSON)
            .header("User-Agent", if (desktop) "Qoder" else "traework2api")
            .header("Cosy-Version", if (desktop) (id?.version ?: QoderMachine.COSY_VERSION) else "1.0.1")
            .header("Cosy-ClientType", QoderMachine.CLIENT_TYPE)
        val req = applyMachineHeaders(base, variant, id).build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    /** POST 带 Bearer + JSON body（claim 用，body 传空对象）。身份头与列表**必须同一档**。 */
    private fun postClaimRaw(
        url: String,
        bearer: String,
        body: JSONObject,
        variant: String,
        id: QoderMachine.Identity?,
    ): String {
        val desktop = variant == VARIANT_DESKTOP
        val base = Request.Builder().url(url)
            .post(body.toString().toRequestBody(JSON.toMediaType()))
            .header("Authorization", "Bearer $bearer")
            .header("Content-Type", JSON)
            .header("Accept", JSON)
            .header("User-Agent", if (desktop) "Qoder" else "traework2api")
            .header("Cosy-Version", if (desktop) (id?.version ?: QoderMachine.COSY_VERSION) else "1.0.1")
            .header("Cosy-ClientType", QoderMachine.CLIENT_TYPE)
        val req = applyMachineHeaders(base, variant, id).build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    /** 桌面档的身份材料：优先用抓包回填的真指纹，否则用本机派生（与官方无 umid 客户端同构）。 */
    private fun campaignIdentity(
        context: Context,
        cred: QoderAuthStore.QCred,
        variant: String,
    ): QoderMachine.Identity? = if (variant != VARIANT_DESKTOP) null
    else QoderMachine.identity(context, QoderAuthStore.fingerprint(context, cred.uid))

    /** 上次验证可用/上次使用的活动请求档位。 */
    private fun campaignVariant(context: Context, mode: String): String =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(KEY_VARIANT + mode, null)
            ?.takeIf { it == VARIANT_DESKTOP || it == VARIANT_MINIMAL }
            // 国际版历史上一直需要身份；国内版先用最小头（本地对照里它就能下发）
            ?: if (mode == MODE_GLOBAL) VARIANT_DESKTOP else VARIANT_MINIMAL

    private fun rememberVariant(context: Context, mode: String, variant: String) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(KEY_VARIANT + mode, variant).apply()
    }

    /**
     * 活动列表一次请求的完整结果：列表 + 服务端给的 `showCampaign`（null = 没这个字段）。
     *
     * ⚠️ `showCampaign=false` **不能**当"确实没活动"：按 qoder2api-hub 的对照，
     * 缺桌面身份头时服务端就是回 `showCampaign=false, campaigns=[]` 来静默隐藏活动。
     * 只有用桌面档（全量身份）问出来的空才是真空 —— 见 [loadCampaigns]。
     */
    internal data class CampaignList(val campaigns: List<QCampaign>, val showCampaign: Boolean?)

    private fun fetchCampaigns(
        jt: String,
        mode: String,
        variant: String,
        id: QoderMachine.Identity?,
    ): CampaignList {
        val url = openApiBase(mode) + EP_CAMPAIGNS + "?forceRefresh=true"
        return parseCampaigns(JSONObject(getWithIdentity(url, jt, variant, id)))
    }

    /**
     * 拉活动列表，并在"空列表"时**升级身份档位**再问一次。
     *
     * 空列表是静默失败（HTTP 200、无错误），所以普通档空了不能就此判定"今天没活动"，
     * 必须换桌面档（`UA: Qoder` + `Cosy-Machine*`）复核；桌面档已经是最全身份，它空才算空。
     * 哪一档真的出活动就记住哪一档（按国内/国际分别记），下次不再白白多打一次上游。
     *
     * @param jt        起始 jobToken
     * @param refreshJt 撞到 401 时续 token；返回 null 表示续不动（按失败处理）
     */
    private fun loadCampaigns(
        context: Context,
        cred: QoderAuthStore.QCred,
        jt: String,
        refreshJt: () -> String?,
    ): CampaignLoad {
        val first = campaignVariant(context, cred.mode)
        // 记住的就是桌面档，就别再拿普通档白跑一趟。
        val ladder = if (first == VARIANT_DESKTOP) listOf(VARIANT_DESKTOP)
        else listOf(VARIANT_MINIMAL, VARIANT_DESKTOP)
        var cur = jt
        var retried401 = false
        var lastError: String? = null
        var i = 0
        while (i < ladder.size) {
            val v = ladder[i]
            try {
                val got = fetchCampaigns(cur, cred.mode, v, campaignIdentity(context, cred, v))
                lastError = null
                if (got.campaigns.isNotEmpty()) {
                    if (v != first) rememberVariant(context, cred.mode, v)
                    return CampaignLoad(got, v, null)
                }
                if (i == ladder.lastIndex) return CampaignLoad(got, v, null)
            } catch (e: Exception) {
                val msg = e.message ?: e.javaClass.simpleName
                lastError = msg
                if (msg.contains("401") && !retried401) {
                    retried401 = true
                    refreshJt()?.let { cur = it }
                    continue   // 同一档用新 token 再来一次
                }
                Log.w(TAG, "campaigns($v) 失败: $msg")
                if (i == ladder.lastIndex) return CampaignLoad(null, v, msg)
            }
            i++
        }
        return CampaignLoad(null, first, lastError ?: "未知错误")
    }

    private data class CampaignLoad(
        val list: CampaignList?,
        val variant: String,
        val error: String?,
    )

    /**
     * 诊断：把领取链路**每一步的原始结果**拼成可复制的文本。
     *
     * 为什么要有：手机端拿不到 logcat（ADB 未授权），用户说"领不到"时
     * 只有"账号页显示什么"这一条线索，双方都在猜。这个把 HTTP 原文摊开，
     * 一次就能定位是 token、是活动列表、还是 claim 被拒。
     */
    fun diagnose(ctx: Context): String {
        val sb = StringBuilder()
        val list = QoderAuthStore.list(ctx)
        sb.append("账号数：").append(list.size).append('\n')
        if (list.isEmpty()) return sb.append("没有 Qoder 账号\n").toString()
        for (m in list) {
            sb.append("\n== ").append(m.name.ifBlank { m.uid }).append(" · ").append(m.uid.take(8))
                .append(" · ").append(m.mode).append(" ==\n")
            val cred = QoderAuthStore.loadByUid(ctx, m.uid)
            if (cred == null) { sb.append("凭据缺失\n"); continue }
            val jt = runCatching { validJobTokenFor(ctx, cred, 60_000L) }.getOrNull()
            if (jt.isNullOrBlank()) { sb.append("jobToken 失败（PAT 无效或网络不通）\n"); continue }
            sb.append("jobToken ok ").append(jt.take(12)).append("…\n")
            sb.append("记忆档位：").append(campaignVariant(ctx, cred.mode)).append('\n')
            // 两档都打一遍：只有这样才分得清"上游真没活动"和"被静默隐藏了"。
            for (v in listOf(VARIANT_MINIMAL, VARIANT_DESKTOP)) {
                try {
                    val id = campaignIdentity(ctx, cred, v)
                    val raw = getWithIdentity(
                        openApiBase(cred.mode) + EP_CAMPAIGNS + "?forceRefresh=true", jt, v, id,
                    )
                    val parsed = parseCampaigns(JSONObject(raw))
                    sb.append('[').append(v).append("] 解析 ").append(parsed.campaigns.size)
                        .append(" 条，可领 ").append(parsed.campaigns.count { it.claimable })
                        .append(" 条，showCampaign=").append(parsed.showCampaign).append('\n')
                    for (c in parsed.campaigns) {
                        sb.append("  · ").append(c.id.take(8)).append(' ').append(c.actionType)
                            .append(' ').append(c.claimStatus)
                            .append(" benefit=").append(c.benefitKind).append('/').append(c.benefitAmount)
                            .append(if (c.claimable) "  ← 可领" else "").append('\n')
                    }
                    for (c in parsed.campaigns.filter { it.claimable }) {
                        val r = runCatching { claimOne(jt, cred.mode, c.id, v, id) }
                        sb.append("  claim ").append(c.id.take(8)).append(" → ")
                            .append(r.fold(
                                { "新领=${it.first} 额度=${it.second}" },
                                { "异常 ${it.javaClass.simpleName}: ${it.message}" },
                            ))
                            .append('\n')
                    }
                    if (parsed.campaigns.isEmpty()) {
                        sb.append("  原文：").append(raw.take(400)).append('\n')
                    }
                } catch (e: Exception) {
                    sb.append('[').append(v).append("] 请求失败：").append(e.javaClass.simpleName)
                        .append(": ").append(e.message).append('\n')
                }
            }
        }
        return sb.toString()
    }

    /** 领一条活动。body 固定 {}（幂等）。身份头与列表**必须是同一档**。返回 (是否新领, 入账额度)。 */
    private fun claimOne(
        jt: String,
        mode: String,
        campaignId: String,
        variant: String,
        id: QoderMachine.Identity?,
    ): Pair<Boolean, Long> {
        val url = openApiBase(mode) + EP_CAMPAIGNS + "/" + campaignId + "/claim"
        return parseClaim(JSONObject(postClaimRaw(url, jt, JSONObject(), variant, id)))
    }

    /**
     * 解析活动列表。响应可能把内容裹在 `data` 下，也可能直接放根上（两种都见过）；
     * 活动 id 的键名也不统一：`campaignId` / `campaignKey` / `id`。
     *
     * 抽出来单独成函数是为了能自检（list/claim 都要打网络）。
     */
    internal fun parseCampaigns(rootIn: JSONObject): CampaignList {
        var root = rootIn
        if (!root.has("campaigns")) root.optJSONObject("data")?.let { root = it }
        val show = when {
            root.has("showCampaign") -> root.optBoolean("showCampaign", false)
            rootIn.has("showCampaign") -> rootIn.optBoolean("showCampaign", false)
            else -> null
        }
        val arr = root.optJSONArray("campaigns") ?: return CampaignList(emptyList(), show)
        val camps = (0 until arr.length()).mapNotNull { i ->
            val o = arr.optJSONObject(i) ?: return@mapNotNull null
            val id = o.optString("campaignId", "")
                .ifBlank { o.optString("campaignKey", "") }
                .ifBlank { o.optString("id", "") }
            if (id.isBlank()) return@mapNotNull null
            val b = o.optJSONObject("benefit")
            QCampaign(
                id = id,
                claimStatus = o.optString("claimStatus", ""),
                actionType = o.optString("actionType", ""),
                benefitKind = b?.optString("kind", "") ?: "",
                benefitAmount = b?.optLong("amount", 0L) ?: 0L,
            )
        }
        return CampaignList(camps, show)
    }

    /** claim 响应：返回 (是否真的新领到, 本次入账额度)。replayed=true 表示今天已领过，不算新领。 */
    internal fun parseClaim(root: JSONObject): Pair<Boolean, Long> {
        // 有的返回把结果裹在 data 下。
        val o = if (root.optString("status", "").isBlank() && root.has("data"))
            (root.optJSONObject("data") ?: root) else root
        val status = o.optString("status", "")
        val replayed = o.optBoolean("replayed", false)
        val amount = o.optJSONObject("benefit")?.optLong("amount", 0L) ?: 0L
        val claimed = status.equals("CLAIMED", true)
        return (claimed && !replayed) to if (claimed && !replayed) amount else 0L
    }

    /** 对指定 uid 领取当前可领的每日 Credits。无有效 token 抛异常。 */
    fun claimDailyForUid(context: Context, uid: String): QClaimResult {
        val cred = QoderAuthStore.loadByUid(context, uid)
            ?: return QClaimResult("fail", 0, "凭据读取失败")
        return claimDailyForCred(context, cred)
    }

    /** 领取编排：取有效 jt → 列活动 → 领 CLAIMABLE 的 → 汇总。失败不抛（一人失败不拖垮整批）。 */
    /**
     * **只读**查这个账号今天领没领（不发 claim、不改任何状态）。
     *
     * 为什么需要：切号后卡片上的按钮如果只信本地记录，就会出现
     * "明明领过了还显示「领取」，非要点一次才变「已领取」"。
     *
     * 返回：true=已领完 / false=还有可领 / null=查不到（token 或网络问题，别据此改显示）。
     */
    fun claimedTodayFor(context: Context, cred: QoderAuthStore.QCred): Boolean? {
        val jt = validJobTokenFor(context, cred, skewMillis = 60_000L) ?: return null
        val load = loadCampaigns(context, cred, jt) { forceRefreshJobToken(context, cred) }
        val campaigns = load.list?.campaigns ?: return null
        if (campaigns.isEmpty()) return null
        if (campaigns.any { it.claimable }) return false
        return if (campaigns.any { it.claimStatus.equals("CLAIMED", ignoreCase = true) }) true else null
    }

    private fun claimDailyForCred(context: Context, cred: QoderAuthStore.QCred): QClaimResult {
        val jtOrNull = validJobTokenFor(context, cred, skewMillis = 60_000L)
        if (jtOrNull.isNullOrBlank()) return QClaimResult("fail", 0, "没有有效 token，请重新粘贴 PAT 登录")
        // 可变的非空副本：401 时要能就地换掉（Kotlin 对 var 的智能转换不成立，故先落地成 String）
        var jt: String = jtOrNull

        // 档位阶梯（普通头 → 桌面身份）：空列表是被静默隐藏时唯一能自救的办法。
        // 以前这里还叠了 3 轮 × 2 秒的 sleep 硬等"活动刷新"，批量领号时等于空转十几秒；
        // 活动列表的动态性交给调用方的定时任务（[QoderCheckin]）重试，这里只做一次快速升级。
        val load = loadCampaigns(context, cred, jt) {
            forceRefreshJobToken(context, cred)?.also { jt = it }
        }
        val list = load.list
            ?: return QClaimResult("fail", 0, "拉取活动失败：${load.error ?: "未知错误"}")
        val campaigns = list.campaigns
        val claimable = campaigns.filter { it.claimable }
        if (claimable.isEmpty()) {
            // 实测（2026-10-25）：活动列表随日期滚动，「每天领 100 Credits」是
            // CLAIM_BENEFIT + 每日 10:00(UTC+8) 重置。列表是动态的 —— 凌晨查可能一条
            // CLAIM_BENEFIT 都没有，到点后才出现，所以调用方必须重试（见 AutoCheckin）。
            val allClaimed = campaigns.isNotEmpty() &&
                campaigns.all { it.claimStatus.equals("CLAIMED", true) }
            // 只有"带 Credits 奖励"的活动才值得说"已领取 N 条"。
            // 实测（2026-10-26）：现在只剩 VIEW_DETAILS + benefit=null 的社区活动，
            // 对着它们说"已领取 2 条"会让人以为白拿了 2 份额度 —— 必须如实说"没有可领的积分活动"。
            val creditCampaigns = campaigns.count { it.grantsCredits }
            val why = when {
                creditCampaigns > 0 && allClaimed -> "已领取 $creditCampaigns 条"
                creditCampaigns == 0 && campaigns.isNotEmpty() ->
                    "无积分活动（${campaigns.size} 条均为查看类，不发 Credits）"
                campaigns.isNotEmpty() -> "无待领活动（${campaigns.size} 条均已处理）"
                else -> "今日无可领活动（上游返回空列表）"
            }
            return QClaimResult("already", 0, why)
        }
        val identity = campaignIdentity(context, cred, load.variant)
        var total = 0L
        var fresh = 0
        var lastErr: String? = null
        for (c in claimable) {
            try {
                val (newly, amt) = claimOne(jt, cred.mode, c.id, load.variant, identity)
                if (newly) { total += amt; fresh++ }
            } catch (e: Exception) {
                lastErr = e.message ?: e.javaClass.simpleName
            }
        }
        return when {
            fresh > 0 && total > 0L -> QClaimResult("ok", total, "")
            fresh > 0 -> QClaimResult("ok", 0, "领取 $fresh 条（无 Credits）")
            else -> QClaimResult("already", 0, lastErr?.let { "领取未生效：$it" } ?: "今日已领取")
        }
    }

    /**
     * 对**全部**已保存 Qoder 账号逐一带领每日 Credits。**阻塞** —— 必须在后台线程跑。
     * 账号间留间隔（上游按设备/账号维度风控）。返回 (uid → 结果) 供 UI 汇总。
     */
    fun claimDailyAll(context: Context): List<Pair<String, QClaimResult>> {
        val out = ArrayList<Pair<String, QClaimResult>>()
        for ((i, m) in QoderAuthStore.list(context).withIndex()) {
            if (i > 0) Thread.sleep(600L)
            out.add(m.uid to runCatching { claimDailyForUid(context, m.uid) }
                .getOrElse { QClaimResult("fail", 0, it.message ?: it.javaClass.simpleName) })
        }
        return out
    }

    /** 纯计算自检：host 分派与积分余量判据。 */
    fun selfCheck() {
        val cn = openApiBase(MODE_CN)
        val gl = openApiBase(MODE_GLOBAL)
        check(cn.contains("qoder.com.cn") && gl.contains("qoder.sh") && cn != gl) {
            "自检失败：Qoder 国内/国际 OpenAPI host 应不同且各自命中"
        }
        check(QCredits(50.0, false).hasCredit) { "自检失败：有剩余额度应判 hasCredit" }
        check(!QCredits(0.0, false).hasCredit) { "自检失败：零额度不应 hasCredit" }
        check(!QCredits(50.0, true).hasCredit) { "自检失败：isQuotaExceeded 应判无额度" }

        // 模型目录：无 token 必须直接返回 null（不许为了一次刷新去打上游）
        check(fetchModelCatalog("", MODE_CN) == null) { "自检失败：无 jobToken 不该发起模型目录请求" }
        check(fetchModelCatalog("   ", MODE_GLOBAL) == null) { "自检失败：空白 jobToken 不该发起模型目录请求" }

        // campaigns host 必须带 /sash 前缀（与 usage 的 /api/v2 不同链路）。
        check((openApiBase(MODE_CN) + EP_CAMPAIGNS).contains("/sash/api/v1/me/campaigns")) {
            "自检失败：Qoder campaigns 端点路径不符"
        }

        // 活动解析：只要 CLAIMABLE 就算可领（判严了会漏领 —— 桌面端 VIEW_DETAILS 也能领）。
        // 注意 parseCampaigns 会**丢弃空 id** 的条目，故 4 条输入 → 3 条有效解析（顺序：act-1, act-2, act-4）。
        val parsed = parseCampaigns(JSONObject("""{"showCampaign":true,"campaigns":[
            {"campaignId":"act-1","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":100}},
            {"id":"act-2","claimStatus":"CLAIMED","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":100}},
            {"campaignId":"","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT"},
            {"campaignId":"act-4","claimStatus":"CLAIMABLE","actionType":"OTHER"}
        ]}"""))
        val camps = parsed.campaigns
        check(parsed.showCampaign == true) { "自检失败：showCampaign 应原样带出（区分『真空』与『被隐藏』）" }
        check(camps.size == 3) { "自检失败：空 id 应被丢弃，4 条输入应解析出 3 条" }
        check(camps[0].id == "act-1" && camps[0].claimable) { "自检失败：CLAIMABLE+CLAIM_BENEFIT 应判可领" }
        check(camps[1].id == "act-2" && !camps[1].claimable) { "自检失败：已领(CLAIMED)不应判可领" }
        check(camps[2].id == "act-4" && camps[2].claimable) { "自检失败：CLAIMABLE 的非 CLAIM_BENEFIT 也该领" }
        check(camps[0].grantsCredits) { "自检失败：带 benefit 的应记额度" }

        // 响应裹在 data 下、id 叫 campaignKey、没有 showCampaign —— 三种兼容都要在。
        val wrapped = parseCampaigns(JSONObject(
            """{"data":{"campaigns":[{"campaignKey":"k-1","claimStatus":"CLAIMABLE"}]}}"""
        ))
        check(wrapped.campaigns.singleOrNull()?.id == "k-1") {
            "自检失败：data 包裹 + campaignKey 应能解析出活动"
        }
        check(wrapped.showCampaign == null) { "自检失败：缺 showCampaign 应为 null（不是 false）" }

        // claim 解析：replayed=true（今天已领过）不算新领、不计额度，避免重复计数。
        check(parseClaim(JSONObject("""{"status":"CLAIMED","replayed":false,"benefit":{"kind":"CREDITS","amount":100}}""")) == (true to 100L)) {
            "自检失败：新领应计成功并累加额度"
        }
        check(parseClaim(JSONObject("""{"status":"CLAIMED","replayed":true,"benefit":{"kind":"CREDITS","amount":100}}""")) == (false to 0L)) {
            "自检失败：replayed 不应算新领"
        }
        check(parseClaim(JSONObject("""{}""")).first.not()) { "自检失败：缺字段不应判成功" }
    }
}
