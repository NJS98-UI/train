package com.example.traework2api

import android.util.Base64
import org.json.JSONObject
import java.math.BigInteger
import java.security.KeyFactory
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.spec.X509EncodedKeySpec
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Qoder COSY `/algo/` 聊天网关的**全部密码学原语**（逐条对齐 `qodercli2api` 的 Go 实现）。
 *
 * 为什么必须自己实现而不是"随便拼个头"：Qoder 的聊天接口不像 OpenAPI 那样认裸 Bearer。
 * 它要求：
 *   1. 请求体先过一层**自定义 base64 + 三段交换**编码（[encodeBody]）；
 *   2. 头部 `Authorization: Bearer COSY.<payload>.<md5签名>`，签名覆盖 body 密文与固定路径；
 *   3. 头部 `Cosy-Key` 与 payload 里的 `info` 必须是**登录时生成一次**的运行时字段
 *      （AES-CBC 加密的用户信息 + RSA-PKCS1v15 加密的 AES 密钥）。
 *
 * 任何一处不一致（少个换行、密钥长度不对、padding 长度算错）上游都只回 401/400，
 * 而且不告诉你哪一步错了 —— 所以这里按参考实现**逐字节复刻**，不自作聪明。
 *
 * ⚠️ 这里没有任何网络调用，纯计算，可被 [selfCheck] 在无网络环境下钉住。
 */
object QoderCosyCrypto {

    // ------------------------------------------------------------------
    // 路径常量（注意：URL 走 `/algo/`，签名路径**不带** `/algo/`，两者不同）
    // ------------------------------------------------------------------

    /** 实际请求路径（带 `/algo` 前缀）。 */
    const val NATIVE_INFER_PATH = "/algo/api/v2/service/pro/sse/agent_chat_generation"

    /** 签名覆盖的路径（**不带** `/algo`）。少一个字符签名就废。 */
    const val COSY_SIGNED_PATH = "/api/v2/service/pro/sse/agent_chat_generation"

    /** 固定查询串。 */
    const val NATIVE_INFER_QUERY = "?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"

    /**
     * 官方**模型目录**接口（动态、与桌面版模型选择器同源）。
     *
     * 同样是 COSY 签名，但 body 为空（GET），签名路径里 **不含** `/algo` 前缀
     * （`computeSigPath` 会把开头的 `/algo` 剥掉，见参考实现 cosy.go）。
     */
    const val NATIVE_MODEL_LIST_PATH = "/algo/api/v2/model/list"
    const val MODEL_LIST_SIGNED_PATH = "/api/v2/model/list"

    /** COSY 协议版本（同时用于 `Cosy-Version` 头与 payload 的 `cosyVersion`）。 */
    const val COSY_VERSION = "1.1.74"

    /** 固定协议场景（cli / agent / assistant，client_type=5）。 */
    const val SCENE_CLIENT_TYPE = "5"
    const val SCENE_BUSINESS_PRODUCT = "cli"
    const val SCENE_BUSINESS_TYPE = "agent"
    const val SCENE_SCENE = "assistant"

    // ------------------------------------------------------------------
    // 请求体编码：自定义 base64 + swapOuterThirds
    // ------------------------------------------------------------------

    /** Qoder 专用 base64 字母表（64 个字符，顺序不能动）。 */
    private const val BODY_ALPHABET = "_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!"

    /** 自定义 base64 的填充字符（不是标准 base64 的 `=`）。 */
    private const val BODY_PAD = '$'

    /**
     * 请求体编码：先自定义 base64，再 [swapOuterThirds]。
     *
     * 返回的是**字节**而不是字符串：字母表里全是 ASCII，但 HTTP body 必须按字节写，
     * 走字符串再转一次纯属多一层出错机会。
     */
    fun encodeBody(raw: ByteArray): ByteArray = swapOuterThirds(customBase64Encode(raw))

    /**
     * 把数组的**首尾各三分之一对调**（中间不动）。
     *
     * 这是 Qoder 的一层混淆，不是加密。关键性质：**自反**（做两次等于没做），
     * 所以解码端用同一个函数。当长度 < 3 时 q=0，三段里有两段为空，结果仍是原数组。
     */
    fun swapOuterThirds(src: ByteArray): ByteArray {
        val q = src.size / 3
        val out = ByteArray(src.size)
        var o = 0
        for (i in src.size - q until src.size) out[o++] = src[i]
        for (i in q until src.size - q) out[o++] = src[i]
        for (i in 0 until q) out[o++] = src[i]
        return out
    }

