package com.example.traework2api

import android.content.Context
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONObject

/**
 * GitHub Copilot 的聊天转发（`api.githubcopilot.com`，OpenAI 兼容协议）。
 *
 * 与 Trae/Qoder 的关系：控制流（换号、流式/非流式聚合、错误上报）全部由
 * [LocalFakeServer.streamWithFailover] 统一处理，这里只负责"怎么打上游"。
 *
 * ## 与 OpenAI 官方接口的三处差别
 *
 * 1. 需要一组 `Copilot-Integration-Id` / `Editor-Version` / `Editor-Plugin-Version` 头，
 *    缺了会被当成未授权客户端（返回 400 而不是 401，容易误判）。
 * 2. 认证用的是**短时 Copilot 令牌**（`tid=…`），不是 GitHub 的 OAuth 令牌；
 *    过期就得用 OAuth 令牌重新换（见 [CopilotApi.validCopilotToken]）。
 * 3. `stream` 必须为 `true`（非流式由本地聚合，跟 Trae/Qoder 一致）。
 */
object CopilotUpstream {

    private const val TAG = "CopilotUpstream"

    private val JSON = "application/json".toMediaType()

    /**
     * 发一次聊天请求，把 OpenAI SSE 帧写进 [out]。
     *
     * @param refreshToken 401 时用来换新 Copilot 令牌的回调（返回 null 表示换不动）
     * @param onResult     (usage, errType)：errType 形如 `http:401` / `transport:IOException`
     */
    fun chatStream(
        context: Context,
        clientBody: JSONObject,
        cred: CopilotAuthStore.Cred,
        out: (String) -> Unit,
        onResult: (JSONObject?, String?) -> Unit,
        refreshToken: (() -> String?)? = null,
    ) {
        val model = clientBody.optString("model", "")
        // 上游只接受流式：非流式由调用方聚合（见 aggregateChunks）
        val upstreamBody = JSONObject(clientBody.toString()).apply {
            put("stream", true)
            // 让上游在最后一帧带 usage，否则本地统计不到 token 数
            put("stream_options", JSONObject().put("include_usage", true))
        }
        val payload = upstreamBody.toString().toRequestBody(JSON)

        var token = CopilotApi.validCopilotToken(context, cred)
        if (token.isNullOrBlank()) {
            val why = CopilotApi.lastError ?: "换不到 Copilot 令牌"
            android.util.Log.w(TAG, "无可用令牌: $why")
            emitError(out, "Copilot 令牌获取失败：$why")
            out("data: [DONE]\n\n")
            onResult(null, "token:${CopilotApi.lastError ?: "unknown"}")
            return
        }

        var settled = false
        fun settle(usage: JSONObject?, err: String?) {
            if (!settled) { settled = true; onResult(usage, err) }
        }

        for (attempt in 1..2) {
            val req = Request.Builder()
                .url("${CopilotApi.HOST_INFER}/chat/completions")
                .post(payload)
                .header("Authorization", "Bearer $token")
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("Copilot-Integration-Id", CopilotApi.INTEGRATION_ID)
                .header("Editor-Version", CopilotApi.EDITOR_VERSION)
                .header("Editor-Plugin-Version", CopilotApi.PLUGIN_VERSION)
                .header("User-Agent", CopilotApi.USER_AGENT)
                .header("X-Initiator", "user")
                .build()

            val resp = try {
                CopilotApi.streamClient.newCall(req).execute()
            } catch (e: Exception) {
                android.util.Log.w(TAG, "传输异常(attempt $attempt): ${e.javaClass.simpleName}: ${e.message}")
                emitError(out, "Copilot 连接失败: ${e.javaClass.simpleName}")
                out("data: [DONE]\n\n")
                settle(null, "transport:${e.javaClass.simpleName}")
                return
            }

            resp.use { r ->
                if (!r.isSuccessful) {
                    val raw = try { r.body?.string().orEmpty() } catch (_: Exception) { "" }
                    // 401/403：令牌可能刚过期，换一次再试
                    if ((r.code == 401 || r.code == 403) && attempt == 1 && refreshToken != null) {
                        val fresh = try { refreshToken() } catch (_: Exception) { null }
                        if (!fresh.isNullOrBlank()) {
                            token = fresh
                            android.util.Log.i(TAG, "令牌已刷新，重试")
                            return@use   // 走下一轮 attempt
                        }
                    }
                    android.util.Log.w(TAG, "上游 HTTP ${r.code}: ${raw.take(200)}")
                    emitError(out, "Copilot HTTP ${r.code}: ${raw.take(200)}")
                    out("data: [DONE]\n\n")
                    settle(null, "http:${r.code}")
                    return
                }

                val source = r.body?.source()
                if (source == null) {
                    emitError(out, "Copilot 响应为空")
                    out("data: [DONE]\n\n")
                    settle(null, "empty")
                    return
                }

                var usage: JSONObject? = null
                var sawData = false
                while (true) {
                    val line = try { source.readUtf8Line() } catch (e: Exception) {
                        android.util.Log.w(TAG, "读取中断: ${e.javaClass.simpleName}")
                        break
                    } ?: break
                    if (line.isEmpty()) {
                        // SSE 事件分隔：原样转发（客户端按空行分帧）
                        out("\n")
                        continue
                    }
                    // 只透传 data 帧与 event 行；注释行（`: ping`）也保留，无害
                    if (line.startsWith("data:")) {
                        sawData = true
                        val payloadStr = line.removePrefix("data:").trim()
                        if (payloadStr != "[DONE]") {
                            try {
                                JSONObject(payloadStr).optJSONObject("usage")?.let { usage = it }
                            } catch (_: Exception) {
                                // 非 JSON 帧（极少见）：照旧透传
                            }
                        }
                    }
                    out(line + "\n")
                }
                out("data: [DONE]\n\n")
                if (!sawData) {
                    settle(null, "empty-stream")
                } else {
                    settle(usage, null)
                }
                return
            }
        }
    }

    /** 与 Qoder 一致的错误帧形状：客户端能显示，聚合器也能抠出原文。 */
    private fun emitError(out: (String) -> Unit, message: String) {
        val escaped = JSONObject.quote(message)
        out("event: error\n")
        out("data: $escaped\n\n")
        out("data: " + JSONObject().put("error", JSONObject().put("message", message)).toString() + "\n\n")
    }
}
