package com.example.traework2api

import android.content.Context
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.util.concurrent.TimeUnit

/**
 * Qoder 聊天上游（COSY `/algo/` 签名链路）→ OpenAI chat.completion.chunk。
 *
 * 与 [TraeUpstream] 并列的第二条转发后端。区别：
 *  - Trae 走 `solo_work_remote` + 裸 `Cloud-IDE-JWT`，SSE 事件是 `output/done`；
 *  - Qoder 走 `/algo/...agent_chat_generation`，**必须**带 COSY 签名头，SSE 帧是
 *    `{statusCodeValue, body}` 的**信封**，真正的 chunk 在 `body` 字符串里。
 *
 * 请求体不是 OpenAI 原样：要包成 `business/chat_task/model_config/...` 那一套
 * （见 [prepareBody]），再整体过 [QoderCosyCrypto.encodeBody] 编码。
 *
 * 输出格式与 [TraeUpstream] **完全一致**（`data: {...}\n\n` + 末尾 `data: [DONE]`），
 * 这样 [LocalFakeServer] 的流式/非流式聚合、换号、用量统计都不用改。
 *
 * token 只进内存与请求头，绝不落日志。
 */
object QoderUpstream {

    private const val TAG = "QoderUpstream"

    /** 参考实现 `callUpstream` 的 maxAttempts。 */
    private const val MAX_ATTEMPTS = 3

    /** 重试等待上限（参考实现 30s）。 */
    private const val MAX_RETRY_WAIT_MS = 30_000L

    /** 客户端没给 max_tokens 时的默认值（与参考实现一致）。 */
    private const val DEFAULT_MAX_TOKENS = 32_000

    private val client = OkHttpClient.Builder()
        .connectTimeout(20, TimeUnit.SECONDS)
        .readTimeout(180, TimeUnit.SECONDS)
        .build()

    // ------------------------------------------------------------------
    // 请求体构造（OpenAI → Qoder remote chat ask）
    // ------------------------------------------------------------------

    /**
     * 把客户端的 OpenAI 请求体改写成 Qoder 上游要的形状。
     *
     * 要点（每条都是踩过的坑）：
     *  - `business` 必带：部分模型（如 ultimate）路由到的节点缺它会直接
     *    `[FAIL]node:... Execution failed: null`。
     *  - system 提示词要**同时**放进顶层 `system` 和 `messages[0]`：
     *    上游实际读的是 messages[0]，只给顶层字段会被静默丢弃。
     *  - `contents` 只在「role != tool 且 content 是非空字符串」时才有；
     *    带 tool_calls 的消息必须保留 `content`（可为空串）。
     *  - 必须补一个 prompt-cache 标记（`cache_control: ephemeral`），
     *    否则整段前缀都不进缓存，长对话每次全价。
     */
    fun prepareBody(src: JSONObject): JSONObject {
        val resolved = QoderModelCatalog.resolve(src.optString("model", ""))

        val sysParts = ArrayList<String>()
        val upMessages = JSONArray()
        val msgs = src.optJSONArray("messages") ?: JSONArray()
        for (i in 0 until msgs.length()) {
            val m = msgs.optJSONObject(i) ?: continue
            val role = m.optString("role", "")
            if (role == "system" || role == "developer") {
                (m.opt("content") as? String)?.let { if (it.isNotEmpty()) sysParts.add(it) }
                continue
            }
            upMessages.put(toUpMessage(m, role))
        }
        ensureCacheMarker(upMessages)
        val system = sysParts.joinToString("\n\n")

        val requestId = java.util.UUID.randomUUID().toString()
        val body = JSONObject()
            .put("business", businessInfo(upMessages))
            .put("request_id", requestId)
            .put("request_set_id", requestId)
            .put("chat_record_id", requestId)
            .put("session_id", QoderModelCatalog.sessionFor(resolved.key))
            .put("stream", true)
            .put("chat_task", "FREE_INPUT")
            .put("chat_context", JSONObject())
            .put("is_reply", true)
            .put("is_retry", false)
            .put("source", 1)
            .put("version", "3")
            .put("agent_id", "agent_common")
            .put("task_id", "common")
            .put("session_type", "qodercli")
            .put("aliyun_user_type", "")
            .put("model_config", modelConfigJson(resolved.model))
            .put("custom_model", JSONObject.NULL)
            .put("system", system)
            .put("messages", upMessages)
            .put("tools", src.optJSONArray("tools") ?: JSONArray())
            .put("parameters", parameters(src))
        return body
    }

    private fun businessInfo(msgs: JSONArray): JSONObject {
        val name = lastUserText(msgs).take(10)
        return JSONObject()
            .put("product", "cli")
            .put("version", QoderCosyCrypto.COSY_VERSION)
            .put("type", "agent")
            .put("id", java.util.UUID.randomUUID().toString())
            .put("name", name)
            .put("begin_at", System.currentTimeMillis())
            .put("stage", "start")
    }

