package com.example.traework2api

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.PowerManager
import kotlin.concurrent.thread

/**
 * Qoder「每天领 Credits」定时闹钟接收器（与 [CheckinReceiver] 同样的独立 receiver 形态）。
 *
 * 独立于 [GatewayService]：网关 onStartCommand 对非 STOP/WATCHDOG intent 会无条件 markShouldRun(true)，
 * 会把用户手动停掉的网关复活；且 Android 12+ 从闹钟拉前台服务受限。故走 receiver + goAsync + 自带 wakeLock。
 */
class QoderCheckinReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "QoderCheckinReceiver"
        /** 覆盖 N 账号 × (list+claims) × 网络超时上界；到期兜底释放，防漏放。 */
        private const val WAKELOCK_TIMEOUT_MS = 2 * 60_000L
    }

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != QoderCheckin.ACTION_ALARM) return
        if (!QoderCheckin.isEnabled(context)) {
            android.util.Log.i(TAG, "闹钟到达但 Qoder 定时领取已关闭 → 忽略")
            return
        }
        val app = context.applicationContext
        val pending = goAsync()
        val pm = app.getSystemService(Context.POWER_SERVICE) as PowerManager
        val wl = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "traework2api:qoder-checkin").apply {
            setReferenceCounted(false)
            acquire(WAKELOCK_TIMEOUT_MS)
        }
        thread(name = "qoder-checkin-alarm") {
            try {
                if (!QoderCheckin.busy.compareAndSet(false, true)) {
                    android.util.Log.i(TAG, "手动领取正在进行 → 本轮定时跳过")
                    return@thread
                }
                try {
                    val result = QoderCheckin.runAll(app)
                    QoderCheckin.notifyIfScheduled(app, result)
                    android.util.Log.i(TAG, "Qoder 定时领取完成: ${result.results.count { it.ok }} 成功")
                } finally {
                    QoderCheckin.busy.set(false)
                }
            } catch (e: Exception) {
                android.util.Log.w(TAG, "Qoder 定时领取异常: ${e.javaClass.simpleName}: ${e.message}")
            } finally {
                QoderCheckin.scheduleNext(app)   // 触发后自续：挂上明天的闹钟
                runCatching { if (wl.isHeld) wl.release() }
                pending.finish()
            }
        }
    }
}
