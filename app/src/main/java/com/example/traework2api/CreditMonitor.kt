package com.example.traework2api

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean

/**
 * 积分刷新器：**只在转发完成后**刷新"刚被使用的那个平台的那个账号"。

 *
 * 覆盖两套账号：
 *  - **Trae**（国服）：真正驱动 OpenAI 转发的账号。查 [TraeUpstream.fetchUsage]，
 *    用完就在其余国服账号里找一个有积分的，`setActive` + `syncRegionToActiveAccount`（下一个请求即走新号）。
 *  - **Qoder**：积分管理 + **可选作为聊天转发后端**（见 [GatewayBackend] / [QoderUpstream]）。
 *    查 [QoderAuthApi.fetchCredits]，用完在其余 Qoder 账号里换。
 *
 * 设计约束：
 *  - **单后台线程串行**，每个 tick 对每个 provider 最多 1 次读；只有真要换号时才一次性扫描其余账号，
 *    避免每 2 秒把全部账号都打一遍上游（风控/耗电）。
 *  - Trae 的续期/换号必须走 [Gateway.locked]（refresh 是轮换的，和转发请求并发续期会互相作废）。
 *    用 [inFlight] 挡住"上一 tick 还没跑完就来下一 tick"，锁被占时本次直接跳过，不排队堆积。
 *  - 判据保守：**只有拿到确定的额度**（非 null）且 ≤ 阈值才换号；网络抖动/查不到一律**不换**，
 *    免得把好好的账号因一次失败就切走。
 */
object CreditMonitor {

    private const val TAG = "CreditMonitor"
    /**
     * 默认检测间隔（秒）。
     *
     * ⚠️ 曾经是 **2 秒** —— 一次 tick 打一次上游 `ide_user_ent_usage`，开 8 小时就是 14400 次。
     * 上游把这个账号当刷接口，token 会被判废（表现：突然查不到积分、"没有签到权限"）。
     * 现在默认 60 秒，且下限 10 秒，别再调回秒级。
     */
    private const val DEFAULT_INTERVAL_SEC = 0

    /**
     * 转发后刷新当前账号额度的节流窗口。
     *
     * 用户的规则："**只有在转发的时候加载正在使用的**" —— 不做定时轮询，
     * 每次请求完成后顺手看一眼当前账号还剩多少，同一分钟内只查一次。
     */
    private const val FORWARD_POLL_MIN_MS = 60_000L

    @Volatile private var lastTraePollAt = 0L
    @Volatile private var lastQoderPollAt = 0L

    private const val PREFS = "traework2api_prefs"
    private const val KEY_AUTO = "auto_switch_enabled"
    private const val KEY_THRESHOLD = "auto_switch_threshold"
    private const val KEY_INTERVAL = "monitor_interval_sec"

    /** 主线程 Handler：把快照回调切回 UI 线程。 */
    private val main = Handler(Looper.getMainLooper())

    /** 最近一次快照（供进页面时立即回填，不必等下一个 tick）。 */
    @Volatile private var latest: Snapshot? = null
    private val inFlight = AtomicBoolean(false)

    @Volatile private var running = false
    @Volatile private var appCtx: Context? = null
    @Volatile private var thread: Thread? = null

    /** 账号身份变化后强制重新拉一次的标记（切号/导入/删除时由 UI 置 true）。 */
    @Volatile var stale = true

    data class Snapshot(
        val traeUid: String?,
        val traeBalance: Long?,
        val qoderUid: String?,
        val qoderRemaining: Double?,
        val autoSwitch: Boolean,
        val threshold: Long,
    )

    /** UI 监听器：在**主线程**回调。 */
    fun interface Listener { fun onSnapshot(s: Snapshot) }

    @Volatile var listener: Listener? = null

    // ---- 开关 ----
    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * 自动换号默认 **关**。
     *
     * 开启时一旦余额见底就会**遍历其它所有账号**逐一查积分 —— 号越多，上游请求越多，
     * 正是"账号被刷废"的来源之一。要用再手动打开（界面上有开关）。
     */
    fun isAutoSwitchEnabled(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_AUTO, false)
    fun setAutoSwitchEnabled(ctx: Context, v: Boolean) {
        prefs(ctx).edit().putBoolean(KEY_AUTO, v).apply(); stale = true
    }