    /** 自定义 base64 编码（字母表 + `$` 填充）。只做编码，聊天链路不需要解码。 */
    private fun customBase64Encode(raw: ByteArray): ByteArray {
        val encLen = ((raw.size + 2) / 3) * 4
        val out = ByteArray(encLen)
        var i = 0
        var o = 0
        while (i + 3 <= raw.size) {
            val b0 = raw[i].toInt() and 0xFF
            val b1 = raw[i + 1].toInt() and 0xFF
            val b2 = raw[i + 2].toInt() and 0xFF
            out[o++] = BODY_ALPHABET[(b0 shr 2) and 0x3F].code.toByte()
            out[o++] = BODY_ALPHABET[((b0 shl 4) or (b1 shr 4)) and 0x3F].code.toByte()
            out[o++] = BODY_ALPHABET[((b1 shl 2) or (b2 shr 6)) and 0x3F].code.toByte()
            out[o++] = BODY_ALPHABET[b2 and 0x3F].code.toByte()
            i += 3
        }
        when (raw.size - i) {
            1 -> {
                val b0 = raw[i].toInt() and 0xFF
                out[o++] = BODY_ALPHABET[(b0 shr 2) and 0x3F].code.toByte()
                out[o++] = BODY_ALPHABET[(b0 shl 4) and 0x3F].code.toByte()
                out[o++] = BODY_PAD.code.toByte()
                out[o] = BODY_PAD.code.toByte()
            }
            2 -> {
                val b0 = raw[i].toInt() and 0xFF
                val b1 = raw[i + 1].toInt() and 0xFF
                out[o++] = BODY_ALPHABET[(b0 shr 2) and 0x3F].code.toByte()
                out[o++] = BODY_ALPHABET[((b0 shl 4) or (b1 shr 4)) and 0x3F].code.toByte()
                out[o++] = BODY_ALPHABET[(b1 shl 2) and 0x3F].code.toByte()
                out[o] = BODY_PAD.code.toByte()
            }
        }
        return out
    }

    // ------------------------------------------------------------------
    // Authorization：COSY.<payload>.<signature>
    // ------------------------------------------------------------------

    /**
     * 计算 `Authorization` 头的值。
     *
     * 签名公式（**末尾没有换行**，多一个 `\n` 就全错）：
     * `hex(md5(payloadB64 + "\n" + key + "\n" + unixSeconds + "\n" + encodedBody + "\n" + COSY_SIGNED_PATH))`
     *
     * @param payloadB64 [authorizationPayload] 的返回值（标准 base64 字符串）
     * @param key        运行时字段里的 `key`（RSA 加密后的 AES 密钥，base64 字符串）
     * @param unixSeconds 当前 unix 秒（字符串，**不要**补零/格式化）
     * @param encodedBody [encodeBody] 得到的**密文**转成的字符串（ASCII）
     */
    fun authorization(
        payloadB64: String,
        key: String,
        unixSeconds: String,
        encodedBody: String,
        signedPath: String = COSY_SIGNED_PATH,
    ): String {
        val signature = md5Hex(
            payloadB64 + "\n" + key + "\n" + unixSeconds + "\n" + encodedBody + "\n" + signedPath,
        )
        return "Bearer COSY.$payloadB64.$signature"
    }

    /**
     * 构造 `Authorization` 里的 payload（标准 base64）。
     *
     * 明文 JSON：`{"version":"v1","requestId":"…","info":"…","cosyVersion":"1.1.74","ideVersion":""}`。
     * `info` 就是运行时字段的 `encrypt_user_info`（它本身已是 base64，**不要再编一次**）。
     */
    fun authorizationPayload(requestId: String, info: String, cosyVersion: String = COSY_VERSION): String {
        val json = JSONObject()
            .put("version", "v1")
            .put("requestId", requestId)
            .put("info", info)
            .put("cosyVersion", cosyVersion)
            .put("ideVersion", "")
            .toString()
        return b64Std(json.toByteArray(Charsets.UTF_8))
    }

