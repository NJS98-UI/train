package com.example.traework2api

import android.content.Context
import android.content.pm.PackageManager
import android.media.MediaDrm
import android.os.Build
import android.provider.Settings
import java.security.MessageDigest
import java.util.UUID

/**
 * 本地网关的**业务持有者**：跑不跑、凭证从哪来、失败怎么换号。
 *
 * 为什么单独抽出来、而不是留在 MainActivity 或 [GatewayService] 里：
 *  - Android 的保活机制（前台服务 / 通知 / 唤醒锁）属于 [GatewayService]，
 *    与"网关跑不跑"无关 —— 分开后两边各自只有一个理由变更。
 *  - ⚠️ 这里**一律只用 applicationContext**。旧代码把这些 provider 建在 MainActivity 里、
 *    捕获了 Activity；保活之后服务会比 Activity 活得更久，那就是长驻内存泄漏。
 */
object Gateway {
    private const val SIXTY_MINUTES_SEC = 3600L
    /**
     * ⚠️ Qoder 的续期阈值单位是**毫秒**（`validJobTokenFor(..., skewMillis)`），
     * 和 Trae 的 `skewSec` 不是一个单位。早前这里错用了 [SIXTY_MINUTES_SEC]，
     * 实际只等于 3.6 秒 —— 等于"永远拿旧 token 去撞 401"，续期形同虚设。
     */
    private const val SIXTY_MINUTES_MS = 60L * 60L * 1000L
    private const val TAG = "Gateway"

    /**
     * 串行化「取凭证 + 续期」。refresh token 是**轮换**的：并发续期会让先换到的那个失效。
     * MainActivity 的"登录后主动续期"也必须走 [locked]，否则会和这里的 credProvider 撞车。
     */
    private val tokenLock = Any()

    /**
     * Qoder 的凭证锁。**必须和 Trae 的分开**：两边是两套 token、两套上游，
     * 共用一把锁时，Trae 的续期/换号会把 Qoder 的转发整个卡住（反之亦然）——
     * 表现就是"一个后端的慢请求把另一个后端也拖成不响应"。
     */
    private val qoderLock = Any()

    @Volatile
    private var server: LocalFakeServer? = null

    val isRunning: Boolean get() = server?.isRunning == true

    /** 供 MainActivity 的主动续期复用同一把锁（别再加第二把：轮换只允许一个续期在跑）。 */
    fun <T> locked(block: () -> T): T = synchronized(tokenLock) { block() }

    /** Qoder 侧的串行段（jobToken 续期同样是轮换的，但只约束 Qoder 自己的链路）。 */
    fun <T> lockedQoder(block: () -> T): T = synchronized(qoderLock) { block() }

    /**
     * 本地 API Key（与上游 access_token 是两套）。
     *
     * **由「APK 签名证书指纹 + Widevine 设备 ID」派生**，同一台设备 + 同一签名永远算出同一个
     * key。Widevine ID 在设备出厂预置阶段写进 TEE/RPMB 硬件安全区，随系统走：
     * 覆盖安装、卸载重装、清除数据、恢复出厂、常规刷机（不动 TEE）都带不走它。
     * 落库里不存 key（旧版残留的一次性清掉），应用数据被清多少次都自愈。
     *
     * 行为矩阵：
     *   - 覆盖安装 / 卸载重装 / 清除数据 / 恢复出厂 / 常规刷机 → key 不变
     *   - 换签名重装 → key 变（签名参与派生；签名不同本来就得卸载重装）
     *   - 罕见边缘：无 Widevine 的设备（部分模拟器）退化用 ANDROID_ID，恢复出厂会变；
     *     刷机动了 TEE 导致 Widevine 重配 → key 变。这两种情况客户端重填一次即可。
     */
    fun apiKey(ctx: Context): String {
        // 一次性清理旧版落库的 key（随机值/自定义覆盖均已废弃）：读到就删，避免残值复活
        val prefs = ctx.getSharedPreferences("traework2api_prefs", Context.MODE_PRIVATE)
        if (prefs.getString("local_api_key", null) != null) {
            prefs.edit().remove("local_api_key").apply()
        }
        return derivedKey(ctx)
    }

    /**
     * 派生规则：`sk-` + sha256(签名指纹 | Widevine 设备ID) 的前 32 个 hex 字符（128 bit）。
     *
     * 为什么选 Widevine 设备 ID 而非 ANDROID_ID：ANDROID_ID 由系统在恢复出厂时重发；
     * Widevine ID 是硬件预置身份，跨恢复出厂/刷机稳定。签名指纹参与派生：防止不同签名的包
     * （包名相同）在同一台设备上共享同一个网关 key。
     */
    private fun derivedKey(ctx: Context): String {
        derivedCache?.let { return it }
        val deviceId = widevineDeviceIdHex()
            // 无 Widevine（个别模拟器）才退化 ANDROID_ID：重装/清数据仍稳，恢复出厂会变
            ?: (Settings.Secure.getString(ctx.contentResolver, Settings.Secure.ANDROID_ID) ?: "noaid")
        val key = "sk-" + QoderMachine.sha256Hex("${signingFingerprint(ctx) ?: "nosig"}|$deviceId").take(32)
        derivedCache = key
        return key
    }

