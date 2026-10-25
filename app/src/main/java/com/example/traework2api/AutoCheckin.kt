package com.example.traework2api

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import org.json.JSONObject
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/**
 * 补签：把**全部平台、全部账号**当天没签的补上。
 *
 * 三条纪律：
 *  1. **按账号记"今天已处理"**（[KEY_DONE] 存 `平台:uid → 日期`），不是按天记一次。
 *     按天记的老毛病：今天已经跑过一轮，用户新加一个账号，那个账号今天永远不会被补签。
 *  2. **只补没签的**：先查状态，`checkedIn` 或上游 `enable=false` 都直接跳过，绝不硬发 claim。
 *  3. **失败不影响 App**：整段包在 runCatching 里，网络/令牌问题只写日志，账号留着下轮再试。
 */
object AutoCheckin {

    private const val TAG = "AutoCheckin"
    private const val PREFS = "traework2api_prefs"

    /** `{"trae:uid":"2026-10-25", ...}` —— 每个账号各自记到哪天处理完。 */
    private const val KEY_DONE = "auto_checkin_done"
    private const val KEY_LAST_TRY = "auto_checkin_last_try"

    /**
     * 重试冷却：没领到就别一直打上游，但**不能一天只试一次**。
     *
     * 活动是到点才开（每天 10:00 UTC+8 刷新）。如果 9:50 打开 App 就把"今天已跑过"
     * 记下来，10:00 之后开多少次都不会再领 —— 用户看到的就是"时间到了 App 也不会领"。
     */
    private const val RETRY_COOLDOWN_MS = 10 * 60 * 1000L

    /** 领取窗口：**新加坡时间 10:00 开窗**，10:05 触发（留 5 分钟余量）。 */
    const val ACTION_ALARM = "com.example.traework2api.AUTO_CHECKIN"
    private const val REQ_AUTO = 8801
    private const val WINDOW_HOUR = 10
    private const val WINDOW_MINUTE = 5

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun today(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())

    private fun doneMap(ctx: Context): JSONObject = runCatching {
        JSONObject(prefs(ctx).getString(KEY_DONE, "{}") ?: "{}")
    }.getOrElse { JSONObject() }

    private fun isDone(ctx: Context, key: String): Boolean = doneMap(ctx).optString(key, "") == today()

    /** 记一个账号"今天处理完"。只写内存外的 prefs，不动其它账号的记录。 */
    fun markDone(ctx: Context, key: String) {
        val o = doneMap(ctx)
        runCatching { o.put(key, today()) }
        prefs(ctx).edit().putString(KEY_DONE, o.toString()).apply()
    }

    /**
     * 今天还没处理的账号，返回 `平台 to uid`。
     *
     * 这就是"新登录的账号会不会被补签"的答案：只要它在账号表里且今天没记过，就会被列进来。
     */
    fun pending(ctx: Context): List<Pair<String, String>> {
        val out = ArrayList<Pair<String, String>>()
        runCatching {
            for (m in TraeAuthStore.list(ctx)) {
                if (!isDone(ctx, "trae:${m.uid}")) out.add("trae" to m.uid)
            }
        }.onFailure { Log.w(TAG, "读 Trae 账号失败: ${it.javaClass.simpleName}") }
        runCatching {
            for (m in QoderAuthStore.list(ctx)) {
                if (!isDone(ctx, "qoder:${m.uid}")) out.add("qoder" to m.uid)
            }
        }.onFailure { Log.w(TAG, "读 Qoder 账号失败: ${it.javaClass.simpleName}") }
        return out
    }

    /**
     * 让下一轮补签立刻可跑（清掉冷却）。
     *
     * 新登录/新粘贴进来的账号必须马上补签，不能等 10 分钟冷却 ——
     * 用户加完账号就切走，等不到下一轮。
     */
    fun requestRunSoon(ctx: Context) {
        prefs(ctx).edit().putLong(KEY_LAST_TRY, 0L).apply()
    }

    /** 今天是否**全部账号**都处理完了。 */
    fun alreadyRanToday(ctx: Context): Boolean = pending(ctx).isEmpty()

