package com.example.traework2api

import android.content.Context
import java.net.InetAddress
import java.util.concurrent.TimeUnit
import okhttp3.Dns
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject

/**
 * GitHub Copilot 的登录与令牌链路。
 *
 * ## 为什么不能直接用 PAT
 *
 * `api.githubcopilot.com` 明确拒绝个人访问令牌：
 * `Personal Access Tokens are not supported for this endpoint`。
 * 唯一可行的是 **OAuth 设备码授权**（Copilot 官方客户端用的同一套），
 * 换到 `ghu_…` 令牌后再去换短时的 Copilot 令牌。
 *
 * ## 三个域名，三种角色
 *
 * | 域名 | 用途 | 本机可达性 |
 * |---|---|---|
 * | `github.com` | 设备码授权（登录） | **默认解析到的 IP 被墙**，需按 IP 兜底 |
 * | `api.github.com` | 换 Copilot 令牌、取用户信息 | 正常 |
 * | `api.githubcopilot.com` | 模型列表 + 聊天 | 正常 |
 *
 * 设备码授权必须走 `github.com`，而它默认解析到 `140.82.121.4`（连不上），
 * 换成 `20.205.243.166` / `140.82.113.4` 就能通 —— 所以这里给 OkHttp 配了一个
 * 只对 `github.com` 生效的 DNS 兜底：先问系统，再追加/前置已知可用 IP。
 */
object CopilotApi {

    private const val TAG = "CopilotApi"

    /** Copilot 官方客户端的 OAuth client_id（公开值，所有第三方实现都用它）。 */
    const val CLIENT_ID = "Iv1.b507a08c87ecfe98"

    const val HOST_WEB = "https://github.com"
    const val HOST_API = "https://api.github.com"
    const val HOST_INFER = "https://api.githubcopilot.com"

    /** 与官方 Copilot 客户端一致的版本标识（上游按它判断协议能力）。 */
    const val EDITOR_VERSION = "vscode/1.99.0"
    const val PLUGIN_VERSION = "copilot-chat/0.26.7"
    const val USER_AGENT = "GitHubCopilotChat/0.26.7"
    const val INTEGRATION_ID = "vscode-chat"

    /**
     * `github.com` 的已知可用 IP（按可用性排序）。
     *
     * ⚠️ 这是**网络层兜底**，不是协议的一部分：某些网络下 `github.com` 默认解析到的
     * `140.82.121.4` 会被阻断，而 `20.205.243.166` 正常。GitHub 会换 IP，
     * 所以这里只做"前置候选"，系统解析结果仍然保留在末尾兜底。
     */
    private val GITHUB_FALLBACK_IPS = listOf("20.205.243.166", "140.82.113.4")

    private val githubDns = object : Dns {
        override fun lookup(hostname: String): List<InetAddress> {
            val system = try {
                Dns.SYSTEM.lookup(hostname)
            } catch (_: Exception) {
                emptyList()
            }
            if (hostname != "github.com") return system
            val extra = GITHUB_FALLBACK_IPS.mapNotNull {
                try { InetAddress.getByName(it) } catch (_: Exception) { null }
            }
            // 兜底 IP 在前：被阻断的默认 IP 排在后面，只有前面都失败才会用到
            return (extra + system).distinctBy { it.hostAddress }
        }
    }

    private val client = OkHttpClient.Builder()
        .dns(githubDns)
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(60, TimeUnit.SECONDS)
        .writeTimeout(30, TimeUnit.SECONDS)
        .build()

    /** 聊天用的客户端：读超时要长（推理模型首字可能很久）。 */
    val streamClient: OkHttpClient = client.newBuilder()
        .readTimeout(10, TimeUnit.MINUTES)
        .build()

    private const val JSON = "application/json"

    // ------------------------------------------------------------------
    // 设备码授权
    // ------------------------------------------------------------------

    /**
     * 一次设备码授权会话。
     *
     * @param userCode 用户在 `verificationUri` 上要输入的码（形如 `1DD7-A5C0`）
     * @param interval 官方要求的轮询间隔（秒）；**必须遵守**，快了会 `slow_down`
     */
    data class DeviceFlow(
        val deviceCode: String,
        val userCode: String,
        val verificationUri: String,
        val interval: Int,
        val expiresIn: Int,
    )

    sealed class PollResult {
        data class Token(val accessToken: String) : PollResult()

        /** 用户还没授权，正常等待。 */
        object Pending : PollResult()

        /**
         * 轮询太快，被上游限流。
         *
         * ⚠️ 必须和 [Pending] 分开：GitHub 对**出口 IP** 限流，移动网络的 NAT
         * 后面有大量设备共用同一个 IP，这个惩罚窗口可能很长（实测 5 秒轮询会被
         * 打回十几轮）。调用方收到它应当**把间隔再加 5 秒**，而不是继续按原节奏打。
         */
        object SlowDown : PollResult()

