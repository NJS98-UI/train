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

    /** 查积分余量（`quota/usage`）。失败/无 token 返回 null。 */
    fun fetchCredits(jt: String, mode: String): QCredits? {
        if (jt.isBlank()) return null
        return try {
            val o = JSONObject(getBearer(openApiBase(mode) + EP_USAGE, jt))
            var remaining = 0.0
            o.optJSONObject("userQuota")?.let { remaining += it.optDouble("remaining", 0.0) }
            o.optJSONObject("orgResourcePackage")?.let { org ->
                if (org.optBoolean("available", true)) remaining += org.optDouble("remaining", 0.0)
            }
            val exceeded = o.optBoolean("isQuotaExceeded", false)
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
    }
}
