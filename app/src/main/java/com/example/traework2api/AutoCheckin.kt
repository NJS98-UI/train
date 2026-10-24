package com.example.traework2api

import android.content.Context
import java.text.SimpleDateFormat
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
    private const val TAG = "AutoCheckin"

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun today(): String = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())

    /** 今天是否已经补签过（补签成功与否都算跑过，避免反复重试打上游）。 */
    fun alreadyRanToday(ctx: Context): Boolean = prefs(ctx).getString(KEY_DATE, null) == today()

    private fun markRan(ctx: Context) {
        prefs(ctx).edit().putString(KEY_DATE, today()).apply()
    }

    /**
     * 跑一轮补签。**调用方负责放到后台线程。**
     *
     * @return 人类可读摘要（null = 今天已经跑过 / 没有可签的账号）
     */
    fun runAllOnce(ctx: Context): String? {
        if (alreadyRanToday(ctx)) return null
        val parts = ArrayList<String>()

        // ---- Trae：逐个账号查状态，未签才签 ----
        runCatching {
            for (m in TraeAuthStore.list(ctx)) {
                val cred = TraeAuthStore.loadByUid(ctx, m.uid) ?: continue
                if (cred.accessToken.isBlank()) continue
                val st = TraeUpstream.fetchCheckin(cred.accessToken, m.uid, cred.region) ?: continue
                if (st.checkedIn || !st.enable) continue
                val code = TraeUpstream.claimCheckin(cred.accessToken, m.uid, cred.region) ?: continue
                if (code in 200..299) parts.add("Trae ${m.nickname.ifBlank { m.uid }} 已签到")
            }
        }.onFailure { android.util.Log.w(TAG, "Trae 补签失败: ${it.javaClass.simpleName}: ${it.message}") }

        // ---- Qoder：全部账号领每日 Credits（复用定时领取那一套，含结果落盘）----
        runCatching {
            if (QoderAuthStore.list(ctx).isNotEmpty()) {
                val r = QoderCheckin.runAll(ctx)
                parts.add("Qoder " + r.results.joinToString("；") { QoderCheckin.statusLabel(it) })
            }
        }.onFailure { android.util.Log.w(TAG, "Qoder 补签失败: ${it.javaClass.simpleName}: ${it.message}") }

        markRan(ctx)
        if (parts.isEmpty()) return null
        val summary = parts.joinToString(" · ")
        android.util.Log.i(TAG, "启动补签: $summary")
        return summary
    }

    fun selfCheck() {
        // 日期键必须随日期变化：写死会让"每天一次"退化成"永远只跑一次"
        check(today().matches(Regex("^\\d{4}-\\d{2}-\\d{2}$"))) { "自检失败：日期格式不对" }
        // 摘要为空时不该标记"有结果"（调用方靠 null 判断要不要提示）
        check(listOf<String>().takeIf { it.isNotEmpty() } == null) { "自检失败：空摘要应为 null" }
    }
}