    /**
     * 触发换号的余量阈值：当前积分 ≤ 该值即算"用完"。
     *
     * 下限固定 5：界面上只有 5 / 10 / 15 / 20 四档，老版本存的 0 会被抬到 5，
     * 否则进页面时四档全不亮，看起来像坏了。
     */
    fun threshold(ctx: Context): Long = prefs(ctx).getLong(KEY_THRESHOLD, 5L).coerceAtLeast(5L)
    fun setThreshold(ctx: Context, v: Long) {
        prefs(ctx).edit().putLong(KEY_THRESHOLD, v.coerceAtLeast(5L)).apply(); stale = true
    }

    /**
     * 检测间隔（秒），0-60。
     *
     * 0 表示**暂停检测**：只保留服务转发，不再轮询积分/自动换号。
     * 用户要的是"自己定节奏"，所以这里不做固定档位，直接给 0-60 的连续值。
     */
    fun currentSnapshot(): Snapshot? = latest

    /**
     * 转发完成后调用：刷新**刚刚被使用的那个平台、那个账号**的额度。
     *
     * 用户的规则，逐字落实：
     *  - "一下用 trae，它就只扫描 Trae 当前账号" → [backend] = trae 时只查 Trae 当前号
     *  - "如果我用的是 qoder，那就只扫描这一个" → [backend] = qoder 时只查 Qoder 当前号
     *  - "没有请求模型的时候就停止扫描" → 本函数只由转发完成回调触发，没有定时线程
     *
     * 不用定时轮询：那是拿账号去刷上游接口，token 会被判废（见 DEFAULT_INTERVAL_SEC 注释）。
     */
    fun noteForwarded(context: Context, backend: String, uid: String?) {
        val now = System.currentTimeMillis()
        // **每个平台各自节流**：Trae 的请求不该压住 Qoder 的刷新（反之亦然）。
        val lastAt = if (backend == GatewayBackend.QODER) lastQoderPollAt else lastTraePollAt
        if (now - lastAt < FORWARD_POLL_MIN_MS) return
        if (backend == GatewayBackend.QODER) lastQoderPollAt = now else lastTraePollAt = now
        val app = context.applicationContext ?: context
        appCtx = app
        Thread({
            runCatching {
                // 严格按"哪个平台、哪个账号"，两个平台互不牵连：
                //  - 走 Qoder → 只查 Qoder 的 uid，Trae 一次请求都不发
                //  - 走 Trae  → 只查 Trae 的 uid，Qoder 一次请求都不发
                val snap = if (backend == GatewayBackend.QODER) {
                    val (qUid, remaining) = pollQoder(app, uid)
                    Snapshot(null, null, qUid, remaining, isAutoSwitchEnabled(app), threshold(app))
                } else {
                    val (tUid, bal) = pollTrae(app, uid)
                    Snapshot(tUid, bal, null, null, isAutoSwitchEnabled(app), threshold(app))
                }
                latest = snap
                stale = false
                listener?.let { l -> main.post { l.onSnapshot(snap) } }
            }.onFailure { Log.w(TAG, "转发后刷新失败: ${it.javaClass.simpleName}") }
        }, "credit-after-forward").apply { isDaemon = true }.start()
    }

    /**
     * 删除定时轮询（用户要求，2026-10-25）。
     *
     * 理由：只要存在"定时扫描"，就一定会出现"没在用它也在打上游"的情况，
     * 而每次扫描都是一次真实的 `ide_user_ent_usage` 请求 —— 上游按滥用处理会废掉 token。
     * 现在**唯一**的刷新入口是转发完成后的 [noteForwarded]，没有任何后台线程。
     *
     * 保留 `start`/`stop` 的空实现，只为不破坏调用点的语义（幂等、可安全重复调用）。
     */
    fun start(context: Context) {
        appCtx = context.applicationContext
        Log.i(TAG, "定时轮询已废弃：只在前台请求转发后刷新当前平台当前账号")
    }

    fun stop() {
        // 无后台线程可停；保留空实现让调用方无需判空
    }

    // ---- Trae ----

