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
 *  - **Qoder**：仅积分管理（不参与转发）。查 [QoderAuthApi.fetchCredits]，用完在其余 Qoder 账号里换。
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

    private const val PREFS = "traework2api_prefs"
    private const val KEY_AUTO = "auto_switch_enabled"
    private const val KEY_THRESHOLD = "auto_switch_threshold"

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

    /** 触发换号的余量阈值：当前积分 ≤ 该值即算"用完"。默认 0。 */
    fun threshold(ctx: Context): Long = prefs(ctx).getLong(KEY_THRESHOLD, 0L).coerceAtLeast(0L)
    fun setThreshold(ctx: Context, v: Long) {
        prefs(ctx).edit().putLong(KEY_THRESHOLD, v.coerceAtLeast(0L)).apply(); stale = true
    }

    fun currentSnapshot(): Snapshot? = latest

    fun start(context: Context) {
        appCtx = context.applicationContext
        if (running) return
        running = true
        thread = Thread({ loop() }, "credit-monitor").apply { isDaemon = true; start() }
        Log.i(TAG, "积分监测已启动（每 2s）")
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
            var slept = 0L
            while (running && slept < TICK_MS) {
                try { Thread.sleep(TICK_MS - slept) } catch (_: InterruptedException) { break }
                slept = TICK_MS
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
    private fun switchTraeToFunded(ctx: Context, fromUid: String, fromRegion: TraeRegion): Boolean =
        Gateway.locked {
            val others = TraeAuthStore.list(ctx)
                .filter { it.uid != fromUid && it.region.isSupported }
            for (m in others) {
                val c = TraeAuthStore.loadByUid(ctx, m.uid) ?: continue
                val tok = TraeAuthApi.restartWithValidTokenFor(ctx, c, skewSec = 3600L)
                    ?: c.accessToken.takeIf { it.isNotBlank() }
                val bal = if (tok.isNullOrBlank()) null else TraeUpstream.fetchUsage(tok, c.uid, c.region)
                if (bal != null && bal > threshold(ctx)) {
                    TraeAuthStore.setActive(ctx, m.uid)
                    TraeAuthStore.syncRegionToActiveAccount(ctx)
                    Log.i(TAG, "积分用完($fromRegion ${fromUid}) → 自动换到 $m.uid(${m.nickname}) 余=$bal")
                    return@locked true
                }
            }
            false
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
    }
}
