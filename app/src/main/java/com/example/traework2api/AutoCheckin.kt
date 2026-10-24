package com.example.traework2api

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.TimeZone
import java.util.Date
import java.util.Locale

/**
 * 启动补签：每次打开 App，把**全部平台、全部账号**当天没签的签到补上。
 *
 * 为什么需要它：定时签到依赖 AlarmManager，而进程被杀 / 手机重启后闹钟不会自己回来
 * （本 App 没有 BOOT 自启）。用户不该为了"今天有没有签到"专门记着去点按钮。
 *
 * 三条纪律：
 *  1. **每天只跑一次**（[KEY_DATE] 记日期）——否则每次切回前台都会把上游打一遍。
 *  2. **只补没签的**：先查状态，`checkedIn` 或上游 `enable=false` 都直接跳过，绝不硬发 claim。
 *  3. **失败不影响 App**：整个流程包在 runCatching 里，网络/令牌问题只写日志。
 */
object AutoCheckin {

    private const val PREFS = "traework2api_prefs"
    private const val KEY_DATE = "auto_checkin_date"
    private const val KEY_LAST_TRY = "auto_checkin_last_try"

    /**
     * 重试冷却：没领到就别一直打上游，但**不能一天只试一次**。
     *
     * 活动是到点才开（例：每天 10:00 开放当日领取）。如果 9:50 打开 App 就把"今天已跑过"
     * 记下来，10:00 之后开多少次都不会再领 —— 用户看到的就是"时间到了 App 也不会领"。
     */
    private const val RETRY_COOLDOWN_MS = 10 * 60 * 1000L

    /** 领取窗口：**新加坡时间 10:00 开窗**，10:05 触发（留 5 分钟余量）。 */
    const val ACTION_ALARM = "com.example.traework2api.AUTO_CHECKIN"
    private const val REQ_AUTO = 8801
    private const val WINDOW_HOUR = 10
    private const val WINDOW_MINUTE = 5

    /** 下一次开窗时刻（毫秒）。今天已过就顺延到明天。 */
    fun nextWindowAt(now: Long = System.currentTimeMillis()): Long {
        val cal = Calendar.getInstance(TimeZone.getTimeZone("Asia/Singapore"))
        cal.timeInMillis = now
        cal.set(Calendar.HOUR_OF_DAY, WINDOW_HOUR)
        cal.set(Calendar.MINUTE, WINDOW_MINUTE)
        cal.set(Calendar.SECOND, 0)
        cal.set(Calendar.MILLISECOND, 0)
        if (cal.timeInMillis <= now) cal.add(Calendar.DAY_OF_YEAR, 1)
        return cal.timeInMillis
    }

    /**
     * 挂"开窗闹钟"：到点自动领一次，不依赖用户开 App。
     *
     * 用 `setAndAllowWhileIdle`（不是 exact）：精确闹钟在 Android 12+ 要单独申请权限，
     * 而领取窗口本身有十几分钟余量，差几分钟无所谓。
     */
    fun scheduleWindowAlarm(ctx: Context) {
        val am = ctx.getSystemService(Context.ALARM_SERVICE) as? AlarmManager ?: return
        val pi = PendingIntent.getBroadcast(
            ctx, REQ_AUTO,
            Intent(ctx, AutoCheckinReceiver::class.java).setAction(ACTION_ALARM),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        runCatching { am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, nextWindowAt(), pi) }
            .onFailure { android.util.Log.w(TAG, "开窗闹钟挂不上: ${it.javaClass.simpleName}") }
    }
    private const val TAG = "AutoCheckin"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun today(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())

    /** 今天是否已经**收工**（领到了，或确认没有可领的）。 */
    fun alreadyRanToday(ctx: Context): Boolean = prefs(ctx).getString(KEY_DATE, null) == today()

    private fun markDone(ctx: Context) {
        prefs(ctx).edit().putString(KEY_DATE, today()).apply()
    }