    /** 最后一条 user 消息的文本（用作会话标题）。 */
    private fun lastUserText(msgs: JSONArray): String {
        for (i in msgs.length() - 1 downTo 0) {
            val m = msgs.optJSONObject(i) ?: continue
            if (m.optString("role") != "user") continue
            val c = m.opt("content")
            if (c is String && c.isNotEmpty()) return c
            val parts = m.optJSONArray("contents") ?: continue
            for (j in 0 until parts.length()) {
                val p = parts.optJSONObject(j) ?: continue
                if (p.optString("type") == "text" && p.optString("text").isNotEmpty()) return p.optString("text")
            }
        }
        return ""
    }

    private fun modelConfigJson(m: QoderModelCatalog.QModel): JSONObject = JSONObject()
        .put("key", m.key)
        .put("format", "openai")
        .put("source", "system")
        .put("enable", true)
        .put("display_name", m.name)
        .put("is_vl", true)
        .put("is_reasoning", m.reasoning)
        // 用**官方标注的真实倍率**（来自上游目录），不要写死 1.0：
        // 写死会让上游按错误倍率计费/路由，也让免费模型看起来像收费。
        // 未知倍率按 1.0 发（参考实现一直发 1.0 且可用）；已知则发官方真实值。
        .put("price_factor", m.priceFactor ?: 1.0)

    /** 采样参数。缺省 max_tokens 用 32000（与参考实现一致，不是无限）。 */
    private fun parameters(src: JSONObject): JSONObject {
        val p = JSONObject()
        val maxTok = when {
            src.optInt("max_completion_tokens", 0) > 0 -> src.optInt("max_completion_tokens")
            src.optInt("max_tokens", 0) > 0 -> src.optInt("max_tokens")
            else -> DEFAULT_MAX_TOKENS
        }
        p.put("max_tokens", maxTok)
        normalizeEffort(src.optString("reasoning_effort", ""))?.let { p.put("reasoning_effort", it) }
        numOrNull(src, "temperature")?.let { p.put("temperature", it) }
        numOrNull(src, "top_p")?.let { p.put("top_p", it) }
        stopFrom(src.opt("stop"))?.let { p.put("stop", it) }
        if (src.has("tool_choice") && !src.isNull("tool_choice")) p.put("tool_choice", src.get("tool_choice"))
        return p
    }

    private fun numOrNull(o: JSONObject, key: String): Double? {
        if (!o.has(key) || o.isNull(key)) return null
        return try { o.getDouble(key) } catch (_: Exception) { null }
    }

    /** `minimal` 归一为 `low`；不在白名单里的直接丢弃（发过去会被上游拒）。 */
    private fun normalizeEffort(e: String): String? = when (e) {
        "minimal" -> "low"
        "none", "low", "medium", "high", "xhigh", "max" -> e
        else -> null
    }

    /** `stop` 可能是字符串或字符串数组；统一成数组（上游只认数组）。 */
    private fun stopFrom(raw: Any?): JSONArray? = when (raw) {
        is String -> if (raw.isEmpty()) null else JSONArray().put(raw)
        is JSONArray -> if (raw.length() == 0) null else raw
        else -> null
    }

    private fun toUpMessage(m: JSONObject, role: String): JSONObject {
        val o = JSONObject().put("role", role)
        m.optString("name", "").takeIf { it.isNotEmpty() }?.let { o.put("name", it) }
        m.optString("tool_call_id", "").takeIf { it.isNotEmpty() }?.let { o.put("tool_call_id", it) }

        val contentRaw = if (m.has("content") && !m.isNull("content")) m.get("content") else null
        // contents 是官方客户端并行发送的结构化副本；tool 角色不发（上游会当成重复内容）
        if (contentRaw is String && contentRaw.isNotEmpty() && role != "tool") {
            o.put("contents", JSONArray().put(JSONObject().put("type", "text").put("text", contentRaw)))
        }
        val toolCalls = m.optJSONArray("tool_calls")
        if (toolCalls != null && toolCalls.length() > 0) {
            o.put("tool_calls", toolCalls)
            // 带工具调用时 content 必须是字符串（可为空），null 会让上游解析失败
            o.put("content", contentRaw ?: "")
        } else if (contentRaw != null) {
            o.put("content", contentRaw)
        }
        return o
    }

