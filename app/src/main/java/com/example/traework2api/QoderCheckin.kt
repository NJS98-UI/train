package com.example.traework2api

import android.app.AlarmManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Qoder「每天领 Credits」的定时领取 + 进 App 补签（编排 [runAll]）。
 *
 * 三个入口共用同一条编排：
 *  1. 账号页「领取每日 Credits / 一键领取全部」手动按钮（在 MainActivity，走 [QoderAuthApi]）
 *  2. 每日定时闹钟（[QoderCheckinReceiver]，默认关闭、用户手动开启）
 *  3. 进 App 补签（[maybeCatchUp]：定时开着但闹钟没跑成 —— 进程被杀/手机重启，无 BOOT 自启）
 *
 * 与 Trae 的 [BatchCheckin] 相互独立：Qoder 不参与本地转发，领额度也不碰 [Gateway.locked]
 * （Trae 那把锁是为轮换 refreshToken 不被并发作废；Qoder 的续期在 [QoderAuthApi] 内部按账号串行做）。
 */
object QoderCheckin {

    private const val TAG = "QoderCheckin"

    private const val PREFS = "qoder_checkin"
    private const val KEY_ENABLED = "schedule_enabled"
    private const val KEY_TIME = "schedule_time"
    private const val KEY_LAST_RUN_DATE = "last_run_date"
    private const val KEY_LAST_RESULTS = "last_results"

    private const val CHANNEL_ID = "qoder_checkin"
    private const val NOTIFICATION_ID = 8794
    /** 闹钟 request code（避开网关 8789/8790 与 Trae 签到 8792）。 */
    private const val ALARM_REQUEST = 8793
    const val ACTION_ALARM = "com.example.traework2api.QODER_CHECKIN_ALARM"

    /** 手动/定时互斥。 */
    val busy = AtomicBoolean(false)

    /** 单账号领取结果（持久化展示用，不含 token）。 */
    data class PerAccount(val uid: String, val name: String, val status: String, val amount: Long, val message: String) {
        val ok: Boolean get() = status == "ok"
    }

    data class BatchResult(val results: List<PerAccount>, val date: String) {
        val allOk: Boolean get() = results.isNotEmpty() &&
            results.all { it.ok || it.status == "already" || it.status == "unsupported" }
    }

    // ------------------------------------------------------------------
    // 设置
    // ------------------------------------------------------------------

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun isEnabled(ctx: Context): Boolean = prefs(ctx).getBoolean(KEY_ENABLED, false)

    /** 定时时刻 "HH:mm"；损坏回落 09:00。 */
    fun timeText(ctx: Context): String =
        prefs(ctx).getString(KEY_TIME, "09:00")?.takeIf { Regex("^\\d{1,2}:\\d{2}$").matches(it) } ?: "09:00"

    private fun timeMinutes(ctx: Context): Int {
        val (h, m) = timeText(ctx).split(":").map { it.toIntOrNull() ?: 0 }
        return h.coerceIn(0, 23) * 60 + m.coerceIn(0, 59)
    }

    fun enable(ctx: Context, time: String) {
        prefs(ctx).edit().putBoolean(KEY_ENABLED, true).putString(KEY_TIME, time).apply()
        scheduleNext(ctx)
        Log.i(TAG, "Qoder 定时领取已开启: $time")
    }

    fun disable(ctx: Context) {
        prefs(ctx).edit().putBoolean(KEY_ENABLED, false).apply()
        cancelAlarm(ctx)
        Log.i(TAG, "Qoder 定时领取已关闭")
    }

    fun setTime(ctx: Context, time: String) {
        prefs(ctx).edit().putString(KEY_TIME, time).apply()
        if (isEnabled(ctx)) scheduleNext(ctx)
    }

    // ------------------------------------------------------------------
    // 每日闹钟
    // ------------------------------------------------------------------