    /** 进程内缓存：硬件 ID 在一次运行内不会变，别在每次界面渲染都构造 MediaDrm。 */
    @Volatile
    private var derivedCache: String? = null

    /** Widevine DRM 的媒体 UUID：edef8ba9-79d6-4ace-a3c8-27dcd51d21ed（公开标准值，负数 hex 是其补码形式）。 */
    private val WIDEVINE_UUID = UUID(-0x121074568629b532L, -0x5c37d8232ae2e13L)

    /** Widevine 设备唯一 ID 的 hex；设备无 Widevine 或读失败返回 null（由调用方退化）。 */
    @Suppress("DEPRECATION")
    private fun widevineDeviceIdHex(): String? = try {
        val drm = MediaDrm(WIDEVINE_UUID)
        try {
            // "deviceUniqueId"：出厂预置的设备级 ID。常量 PROPERTY_DEVICE_UNIQUE_ID 要 API 28，
            // 字面量全版本可用（同一个值）。
            val id = drm.getPropertyByteArray("deviceUniqueId")
            id?.joinToString("") { b -> String.format("%02x", b.toInt() and 0xFF) }
        } finally {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) drm.close() else drm.release()
        }
    } catch (_: Throwable) {
        null
    }

    /** APK 签名证书的 SHA-256 指纹；拿不到（理论边缘）返回 null，派生退化为纯设备 ID。 */
    @Suppress("DEPRECATION")
    private fun signingFingerprint(ctx: Context): String? = try {
        val pm = ctx.packageManager
        val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            pm.getPackageInfo(ctx.packageName, PackageManager.GET_SIGNING_CERTIFICATES)
        } else {
            pm.getPackageInfo(ctx.packageName, PackageManager.GET_SIGNATURES)
        }
        val cert = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.signingInfo?.apkContentsSigners?.firstOrNull()?.toByteArray()
        } else {
            info.signatures?.firstOrNull()?.toByteArray()
        }
        cert?.let {
            val d = MessageDigest.getInstance("SHA-256").digest(it)
            d.joinToString("") { b -> String.format("%02x", b.toInt() and 0xFF) }
        }
    } catch (_: Throwable) {
        null
    }

    /**
     * 幂等启动：已在跑就直接返回。
     * 重复 start 会去抢同一个端口 → `EADDRINUSE` 崩掉，所以这道闸必须留着。
     */
    @Synchronized
    fun start(ctx: Context) {
        if (isRunning) return
        val app = ctx.applicationContext
        val s = LocalFakeServer(apiKey(app))
        s.usageRecorder = { model, usage, ms, ok, err, backend, uid ->
            UsageStore.record(app, model,
                usage?.optInt("prompt_tokens", 0) ?: 0,
                usage?.optInt("completion_tokens", 0) ?: 0,
                usage?.optInt("total_tokens", 0) ?: 0,
                ms, ok, err ?: "")
            // 「只在转发的时候加载正在使用的」：每次转发后顺手刷新当前账号额度（内部节流 60s），
            // 不再有定时轮询 —— 那是把账号拿去刷接口，token 会被判废。
            // 只刷新"这次实际用到的那个平台、那个账号"，跨平台/跨账号一律不碰。
            //
            // ⚠️ 失败（ok=false）时传 urgent=true：**跳过节流立刻判定换号**。
            // 否则余额见底/模型排队时要干等最多 60 秒才换 —— 那就是"换号不积极"。
            CreditMonitor.noteForwarded(app, backend, uid, urgent = !ok)
        }
        // 每次转发前取「当下有效」的凭证（必要时续期）。服务是长驻的、token 会过期，
        // 不能只在启动/登录/切账号时推一次快照（那样跑够 14 天后全是 401）。
        s.credProvider = {
            locked {
                TraeAuthApi.restartWithValidToken(app, skewSec = SIXTY_MINUTES_SEC)
                    ?.let { t -> TraeAuthStore.load(app)?.let { c -> c.copy(accessToken = t) } }
            }
        }
        // 故障转移：上游**还没吐正文**就失败（token 死了的 401 / 限流 429 / 5xx）时换另一个账号重试一次。
        // 换号会**真的切过去并持久化** —— 否则下一个请求还拿同一个死账号去撞墙。
        s.failoverProvider = failover@{ failedUid, err ->
            val next = TraeAuthStore.pickFailoverCandidate(TraeAuthStore.list(app), failedUid)
                ?: return@failover null
            android.util.Log.i(TAG, "转发失败($err)，自动换到账号 ${next.uid}(${next.regionId})")
            locked {
                TraeAuthStore.setActive(app, next.uid)
                // 必须同步区域：否则会拿新账号的 token 打旧区域的 host
                TraeAuthStore.syncRegionToActiveAccount(app)
                TraeAuthApi.restartWithValidToken(app, skewSec = SIXTY_MINUTES_SEC)
                    ?.let { t -> TraeAuthStore.load(app)?.let { c -> c.copy(accessToken = t) } }
            }
        }
        // 换号也没救活 → 切回用户原来选的账号（否则账号会在请求间来回跳）
        s.failoverRollback = { originalUid ->
            android.util.Log.i(TAG, "换号后仍失败，切回原账号 $originalUid")
            locked {
                TraeAuthStore.setActive(app, originalUid)
                TraeAuthStore.syncRegionToActiveAccount(app)
            }
        }
        // ---- Qoder 后端（COSY 签名聊天，见 QoderUpstream）----
        // 模型表/后端选择：auto 时"只登了 Qoder 账号"就直接走 Qoder，老用户行为不变。
        s.appContext = app
        s.modelsProvider = { GatewayBackend.modelsFor(app) }
        s.backendProvider = { GatewayBackend.resolved(app) }
        // `auto` 时按模型名路由：Qoder 目录里的模型走 COSY，其余走 Trae。
        s.backendForModelProvider = { model -> GatewayBackend.routeFor(app, model) }
        s.qoderCredProvider = {
            lockedQoder<QoderAuthStore.QCred?> {
                val cred = QoderAuthStore.load(app) ?: return@lockedQoder null
                // 转发前续期：jobToken 是短时的，服务长驻不能只靠登录时那一个。
                val jt = QoderAuthApi.validJobTokenFor(app, cred, SIXTY_MINUTES_MS) ?: cred.jobToken
                if (jt.isBlank()) null else cred.copy(jobToken = jt)
            }
        }
        s.qoderFailoverProvider = qoderFailover@{ failedUid, err ->
            val next = QoderAuthStore.list(app).firstOrNull { it.uid != failedUid }
                ?: return@qoderFailover null
            android.util.Log.i(TAG, "Qoder 转发失败($err)，自动换到账号 ${next.uid}")
            lockedQoder<QoderAuthStore.QCred?> {
                QoderAuthStore.setActive(app, next.uid)
                val c = QoderAuthStore.load(app) ?: return@lockedQoder null
                val jt = QoderAuthApi.validJobTokenFor(app, c, SIXTY_MINUTES_MS) ?: c.jobToken
                if (jt.isBlank()) null else c.copy(jobToken = jt)
            }
        }
        s.qoderRollback = { originalUid ->
            android.util.Log.i(TAG, "Qoder 换号后仍失败，切回原账号 $originalUid")
            lockedQoder { QoderAuthStore.setActive(app, originalUid) }
        }
        server = s
        s.start()
        android.util.Log.i(TAG, "网关已启动，端口 ${LocalFakeServer.PORT}")
        // **不**在这里拉积分监测。
        //
        // 以前服务一启动就起一个每 2 秒轮询的线程 —— 服务是长驻的，于是没有请求时也一直打上游，
        // 一天上万次，账号的 token 会被上游判废。用户的要求是"没有请求模型的时候就停止扫描"，
        // 所以现在唯一的触发点是转发完成后的 [CreditMonitor.noteForwarded]（只查刚用到的那个账号）。
        // 用户若在界面上手动把检测间隔调成 > 0，由 MainActivity.onResume 起线程。
    }

    /** 幂等停止：没启动时调用也安全（用户可能没开就退出）。 */
    @Synchronized
    fun stop() {
        CreditMonitor.stop()
        server?.stop()
        server = null
        android.util.Log.i(TAG, "网关已停止")
    }

    /**
     * 自检：本地 API Key 必须**多次取值一致且非空**。
     *
     * 为什么值得钉：key 一旦变成"每次访问都重新生成"，所有已经填了 key 的客户端
     * 全部 401，而且现象是"昨天能用今天不能"，极难定位。
     */
    fun selfCheck(ctx: Context) {
        val a = apiKey(ctx)
        check(a.isNotBlank() && a.startsWith("sk-")) { "自检失败：本地 API Key 格式不对" }
        // 派生 key 是确定性的：两次取值必须一致（钉住"每次取值都变"会让所有客户端 401 的老 bug）
        check(a == apiKey(ctx)) { "自检失败：本地 API Key 不稳定（每次取值都变会让所有客户端 401）" }
        check(a.length == "sk-".length + 32) { "自检失败：派生 Key 长度不对（应为 sk- + 32 hex）" }
    }
}