package com.example.traework2api

import android.util.Base64
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Qoder CLI **模型目录缓存**（`catalog-v7` / `catalog-v6`）的解密与导入。
 *
 * ## 为什么需要它
 *
 * 官方 UI 上的模型名只是展示名，请求要的是 **key**（`model_config.key`）。
 * 名字能猜（`Qwen3.8-Flash`），key 猜不出来（`qmodel_38flash`？`qmodel_flash`？猜错就是上游 400）。
 * 官方标注的**倍率**（0.0x / 0.1x / 8.0x）同样只在这个目录里是权威值。
 *
 * Qoder CLI 会把服务端下发的目录加密缓存在：
 *
 * ```
 * <cli 配置根>/.models/<uid>/catalog-v7
 * ```
 *
 * 文件内容是一段 **base64 文本**（不是二进制），解开就是
 * `{"chat":[{key,display_name,price_factor,max_input_tokens,...}], "byok_enterprise":[...], ...}`。
 *
 * ## 信封格式（对齐参考实现 `native_model_cache.go`）
 *
 * ```
 * base64( "QMC\x01" || nonce[12] || AES-256-GCM(key, nonce, plaintext) || tag[16] )
 * key = HKDF-SHA256(ikm = uid, salt = "qoder-model-cache-enc", info = "model-cache-v1", L = 32)
 * ```
 *
 * `uid` 就是当前登录账号的 uid —— 本 App 本来就有，所以**用户只需粘贴文件内容**，
 * 不需要自己做任何解密。
 *
 * 导入后走 [QoderModelCatalog.setDynamic] 合并进内置清单并落盘，
 * 于是 0 倍率模型、真实倍率、准确 key 一次到位。
 */
object QoderModelCache {

    /** 信封魔数：`QMC` + 版本字节 0x01。 */
    private val MAGIC = byteArrayOf(0x51, 0x4D, 0x43, 0x01)

    private const val NONCE_SIZE = 12
    private const val TAG_BITS = 128
    private const val KEY_LEN = 32

    private const val SALT = "qoder-model-cache-enc"
    private const val INFO = "model-cache-v1"

    /** 导入结果。 */
    data class Result(
        val ok: Boolean,
        /** 成功时：解析出的模型数。 */
        val models: Int = 0,
        /** 失败原因（给人看的中文）。 */
        val error: String? = null,
        /** 是否走了解密（false = 当明文 JSON 处理）。 */
        val decrypted: Boolean = false,
        /** 解密/直通后的**明文目录 JSON**。落盘只能存这个 —— 存密文的话下次启动解析不出来。 */
        val plain: String? = null,
    )

    /**
     * 把用户粘贴的内容导入成动态目录。
     *
     * 自动识别两种输入：
     *  - **加密信封**（`catalog-v7` 文件原文，base64）：用 [uid] 解密
     *  - **明文 JSON**：直接解析（用户自己解过密，或从别处拿到的目录）
     *
     * @param uid 当前 Qoder 账号的 uid（解密密钥的来源；明文输入时忽略）
     */
    fun importBlob(blob: String, uid: String): Result {
        val text = blob.trim()
        if (text.isEmpty()) return Result(false, error = "内容为空")

        // 先按明文 JSON 试：以 { 或 [ 开头就不必解密
        if (text.startsWith("{") || text.startsWith("[")) {
            val parsed = QoderModelCatalog.parseCatalogJson(text)
            if (parsed.isEmpty()) return Result(false, error = "是 JSON，但里面没有能识别的模型条目")
            QoderModelCatalog.setDynamic(parsed)
            return Result(true, models = parsed.size, decrypted = false, plain = text)
        }

        // 加密信封
        if (uid.isBlank()) return Result(false, error = "需要先登录 Qoder 账号（解密要用账号 uid）")
        val plain = decrypt(text, uid)
            ?: return Result(false, error = "解密失败：不是有效的 catalog-v7，或 uid 与该账号不匹配")
        val parsed = QoderModelCatalog.parseCatalogJson(plain)
        if (parsed.isEmpty()) return Result(false, error = "解密成功但目录里没有模型（格式变了？）")
        QoderModelCatalog.setDynamic(parsed)
        return Result(true, models = parsed.size, decrypted = true, plain = plain)
    }

