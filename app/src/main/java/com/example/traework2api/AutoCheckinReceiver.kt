package com.example.traework2api

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlin.concurrent.thread

/**
 * 「开窗闹钟」接收器：新加坡时间 10:05 触发一次自动领取。
 *
 * 为什么单独一个 receiver：签到不该依赖用户打开 App（他可能整晚没碰手机），
 * 也不该寄生在网关服务上（那会连累用户的"停止服务"开关，见 [CheckinReceiver] 的说明）。
 */
class AutoCheckinReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != AutoCheckin.ACTION_ALARM) return
        val app = context.applicationContext
        val pending = goAsync()
        thread(name = "auto-checkin-alarm") {
            try {
                AutoCheckin.runAllOnce(app)
            } catch (e: Exception) {
                android.util.Log.w("AutoCheckinReceiver", "开窗补签异常: ${e.javaClass.simpleName}: ${e.message}")
            } finally {
                // 明天再来：闹钟是一次性的，必须自续
                AutoCheckin.scheduleWindowAlarm(app)
                pending.finish()
            }
        }
    }
}
