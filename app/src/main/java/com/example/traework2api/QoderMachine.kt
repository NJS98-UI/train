package com.example.traework2api

import android.content.Context
import java.security.MessageDigest
import java.util.UUID

/**
 * Qoder 的「桌面端机器身份」—— 按官方 CLI 的真实逻辑复刻，不是瞎编。
 *
 * 依据（`@qoder-ai/qodercli` 1.1.78 的 bundle，反编译可见）：
 * ```
 * async function J9i(){ let A = await OAl();              // 硬件指纹，拿不到就 null
 *   return C.log(`... source=${A?"hardware":"random"} ...`), A ?? ZSn() }   // ZSn = crypto.randomUUID
 * function Z9i(A){ return sha256(A).hex }                 // 落盘/上线的身份 = sha256(seed)
 * function iYt(A){ return {"Cosy-MachineCode":A.machineCode,"Cosy-MachineToken":A.machineToken,"Cosy-MachineType":A.machineType} }
 * // umid（阿里原生 SDK）不可用时：
 * {token: await gl.getMachineId(), type:"", code:""}
 * ```
 * 也就是说官方客户端在拿不到 umid 时，**自己就退化成 `MachineToken = MachineId = sha256(seed)`**，
 * 并且 `MachineType` / `MachineCode` 留空。我们照抄这一条，服务端看到的就与官方
 * 「无 umid 的桌面端」完全同构，而不是一个明显自造的哈希串。
 *
 * 注意：真·umid token（`Cosy-MachineToken` 为原生 SDK 签发的那串）优先级更高，
 * 用户抓包回填时走 [QoderAuthStore.fingerprint]；本类只是**没有真货时的等价身份**。
 */
object QoderMachine {

    /** Cosy-Version：CLI 里 `aog = bJ || "1.1.78"`。 */
    const val COSY_VERSION = "1.1.78"

    /** Cosy-ClientType：CLI 里 `A ? "6" : "5"`，默认走 5。 */
    const val CLIENT_TYPE = "5"

    /** Cosy-MachineOS：CLI 里 `${arch}_${platform}`，桌面端典型值。 */
    const val MACHINE_OS = "aarch64_darwin"

    fun sha256Hex(input: String): String {
        val d = MessageDigest.getInstance("SHA-256").digest(input.toByteArray(Charsets.UTF_8))
        val sb = StringBuilder(d.size * 2)
        for (b in d) sb.append(String.format("%02x", b))
        return sb.toString()
    }

    /**
     * 本机的机器身份（64 位十六进制）。
     *
     * seed 复用 [QoderAuthStore.machineId] 已持久化的 UUID —— 同一台设备永远同一个身份，
     * 不会因为重装/重启漂移（漂移等于换设备，会被风控当成新机器）。
     */
    fun machineId(context: Context): String = sha256Hex(QoderAuthStore.machineId(context))

    /** 组装本次请求要用的身份：优先真实指纹，否则用本机派生。 */
    fun identity(context: Context, captured: QoderAuthStore.QFingerprint?): Identity {
        if (captured != null) return Identity(
            machineId = captured.machineId,
            machineToken = captured.machineToken,
            machineType = captured.machineType,
            machineOS = captured.machineOS,
            hostname = captured.hostname,
            clientType = captured.clientType,
            version = captured.version,
            fromCapture = true,
        )
        val id = machineId(context)
        return Identity(
            machineId = id,
            // 官方在无 umid 时就是 token = machineId（见类注释），不是随便编一个
            machineToken = id,
            machineType = "",
            machineOS = MACHINE_OS,
            hostname = "",
            clientType = CLIENT_TYPE,
            version = COSY_VERSION,
            fromCapture = false,
        )
    }

    data class Identity(
        val machineId: String,
        val machineToken: String,
        val machineType: String,
        val machineOS: String,
        val hostname: String,
        val clientType: String,
        val version: String,
        /** true = 用户抓包回填的真货；false = 本机派生（等价官方无 umid 客户端）。 */
        val fromCapture: Boolean,
    )

    fun selfCheck() {
        // 已知向量：sha256("abc") 是标准值，写死钉住编码（大小写/前导零都容易写错）
        check(sha256Hex("abc") == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad") {
            "自检失败：sha256 实现不对"
        }
        check(sha256Hex("").length == 64) { "自检失败：摘要长度应为 64" }
        check(UUID.fromString("00000000-0000-0000-0000-000000000000").toString().isNotEmpty()) {
            "自检失败：UUID 解析异常"
        }
    }
}