    /**
     * 保证整段 prompt 里**恰好一个** ephemeral 缓存标记：放在最后一条非 tool 消息的
     * 最后一个可缓存 part 上。客户端已经带了就不动。
     */
    private fun ensureCacheMarker(msgs: JSONArray) {
        for (i in 0 until msgs.length()) {
            val parts = msgs.optJSONObject(i)?.optJSONArray("contents") ?: continue
            for (j in 0 until parts.length()) {
                if (parts.optJSONObject(j)?.has("cache_control") == true) return
            }
        }
        for (i in msgs.length() - 1 downTo 0) {
            val m = msgs.optJSONObject(i) ?: continue
            if (m.optString("role") == "tool") continue
            val parts = m.optJSONArray("contents") ?: continue
            for (j in parts.length() - 1 downTo 0) {
                val p = parts.optJSONObject(j) ?: continue
                val t = p.optString("type")
                if (t == "text" || t == "image_url") {
                    p.put("cache_control", JSONObject().put("type", "ephemeral"))
                    return
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 运行时字段缓存（每次登录生成一次）
    // ------------------------------------------------------------------

    /**
     * key = `uid|jobToken`。
     *
     * 为什么要带上 jobToken：AES 明文里含 `security_oauth_token`，
     * token 一换旧材料就作废。带上它，续期后自动重新生成，不用手动失效。
     */
    private val runtimeCache = HashMap<String, QoderCosyCrypto.RuntimeFields>()

    @Synchronized
    private fun runtimeFieldsFor(cred: QoderAuthStore.QCred, jobToken: String): QoderCosyCrypto.RuntimeFields {
        val key = "${cred.uid}|$jobToken"
        runtimeCache[key]?.let { return it }
        if (runtimeCache.size > 8) runtimeCache.clear()
        val rf = QoderCosyCrypto.generateRuntimeFields(
            uid = cred.uid,
            jobToken = jobToken,
            organizationId = cred.organizationId,
            organizationTags = cred.organizationTags,
            dataPolicyAgreed = cred.dataPolicyAgreed,
        )
        runtimeCache[key] = rf
        return rf
    }

    // ------------------------------------------------------------------
    // 转发主流程
    // ------------------------------------------------------------------

    /**
     * 发一次聊天请求并把结果以 OpenAI SSE 帧写进 [out]。
     *
     * @param refreshJobToken 401 时用来换新 jobToken 的回调（返回 null 表示换不动）
     * @param onResult        (usage, errType)：errType 形如 `http:401` / `cosy:xxx` / `transport:IOException`
     */
    fun chatStream(
        context: Context,
        clientBody: JSONObject,
        cred: QoderAuthStore.QCred,
        out: (String) -> Unit,
        onResult: (JSONObject?, String?) -> Unit,
        refreshJobToken: (() -> String?)? = null,
    ) {
        val clientModel = clientBody.optString("model", "")
        val upstreamBody = try {
            prepareBody(clientBody)
        } catch (e: Exception) {
            emitError(out, "请求体构造失败: ${e.javaClass.simpleName}")
            out("data: [DONE]\n\n")
            onResult(null, "body:${e.javaClass.simpleName}")
            return
        }
        // 每次尝试实际使用的模型 key：排队降级时会临时改它（请求体里的 model_config 同步改）
        var modelKey = upstreamBody.optJSONObject("model_config")?.optString("key", "")
            ?: QoderModelCatalog.DEFAULT_KEY
        var jobToken = cred.jobToken
        var settled = false
        fun settle(usage: JSONObject?, err: String?) {
            if (!settled) { settled = true; onResult(usage, err) }
        }

        var encoded = QoderCosyCrypto.encodeBody(upstreamBody.toString().toByteArray(Charsets.UTF_8))
        var encodedBodyStr = String(encoded, Charsets.US_ASCII)
        for (attempt in 1..MAX_ATTEMPTS) {
            // 每次尝试都要新的 requestId / 时间戳 / 签名：上游按 (requestId, date) 防重放。
            val unixSeconds = (System.currentTimeMillis() / 1000).toString()
            val runtime = runtimeFieldsFor(cred, jobToken)
            val payloadB64 = QoderCosyCrypto.authorizationPayload(
                QoderModelCatalog.newRequestId(),
                runtime.encryptUserInfo,
            )
            val authorization = QoderCosyCrypto.authorization(payloadB64, runtime.key, unixSeconds, encodedBodyStr)
            val req = try {
                buildRequest(context, cred, authorization, runtime.key, unixSeconds, encoded, modelKey)
            } catch (e: Exception) {
                android.util.Log.w(TAG, "构造请求失败: ${e.javaClass.simpleName}")
                settle(null, "prepare:${e.javaClass.simpleName}")
                return
            }

            val resp = try {
                client.newCall(req).execute()
            } catch (e: Exception) {
                android.util.Log.w(TAG, "上游传输异常(attempt $attempt): ${e.javaClass.simpleName}: ${e.message}")
                if (attempt < MAX_ATTEMPTS) {
                    sleepQuietly(attempt * 1000L)
                    continue
                }
                emitError(out, "上游连接失败: ${e.javaClass.simpleName}")
                settle(null, "transport:${e.javaClass.simpleName}")
                return
            }

            if (resp.isSuccessful) {
                var usage: JSONObject? = null
                val streamErr = resp.use { r ->
                    val br = BufferedReader(InputStreamReader(r.body!!.byteStream(), Charsets.UTF_8))
                    readEnvelopeSse(br, clientModel, out) { usage = it }
                }
                if (streamErr != null) {
                    android.util.Log.w(TAG, "上游流内错误: $streamErr")
                    emitError(out, streamErr)
                    out("data: [DONE]\n\n")
                    settle(usage, "cosy:$streamErr")
                } else {
                    out("data: [DONE]\n\n")
                    settle(usage, null)
                }
                return
            }

            val code = resp.code
            val raw = resp.use { it.body?.string().orEmpty() }
            val retryAfter = resp.header("Retry-After")

            // 401：只在第一次尝试时续 token 重来（续完还 401 就是账号问题，别再打）
            if (code == 401 && attempt == 1 && refreshJobToken != null) {
                val renewed = try { refreshJobToken.invoke() } catch (_: Exception) { null }
                if (!renewed.isNullOrBlank()) {
                    android.util.Log.i(TAG, "上游 401，已续期 jobToken 后重试")
                    jobToken = renewed
                    continue
                }
            }

            val queued = queueWaitMs(raw)
            val retryable = code == 408 || code == 429 || code >= 500 || queued != null
            if (retryable && attempt < MAX_ATTEMPTS) {
                if (queued != null) {
                    // 排队/资源受限：**别干等 30 秒**，直接换一个实测不排队的模型重试。
                    // 客户端指定了具体模型时不擅自更换（那会让用户拿到错模型的回答），
                    // 只有当前用的就是排队重灾区（auto/ultimate 等）时才降级。
                    val fb = QoderModelCatalog.QUEUE_FALLBACK_KEYS.firstOrNull { it != modelKey }
                    if (fb != null && (modelKey == "auto" || modelKey == "ultimate" || modelKey == "performance")) {
                        android.util.Log.i(TAG, "上游排队($modelKey, ${queued}ms)，降级到 $fb 重试")
                        modelKey = fb
                        // 请求体里的 model_config.key 必须跟着换，否则上游仍按旧 key 路由
                        upstreamBody.optJSONObject("model_config")?.put("key", fb)
                        encoded = QoderCosyCrypto.encodeBody(upstreamBody.toString().toByteArray(Charsets.UTF_8))
                        encodedBodyStr = String(encoded, Charsets.US_ASCII)
                        continue
                    }
                    // 用户明确要这个模型 → 等一小会儿再试一次，但最多等 5 秒
                    sleepQuietly(queued.coerceAtMost(5_000L))
                    continue
                }
                val wait = retryAfterMs(retryAfter) ?: ((1L shl (attempt - 1)) * 1000L)
                android.util.Log.i(TAG, "上游 $code (attempt $attempt/$MAX_ATTEMPTS)，${wait}ms 后重试")
                sleepQuietly(wait.coerceAtMost(MAX_RETRY_WAIT_MS))
                continue
            }

            android.util.Log.w(TAG, "上游最终失败 $code: ${raw.take(200)}")
            emitError(out, "上游 HTTP $code: ${raw.take(200)}")
            out("data: [DONE]\n\n")
            settle(null, "http:$code")
            return
        }
        settle(null, "exhausted")
    }

    // ------------------------------------------------------------------
    // 官方模型目录（动态拉取，签名版）
    // ------------------------------------------------------------------

    /**
     * 用 COSY 签名直接拉官方**模型目录**：`GET {infer}/algo/api/v2/model/list`，空 body。
     *
     * 为什么走签名而不是 openapi 的 `/api/v1/models`：参考实现把数据源优先级写得很清楚 ——
     * **网关动态接口 > 本机客户端缓存 > 冻结快照**。动态接口与桌面版模型选择器同源，
     * 是唯一"此刻"准确的来源（key、倍率、上下文、是否免费都在里面）。
     * 走它就不需要用户去导 `catalog-v7`、也不需要用户点同步。
     *
     * 签名要点（与聊天请求同一套 COSY 方案，仅三处不同）：
     *  1. body 为空 → 参与签名的 body 也是空串；
     *  2. 签名路径是 `/api/v2/model/list`（**不含** `/algo` 前缀）；
     *  3. 不需要 `X-Model-Key` / `X-Model-Source`（没有具体模型）。
     *
     * @return 响应原文；失败返回 null，原因写进 [QoderAuthApi.lastCatalogError]
     */
    fun fetchModelListRaw(context: Context, cred: QoderAuthStore.QCred, jobToken: String): String? {
        val unixSeconds = (System.currentTimeMillis() / 1000).toString()
        val runtime = try {
            runtimeFieldsFor(cred, jobToken)
        } catch (e: Exception) {
            QoderAuthApi.noteCatalogAttempt(null, "生成签名材料失败: ${e.javaClass.simpleName}")
            return null
        }
        val payloadB64 = QoderCosyCrypto.authorizationPayload(
            QoderModelCatalog.newRequestId(),
            runtime.encryptUserInfo,
        )
        val authorization = QoderCosyCrypto.authorization(
            payloadB64, runtime.key, unixSeconds, "",
            QoderCosyCrypto.MODEL_LIST_SIGNED_PATH,
        )
        val machineId = QoderAuthStore.machineId(context)
        val url = QoderAuthApi.inferBase(cred.mode) + QoderCosyCrypto.NATIVE_MODEL_LIST_PATH

        val b = Request.Builder()
            .url(url)
            .get()
            .header("Accept", "application/json")
            .header("Authorization", authorization)
            .header("Cache-Control", "no-cache")
            .header("Cosy-Business-Product", QoderCosyCrypto.SCENE_BUSINESS_PRODUCT)
            .header("Cosy-Business-Type", QoderCosyCrypto.SCENE_BUSINESS_TYPE)
            .header("Cosy-ClientType", QoderCosyCrypto.SCENE_CLIENT_TYPE)
            .header("Cosy-Data-Policy", if (cred.dataPolicyAgreed) "agree" else "disagree")
            .header("Cosy-Date", unixSeconds)
            .header("Cosy-Key", runtime.key)
            .header("Cosy-MachineId", machineId)
            .header("Cosy-MachineOS", MACHINE_OS)
            .header("Cosy-Scene", QoderCosyCrypto.SCENE_SCENE)
            .header("Cosy-User", cred.uid)
            .header("Cosy-Version", QoderCosyCrypto.COSY_VERSION)
            .header("Login-Version", "v2")
        if (cred.organizationId.isNotEmpty()) b.header("Cosy-Organization-Id", cred.organizationId)
        if (cred.organizationTags.isNotEmpty()) {
            b.header("Cosy-Organization-Tags", cred.organizationTags.joinToString(","))
        }

        return try {
            client.newCall(b.build()).execute().use { resp ->
                val text = resp.body?.string().orEmpty()
                if (!resp.isSuccessful) {
                    android.util.Log.w(TAG, "模型目录 HTTP ${resp.code}: ${text.take(200)}")
                    QoderAuthApi.noteCatalogAttempt(text, "签名目录 HTTP ${resp.code}: ${text.take(200)}")
                    null
                } else {
                    QoderAuthApi.noteCatalogAttempt(text, null)
                    text
                }
            }
        } catch (e: Exception) {
            android.util.Log.w(TAG, "模型目录传输异常: ${e.javaClass.simpleName}: ${e.message}")
            QoderAuthApi.noteCatalogAttempt(null, "传输异常: ${e.javaClass.simpleName}")
            null
        }
    }

    private fun buildRequest(
        context: Context,
        cred: QoderAuthStore.QCred,
        authorization: String,
        cosyKey: String,
        unixSeconds: String,
        encoded: ByteArray,
        modelKey: String,
    ): Request {
        val machineId = QoderAuthStore.machineId(context)
        val url = QoderAuthApi.inferBase(cred.mode) +
            QoderCosyCrypto.NATIVE_INFER_PATH + QoderCosyCrypto.NATIVE_INFER_QUERY

        val b = Request.Builder()
            .url(url)
            .post(encoded.toRequestBody("application/json".toMediaType()))
            .header("Accept", "text/event-stream")
            .header("Authorization", authorization)
            .header("Cache-Control", "no-cache")
            .header("Content-Type", "application/json")
            .header("Cosy-Business-Product", QoderCosyCrypto.SCENE_BUSINESS_PRODUCT)
            .header("Cosy-Business-Type", QoderCosyCrypto.SCENE_BUSINESS_TYPE)
            .header("Cosy-ClientType", QoderCosyCrypto.SCENE_CLIENT_TYPE)
            .header("Cosy-Data-Policy", if (cred.dataPolicyAgreed) "agree" else "disagree")
            .header("Cosy-Date", unixSeconds)
            .header("Cosy-Key", cosyKey)
            .header("Cosy-MachineId", machineId)
            .header("Cosy-MachineOS", MACHINE_OS)
            .header("Cosy-Scene", QoderCosyCrypto.SCENE_SCENE)
            .header("Cosy-User", cred.uid)
            .header("Cosy-Version", QoderCosyCrypto.COSY_VERSION)
            .header("Login-Version", "v2")
            .header("X-Model-Key", modelKey)
            .header("X-Model-Source", "system")
        if (cred.organizationId.isNotEmpty()) b.header("Cosy-Organization-Id", cred.organizationId)
        if (cred.organizationTags.isNotEmpty()) {
            b.header("Cosy-Organization-Tags", cred.organizationTags.joinToString(","))
        }
        // ⚠️ 刻意不带 User-Agent（参考实现也没带：带 UA 反而与 COSY 签名身份不一致）。
        // ⚠️ 不设 Cosy-MachineToken / Cosy-MachineType：参考实现在发请求前把它们删掉了。
        // ⚠️ 不设 Connection: keep-alive —— OkHttp 自己管理连接复用，手写这个头会被忽略甚至报错。
        return b.build()
    }

    /**
     * 机器 OS 标签。参考实现是 `goarch_goos`（amd64→x86_64、arm64→aarch64、darwin→macos）。
     * 安卓真机绝大多数是 arm64，故按 ABI 取 `aarch64_android`（适配器选择，不是抓包值）。
     */
    private val MACHINE_OS: String by lazy {
        val abi = runCatching { android.os.Build.SUPPORTED_ABIS.firstOrNull() }.getOrNull().orEmpty()
        when {
            abi.contains("arm64") || abi.contains("aarch64") -> "aarch64_android"
            abi.contains("x86_64") -> "x86_64_android"
            abi.contains("armeabi") -> "arm_android"
            abi.contains("x86") -> "x86_android"
            else -> "aarch64_android"
        }
    }

    // ------------------------------------------------------------------
    // SSE 信封解析
    // ------------------------------------------------------------------

    /**
     * 解析 Qoder 的 SSE **信封**流。
     *
     * 帧格式：`event: <name>` / `data: {"statusCodeValue":200,"body":"<chunk json>"}`。
     *  - `event: finish` → 正常终止
     *  - `statusCodeValue` 既不是 0 也不是 200 → 错误（消息在 `body.message`）
     *  - `body` 里的 chunk 带 `error.message` → 错误
     *
     * @return 错误消息；null 表示正常结束
     */
    private fun readEnvelopeSse(
        br: BufferedReader,
        clientModel: String,
        out: (String) -> Unit,
        onUsage: (JSONObject) -> Unit,
    ): String? {
        val event = StringBuilder()
        val data = StringBuilder()
        var error: String? = null
        var stopped = false
        val indexer = ToolIndexer()

        fun flush(): Boolean {
            if (data.isEmpty() && event.isEmpty()) return true
            val ev = event.toString()
            val payload = data.toString().trim()
            event.setLength(0)
            data.setLength(0)
            if (ev == "finish") return false
            if (payload.isEmpty()) return true
            val env = try { JSONObject(payload) } catch (_: Exception) { return true }

            val status = env.optInt("statusCodeValue", 0)
            val envBody = env.optString("body", "")
            if (status != 0 && status != 200) {
                var msg = ""
                try { msg = JSONObject(envBody).optString("message", "") } catch (_: Exception) {}
                error = msg.ifEmpty { "upstream error status $status" }
                return false
            }
            if (envBody.isEmpty()) return true
            val chunk = try { JSONObject(envBody) } catch (_: Exception) { return true }
            chunk.optJSONObject("error")?.let {
                val msg = it.optString("message", "")
                if (msg.isNotEmpty()) { error = msg; return false }
            }
            chunk.optJSONObject("usage")?.let(onUsage)
            indexer.remap(chunk)
            chunk.put("model", clientModel)
            out("data: ${chunk.toString()}\n\n")
            return true
        }

        while (true) {
            val line = try { br.readLine() } catch (_: Exception) { null } ?: break
            val trim = line.trimEnd('\r')
            if (trim.isEmpty()) {
                if (!flush()) { stopped = true; break }
            } else if (trim.startsWith("event:")) {
                if (event.isNotEmpty()) event.append(' ')
                event.append(trim.removePrefix("event:").trim())
            } else if (trim.startsWith("data:")) {
                var d = trim.removePrefix("data:")
                if (d.startsWith(" ")) d = d.substring(1)   // 只剥**一个**前导空格
                if (data.isNotEmpty()) data.append('\n')
                data.append(d)
            }
        }
        if (!stopped) flush()
        return error
    }

    /**
     * 重排 tool_call 的 index。
     *
     * 上游给每个并行工具调用都发 `index: 0`，只靠新 `id` 区分 —— 客户端按 index 累加
     * 会把两个不同的工具合并成一个。这里遇到新 id 就 +1。
     */
    private class ToolIndexer {
        private var cur = 0
        private var seen = false

        fun remap(chunk: JSONObject) {
            val choices = chunk.optJSONArray("choices") ?: return
            for (ci in 0 until choices.length()) {
                val delta = choices.optJSONObject(ci)?.optJSONObject("delta") ?: continue
                val tcs = delta.optJSONArray("tool_calls") ?: continue
                for (i in 0 until tcs.length()) {
                    val tc = tcs.optJSONObject(i) ?: continue
                    if (tc.optString("id", "").isNotEmpty()) {
                        if (seen) cur++ else seen = true
                    }
                    tc.put("index", cur)
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 错误输出 / 重试辅助
    // ------------------------------------------------------------------

    /** 与 Trae 一致：`event: error` + `data: [DONE]`（LocalFakeServer 会据此判定"没成功"）。 */
    private fun emitError(out: (String) -> Unit, message: String) {
        val err = JSONObject().put("error", JSONObject().put("message", message).put("type", "api_error"))
        out("event: error\ndata: ${JSONObject.quote(message)}\n\n")
        out("data: ${err.toString()}\n\n")
    }

    /**
     * 上游排队/过载时会在 body 里给等待时间：
     * `{"code":"10605","retryAfterMs":ms}` 或 `{"queue":{"isQueued":true,"waitTime":ms}}`。
     */
    /**
     * 上游"排队/资源受限"的等待毫秒；不是这类错误返回 null。
     *
     * ⚠️ 这里踩过坑（实测 2026-10-26）：`auto` 等热门模型的 403 响应长这样 ——
     * `{"code":"403","message":"{\"code\":\"10605\",\"message\":
     *   \"{\\\"isQueued\\\":true,\\\"modelKey\\\":\\\"auto\\\",...,
     *     \\\"retryAfterSeconds\\\":30,\\\"subCode\\\":\\\"RESOURCE_LIMIT\\\"}\"}"}`
     *
     * 也就是关键信息在**多层转义的 JSON 字符串**里，顶层根本没有 `queue` 字段。
     * 旧实现只看顶层 `optJSONObject("queue")`，于是永远返回 null →
     * 被当成不可重试的致命错误直接报给客户端（"一次都用不了"就是这么来的）。
     *
     * 现在把整串原文当文本扫：只要出现 `isQueued:true` 或 `subCode:RESOURCE_LIMIT`，
     * 就按"可重试"处理，并从 `retryAfterSeconds` / `waitTime` 里取等待时间。
     */
    private fun queueWaitMs(raw: String): Long? {
        val looksQueued = raw.contains("\"isQueued\":true") ||
            raw.contains("RESOURCE_LIMIT") ||
            raw.contains("\"code\":\"10605\"") ||
            raw.contains("10605")
        if (!looksQueued) {
            // 仍然兼容"规规矩矩给了 queue 对象"的形状
            val o = try { JSONObject(raw) } catch (_: Exception) { return null }
            val queue = o.optJSONObject("queue")
            if (queue != null && queue.optBoolean("isQueued", false)) {
                val ms = queue.optLong("waitTime", 0L)
                return if (ms > 0) ms else 2_000L
            }
            return null
        }
        // 从转义文本里抠秒数：retryAfterSeconds / waitTime / retryAfterMs 都认
        fun num(key: String): Long? {
            val m = Regex("\\*\"" + key + "\\*\"\\s*:\\s*(\\d+)").find(raw) ?: return null
            return m.groupValues[1].toLongOrNull()
        }
        num("retryAfterSeconds")?.let { return (it * 1000L).coerceAtLeast(1_000L) }
        num("waitTime")?.let { return (it * 1000L).coerceAtLeast(1_000L) }
        num("retryAfterMs")?.let { return it.coerceAtLeast(1_000L) }
        return 2_000L
    }

    /** `Retry-After: 5` → 5000ms（只认秒，与参考实现 `ParseDuration(+"s")` 一致）。 */
    private fun retryAfterMs(value: String?): Long? {
        val secs = value?.trim()?.toDoubleOrNull() ?: return null
        return (secs * 1000.0).toLong()
    }

    private fun sleepQuietly(ms: Long) {
        try { Thread.sleep(ms) } catch (_: InterruptedException) { Thread.currentThread().interrupt() }
    }

    // ------------------------------------------------------------------
    // 自检（纯计算，无网络）
    // ------------------------------------------------------------------

    fun selfCheck() {
        val src = JSONObject(
            """
            {"model":"Ultimate","stream":false,
             "messages":[
               {"role":"system","content":"你是助手"},
               {"role":"developer","content":"要简洁"},
               {"role":"user","content":"你好"},
               {"role":"assistant","content":null,"tool_calls":[
                  {"id":"t1","type":"function","function":{"name":"f","arguments":"{}"}}]},
               {"role":"tool","tool_call_id":"t1","content":"结果"}
             ],
             "max_tokens":123,"temperature":0.3,"top_p":0.9,"stop":["\n"],
             "tool_choice":"auto","reasoning_effort":"minimal"}
            """.trimIndent(),
        )
        val body = prepareBody(src)

        // 顶层必带字段
        for (k in listOf("business", "request_id", "request_set_id", "chat_record_id", "session_id",
            "chat_task", "model_config", "messages", "tools", "parameters", "system", "version")) {
            check(body.has(k)) { "自检失败：上游请求体缺少 $k" }
        }
        check(body.getBoolean("stream")) { "自检失败：stream 必须为 true" }
        check(body.getString("chat_task") == "FREE_INPUT") { "自检失败：chat_task 不对" }
        check(body.getString("session_type") == "qodercli") { "自检失败：session_type 不对" }
        check(body.getString("version") == "3") { "自检失败：version 不对" }

        // 模型解析：展示名 Ultimate → key ultimate
        check(body.getJSONObject("model_config").getString("key") == "ultimate") {
            "自检失败：Ultimate 未解析成 ultimate"
        }

        // system 拼接：两条用 \n\n 连接，且 messages[0] 也必须是 system（上游只读那里）
        check(body.getString("system") == "你是助手\n\n要简洁") {
            "自检失败：system 拼接不对，得到 [${body.optString("system")}]"
        }
        val msgs = body.getJSONArray("messages")
        check(msgs.length() == 3) { "自检失败：system/developer 应被抽走，剩 3 条，实际 ${msgs.length()}" }

        // 带 tool_calls 的消息：content 必须是字符串（空串），tool_calls 保留
        val assistant = msgs.getJSONObject(1)
        check(assistant.has("tool_calls")) { "自检失败：tool_calls 丢了" }
        check(assistant.opt("content") is String && assistant.getString("content").isEmpty()) {
            "自检失败：带 tool_calls 时 content 必须是空串"
        }

        // tool 角色不发 contents（上游会当成重复内容）
        val toolMsg = msgs.getJSONObject(2)
        check(toolMsg.optString("role") == "tool") { "自检失败：tool 消息角色不对" }
        check(!toolMsg.has("contents")) { "自检失败：tool 角色不该发 contents" }
        check(toolMsg.getString("tool_call_id") == "t1") { "自检失败：tool_call_id 丢了" }

        // prompt 缓存标记：必须恰好补一个，且落在最后一条非 tool 消息上（这里是 user）
        val lastCacheable = msgs.getJSONObject(0)
        val parts = lastCacheable.getJSONArray("contents")
        check(parts.getJSONObject(0).optJSONObject("cache_control")?.optString("type") == "ephemeral") {
            "自检失败：未补 prompt-cache 标记"
        }

        // 参数
        val params = body.getJSONObject("parameters")
        check(params.getInt("max_tokens") == 123) { "自检失败：max_tokens 未透传" }
        check(params.optString("reasoning_effort") == "low") { "自检失败：minimal 应归一为 low" }
        check(kotlin.math.abs(params.getDouble("temperature") - 0.3) < 1e-9) { "自检失败：temperature 未透传" }
        check(params.getJSONArray("stop").getString(0) == "\n") { "自检失败：stop 未归一成数组" }

        // business.name 取最后一条 user 文本且截断 10 字
        check(body.getJSONObject("business").getString("name") == "你好") {
            "自检失败：business.name 不对"
        }

        // 排队等待解析
        check(queueWaitMs("""{"code":"10605","retryAfterMs":1500}""") == 1500L) {
            "自检失败：10605 排队未识别"
        }
        check(queueWaitMs("""{"queue":{"isQueued":true,"waitTime":3000}}""") == 3000L) {
            "自检失败：queue.isQueued 未识别"
        }
        check(queueWaitMs("""{"code":"ok"}""") == null) { "自检失败：普通响应不该判为排队" }

        // tool index 重排：两个新 id 必须得到 0、1
        val chunk = JSONObject(
            """{"choices":[{"delta":{"tool_calls":[
                 {"id":"a","index":0,"function":{"name":"f1","arguments":"{}"}},
                 {"id":"b","index":0,"function":{"name":"f2","arguments":"{}"}}]}}]}""",
        )
        ToolIndexer().remap(chunk)
        val tcs = chunk.getJSONArray("choices").getJSONObject(0).getJSONObject("delta").getJSONArray("tool_calls")
        check(tcs.getJSONObject(0).getInt("index") == 0 && tcs.getJSONObject(1).getInt("index") == 1) {
            "自检失败：并行 tool_call 的 index 未重排"
        }
    }
}