    /** 每次请求都要**新生成**的 requestId（16 随机字节 → 反序掩码 → 小写 UUID）。 */
    fun newRequestId(): String = formatLowerUUID(reverseMaskUUID(randomBytes(16)))

    // ------------------------------------------------------------------
    // 运行时字段（登录时生成一次，缓存）
    // ------------------------------------------------------------------

    /** 运行时字段输出：`encrypt_user_info`（AES 密文 b64）与 `key`（RSA 密文 b64）。 */
    data class RuntimeFields(val encryptUserInfo: String, val key: String)

    /**
     * 生成运行时字段。**每次登录生成一次并缓存**，不要每个请求重新生成 ——
     * 上游会把 `Cosy-Key` 与 payload 里的 `info` 做配对校验，且这份材料代表"这次会话"。
     *
     * 步骤（逐条对齐参考实现）：
     *  1. 16 随机字节 → [reverseMaskUUID] → uuid
     *  2. `key` = uuid 前 8 字节的**十六进制小写**（16 个 ASCII 字符，同时当 AES key 与 IV）
     *  3. `ciphertext` = AES-CBC-PKCS7(明文 JSON, key, key)
     *  4. `encryptedKey` = RSA-PKCS1v15(key, 固定公钥)
     *  5. 输出两个字段各自的**标准** base64
     */
    fun generateRuntimeFields(
        uid: String,
        jobToken: String,
        organizationId: String,
        organizationTags: List<String>,
        dataPolicyAgreed: Boolean,
    ): RuntimeFields {
        val plaintext = runtimeFieldInputJson(uid, jobToken, organizationId, organizationTags, dataPolicyAgreed)
        val uuid = reverseMaskUUID(randomBytes(16))
        val key = runtimeASCIIKey(uuid).toByteArray(Charsets.US_ASCII)
        val ciphertext = aesCbcEncryptPkcs7(plaintext.toByteArray(Charsets.UTF_8), key, key)
        val encryptedKey = rsaEncryptPkcs1v15(key)
        return RuntimeFields(b64Std(ciphertext), b64Std(encryptedKey))
    }

    /**
     * 运行时字段的明文 JSON。
     *
     * 字段顺序与 Go 结构体一致（uid → security_oauth_token → organization_id →
     * organization_tags → data_policy_agreed）。`security_oauth_token` 为 `omitempty`：
     * jobToken 为空时**整个键不出现**（空字符串会让上游判定 token 无效）。
     */
    fun runtimeFieldInputJson(
        uid: String,
        jobToken: String,
        organizationId: String,
        organizationTags: List<String>,
        dataPolicyAgreed: Boolean,
    ): String {
        val o = JSONObject()
        o.put("uid", uid)
        if (jobToken.isNotEmpty()) o.put("security_oauth_token", jobToken)
        o.put("organization_id", organizationId)
        o.put("organization_tags", org.json.JSONArray().apply { organizationTags.forEach { put(it) } })
        o.put("data_policy_agreed", dataPolicyAgreed)
        return o.toString()
    }

    // ------------------------------------------------------------------
    // UUID / 密钥派生
    // ------------------------------------------------------------------

    /** 16 字节**整体反序**，再把第 7 字节置 `0100xxxx`、第 9 字节置 `10xxxxxx`（UUIDv4 掩码）。 */
    fun reverseMaskUUID(raw: ByteArray): ByteArray {
        require(raw.size == 16) { "UUID 原始字节必须是 16 个" }
        val v = ByteArray(16)
        for (i in 0 until 16) v[i] = raw[15 - i]
        v[6] = ((v[6].toInt() and 0x0F) or 0x40).toByte()
        v[8] = ((v[8].toInt() and 0x3F) or 0x80).toByte()
        return v
    }

    /** 小写 UUID 字符串（8-4-4-4-12）。 */
    fun formatLowerUUID(v: ByteArray): String {
        require(v.size == 16) { "UUID 字节必须是 16 个" }
        val sb = StringBuilder(36)
        for (i in 0 until 16) {
            if (i == 4 || i == 6 || i == 8 || i == 10) sb.append('-')
            sb.append(HEX[(v[i].toInt() shr 4) and 0x0F]).append(HEX[v[i].toInt() and 0x0F])
        }
        return sb.toString()
    }

