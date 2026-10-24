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
 * ⚠️ Qoder 账号**既参与积分检测，也可以作为本地聊天转发的后端**（见 [QoderUpstream]）。
 * 聊天走 COSY `/algo/` 签名链路，需要 `organization_id` / `organization_tags` /
 * `data_policy_agreed` 这三样身份材料，所以它们也随索引一起持久化（都是非敏感字段）。
 * 默认后端仍是 Trae，用户在 Qoder 账号页显式切换后才走 Qoder（见 [GatewayBackend]）。
 */
object QoderAuthStore {

    private const val KEY_ALIAS = "qoder_aes_key"
    private const val PREF_NAME = "qoder2api_prefs"
    private const val SEP = ":"

    private const val KEY_INDEX = "qoder_accounts"
    private const val KEY_ACTIVE = "q_active_uid"

    /** 持久化的机器标识（COSY 头 `Cosy-MachineId` 用）。 */
    private const val KEY_MACHINE_ID = "q_machine_id"

    /** 导出 JSON 的格式标识。 */
    const val EXPORT_FORMAT = "traework2api-qoder-accounts"

    /** Qoder 单账号元数据（不含任何 token）。 */
    data class QMeta(
        val uid: String,
        val name: String,
        val email: String,
        val mode: String,      // "cn" | "global"
        val expiresAt: Long,   // jobToken 到期，unix **毫秒**；0=未知
        // ---- COSY 聊天签名所需的身份材料（非敏感，随索引明文存）----
        val organizationId: String = "",
        val organizationName: String = "",
        val organizationTags: List<String> = emptyList(),
        val dataPolicyAgreed: Boolean = false,
        val isDataPolicyModifiable: Boolean = true,
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
        // ---- COSY 聊天签名所需的身份材料 ----
        val organizationId: String = "",
        val organizationName: String = "",
        val organizationTags: List<String> = emptyList(),
        val dataPolicyAgreed: Boolean = false,
        val isDataPolicyModifiable: Boolean = true,
    ) {
        fun toMeta() = QMeta(
            uid, name, email, mode, expiresAt,
            organizationId, organizationName, organizationTags, dataPolicyAgreed, isDataPolicyModifiable,
        )

        /** 是否具备走 COSY 聊天的最小条件（uid + jobToken 必须有）。 */
        val chatReady: Boolean get() = uid.isNotBlank() && jobToken.isNotBlank()
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

    /**
     * 机器标识（COSY 聊天头 `Cosy-MachineId`）。
     *
     * 为什么必须**持久且稳定**：上游把 machine_id 当作设备身份的一部分，
     * 每次请求都换一个新值等于"每次都是新设备"，容易被风控盯上；而且参考实现的
     * 凭据加密密钥也取它前 16 字符，变了就解不开旧凭据。
     * 首次访问生成 UUID 并落盘（明文即可 —— 它不是密钥，只是个标识）。
     */
    @Synchronized
    fun machineId(context: Context): String {
        val p = prefs(context)
        p.getString(KEY_MACHINE_ID, null)?.takeIf { it.length >= 16 }?.let { return it }
        val id = java.util.UUID.randomUUID().toString()
        p.edit().putString(KEY_MACHINE_ID, id).apply()
        return id
    }

    fun loadByUid(context: Context, uid: String): QCred? {
        val p = prefs(context)
        val meta = readIndex(context).firstOrNull { it.uid == uid } ?: return null
        fun dec(key: String): String = p.getString(key, null)?.let { decrypt(it) }.orEmpty()
        val pat = dec(patKey(uid))
        val jt = dec(jtKey(uid))
        val jrt = dec(jrtKey(uid))
        if (pat.isBlank() && jt.isBlank()) return null
        return QCred(
            pat, jt, jrt, meta.expiresAt, meta.uid, meta.name, meta.email, meta.mode,
            meta.organizationId, meta.organizationName, meta.organizationTags,
            meta.dataPolicyAgreed, meta.isDataPolicyModifiable,
        )
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
                .put("organizationId", c.organizationId)
                .put("organizationName", c.organizationName)
                .put("organizationTags", JSONArray().apply { c.organizationTags.forEach { put(it) } })
                .put("dataPolicyAgreed", c.dataPolicyAgreed)
                .put("isDataPolicyModifiable", c.isDataPolicyModifiable)
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
                organizationId = o.optString("organizationId", ""),
                organizationName = o.optString("organizationName", ""),
                organizationTags = jsonToStringList(o.opt("organizationTags")),
                dataPolicyAgreed = o.optBoolean("dataPolicyAgreed", false),
                isDataPolicyModifiable = o.optBoolean("isDataPolicyModifiable", true),
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
                QMeta(
                    uid = uid,
                    name = o.optString("name", ""),
                    email = o.optString("email", ""),
                    mode = o.optString("mode", QoderAuthApi.MODE_CN),
                    expiresAt = o.optLong("expiresAt", 0L),
                    organizationId = o.optString("organizationId", ""),
                    organizationName = o.optString("organizationName", ""),
                    organizationTags = jsonToStringList(o.opt("organizationTags")),
                    dataPolicyAgreed = o.optBoolean("dataPolicyAgreed", false),
                    isDataPolicyModifiable = o.optBoolean("isDataPolicyModifiable", true),
                )
            }
        } catch (_: Exception) { emptyList() }
    }

    private fun toJson(metas: List<QMeta>): JSONArray = JSONArray().apply {
        metas.forEach { m ->
            put(JSONObject().put("uid", m.uid).put("name", m.name).put("email", m.email)
                .put("mode", m.mode).put("expiresAt", m.expiresAt)
                .put("organizationId", m.organizationId)
                .put("organizationName", m.organizationName)
                .put("organizationTags", JSONArray().apply { m.organizationTags.forEach { put(it) } })
                .put("dataPolicyAgreed", m.dataPolicyAgreed)
                .put("isDataPolicyModifiable", m.isDataPolicyModifiable))
        }
    }

    /** 索引里的标签既可能是数组也可能是逗号串（旧版本/手工编辑），两种都吃。 */
    private fun jsonToStringList(raw: Any?): List<String> = when (raw) {
        is JSONArray -> (0 until raw.length()).mapNotNull { raw.optString(it, "").takeIf { s -> s.isNotBlank() } }
        is String -> raw.split(',').map { it.trim() }.filter { it.isNotEmpty() }
        else -> emptyList()
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

    /** 纯计算自检：索引 JSON 往返无损（uid 是主键），**含 COSY 聊天所需的组织字段**。 */
    fun selfCheck() {
        val src = listOf(
            QMeta("q-1", "张三", "a@b.com", QoderAuthApi.MODE_CN, 1791341100000L),
            QMeta(
                "q-2", "李四", "c@d.com", QoderAuthApi.MODE_GLOBAL, 1791341100001L,
                organizationId = "org-9", organizationName = "示例组织",
                organizationTags = listOf("team-a", "team-b"),
                dataPolicyAgreed = true, isDataPolicyModifiable = false,
            ),
        )
        val arr = toJson(src)
        val back = (0 until arr.length()).map { i ->
            val o = arr.getJSONObject(i)
            QMeta(
                uid = o.getString("uid"), name = o.getString("name"), email = o.getString("email"),
                mode = o.getString("mode"), expiresAt = o.getLong("expiresAt"),
                organizationId = o.optString("organizationId", ""),
                organizationName = o.optString("organizationName", ""),
                organizationTags = jsonToStringList(o.opt("organizationTags")),
                dataPolicyAgreed = o.optBoolean("dataPolicyAgreed", false),
                isDataPolicyModifiable = o.optBoolean("isDataPolicyModifiable", true),
            )
        }
        check(back == src) { "自检失败：Qoder 账号索引往返不一致（组织字段丢了会让聊天签名 401）" }
        check(back[1].organizationTags == listOf("team-a", "team-b")) { "自检失败：组织标签往返丢失" }
        check(back[1].dataPolicyAgreed) { "自检失败：data_policy_agreed 往返丢失" }

        // 旧索引（无组织字段）必须能读出默认值，不能整条丢弃
        val legacy = JSONArray("""[{"uid":"old","name":"n","email":"e","mode":"cn","expiresAt":1}]""")
        val legacyMeta = (0 until legacy.length()).map { i ->
            val o = legacy.getJSONObject(i)
            QMeta(o.getString("uid"), o.optString("name"), o.optString("email"),
                o.optString("mode"), o.optLong("expiresAt"),
                o.optString("organizationId", ""), o.optString("organizationName", ""),
                jsonToStringList(o.opt("organizationTags")),
                o.optBoolean("dataPolicyAgreed", false), o.optBoolean("isDataPolicyModifiable", true))
        }
        check(legacyMeta.size == 1 && legacyMeta[0].organizationTags.isEmpty()) {
            "自检失败：旧版索引应兼容读出（组织字段取默认值）"
        }
    }
}
