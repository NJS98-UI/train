package com.example.traework2api

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import org.json.JSONArray
import org.json.JSONObject
import java.nio.charset.StandardCharsets
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Qoder 账号的**多账号**加密存取（与 [TraeAuthStore] 同一套模式，独立 prefs / 独立 KeyStore 别名）。
 *
 * Qoder 桌面/CLI 版对外可用的凭据是 **PAT（`pt-`）**：它不能直接调 API，要先换短时的
 * **jobToken（`jt-`）**（见 [QoderAuthApi]）。这里三类凭据都加密存：
 *   - PAT：长期有效，能随时重新换 jobToken，也是"换设备一键登录"的关键（导出它即可在新设备再换）。
 *   - jobToken / jobRefreshToken：短时会话，轮询积分时优先复用，过期再用 jrt 续、再不行用 PAT 换。
 *
 * 存储布局（同一个 prefs）：
 *   qoder_accounts           → 账号索引 JSON 数组（非敏感元数据：uid/name/email/mode/expiresAt）
 *   q_acc_<uid>_pat / _jt / _jrt → 三类凭据密文
 *   q_active_uid             → 当前选中的 Qoder 账号 uid
 *
 * ⚠️ Qoder 账号目前**只用于积分检测 / 换号 / 导出导入**，不参与本 App 的 OpenAI 转发
 * （转发仍是 Trae；Qoder 聊天需要 COSY 签名，未接入）。故它不参与 [Gateway] 的 credProvider。
 */
object QoderAuthStore {

    private const val KEY_ALIAS = "qoder_aes_key"
    private const val PREF_NAME = "qoder2api_prefs"
    private const val SEP = ":"

    private const val KEY_INDEX = "qoder_accounts"
    private const val KEY_ACTIVE = "q_active_uid"

    /** 导出 JSON 的格式标识。 */
    const val EXPORT_FORMAT = "traework2api-qoder-accounts"

    /** Qoder 单账号元数据（不含任何 token）。 */
    data class QMeta(
        val uid: String,
        val name: String,
        val email: String,
        val mode: String,      // "cn" | "global"
        val expiresAt: Long,   // jobToken 到期，unix **毫秒**；0=未知
    )

    /** Qoder 账号完整凭证（内存中才有 token）。 */
    data class QCred(
        val pat: String,
        val jobToken: String,
        val jobRefreshToken: String,
        val expiresAt: Long,   // unix 毫秒
        val uid: String,
        val name: String,
        val email: String,
        val mode: String,
    ) {
        fun toMeta() = QMeta(uid, name, email, mode, expiresAt)
    }

    fun list(context: Context): List<QMeta> = readIndex(context)

    fun activeUid(context: Context): String? {
        val metas = readIndex(context)
        if (metas.isEmpty()) return null
        val saved = prefs(context).getString(KEY_ACTIVE, null)
        return saved?.takeIf { s -> metas.any { it.uid == s } } ?: metas.first().uid
    }

    fun setActive(context: Context, uid: String) {
        if (readIndex(context).none { it.uid == uid }) return
        prefs(context).edit().putString(KEY_ACTIVE, uid).apply()
    }

    fun load(context: Context): QCred? = activeUid(context)?.let { loadByUid(context, it) }

    fun loadByUid(context: Context, uid: String): QCred? {
        val p = prefs(context)
        val meta = readIndex(context).firstOrNull { it.uid == uid } ?: return null
        fun dec(key: String): String = p.getString(key, null)?.let { decrypt(it) }.orEmpty()
        val pat = dec(patKey(uid))
        val jt = dec(jtKey(uid))
        val jrt = dec(jrtKey(uid))
        if (pat.isBlank() && jt.isBlank()) return null
        return QCred(pat, jt, jrt, meta.expiresAt, meta.uid, meta.name, meta.email, meta.mode)
    }

    /** 保存（新增或更新）一个账号并设为当前账号。 */
    fun save(context: Context, cred: QCred) {
        val p = prefs(context)
        p.edit()
            .putString(patKey(cred.uid), encrypt(cred.pat))
            .putString(jtKey(cred.uid), encrypt(cred.jobToken))
            .putString(jrtKey(cred.uid), encrypt(cred.jobRefreshToken))
            .apply()
        writeMeta(context, cred.toMeta())
        setActive(context, cred.uid)
    }

    /** 只更新 token（PAT/jt/jrt）与到期时间，不动当前选中账号。 */
    fun updateTokens(context: Context, cred: QCred) {
        if (readIndex(context).none { it.uid == cred.uid }) return
        prefs(context).edit()
            .putString(patKey(cred.uid), encrypt(cred.pat))
            .putString(jtKey(cred.uid), encrypt(cred.jobToken))
            .putString(jrtKey(cred.uid), encrypt(cred.jobRefreshToken))
            .apply()
        writeMeta(context, cred.toMeta())
    }

    fun remove(context: Context, uid: String) {
        val metas = readIndex(context).filterNot { it.uid == uid }
        val p = prefs(context)
        p.edit()
            .remove(patKey(uid)).remove(jtKey(uid)).remove(jrtKey(uid))
            .putString(KEY_INDEX, toJson(metas).toString())
            .apply()
        if (p.getString(KEY_ACTIVE, null) == uid) {
            p.edit().putString(KEY_ACTIVE, metas.firstOrNull()?.uid.orEmpty()).apply()
        }
    }

    // ------------------------------------------------------------------
    // 导出 / 导入（JSON 明文，含 PAT）
    // ------------------------------------------------------------------