    /**
     * 现在该不该尝试一轮。
     *
     * - 今天已收工 → 不试
     * - 距上次尝试不到 [RETRY_COOLDOWN_MS] → 不试（防抖）
     * - 否则 → 试（这就是"到点后再开 App 还能领"的那条路）
     */
    fun shouldAttempt(ctx: Context): Boolean {
        if (alreadyRanToday(ctx)) return false
        val last = prefs(ctx).getLong(KEY_LAST_TRY, 0L)
        return System.currentTimeMillis() - last >= RETRY_COOLDOWN_MS
    }

    /**
     * 跑一轮补签。**调用方负责放到后台线程。**
     *
     * @return 人类可读摘要（null = 今天已经跑过 / 没有可签的账号）
     */
    fun runAllOnce(ctx: Context): String? {
        if (!shouldAttempt(ctx)) return null
        prefs(ctx).edit().putLong(KEY_LAST_TRY, System.currentTimeMillis()).apply()
        val parts = ArrayList<String>()
        var claimed = false
        var allSettled = true

        // ---- Trae：逐个账号查状态，未签才签 ----
        runCatching {
            for (m in TraeAuthStore.list(ctx)) {
                val cred = TraeAuthStore.loadByUid(ctx, m.uid) ?: continue
                if (cred.accessToken.isBlank()) continue
                val st = TraeUpstream.fetchCheckin(cred.accessToken, m.uid, cred.region) ?: continue
                if (st.checkedIn || !st.enable) continue
                val code = TraeUpstream.claimCheckin(cred.accessToken, m.uid, cred.region) ?: continue
                if (code in 200..299) {
                    claimed = true
                    parts.add("Trae ${m.nickname.ifBlank { m.uid }} 已签到")
                }
            }
        }.onFailure { android.util.Log.w(TAG, "Trae 补签失败: ${it.javaClass.simpleName}: ${it.message}") }

        // ---- Qoder：全部账号领每日 Credits（复用定时领取那一套，含结果落盘）----
        runCatching {
            if (QoderAuthStore.list(ctx).isNotEmpty()) {
                val r = QoderCheckin.runAll(ctx)
                if (r.results.any { it.ok }) claimed = true
                // 全失败（网络/令牌）说明还没跑成，别封盘，等下一轮重试
                if (r.results.any { !it.ok && it.status != "already" }) allSettled = false
                parts.add("Qoder " + r.results.joinToString("；") { QoderCheckin.statusLabel(it) })
            }
        }.onFailure { android.util.Log.w(TAG, "Qoder 补签失败: ${it.javaClass.simpleName}: ${it.message}") }

        // 领到了、或确实没有可领的 → 今天收工；否则留着下次再试（活动可能到点才开）
        if (claimed || allSettled) markDone(ctx)
        if (parts.isEmpty()) return null
        val summary = parts.joinToString(" · ")
        android.util.Log.i(TAG, "启动补签: $summary")
        return summary
    }

    fun selfCheck() {
        // 日期键必须随日期变化：写死会让"每天一次"退化成"永远只跑一次"
        check(today().matches(Regex("^\\d{4}-\\d{2}-\\d{2}$"))) { "自检失败：日期格式不对" }
        // 冷却必须是正数：写成 0 就等于每次前台都打一遍上游
        check(RETRY_COOLDOWN_MS >= 60_000L) { "自检失败：重试冷却过短" }
        // 窗口必须落在 10:00 之后几分钟：算成 9:55 就等于永远抢不到当天活动
        val t = nextWindowAt(0L)
        val cal = Calendar.getInstance(TimeZone.getTimeZone("Asia/Singapore")).apply { timeInMillis = t }
        check(cal.get(Calendar.HOUR_OF_DAY) == WINDOW_HOUR) { "自检失败：开窗小时不对" }
        check(nextWindowAt(System.currentTimeMillis()) > System.currentTimeMillis()) {
            "自检失败：下一次开窗必须在未来"
        }
    }
}