    /** AES 密钥/IV：uuid 前 8 字节的十六进制小写 = 16 个 ASCII 字符。 */
    fun runtimeASCIIKey(uuid: ByteArray): String {
        require(uuid.size == 16) { "UUID 字节必须是 16 个" }
        val sb = StringBuilder(16)
        for (i in 0 until 8) {
            sb.append(HEX[(uuid[i].toInt() shr 4) and 0x0F]).append(HEX[uuid[i].toInt() and 0x0F])
        }
        return sb.toString()
    }

    // ------------------------------------------------------------------
    // AES-CBC / PKCS7
    // ------------------------------------------------------------------

    /** PKCS#7 填充：补 `block - len % block` 个字节，每个字节的值等于补的长度。 */
    fun pkcs7Pad(src: ByteArray, blockSize: Int): ByteArray {
        require(blockSize in 1..255) { "PKCS#7 block size 非法: $blockSize" }
        val padLen = blockSize - src.size % blockSize
        val out = ByteArray(src.size + padLen)
        System.arraycopy(src, 0, out, 0, src.size)
        for (i in src.size until out.size) out[i] = padLen.toByte()
        return out
    }

    /** AES-CBC 加密（PKCS7 填充，key 长度 16/24/32，IV 固定 16 字节）。 */
    fun aesCbcEncryptPkcs7(plain: ByteArray, key: ByteArray, iv: ByteArray): ByteArray {
        require(key.size == 16 || key.size == 24 || key.size == 32) { "AES key 长度非法: ${key.size}" }
        require(iv.size == 16) { "AES-CBC IV 长度非法: ${iv.size}" }
        val cipher = Cipher.getInstance("AES/CBC/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), IvParameterSpec(iv))
        return cipher.doFinal(pkcs7Pad(plain, 16))
    }

    // ------------------------------------------------------------------
    // RSA PKCS#1 v1.5（手动构造 EM，不用 Cipher 的 RSA/ECB/PKCS1Padding）
    // ------------------------------------------------------------------

    /**
     * 固定公钥（1024-bit，PKIX/SubjectPublicKeyInfo，指数 65537）。
     *
     * ⚠️ 这是 Qoder 客户端的**内置公钥**，不是服务端密钥；它只用于"把 AES 密钥安全送给上游"。
     * 换掉它上游就解不开 `Cosy-Key`，聊天直接 401。
     */
    private const val RUNTIME_PUBLIC_KEY_B64 =
        "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc" +
            "4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l" +
            "6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17" +
            "XcW+ML9FoCI6AOvOzwIDAQAB"

    /**
     * RSA PKCS#1 v1.5 加密（与 Go `rsaEncryptPKCS1v15Exact` 逐字节一致）。
     *
     * 为什么不用 `Cipher.getInstance("RSA/ECB/PKCS1Padding")`：Android 各版本对
     * "随机填充里出现 0 字节要重抽"的实现细节不完全一致，而这里必须和参考实现完全同构
     * （否则上游虽然能解密，但某些实现会做 padding 合法性检查）。手动构造 EM 最稳。
     *
     * EM = `0x00 || 0x02 || PS(非零随机, 长度 k-len(M)-3) || 0x00 || M`，再 `c = EM^E mod N`。
     */
    fun rsaEncryptPkcs1v15(message: ByteArray): ByteArray {
        val spec = X509EncodedKeySpec(Base64.decode(RUNTIME_PUBLIC_KEY_B64, Base64.NO_WRAP))
        val pub = KeyFactory.getInstance("RSA").generatePublic(spec) as java.security.interfaces.RSAPublicKey
        val n = pub.modulus
        val e = pub.publicExponent
        val k = (n.bitLength() + 7) / 8
        require(k >= 11 && message.size <= k - 11) {
            "RSA 消息长度 ${message.size} 超过上限 ${k - 11}"
        }
        val psLength = k - message.size - 3
        val em = ByteArray(k)
        em[1] = 0x02.toByte()
        val rnd = SecureRandom()
        var i = 2
        while (i < 2 + psLength) {
            val b = ByteArray(1)
            rnd.nextBytes(b)
            if (b[0] != 0.toByte()) { em[i] = b[0]; i++ }
        }
        em[2 + psLength] = 0x00.toByte()
        System.arraycopy(message, 0, em, 3 + psLength, message.size)

        val m = BigInteger(1, em)
        require(m < n) { "RSA 编码后的消息不小于模数" }
        return fillBytes(m.modPow(e, n), k)
    }

    /** 大端左补零到恰好 [len] 字节（等价 Go 的 `big.Int.FillBytes`）。 */
    private fun fillBytes(value: BigInteger, len: Int): ByteArray {
        val raw = value.toByteArray()
        val out = ByteArray(len)
        when {
            raw.size == len -> System.arraycopy(raw, 0, out, 0, len)
            raw.size > len -> System.arraycopy(raw, raw.size - len, out, 0, len)
            else -> System.arraycopy(raw, 0, out, len - raw.size, raw.size)
        }
        return out
    }

    // ------------------------------------------------------------------
    // 小工具
    // ------------------------------------------------------------------

    private val HEX = "0123456789abcdef".toCharArray()

    /** 标准 base64（无换行）。 */
    fun b64Std(bytes: ByteArray): String = Base64.encodeToString(bytes, Base64.NO_WRAP)

    private fun md5Hex(s: String): String {
        val digest = MessageDigest.getInstance("MD5").digest(s.toByteArray(Charsets.UTF_8))
        val sb = StringBuilder(32)
        for (b in digest) sb.append(HEX[(b.toInt() shr 4) and 0x0F]).append(HEX[b.toInt() and 0x0F])
        return sb.toString()
    }

    private fun randomBytes(n: Int): ByteArray = ByteArray(n).also { SecureRandom().nextBytes(it) }

    // ------------------------------------------------------------------
    // 自检（纯计算，无网络）
    // ------------------------------------------------------------------

    /**
     * 钉住几件"错了就必然 401 且极难定位"的事：
     *  - swapOuterThirds 自反（编解码两端必须同一函数）
     *  - base64 长度/填充字符正确（`$` 而不是 `=`）
     *  - 签名公式里的换行位置（少一个 `\n` 全错）
     *  - UUID 掩码位（版本位 4、变体位 10）
     *  - 运行时字段能生成且长度合理（AES 密文块对齐、RSA 密文 = 128 字节）
     */
    fun selfCheck() {
        // 1) 三段交换自反
        val probe = "0123456789abcdef".toByteArray(Charsets.US_ASCII)
        check(swapOuterThirds(swapOuterThirds(probe)).contentEquals(probe)) {
            "自检失败：swapOuterThirds 不满足自反性"
        }
        // 长度 6：q=2，首尾各 2 字节对调，中间 2 字节不动
        val six = "abcdef".toByteArray(Charsets.US_ASCII)
        check(String(swapOuterThirds(six), Charsets.US_ASCII) == "efcdab") {
            "自检失败：swapOuterThirds 结果不对，得到 [${String(swapOuterThirds(six))}]"
        }

        // 2) 自定义 base64：1 字节 → 4 字符，其中 2 个是 `$` 填充
        //    注意：`encodeBody` 末尾会做三段交换，填充字符**会被挪位**（1 字节时跑到下标 0、2），
        //    所以这里只能数字符个数，不能断言"末尾两个是 $"。
        val one = encodeBody(byteArrayOf(0x66))
        check(one.size == 4 && one.count { it.toInt().toChar() == BODY_PAD } == 2) {
            "自检失败：1 字节必须编成 4 字符且含 2 个 '$' 填充，得到 [${String(one)}]"
        }
        // 3 字节 → 4 字符，无填充；且每个字符都来自字母表
        val three = encodeBody("abc".toByteArray(Charsets.US_ASCII))
        check(three.size == 4 && !String(three).contains(BODY_PAD)) {
            "自检失败：3 字节编码不该有填充，得到 [${String(three)}]"
        }
        check(three.all { BODY_ALPHABET.contains(it.toInt().toChar()) }) {
            "自检失败：编码结果不在字母表内，得到 [${String(three)}]"
        }

        // 3) 签名：固定输入必须得到固定摘要（换行位置钉死）
        val sig = authorization("PAYLOAD", "KEY", "1700000000", "BODY")
        check(sig.startsWith("Bearer COSY.PAYLOAD.")) { "自检失败：Authorization 前缀不对: $sig" }
        val expected = md5Hex("PAYLOAD\nKEY\n1700000000\nBODY\n$COSY_SIGNED_PATH")
        check(sig.endsWith(expected)) { "自检失败：签名公式不对（换行/路径长度），得到 [$sig]" }
        // 末尾多一个换行必须产生不同摘要（这是最容易写错的一处）
        // 模型目录接口用的是另一条签名路径（/api/v2/model/list，无 /algo 前缀）：
        // 同一份输入换个路径必须得到不同签名，否则说明路径没进签名 → 上游必 401/400。
        val listAuth = authorization("PAYLOAD", "KEY", "1700000000", "", MODEL_LIST_SIGNED_PATH)
        val listExpected = "Bearer COSY.PAYLOAD." +
            md5Hex("PAYLOAD\nKEY\n1700000000\n\n$MODEL_LIST_SIGNED_PATH")
        check(listAuth == listExpected) { "自检失败：模型目录签名路径未生效" }
        check(listAuth != authorization("PAYLOAD", "KEY", "1700000000", "")) {
            "自检失败：换签名路径必须改变签名"
        }
        check(md5Hex("PAYLOAD\nKEY\n1700000000\nBODY\n$COSY_SIGNED_PATH\n") != expected) {
            "自检失败：签名末尾换行未生效"
        }

        // 4) UUID 掩码位
        val masked = reverseMaskUUID(ByteArray(16) { (it + 1).toByte() })
        check((masked[6].toInt() and 0xF0) == 0x40) { "自检失败：UUID 版本位不是 4" }
        check((masked[8].toInt() and 0xC0) == 0x80) { "自检失败：UUID 变体位不是 10" }
        check(reverseMaskUUID(ByteArray(16) { (it + 1).toByte() })[0] == 16.toByte()) {
            "自检失败：UUID 未整体反序"
        }
        val uuid = formatLowerUUID(masked)
        check(uuid.length == 36 && uuid.count { it == '-' } == 4) { "自检失败：UUID 字符串格式不对: $uuid" }
        check(uuid == uuid.lowercase()) { "自检失败：UUID 必须小写" }

        // 5) runtimeASCIIKey = 前 8 字节 hex
        val asciiKey = runtimeASCIIKey(masked)
        check(asciiKey.length == 16) { "自检失败：AES 密钥必须是 16 个 ASCII 字符" }
        check(asciiKey == uuid.replace("-", "").substring(0, 16)) { "自检失败：AES 密钥与 UUID 前 8 字节不一致" }

        // 6) PKCS7 填充
        check(pkcs7Pad(ByteArray(16), 16).size == 32) { "自检失败：整块明文必须补满一整块" }
        check(pkcs7Pad(ByteArray(15), 16).size == 16) { "自检失败：15 字节应补 1 字节" }
        check(pkcs7Pad(ByteArray(0), 16).let { it.size == 16 && it.all { b -> b.toInt() == 16 } }) {
            "自检失败：空明文应补 16 个 0x10"
        }

        // 7) 运行时字段可生成，且尺寸合规
        val rf = generateRuntimeFields("u-1", "jt-1", "org-1", listOf("t1", "t2"), true)
        check(rf.encryptUserInfo.isNotEmpty() && rf.key.isNotEmpty()) { "自检失败：运行时字段为空" }
        val cipherLen = Base64.decode(rf.encryptUserInfo, Base64.NO_WRAP).size
        check(cipherLen % 16 == 0) { "自检失败：AES 密文未按 16 字节块对齐，长度 $cipherLen" }
        check(Base64.decode(rf.key, Base64.NO_WRAP).size == 128) {
            "自检失败：RSA 密文必须是 128 字节（1024-bit 模数）"
        }
        // 明文里必须有 jobToken（上游靠它认身份），空 token 时该键必须缺席
        check(runtimeFieldInputJson("u", "jt", "", emptyList(), false).contains("security_oauth_token")) {
            "自检失败：jobToken 非空时必须写进 AES 明文"
        }
        check(!runtimeFieldInputJson("u", "", "", emptyList(), false).contains("security_oauth_token")) {
            "自检失败：jobToken 为空时必须省略该键（omitempty 语义）"
        }
    }
}