    /** 解 `QMC\x01` 信封，返回明文 JSON；任何一步不对返回 null。 */
    fun decrypt(blob: String, uid: String): String? {
        val envelope = try {
            Base64.decode(blob, Base64.DEFAULT)
        } catch (_: Exception) {
            return null
        }
        val min = MAGIC.size + NONCE_SIZE + 16
        if (envelope.size < min) return null
        for (i in MAGIC.indices) if (envelope[i] != MAGIC[i]) return null

        val nonce = envelope.copyOfRange(MAGIC.size, MAGIC.size + NONCE_SIZE)
        val ciphertext = envelope.copyOfRange(MAGIC.size + NONCE_SIZE, envelope.size)
        return try {
            val key = hkdfSha256(uid.toByteArray(Charsets.UTF_8), SALT.toByteArray(Charsets.UTF_8), INFO.toByteArray(Charsets.UTF_8), KEY_LEN)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(TAG_BITS, nonce))
            String(cipher.doFinal(ciphertext), Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }

    /**
     * HKDF-SHA256（RFC 5869），只做一次 expand 就够（输出 32 字节 = 一个块）。
     *
     * 与 Go `crypto/hkdf.Key(sha256.New, ikm, salt, info, 32)` 等价：
     * `PRK = HMAC(salt, ikm)`，`OKM = HMAC(PRK, info || 0x01)`。
     */
    fun hkdfSha256(ikm: ByteArray, salt: ByteArray, info: ByteArray, length: Int): ByteArray {
        require(length in 1..32) { "本实现只支持 ≤32 字节输出，请求 $length" }
        val mac = Mac.getInstance("HmacSHA256")

        // extract
        mac.init(SecretKeySpec(if (salt.isEmpty()) ByteArray(32) else salt, "HmacSHA256"))
        val prk = mac.doFinal(ikm)

        // expand（一个块足够）
        mac.init(SecretKeySpec(prk, "HmacSHA256"))
        mac.update(info)
        mac.update(0x01.toByte())
        return mac.doFinal().copyOf(length)
    }

    /** 给 UI 用：这段文本看起来像加密缓存吗（用来提示"你贴的是 catalog-v7"）。 */
    fun looksEncrypted(text: String): Boolean {
        val t = text.trim()
        if (t.isEmpty() || t.startsWith("{") || t.startsWith("[")) return false
        val head = try { Base64.decode(t.take(8), Base64.DEFAULT) } catch (_: Exception) { return false }
        return head.size >= 4 && head[0] == MAGIC[0] && head[1] == MAGIC[1] && head[2] == MAGIC[2] && head[3] == MAGIC[3]
    }

    // ------------------------------------------------------------------
    // 自检
    // ------------------------------------------------------------------

    /**
     * 自检：**自己加密再解密**跑一遍往返（不依赖任何外部文件）。
     * 顺带钉住 HKDF 的 RFC 5869 测试向量，防止把 extract/expand 顺序写反。
     */
    fun selfCheck() {
        // RFC 5869 Test Case 1（SHA-256）：ikm=0x0b*22, salt=0x00..0x0c, info=0xf0..0xf9, L=42
        // 本实现只支持 ≤32，所以只校验前 32 字节。
        val ikm = ByteArray(22) { 0x0b }
        val salt = ByteArray(13) { it.toByte() }
        val info = ByteArray(10) { (0xf0 + it).toByte() }
        val okm = hkdfSha256(ikm, salt, info, 32)
        val expectHex = "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        check(hex(okm) == expectHex) { "自检失败：HKDF-SHA256 与 RFC 5869 测试向量不符，得到 ${hex(okm)}" }

        // 加密 → 解密往返（模拟 CLI 写出来的 catalog-v7）
        val uid = "11111111-2222-4333-8444-555555555555"
        val plain = """{"chat":[{"key":"free-model","display_name":"Free","price_factor":0.0}]}"""
        val blob = encryptForTest(plain, uid)
        check(looksEncrypted(blob)) { "自检失败：自造的密文应被识别为加密信封" }
        check(decrypt(blob, uid) == plain) { "自检失败：加解密往返不一致" }
        check(decrypt(blob, "another-uid") == null) { "自检失败：换 uid 必须解不开（GCM 认证应失败）" }

        // 信封结构破坏都要返回 null，不能抛
        check(decrypt("", uid) == null) { "自检失败：空串应返回 null" }
        check(decrypt("not-base64!!!", uid) == null) { "自检失败：非 base64 应返回 null" }
        check(decrypt(Base64.encodeToString(ByteArray(4), Base64.NO_WRAP), uid) == null) {
            "自检失败：太短的信封应返回 null"
        }
        val wrongMagic = Base64.encodeToString(ByteArray(40) { 0x7f }, Base64.NO_WRAP)
        check(decrypt(wrongMagic, uid) == null) { "自检失败：魔数不对应返回 null" }

        // 明文 JSON 直通
        check(!looksEncrypted("""{"chat":[]}""")) { "自检失败：明文 JSON 不该被当成密文" }

        val r = importBlob("""{"chat":[{"key":"k1","display_name":"N1","price_factor":0.0}]}""", uid)
        check(r.ok && !r.decrypted && r.models == 1) { "自检失败：明文导入应成功且标记未解密" }
        check(r.plain != null && r.plain.startsWith("{")) { "自检失败：明文导入必须回传可落盘的明文" }
        check(QoderModelCatalog.byKey("k1")?.free == true) { "自检失败：明文导入后免费模型应可解析" }

        val bad = importBlob("garbage-not-json", uid)
        check(!bad.ok && bad.error != null) { "自检失败：垃圾输入应失败并给原因" }
        val noUid = importBlob(encryptForTest(plain, uid), "")
        check(!noUid.ok) { "自检失败：没有 uid 时不能解密，应失败" }

        QoderModelCatalog.setDynamic(null)   // 别把自检数据留在全局状态里
    }

    /** 仅供自检：按 CLI 的信封格式加密。 */
    private fun encryptForTest(plain: String, uid: String): String {
        val key = hkdfSha256(uid.toByteArray(Charsets.UTF_8), SALT.toByteArray(Charsets.UTF_8), INFO.toByteArray(Charsets.UTF_8), KEY_LEN)
        val nonce = ByteArray(NONCE_SIZE).also { java.security.SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(TAG_BITS, nonce))
        val sealed = cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        val out = ByteArray(MAGIC.size + NONCE_SIZE + sealed.size)
        System.arraycopy(MAGIC, 0, out, 0, MAGIC.size)
        System.arraycopy(nonce, 0, out, MAGIC.size, NONCE_SIZE)
        System.arraycopy(sealed, 0, out, MAGIC.size + NONCE_SIZE, sealed.size)
        return Base64.encodeToString(out, Base64.NO_WRAP)
    }

    private fun hex(b: ByteArray): String {
        val sb = StringBuilder(b.size * 2)
        for (x in b) {
            val v = x.toInt() and 0xff
            sb.append("0123456789abcdef"[v ushr 4]).append("0123456789abcdef"[v and 0x0f])
        }
        return sb.toString()
    }

    /** 给 UI 用：一条可直接抄走的 shell 命令（用户在 codespace 里跑）。 */
    fun cliHint(uid: String): String =
        "cat ~/.qoder/.models/${uid.ifBlank { "<uid>" }}/catalog-v7"
}
