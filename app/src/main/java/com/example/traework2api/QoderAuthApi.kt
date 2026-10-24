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
 * - `jobToken/exchange`、`jobToken/refresh`、`quota/usage` **都不需要 COSY 签名**
 *   （COSY 只有聊天网关 `/algo/...` 才要，本 App 不接入聊天，故这里无需实现签名）。
 * - 区域不同 host 不同：国内 `openapi.qoder.com.cn`；国际 `openapi.qoder.sh`。
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

    private const val JSON = "application/json"

    private val client = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .build()

    /** 按 mode 取 OpenAPI host。未知 mode 一律按国内（主链路）。 */
    private fun openApiBase(mode: String): String =
        if (mode == MODE_GLOBAL) "https://openapi.qoder.sh" else "https://openapi.qoder.com.cn"

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

    data class QUserInfo(val uid: String, val email: String, val name: String)

    /** 用 jobToken 拉账号身份；失败返回空字段（调用方 login 需判 uid 非空）。 */
    fun fetchUserInfo(jt: String, mode: String): QUserInfo = try {
        val o = JSONObject(getBearer(openApiBase(mode) + EP_USERINFO, jt))
        QUserInfo(
            uid = o.optString("id", o.optString("userID", "")),
            email = o.optString("email", ""),
            name = o.optString("name", o.optString("username", "")),
        )
    } catch (e: Exception) {
        Log.w(TAG, "userinfo 失败: ${e.javaClass.simpleName}")
        QUserInfo("", "", "")
    }

    data class QCredits(val remaining: Double, val exceeded: Boolean) {
        val hasCredit: Boolean get() = !exceeded && remaining > 0.0
    }

    /** 查积分余量（`quota/usage`）。失败/无 token 返回 null。用 Cosy-ClientType:10 与桌面端一致。 */
    fun fetchCredits(jt: String, mode: String): QCredits? {
        if (jt.isBlank()) return null
        return try {
            var root = JSONObject(getForCampaigns(openApiBase(mode) + EP_USAGE, jt))
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
        val claimable: Boolean
            get() = id.isNotBlank() &&
                claimStatus.equals("CLAIMABLE", true) &&
                actionType.equals("CLAIM_BENEFIT", true)
    }

    /** 领取结果。status ∈ ok / already / fail（国际与国内同一套判定，不再区分 unsupported）。 */
    data class QClaimResult(val status: String, val amount: Long, val message: String) {
        val ok: Boolean get() = status == "ok"
    }

    /** GET 带 Bearer。刻意不带 Cosy-Machine* 头：伪造派生指纹会被 campaigns 服务静默过滤（见 Qoder-2API-Go），干净请求反而能领通用活动。 */
    private fun getForCampaigns(url: String, bearer: String): String {
        val req = Request.Builder().url(url).get()
            .header("Authorization", "Bearer $bearer")
            .header("Accept", JSON)
            .header("User-Agent", "traework2api")
            .header("Cosy-Version", "1.0.1")
            .header("Cosy-ClientType", "10")
            .build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    /** POST 带 Bearer + JSON body（claim 用，body 传空对象）。 */
    private fun postBearer(url: String, bearer: String, body: JSONObject): String {
        val req = Request.Builder().url(url)
            .post(body.toString().toRequestBody(JSON.toMediaType()))
            .header("Authorization", "Bearer $bearer")
            .header("Content-Type", JSON)
            .header("Accept", JSON)
            .header("User-Agent", "traework2api")
            .header("Cosy-Version", "1.0.1")
            .header("Cosy-ClientType", "10")
            .build()
        client.newCall(req).execute().use { r ->
            val text = r.body?.string().orEmpty()
            if (!r.isSuccessful) throw java.io.IOException("Qoder ${r.code}: ${text.take(200)}")
            return text
        }
    }

    /**
     * 拉取活动列表（可领的在里面）。解析见 [parseCampaigns]。
     * 国际/国内同走干净请求（不带 Cosy-Machine* 头）：伪指纹会让服务端静默隐藏「每天领」活动，
     * 无指纹则通用活动保持可见；「每天领 100 Credits」这类仅原生桌面端身份下发的活动会正常缺席。
     */
    private fun fetchCampaigns(jt: String, mode: String): List<QCampaign> {
        val url = openApiBase(mode) + EP_CAMPAIGNS + "?forceRefresh=true"
        val text = getForCampaigns(url, jt)
        return parseCampaigns(JSONObject(text))
    }

    /** 领一条活动。body 固定 {}（幂等）。返回本次入账额度（replayed 时为 0）。 */
    private fun claimOne(jt: String, mode: String, campaignId: String): Pair<Boolean, Long> {
        val url = openApiBase(mode) + EP_CAMPAIGNS + "/" + campaignId + "/claim"
        val text = postBearer(url, jt, JSONObject())
        return parseClaim(JSONObject(text))
    }

    /** 抽出来单独成函数是为了能自检（list/claim 都要打网络）。 */
    internal fun parseCampaigns(root: JSONObject): List<QCampaign> {
        val arr = root.optJSONArray("campaigns") ?: return emptyList()
        return (0 until arr.length()).mapNotNull { i ->
            val o = arr.optJSONObject(i) ?: return@mapNotNull null
            val id = o.optString("campaignId", o.optString("id", ""))
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
    private fun claimDailyForCred(context: Context, cred: QoderAuthStore.QCred): QClaimResult {
        // 国际/国内同走干净链路（不带 Cosy-Machine* 头）。桌面端专属「每天领 100 Credits」
        // 活动在没有原生指纹时服务端会正常缺席，但其它通用活动仍可领；无需再对国际版硬挡 unsupported。
        val jt = validJobTokenFor(context, cred, skewMillis = 60_000L)
        if (jt.isNullOrBlank()) return QClaimResult("fail", 0, "没有有效 token，请重新粘贴 PAT 登录")
        val campaigns = try {
            fetchCampaigns(jt, cred.mode)
        } catch (e: Exception) {
            return QClaimResult("fail", 0, "拉取活动失败：${e.message ?: e.javaClass.simpleName}")
        }
        val claimable = campaigns.filter { it.claimable }
        if (claimable.isEmpty())
            return QClaimResult("already", 0, "今日无可领活动（可能已领或未到 10:00 领取窗口）")
        var total = 0L
        var fresh = 0
        var lastErr: String? = null
        for (c in claimable) {
            try {
                val (newly, amt) = claimOne(jt, cred.mode, c.id)
                if (newly) { total += amt; fresh++ }
            } catch (e: Exception) {
                lastErr = e.message ?: e.javaClass.simpleName
            }
        }
        return if (fresh > 0) QClaimResult("ok", total, "")
        else QClaimResult("already", 0, lastErr?.let { "领取未生效：$it" } ?: "今日已领取")
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

        // campaigns host 必须带 /sash 前缀（与 usage 的 /api/v2 不同链路）。
        check((openApiBase(MODE_CN) + EP_CAMPAIGNS).contains("/sash/api/v1/me/campaigns")) {
            "自检失败：Qoder campaigns 端点路径不符"
        }

        // 活动解析：只有 CLAIMABLE ∧ CLAIM_BENEFIT 才算可领（判松了会白发 claim，判严了漏领）。
        // 注意 parseCampaigns 会**丢弃空 id** 的条目，故 4 条输入 → 3 条有效解析（顺序：act-1, act-2, act-4）。
        val camps = parseCampaigns(JSONObject("""{"campaigns":[
            {"campaignId":"act-1","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":100}},
            {"id":"act-2","claimStatus":"CLAIMED","actionType":"CLAIM_BENEFIT","benefit":{"kind":"CREDITS","amount":100}},
            {"campaignId":"","claimStatus":"CLAIMABLE","actionType":"CLAIM_BENEFIT"},
            {"campaignId":"act-4","claimStatus":"CLAIMABLE","actionType":"OTHER"}
        ]}"""))
        check(camps.size == 3) { "自检失败：空 id 应被丢弃，4 条输入应解析出 3 条" }
        check(camps[0].id == "act-1" && camps[0].claimable) { "自检失败：CLAIMABLE+CLAIM_BENEFIT 应判可领" }
        check(camps[1].id == "act-2" && !camps[1].claimable) { "自检失败：已领(CLAIMED)不应判可领" }
        check(camps[2].id == "act-4" && !camps[2].claimable) { "自检失败：非 CLAIM_BENEFIT 不应判可领" }

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
