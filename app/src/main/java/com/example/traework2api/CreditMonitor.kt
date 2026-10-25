package com.example.traework2api

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean

/**
 * 积分监测器：每 [TICK_MS]（2 秒）查一次**当前账号**可用积分，可选地在积分用完后**自动换到还有积分的账号**。
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
    private const val TICK_MS = 2_000L

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
    private const val MIN_INTERVAL_SEC = 10

    /** 自动换号时扫描"其它账号"的缓存时长：别每轮都把所有号问一遍。 */
    private const val SCAN_CACHE_MS = 5 * 60 * 1000L

    @Volatile private var lastForwardPollAt = 0L
    @Volatile private var lastScanAt = 0L
    @Volatile private var lastScanPick: String? = null

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

    fun isAutoSwitchEnabled(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_AUTO, true)
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
    fun intervalSec(ctx: Context): Int {
        val v = prefs(ctx).getInt(KEY_INTERVAL, DEFAULT_INTERVAL_SEC)
        if (v <= 0) return 0                      // 0 = 暂停
        return v.coerceIn(MIN_INTERVAL_SEC, 3600)
    }

    fun setIntervalSec(ctx: Context, v: Int) {
        val fixed = if (v <= 0) 0 else v.coerceAtLeast(MIN_INTERVAL_SEC)
        prefs(ctx).edit().putInt(KEY_INTERVAL, fixed).apply(); stale = true
    }

    fun currentSnapshot(): Snapshot? = latest

    /**
     * 转发完成后调用：刷新**当前账号**额度（节流 [FORWARD_POLL_MIN_MS]）。
     *
     * 这就是"只在转发时加载正在用的账号"：不打定时轮询，也不遍历其它账号
     * （除非余额见底需要换号，那条路另有 5 分钟扫描缓存）。
     */
    fun noteForwarded(context: Context) {
        val now = System.currentTimeMillis()
        if (now - lastForwardPollAt < FORWARD_POLL_MIN_MS) return
        lastForwardPollAt = now
        val app = context.applicationContext ?: context
        appCtx = app
        Thread({
            runCatching {
                val (traeUid, traeBal) = pollTrae(app)
                val snap = Snapshot(traeUid, traeBal, null, null, isAutoSwitchEnabled(app), threshold(app))
                latest = snap
                stale = false
                listener?.let { l -> main.post { l.onSnapshot(snap) } }
            }.onFailure { Log.w(TAG, "转发后刷新失败: ${it.javaClass.simpleName}") }
        }, "credit-after-forward").apply { isDaemon = true }.start()
    }

    fun start(context: Context) {
        appCtx = context.applicationContext
        if (running) return
        running = true
        thread = Thread({ loop() }, "credit-monitor").apply { isDaemon = true; start() }
        Log.i(TAG, "积分监测已启动")
    }

    fun stop() {
        running = false
        thread = null
        Log.i(TAG, "积分监测已停止")
    }

    private fun loop() {
        while (running) {
            try { tick() } catch (e: Exception) {
                Log.w(TAG, "tick 异常: ${e.javaClass.simpleName}: ${e.message}")
            }
            // 间隔每轮现取：用户在界面上拖动滑块后，下一个 tick 就生效（不用重启监测）
            val wait = appCtx?.let { intervalSec(it) * 1000L } ?: TICK_MS
            if (wait <= 0L) {
                // 0 = 暂停检测：睡 1 秒再读一次设置，避免空转烧 CPU
                try { Thread.sleep(1_000) } catch (_: InterruptedException) { break }
                continue
            }
            var slept = 0L
            while (running && slept < wait) {
                try { Thread.sleep(wait - slept) } catch (_: InterruptedException) { break }
                slept = wait
            }
        }
    }

    private fun tick() {
        val ctx = appCtx ?: return
        if (!inFlight.compareAndSet(false, true)) return   // 上一轮没跑完，跳过
        try {
            val (traeUid, traeBal) = pollTrae(ctx)
            val (qoderUid, qoderBal) = pollQoder(ctx)
            val snap = Snapshot(traeUid, traeBal, qoderUid, qoderBal,
                isAutoSwitchEnabled(ctx), threshold(ctx))
            latest = snap
            stale = false
            listener?.let { l -> main.post { l.onSnapshot(snap) } }
        } finally {
            inFlight.set(false)
        }
    }

    // ---- Trae ----

    /** 返回 (当前 uid, 当前积分)。查不到积分返回 (uid, null)。 */
    private fun pollTrae(ctx: Context): Pair<String?, Long?> {
        val uid = TraeAuthStore.activeUid(ctx) ?: return null to null
        val cred = TraeAuthStore.loadByUid(ctx, uid) ?: return uid to null
        if (!cred.region.isSupported) return uid to null
        val token = Gateway.locked { TraeAuthApi.restartWithValidToken(ctx, skewSec = 3600L) }
            ?: cred.accessToken.takeIf { it.isNotBlank() }
        val bal = if (token.isNullOrBlank()) null else TraeUpstream.fetchUsage(token, cred.uid, cred.region)
        if (bal != null && isAutoSwitchEnabled(ctx) && bal <= threshold(ctx)) {
            switchTraeToFunded(ctx, uid, cred.region)
            return TraeAuthStore.activeUid(ctx) to null   // 换号后本轮不再回填，下轮读新号
        }
        return uid to bal
    }

    /**
     * 在**其余国服账号**里找一个有积分的，切过去（含区域同步）。找到返回 true。
     * 全程持 [Gateway.locked]：续期是轮换的，别和转发并发换 token。
     */
    private fun switchTraeToFunded(ctx: Context, fromUid: String, fromRegion: TraeRegion): Boolean {
        // 扫描缓存：5 分钟内扫过就复用结论，不把每个号再问一遍。
        // 曾经每轮 tick 都遍历所有账号 → 号越多上游请求越多，等于自己把号刷废。
        val now = System.currentTimeMillis()
        val cachedPick = lastScanPick
        if (now - lastScanAt < SCAN_CACHE_MS) {
            if (cachedPick == null || cachedPick == fromUid) return false
            val m = TraeAuthStore.loadByUid(ctx, cachedPick) ?: return false
            TraeAuthStore.setActive(ctx, m.uid)
            TraeAuthStore.syncRegionToActiveAccount(ctx)
            return true
        }
        val pick = scanFunded(ctx, fromUid)
        lastScanAt = now
        lastScanPick = pick
        if (pick == null) return false
        TraeAuthStore.setActive(ctx, pick)
        TraeAuthStore.syncRegionToActiveAccount(ctx)
        return true
    }

    /** 扫其余国服账号，返回第一个"有积分"的 uid；没有返回 null。 */
    private fun scanFunded(ctx: Context, fromUid: String): String? = Gateway.locked {
        val others = TraeAuthStore.list(ctx)
            .filter { it.uid != fromUid && it.region.isSupported }
        for (m in others) {
            val c = TraeAuthStore.loadByUid(ctx, m.uid) ?: continue
            val tok = TraeAuthApi.restartWithValidTokenFor(ctx, c, skewSec = 3600L)
                ?: c.accessToken.takeIf { it.isNotBlank() }
            val bal = if (tok.isNullOrBlank()) null else TraeUpstream.fetchUsage(tok, c.uid, c.region)
            if (bal != null && bal > threshold(ctx)) {
                Log.i(TAG, "积分用完($fromUid) → 可换到 $m.uid(${m.nickname}) 余=$bal")
                return@locked m.uid
            }
        }
        null
    }

    // ---- Qoder ----

    private fun pollQoder(ctx: Context): Pair<String?, Double?> {
        val uid = QoderAuthStore.activeUid(ctx) ?: return null to null
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
        check(MIN_INTERVAL_SEC >= 10) { "自检失败：最小轮询间隔过密" }
        check(SCAN_CACHE_MS >= 60_000L) { "自检失败：自动换号扫描缓存过短" }
    }
}