    /**
     * 现在该不该尝试一轮：还有没处理的账号 + 距上次尝试超过冷却。
     */
    fun shouldAttempt(ctx: Context): Boolean {
        if (pending(ctx).isEmpty()) return false
        val last = prefs(ctx).getLong(KEY_LAST_TRY, 0L)
        return System.currentTimeMillis() - last >= RETRY_COOLDOWN_MS
    }

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
            .onFailure { Log.w(TAG, "开窗闹钟挂不上: ${it.javaClass.simpleName}") }
    }

    /**
     * 跑一轮补签。返回摘要（无账号或未到冷却返回 null）。
     *
     * 逐账号处理：每个账号单独查状态、单独记"今天处理完"，
     * 所以中途加进来的新账号、上轮失败的账号都会在下一轮被捡起来。
     */
    fun runAllOnce(ctx: Context): String? {
        if (!shouldAttempt(ctx)) return null
        prefs(ctx).edit().putLong(KEY_LAST_TRY, System.currentTimeMillis()).apply()
        val parts = ArrayList<String>()
        val todo = pending(ctx)
        if (todo.isEmpty()) return null

        // ---- Trae：逐账号查状态，未签才签 ----
        val traeNames = runCatching { TraeAuthStore.list(ctx).associate { it.uid to it.nickname } }
            .getOrElse { emptyMap() }
        for ((plat, uid) in todo) {
            if (plat != "trae") continue
            runCatching {
                val cred = TraeAuthStore.loadByUid(ctx, uid) ?: return@runCatching
                if (cred.accessToken.isBlank()) return@runCatching
                val st = TraeUpstream.fetchCheckinHealing(ctx, cred) ?: return@runCatching
                if (st.checkedIn || !st.enable) {
                    markDone(ctx, "trae:$uid")     // 已签 / 未开放 → 今天不用再管这个号
                    return@runCatching
                }
                val code = TraeUpstream.claimCheckin(cred.accessToken, uid, cred.region) ?: return@runCatching
                if (code in 200..299) {
                    markDone(ctx, "trae:$uid")
                    parts.add("Trae ${traeNames[uid]?.ifBlank { null } ?: uid} 已签到")
                }
            }.onFailure { Log.w(TAG, "Trae $uid 补签失败: ${it.javaClass.simpleName}") }
        }

        // ---- Qoder：有未处理的账号才跑整批（复用定时领取那一套，含结果落盘）----
        if (todo.any { it.first == "qoder" }) {
            runCatching {
                val r = QoderCheckin.runAll(ctx)
                for (p in r.results) {
                    // 领到了或确认无可领 → 这个号今天收工；失败留着下轮重试
                    if (p.status == "ok" || p.status == "already") markDone(ctx, "qoder:${p.uid}")
                }
                parts.add("Qoder " + r.results.joinToString("；") { QoderCheckin.statusLabel(it) })
            }.onFailure { Log.w(TAG, "Qoder 补签失败: ${it.javaClass.simpleName}") }
        }

        if (parts.isEmpty()) return null
        val summary = parts.joinToString(" · ")
        Log.i(TAG, "补签: $summary")
        return summary
    }

    /** 纯计算自检：不碰网络/闹钟。 */
    fun selfCheck() {
        // 日期键必须随日期变化：写死会让"每天一次"退化成"永远只跑一次"
        check(today().matches(Regex("^\\d{4}-\\d{2}-\\d{2}$"))) { "自检失败：日期格式不对" }
        check(RETRY_COOLDOWN_MS >= 60_000L) { "自检失败：重试冷却过短" }
        // 账号维度的键必须带平台前缀，否则 Trae/Qoder 撞 uid 时会互相顶掉
        check("trae:abc" != "qoder:abc") { "自检失败：账号键必须带平台前缀" }
        // 窗口必须落在 10:00 之后几分钟：算成 9:55 就等于永远抢不到当天活动
        val cal = Calendar.getInstance(TimeZone.getTimeZone("Asia/Singapore"))
            .apply { timeInMillis = nextWindowAt(0L) }
        check(cal.get(Calendar.HOUR_OF_DAY) == WINDOW_HOUR) { "自检失败：开窗小时不对" }
        check(nextWindowAt(System.currentTimeMillis()) > System.currentTimeMillis()) {
            "自检失败：下一次开窗必须在未来"
        }
    }
}