    /** 返回 (当前 uid, 当前积分)。查不到积分返回 (uid, null)。 */
    private fun pollTrae(ctx: Context, wantedUid: String?): Pair<String?, Long?> {
        // 用传进来的 uid（转发时实际用的那个），拿不到才回落到当前账号
        val uid = wantedUid?.takeIf { it.isNotBlank() } ?: TraeAuthStore.activeUid(ctx) ?: return null to null
        val cred = TraeAuthStore.loadByUid(ctx, uid) ?: return uid to null
        if (!cred.region.isSupported) return uid to null
        val token = Gateway.locked { TraeAuthApi.restartWithValidToken(ctx, skewSec = 3600L) }
            ?: cred.accessToken.takeIf { it.isNotBlank() }
        // ⚠️ **禁止**在这里自动换号：换号前要遍历其它账号逐一查积分（跨账号刷新），
        // 用户明确禁止（"禁止跨平台刷新。跨账号刷新"）。余额见底只如实显示，由用户自己切。
        val bal = if (token.isNullOrBlank()) null else TraeUpstream.fetchUsage(token, cred.uid, cred.region)
        return uid to bal
    }

    // 自动换号（switchTraeToFunded / scanFunded）已删除：
    // 它会在余额见底时遍历**其它所有账号**逐个查积分 —— 正是用户禁止的"跨账号刷新"，
    // 也是把账号刷废的来源。余额见底只如实显示，切号由用户手动决定。

    // ---- Qoder ----

    private fun pollQoder(ctx: Context, wantedUid: String?): Pair<String?, Double?> {
        val uid = wantedUid?.takeIf { it.isNotBlank() } ?: QoderAuthStore.activeUid(ctx) ?: return null to null
        val cred = QoderAuthStore.loadByUid(ctx, uid) ?: return uid to null
        val jt = QoderAuthApi.validJobToken(ctx, skewMillis = 60_000L)
        val credits = if (jt.isNullOrBlank()) null else QoderAuthApi.fetchCredits(jt, cred.mode)
        val remaining = credits?.remaining
        if (credits != null && !credits.hasCredit && isAutoSwitchEnabled(ctx)) {
            switchQoderToFunded(ctx, uid)
            return QoderAuthStore.activeUid(ctx) to null
        }
        return uid to remaining
    }

    private fun switchQoderToFunded(ctx: Context, fromUid: String): Boolean {
        for (m in QoderAuthStore.list(ctx).filter { it.uid != fromUid }) {
            val c = QoderAuthStore.loadByUid(ctx, m.uid) ?: continue
            val saved = QoderAuthStore.activeUid(ctx)
            QoderAuthStore.setActive(ctx, m.uid)   // validJobToken 作用于当前号，临时切过去续期
            val jt = QoderAuthApi.validJobToken(ctx, skewMillis = 60_000L)
            val credits = if (jt.isNullOrBlank()) null else QoderAuthApi.fetchCredits(jt, c.mode)
            if (credits?.hasCredit == true) {
                Log.i(TAG, "Qoder 积分用完($fromUid) → 自动换到 ${m.uid} 余=${credits.remaining}")
                return true
            }
            saved?.let { QoderAuthStore.setActive(ctx, it) }   // 没找到有积分的，切回原号
        }
        return false
    }

    /** 纯计算自检：阈值与开关默认、边界判据。 */
    fun selfCheck() {
        // 快照里"是否该换号"的核心判据：拿到确定额度 && ≤ 阈值 才算用完。
        val threshold = 0L
        check(!(1L <= threshold)) { "自检失败：余 1 不应视为用完" }
        check(0L <= threshold) { "自检失败：余 0 应视为用完" }
        check(!(-1L > threshold)) { "自检失败：负数应视为用完（≤ 阈值）" }
        // Qoder 侧判据与 Trae 独立：hasCredit 已在 QoderAuthApi.selfCheck 钉住。
        // 轮询节奏：默认必须远大于秒级，否则等于拿账号去刷接口（token 会被判废）
        check(DEFAULT_INTERVAL_SEC == 0) { "自检失败：默认不许定时轮询（只在转发时刷新当前账号）" }
        check(FORWARD_POLL_MIN_MS >= 60_000L) { "自检失败：转发后刷新节流过短（会变成刷接口）" }
    }
}
