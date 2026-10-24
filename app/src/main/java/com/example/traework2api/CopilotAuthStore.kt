package com.example.traework2api

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.nio.charset.StandardCharsets
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

/**
 * GitHub Copilot 账号存储（**单账号**，与 Trae/Qoder 的多账号模型不同）。
 *
 * 为什么单账号够用：Copilot 的额度绑在 GitHub 账号本身，同一台设备上换号没有
 * "多账号轮换"的价值（而且换号要重新走设备码授权，体验成本很高）。
 *
 * 存两份令牌，生命周期不同：
 *  - `oauthToken`（`ghu_…`）：设备码授权换来的长期令牌，**只有它需要用户授权**；
 *  - `copilotToken`：用 oauthToken 去 `api.github.com/copilot_internal/v2/token`
 *    换来的短时令牌（约 30 分钟），**每次转发前检查过期并自动续**，用户完全无感。
 *
 * 两者都是密钥，用 AndroidKeyStore 的 AES-GCM 加密后落盘（别名与 Trae/Qoder 分开）。
 */
object CopilotAuthStore {

    private const val PREF = "traework2api_copilot"
    private const val KEY_BLOB = "cred"
    private const val KEY_ALIAS = "traework2api_copilot_key"
    private const val SEP = "\u0001"

    /**
     * @param oauthToken       `ghu_…`（设备码授权结果，长期有效，可撤销）
     * @param login            GitHub 用户名
     * @param sku              套餐标识，如 `free_limited_copilot` / `copilot_for_individual`
     * @param plan             套餐名，如 `individual` / `business`
     * @param copilotToken     短时 Copilot 令牌（`tid=…` 开头）
     * @param copilotExpiresAt 过期时间（**unix 秒**，不是毫秒 —— 上游给的就是秒）
     */
    data class Cred(
        val oauthToken: String,
        val login: String = "",
        val sku: String = "",
        val plan: String = "",
        val copilotToken: String = "",
        val copilotExpiresAt: Long = 0L,
    ) {
        /** 能不能拿来转发（只要 OAuth 令牌在，Copilot 令牌可以随时换）。 */
        val chatReady: Boolean get() = oauthToken.isNotBlank()

        /** Copilot 令牌是否仍然可用（提前 60 秒视为过期，避免卡在边界上）。 */
        fun copilotFresh(nowSec: Long = System.currentTimeMillis() / 1000): Boolean =
            copilotToken.isNotBlank() && copilotExpiresAt - 60 > nowSec

        fun toJson(): JSONObject = JSONObject()
            .put("oauth_token", oauthToken)
            .put("login", login)
            .put("sku", sku)
            .put("plan", plan)
            .put("copilot_token", copilotToken)
            .put("copilot_expires_at", copilotExpiresAt)

        companion object {
            fun fromJson(o: JSONObject) = Cred(
                oauthToken = o.optString("oauth_token", ""),
                login = o.optString("login", ""),
                sku = o.optString("sku", ""),
                plan = o.optString("plan", ""),
                copilotToken = o.optString("copilot_token", ""),
                copilotExpiresAt = o.optLong("copilot_expires_at", 0L),
            )
        }
    }

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREF, Context.MODE_PRIVATE)

    fun load(ctx: Context): Cred? {
        val blob = prefs(ctx).getString(KEY_BLOB, null) ?: return null
        val plain = decrypt(blob) ?: return null
        return try {
            Cred.fromJson(JSONObject(plain))
        } catch (_: Exception) {
            null
        }
    }

    fun save(ctx: Context, cred: Cred) {
        prefs(ctx).edit().putString(KEY_BLOB, encrypt(cred.toJson().toString())).apply()
    }

    fun clear(ctx: Context) {
        prefs(ctx).edit().remove(KEY_BLOB).apply()
    }

    fun isLoggedIn(ctx: Context): Boolean = load(ctx)?.chatReady == true

    /** 只在拿到新 Copilot 令牌时更新，别把整条凭证重写一遍（省一次加解密）。 */
    fun updateCopilotToken(ctx: Context, token: String, expiresAt: Long) {
        val cur = load(ctx) ?: return
        save(ctx, cur.copy(copilotToken = token, copilotExpiresAt = expiresAt))
    }

    // ------------------------------------------------------------------
    // AES-GCM（KeyStore 主密钥，别名与 Trae/Qoder 分开）
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

    private fun decrypt(b64: String): String? = try {
        val parts = b64.split(SEP)
        if (parts.size != 2) null else {
            val iv = Base64.decode(parts[0], Base64.NO_WRAP)
            val ct = Base64.decode(parts[1], Base64.NO_WRAP)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(128, iv))
            String(cipher.doFinal(ct), StandardCharsets.UTF_8)
        }
    } catch (_: Exception) {
        null
    }

    /** 纯计算自检：JSON 往返无损、过期判定边界正确。 */
    fun selfCheck() {
        val src = Cred(
            oauthToken = "ghu_test", login = "octocat", sku = "free_limited_copilot",
            plan = "individual", copilotToken = "tid=abc", copilotExpiresAt = 2_000_000_000L,
        )
        val back = Cred.fromJson(src.toJson())
        check(back == src) { "自检失败：Copilot 凭证 JSON 往返不一致" }

        // 过期判定：提前 60 秒算过期
        check(src.copilotFresh(nowSec = 1_999_999_900L)) { "自检失败：未过期应算可用" }
        check(!src.copilotFresh(nowSec = 1_999_999_950L)) { "自检失败：距过期 <60s 应算过期" }
        check(!src.copilotFresh(nowSec = 2_000_000_001L)) { "自检失败：已过期应算过期" }
        check(!Cred(oauthToken = "ghu_x").copilotFresh()) { "自检失败：空 Copilot 令牌不该算新鲜" }
        check(Cred(oauthToken = "ghu_x").chatReady) { "自检失败：有 OAuth 令牌就该能转发" }
        check(!Cred(oauthToken = "").chatReady) { "自检失败：没有 OAuth 令牌不该能转发" }
    }
}