    /** 挂下一个触发时刻的闹钟（幂等；写法对齐 [BatchCheckin.scheduleNext]）。 */
    fun scheduleNext(ctx: Context) {
        if (!isEnabled(ctx)) return
        runCatching {
            val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
            val pi = PendingIntent.getBroadcast(
                ctx, ALARM_REQUEST,
                Intent(ctx, QoderCheckinReceiver::class.java).setAction(ACTION_ALARM),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            )
            val cal = Calendar.getInstance()
            val minutes = timeMinutes(ctx)
            cal.set(Calendar.HOUR_OF_DAY, minutes / 60)
            cal.set(Calendar.MINUTE, minutes % 60)
            cal.set(Calendar.SECOND, 0)
            cal.set(Calendar.MILLISECOND, 0)
            if (cal.timeInMillis <= System.currentTimeMillis()) cal.add(Calendar.DAY_OF_YEAR, 1)
            val canExact = Build.VERSION.SDK_INT < 31 || am.canScheduleExactAlarms()
            if (canExact) {
                runCatching { am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, cal.timeInMillis, pi) }
                    .onFailure { am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, cal.timeInMillis, pi) }
            } else {
                Log.w(TAG, "精确闹钟未授权，Qoder 定时降级为非精确（Doze 下可能延迟）")
                am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, cal.timeInMillis, pi)
            }
            Log.i(TAG, "已挂 Qoder 领取闹钟: ${SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.US).format(cal.time)}")
        }.onFailure { Log.w(TAG, "Qoder 领取闹钟调度失败", it) }
    }

    fun cancelAlarm(ctx: Context) {
        runCatching {
            val am = ctx.getSystemService(Context.ALARM_SERVICE) as AlarmManager
            am.cancel(PendingIntent.getBroadcast(
                ctx, ALARM_REQUEST,
                Intent(ctx, QoderCheckinReceiver::class.java).setAction(ACTION_ALARM),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            ))
        }.onFailure { Log.w(TAG, "Qoder 领取闹钟取消失败", it) }
    }

    // ------------------------------------------------------------------
    // 批量编排（唯一领取执行路径）
    // ------------------------------------------------------------------

    /**
     * 对全部已保存 Qoder 账号领取每日 Credits。**阻塞** —— 必须在后台线程跑。
     * 签到走 OpenAPI、与网关的聊天链路互不影响，故不加 [Gateway.locked]。
     * 结果持久化并把 last_run_date 记为今天。
     */
    fun runAll(ctx: Context): BatchResult {
        val raw = QoderAuthApi.claimDailyAll(ctx)
        val names = QoderAuthStore.list(ctx).associate { it.uid to it.name.ifBlank { it.uid } }
        val results = raw.map { (uid, r) ->
            PerAccount(uid, names[uid] ?: uid, r.status, r.amount, r.message)
        }
        val today = today()
        prefs(ctx).edit()
            .putString(KEY_LAST_RUN_DATE, today)
            .putString(KEY_LAST_RESULTS, toJson(results).toString())
            .apply()
        Log.i(TAG, "Qoder 定时领取完成: ${results.count { it.ok }} 成功 / ${results.size} 账号")
        return BatchResult(results, today)
    }

    // ------------------------------------------------------------------
    // 进 App 补签 + 自愈重挂
    // ------------------------------------------------------------------

    /**
     * 进 App 调用（后台线程）：定时开着 && 今天还没全领 && 还有账号 → 补跑一轮；
     * 无论跑不跑，只要开关开着就重挂一次闹钟（自愈：重启/被 ROM 清掉的闹钟在此恢复）。
     * @return 本次是否真的跑了（调用方据此刷新 UI）。
     */
    fun maybeCatchUp(ctx: Context): Boolean {
        if (!isEnabled(ctx)) return false
        scheduleNext(ctx)   // 自愈重挂：幂等
        if (prefs(ctx).getString(KEY_LAST_RUN_DATE, null) == today()) {
            val last = lastResults(ctx)
            if (last != null && last.allOk) return false   // 今天全领上了，别再打上游
        }
        if (!busy.compareAndSet(false, true)) return false
        try {
            runAll(ctx)
            return true
        } finally {
            busy.set(false)
        }
    }

    /** 定时触发路径的成功通知（手动路径不发 —— 用户就在屏幕前）。 */
    fun notifyIfScheduled(ctx: Context, result: BatchResult) {
        val okCount = result.results.count { it.ok }
        if (okCount == 0) return
        runCatching { ensureChannel(ctx) }.onFailure { return }
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(ctx, CHANNEL_ID)
        else @Suppress("DEPRECATION") Notification.Builder(ctx)
        val total = result.results.filter { it.ok }.sumOf { it.amount }
        val detail = result.results.filter { it.ok }
            .joinToString("、") { "${it.name} +${it.amount}" }
        val n = builder
            .setSmallIcon(R.drawable.ic_launcher_foreground)
            .setContentTitle("Qoder 每日领取完成")
            .setContentText("$okCount 个账号领取成功，共 +$total Credits")
            .setStyle(Notification.BigTextStyle().bigText("$okCount 个账号领取成功：$detail"))
            .setCategory(Notification.CATEGORY_EVENT)
            .setAutoCancel(true)
            .build()
        runCatching { nm.notify(NOTIFICATION_ID, n) }
    }

    private fun ensureChannel(ctx: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val mgr = ctx.getSystemService(NotificationManager::class.java) ?: return
        if (mgr.getNotificationChannel(CHANNEL_ID) != null) return
        val ch = NotificationChannel(CHANNEL_ID, "Qoder 领取结果", NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = "Qoder 每日定时领取 Credits 的成功通知"
            setShowBadge(false)
            setSound(null, null)
            enableVibration(false)
        }
        mgr.createNotificationChannel(ch)
    }

    // ------------------------------------------------------------------
    // 结果读取
    // ------------------------------------------------------------------

    /** 上次批量结果（无/损坏返回 null）。 */
    fun lastResults(ctx: Context): BatchResult? {
        val raw = prefs(ctx).getString(KEY_LAST_RESULTS, null) ?: return null
        val date = prefs(ctx).getString(KEY_LAST_RUN_DATE, null) ?: return null
        return try {
            val arr = JSONArray(raw)
            val list = (0 until arr.length()).mapNotNull { i ->
                val o = arr.optJSONObject(i) ?: return@mapNotNull null
                PerAccount(o.optString("uid", ""), o.optString("name", ""),
                    o.optString("status", "fail"), o.optLong("amount", 0L), o.optString("message", ""))
            }
            BatchResult(list, date)
        } catch (_: Exception) { null }
    }

    fun lastRunToday(ctx: Context): Boolean = prefs(ctx).getString(KEY_LAST_RUN_DATE, null) == today()

    private fun toJson(list: List<PerAccount>): JSONArray = JSONArray().apply {
        list.forEach { r ->
            put(JSONObject().put("uid", r.uid).put("name", r.name).put("status", r.status)
                .put("amount", r.amount).put("message", r.message))
        }
    }

    private fun today(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())

    /** 结果状态 → 人话（账号页每行用）。 */
    fun statusLabel(r: PerAccount): String = when (r.status) {
        "ok" -> "✓ 领取成功 +${r.amount}"
        "already" -> r.message.ifBlank { "今日已领取" }
        "unsupported" -> "该区域/活动暂不可领（见消息）"
        else -> "失败：${r.message}"
    }

    /**
     * 纯计算自检：不碰网络/闹钟。钉住日期去重、allOk 聚合、状态文案、结果往返。
     */
    fun selfCheck() {
        val src = listOf(
            PerAccount("q-1", "甲", "ok", 100, ""),
            PerAccount("q-2", "乙", "fail", 0, "拉取活动失败"),
        )
        val back = try {
            val arr = toJson(src)
            (0 until arr.length()).map { i ->
                val o = arr.getJSONObject(i)
                PerAccount(o.getString("uid"), o.getString("name"), o.getString("status"),
                    o.getLong("amount"), o.getString("message"))
            }
        } catch (e: Exception) { throw IllegalStateException("自检失败：结果序列化异常 ${e.message}") }
        check(back == src) { "自检失败：Qoder 领取结果往返不一致" }

        listOf("ok", "already", "unsupported", "fail").forEach { s ->
            check(statusLabel(PerAccount("u", "n", s, 0, "x")).isNotBlank()) { "自检失败：status=$s 缺文案" }
        }

        check(BatchResult(listOf(PerAccount("a", "甲", "ok", 1, "")), "2026-01-01").allOk) { "自检失败：全成功应 allOk" }
        check(BatchResult(listOf(PerAccount("a", "甲", "already", 0, "")), "2026-01-01").allOk) { "自检失败：已领应 allOk" }
        check(BatchResult(listOf(PerAccount("a", "甲", "fail", 0, "x")), "2026-01-01").allOk.not()) { "自检失败：有失败不应 allOk" }
        check(BatchResult(emptyList(), "2026-01-01").allOk.not()) { "自检失败：空结果不应 allOk" }
    }
}