        data class Failed(val error: String) : PollResult()
    }

    /** 最近一次失败原因（UI 展示 + 排查）。 */
    @Volatile
    var lastError: String? = null
        private set

    fun startDeviceFlow(): DeviceFlow? {
        lastError = null
        val body = "client_id=$CLIENT_ID&scope=read:user".toRequestBody(JSON.toMediaType())
        val req = Request.Builder()
            .url("$HOST_WEB/login/device/code")
            .header("Accept", JSON)
            .post(body)
            .build()
        return try {
            client.newCall(req).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                if (!resp.isSuccessful) {
                    lastError = "设备码 HTTP ${resp.code}: ${text.take(200)}"
                    return null
                }
                val o = JSONObject(text)
                val dc = o.optString("device_code", "")
                val uc = o.optString("user_code", "")
                if (dc.isBlank() || uc.isBlank()) {
                    lastError = "设备码响应缺字段: ${text.take(200)}"
                    return null
                }
                DeviceFlow(
                    deviceCode = dc,
                    userCode = uc,
                    verificationUri = o.optString("verification_uri", "$HOST_WEB/login/device"),
                    interval = o.optInt("interval", 5).coerceAtLeast(5),
                    expiresIn = o.optInt("expires_in", 900),
                )
            }
        } catch (e: Exception) {
            lastError = "设备码请求失败: ${e.javaClass.simpleName}: ${e.message}"
            android.util.Log.w(TAG, lastError!!)
            null
        }
    }

    /** 轮询一次。调用方自己按 [DeviceFlow.interval] 控制节奏。 */
    fun pollOnce(deviceCode: String): PollResult {
        val body = ("client_id=$CLIENT_ID&device_code=$deviceCode" +
            "&grant_type=urn:ietf:params:oauth:grant-type:device_code")
            .toRequestBody(JSON.toMediaType())
        val req = Request.Builder()
            .url("$HOST_WEB/login/oauth/access_token")
            .header("Accept", JSON)
            .post(body)
            .build()
        return try {
            client.newCall(req).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                val o = try { JSONObject(text) } catch (_: Exception) { null }
                    ?: return PollResult.Failed("响应不是 JSON: ${text.take(120)}")
                val token = o.optString("access_token", "")
                if (token.isNotBlank()) return PollResult.Token(token)
                when (val err = o.optString("error", "")) {
                    "authorization_pending" -> PollResult.Pending
                    // 轮询过快：不是错误，但**必须退避**（见 PollResult.SlowDown 注释）
                    "slow_down" -> PollResult.SlowDown
                    "" -> PollResult.Failed("未知响应: ${text.take(120)}")
                    "expired_token" -> PollResult.Failed("授权码已过期，请重新获取")
                    "access_denied" -> PollResult.Failed("你拒绝了授权")
                    else -> PollResult.Failed(err)
                }
            }
        } catch (e: Exception) {
            PollResult.Failed("轮询失败: ${e.javaClass.simpleName}")
        }
    }

    // ------------------------------------------------------------------
    // 令牌与用户信息
    // ------------------------------------------------------------------

    /**
     * 用 OAuth 令牌换 Copilot 短时令牌。
     *
     * @return (token, 过期 unix 秒)；失败返回 null 并写 [lastError]
     */
    fun exchangeCopilotToken(oauthToken: String): Pair<String, Long>? {
        val req = Request.Builder()
            .url("$HOST_API/copilot_internal/v2/token")
            .header("Authorization", "token $oauthToken")
            .header("Accept", JSON)
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", PLUGIN_VERSION)
            .header("User-Agent", USER_AGENT)
            .build()
        return try {
            client.newCall(req).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                if (!resp.isSuccessful) {
                    lastError = "换 Copilot 令牌 HTTP ${resp.code}: ${text.take(200)}"
                    return null
                }
                val o = JSONObject(text)
                val t = o.optString("token", "")
                if (t.isBlank()) {
                    lastError = "换 Copilot 令牌缺 token 字段: ${text.take(200)}"
                    return null
                }
                lastError = null
                t to o.optLong("expires_at", System.currentTimeMillis() / 1000 + 1_500)
            }
        } catch (e: Exception) {
            lastError = "换 Copilot 令牌失败: ${e.javaClass.simpleName}: ${e.message}"
            null
        }
    }

    /**
     * 保证拿到一个**当下可用**的 Copilot 令牌（过期自动续）。
     *
     * 与 Qoder 的 jobToken 同理：服务长驻，令牌会过期，必须每次转发前检查。
     */
    fun validCopilotToken(ctx: Context, cred: CopilotAuthStore.Cred, force: Boolean = false): String? {
        if (!force && cred.copilotFresh()) return cred.copilotToken
        val got = exchangeCopilotToken(cred.oauthToken) ?: return null
        CopilotAuthStore.updateCopilotToken(ctx, got.first, got.second)
        return got.first
    }

    /** 用户信息：用户名 + 套餐。失败不致命（只影响 UI 展示）。 */
    fun fetchUser(oauthToken: String): Triple<String, String, String>? {
        fun get(url: String): JSONObject? = try {
            val req = Request.Builder()
                .url(url)
                .header("Authorization", "token $oauthToken")
                .header("Accept", JSON)
                .header("Editor-Version", EDITOR_VERSION)
                .header("User-Agent", USER_AGENT)
                .build()
            client.newCall(req).execute().use { r ->
                if (!r.isSuccessful) null else JSONObject(r.body?.string().orEmpty())
            }
        } catch (_: Exception) {
            null
        }
        val user = get("$HOST_API/user") ?: return null
        val cp = get("$HOST_API/copilot_internal/user")
        return Triple(
            user.optString("login", ""),
            cp?.optString("access_type_sku", "").orEmpty(),
            cp?.optString("copilot_plan", "").orEmpty(),
        )
    }

    // ------------------------------------------------------------------
    // 模型列表
    // ------------------------------------------------------------------

    /** 最近一次模型列表的原文（排查用，与 Qoder 的 lastCatalogRaw 同思路）。 */
    @Volatile
    var lastModelsRaw: String? = null
        private set

    /**
     * 拉 Copilot 的模型目录（`GET /models`）。
     *
     * 返回原文，交给 [CopilotModelCatalog] 解析 —— 与 Qoder 一样把"取"和"解析"分开，
     * 上游改结构时还能看到原文。
     */
    fun fetchModels(copilotToken: String): String? {
        val req = Request.Builder()
            .url("$HOST_INFER/models")
            .header("Authorization", "Bearer $copilotToken")
            .header("Accept", JSON)
            .header("Copilot-Integration-Id", INTEGRATION_ID)
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", PLUGIN_VERSION)
            .header("User-Agent", USER_AGENT)
            .build()
        return try {
            client.newCall(req).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                lastModelsRaw = text
                if (!resp.isSuccessful) {
                    lastError = "模型列表 HTTP ${resp.code}: ${text.take(200)}"
                    null
                } else {
                    text
                }
            }
        } catch (e: Exception) {
            lastError = "模型列表失败: ${e.javaClass.simpleName}"
            null
        }
    }

    // ------------------------------------------------------------------
    // 自检
    // ------------------------------------------------------------------

    fun selfCheck() {
        // 兜底 IP 列表必须是合法地址（写错会让 github.com 直接不可用）
        for (ip in GITHUB_FALLBACK_IPS) {
            check(Regex("^\\d{1,3}(\\.\\d{1,3}){3}$").matches(ip)) { "自检失败：兜底 IP 格式不对: $ip" }
        }
        check(GITHUB_FALLBACK_IPS.size >= 2) { "自检失败：兜底 IP 至少要两个，单个挂了就没了" }

        // 端点拼接
        check(HOST_WEB.startsWith("https://")) { "自检失败：设备码必须走 https" }
        check(CLIENT_ID.isNotBlank()) { "自检失败：client_id 不能为空" }

        // 轮询结果分类：pending/slow_down 都不是错误，但 slow_down 必须退避
        check(pendingLike("authorization_pending")) { "自检失败：authorization_pending 应视为等待" }
        check(pendingLike("slow_down")) { "自检失败：slow_down 不该算失败" }
        check(!pendingLike("expired_token")) { "自检失败：expired_token 是失败" }
        check(!pendingLike("")) { "自检失败：空错误码不是等待" }
        // 退避步长：+5 秒，且要有个上限免得等到授权过期
        check(nextInterval(5000) == 10_000) { "自检失败：slow_down 后间隔应 +5s" }
        check(nextInterval(60_000) == 60_000) { "自检失败：退避要有上限" }
    }

    /** 自检辅助：这两类错误码都表示"继续等"，都不该终止登录。 */
    private fun pendingLike(error: String): Boolean =
        error == "authorization_pending" || error == "slow_down"

    /**
     * `slow_down` 之后的轮询间隔：每次 +5 秒，封顶 60 秒。
     *
     * 封顶的理由：设备码本身 15 分钟就过期，间隔再大就永远等不到授权结果了。
     */
    fun nextInterval(currentMs: Long): Long = (currentMs + 5_000).coerceAtMost(60_000)
}