    /** 导出全部 Qoder 账号为 JSON（⚠️ 含明文 PAT/jobToken/jrt，理由见 [TraeAuthStore.exportJson]）。 */
    fun exportJson(context: Context): String {
        val arr = JSONArray()
        for (m in readIndex(context)) {
            val c = loadByUid(context, m.uid) ?: continue
            arr.put(JSONObject()
                .put("uid", c.uid).put("name", c.name).put("email", c.email).put("mode", c.mode)
                .put("expiresAt", c.expiresAt)
                .put("pat", c.pat).put("jobToken", c.jobToken).put("jobRefreshToken", c.jobRefreshToken))
        }
        return JSONObject()
            .put("format", EXPORT_FORMAT).put("version", 1)
            .put("activeUid", activeUid(context).orEmpty()).put("accounts", arr)
            .toString(2)
    }

    /** 从 [exportJson] 的 JSON 导入（按 uid 合并，不动当前号除非带 activeUid）。返回导入数。 */
    fun importJson(context: Context, json: String): Int {
        val rootOpt = try { JSONObject(json) } catch (_: Exception) { null }
        val accounts = if (rootOpt != null) rootOpt.optJSONArray("accounts") ?: JSONArray()
        else try { JSONArray(json) } catch (e: Exception) { throw IllegalArgumentException("不是有效的 Qoder 账号 JSON") }
        var imported = 0
        for (i in 0 until accounts.length()) {
            val o = accounts.optJSONObject(i) ?: continue
            val uid = o.optString("uid", "").takeIf { it.isNotBlank() } ?: continue
            val pat = o.optString("pat", "")
            val jt = o.optString("jobToken", "")
            if (pat.isBlank() && jt.isBlank()) continue
            val cred = QCred(
                pat = pat, jobToken = jt, jobRefreshToken = o.optString("jobRefreshToken", ""),
                expiresAt = o.optLong("expiresAt", 0L), uid = uid,
                name = o.optString("name", ""), email = o.optString("email", ""),
                mode = o.optString("mode", QoderAuthApi.MODE_CN),
            )
            val p = prefs(context)
            p.edit()
                .putString(patKey(uid), encrypt(cred.pat))
                .putString(jtKey(uid), encrypt(cred.jobToken))
                .putString(jrtKey(uid), encrypt(cred.jobRefreshToken))
                .apply()
            writeMeta(context, cred.toMeta())
            imported++
        }
        check(imported > 0) { "JSON 里没有可导入的 Qoder 账号（缺 uid 或 pat）" }
        val active = rootOpt?.optString("activeUid", "")?.takeIf { it.isNotBlank() }
        if (active != null && readIndex(context).any { it.uid == active }) setActive(context, active)
        return imported
    }

    // ------------------------------------------------------------------
    // 索引读写
    // ------------------------------------------------------------------

    private fun prefs(context: Context) = context.getSharedPreferences(PREF_NAME, Context.MODE_PRIVATE)
    private fun patKey(uid: String) = "q_acc_${uid}_pat"
    private fun jtKey(uid: String) = "q_acc_${uid}_jt"
    private fun jrtKey(uid: String) = "q_acc_${uid}_jrt"

    private fun readIndex(context: Context): List<QMeta> {
        val raw = prefs(context).getString(KEY_INDEX, null) ?: return emptyList()
        return try {
            val arr = JSONArray(raw)
            (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                val uid = o.optString("uid", "").takeIf { it.isNotBlank() } ?: return@mapNotNull null
                QMeta(uid, o.optString("name", ""), o.optString("email", ""),
                    o.optString("mode", QoderAuthApi.MODE_CN), o.optLong("expiresAt", 0L))
            }
        } catch (_: Exception) { emptyList() }
    }

    private fun toJson(metas: List<QMeta>): JSONArray = JSONArray().apply {
        metas.forEach { m ->
            put(JSONObject().put("uid", m.uid).put("name", m.name).put("email", m.email)
                .put("mode", m.mode).put("expiresAt", m.expiresAt))
        }
    }

    private fun writeMeta(context: Context, meta: QMeta) {
        val metas = readIndex(context).toMutableList()
        val i = metas.indexOfFirst { it.uid == meta.uid }
        if (i >= 0) metas[i] = meta else metas.add(meta)
        prefs(context).edit().putString(KEY_INDEX, toJson(metas).toString()).apply()
    }

    // ------------------------------------------------------------------
    // AES-GCM（KeyStore 主密钥，别名与 Trae 分开）
    // ------------------------------------------------------------------

    private fun secretKey(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256).build()
        )
        return gen.generateKey()
    }

    private fun encrypt(plain: String): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, secretKey())
        val ct = cipher.doFinal(plain.toByteArray(StandardCharsets.UTF_8))
        return Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + SEP + Base64.encodeToString(ct, Base64.NO_WRAP)
    }

    private fun decrypt(b64: String): String? {
        return try {
            val parts = b64.split(SEP)
            if (parts.size != 2) return null
            val iv = Base64.decode(parts[0], Base64.NO_WRAP)
            val ct = Base64.decode(parts[1], Base64.NO_WRAP)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(128, iv))
            String(cipher.doFinal(ct), StandardCharsets.UTF_8)
        } catch (_: Exception) { null }
    }

    /** 纯计算自检：索引 JSON 往返无损（uid 是主键）。 */
    fun selfCheck() {
        val src = listOf(QMeta("q-1", "张三", "a@b.com", QoderAuthApi.MODE_CN, 1791341100000L))
        val arr = toJson(src)
        val back = (0 until arr.length()).map { i ->
            val o = arr.getJSONObject(i)
            QMeta(o.getString("uid"), o.getString("name"), o.getString("email"),
                o.getString("mode"), o.getLong("expiresAt"))
        }
        check(back == src) { "自检失败：Qoder 账号索引往返不一致" }
    }
}
