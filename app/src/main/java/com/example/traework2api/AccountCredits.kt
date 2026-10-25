package com.example.traework2api

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * **按账号**的积分快照（Trae + Qoder 两个平台各自独立）。
 *
 * ## 为什么单独做一份，而不是复用 [CreditMonitor]
 *
 * [CreditMonitor] 是**转发链路**的守卫：每 2 秒只盯**当前账号**，用完就自动换号。
 * 它答不了「账号页上每个账号分别还剩多少」——那需要遍历**全部**账号，
 * 而每 2 秒遍历全部账号去打上游是明确的滥用（N 个账号 × 每 2 秒）。
 *
 * 所以这里的分工是：
 *  - 触发时机：**打开账号页** / 手动点刷新（不是定时轮询）
 *  - 结果：内存 + prefs 双缓存，进页面**先显示上次的数**（带时间戳），再后台刷新
 *  - 并发：`AtomicBoolean` 保证同一时刻只有一轮在跑（连点刷新不会打多轮）
 *  - 失败：单个账号失败**只影响它自己**（保留上一次的值 + 记错误），不整批作废
 *
 * ## 为什么缓存要落盘
 *
 * 账号页每次 render 都会重建视图。只放内存的话，从别的页面切回来会先闪一片"查询中"；
 * 落盘后进页面立刻有数，后台刷新再原地更新，观感是"一直有数"。
 */
object AccountCredits {

    private const val TAG = "AccountCredits"

    /** 平台标识。 */
    const val TRAE = "trae"
    const val QODER = "qoder"

    /** 缓存多久之内不重复打上游（手动刷新会无视它）。 */
    private const val TTL_MS = 60_000L

    private const val PREF = "traework2api_credits"
    private const val KEY_CACHE = "cache_json"

    /** 单个账号的积分状态。 */
    data class Item(
        val platform: String,
        val uid: String,
        /** 剩余积分；null = 本次没查到（见 [error]），或从未查过。 */
        val remaining: Double?,
        /** 上游明确报"额度已用尽"（Qoder 有独立字段；Trae 只能靠 remaining<=0 推）。 */
        val exceeded: Boolean,
        /** 本次失败原因；null = 成功。失败时 [remaining] 保留上一次的值。 */
        val error: String?,
        /** 这个数是什么时候拿到的（unix 毫秒）。0 = 从未成功过。 */
        val at: Long,
    ) {
        val known: Boolean get() = remaining != null
    }

    /** 整批快照。 */
    data class Snapshot(
        val items: Map<String, Item>,
        /** 这一批的完成时间（0 = 从未跑过）。 */
        val at: Long,
        val loading: Boolean,
    ) {
        fun trae(uid: String): Item? = items[key(TRAE, uid)]
        fun qoder(uid: String): Item? = items[key(QODER, uid)]

        /** 某平台里"查到了数"的账号条目。 */
        fun known(platform: String): List<Item> =
            items.values.filter { it.platform == platform && it.known }

        /** 某平台里"这次失败"的账号数。 */
        fun failed(platform: String): Int =
            items.values.count { it.platform == platform && it.error != null }

        /** 某平台已知账号总数（含失败）。 */
        fun total(platform: String): Int = items.values.count { it.platform == platform }

        /**
         * 某平台的**合计积分**。
         *
         * 只对"查到了数"的账号求和。若该平台一个都没查到 → null（UI 显示「—」，
         * 不要显示 0 —— 0 会被误读成"真的没额度了"）。
         * 有账号失败时合计是**下界**，UI 必须标出 `known/total`。
         */
        fun sum(platform: String): Double? {
            val known = known(platform)
            return if (known.isEmpty()) null else known.sumOf { it.remaining ?: 0.0 }
        }

        /** 合计是否完整（全部账号都查到了）。 */
        fun complete(platform: String): Boolean {
            val t = total(platform)
            return t > 0 && known(platform).size == t
        }
    }

    private fun key(platform: String, uid: String) = "$platform|$uid"

    @Volatile
    private var snapshot: Snapshot = Snapshot(emptyMap(), 0L, false)

    private val inFlight = AtomicBoolean(false)
    private val main = Handler(Looper.getMainLooper())

    /** 当前快照（永不返回 null）。 */
    fun current(): Snapshot = snapshot

    /** 缓存是否够新（够新就别再打上游了）。 */
    private fun fresh(): Boolean {
        val s = snapshot
        return s.at > 0 && System.currentTimeMillis() - s.at < TTL_MS
    }

    // ------------------------------------------------------------------
    // 缓存读写
    // ------------------------------------------------------------------

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREF, Context.MODE_PRIVATE)

    /** 启动时装载磁盘缓存（进页面立刻有数）。 */
    fun loadCache(ctx: Context) {
        val raw = prefs(ctx).getString(KEY_CACHE, null) ?: return
        val arr = try { org.json.JSONArray(raw) } catch (_: Exception) { return }
        val map = LinkedHashMap<String, Item>()
        for (i in 0 until arr.length()) {
            val o = arr.optJSONObject(i) ?: continue
            val platform = o.optString("platform", "")
            val uid = o.optString("uid", "")
            if (platform.isEmpty() || uid.isEmpty()) continue
            val remaining = if (o.isNull("remaining")) null else o.optDouble("remaining", 0.0)
            map[key(platform, uid)] = Item(
                platform = platform,
                uid = uid,
                remaining = remaining,
                exceeded = o.optBoolean("exceeded", false),
                error = o.optString("error", "").takeIf { it.isNotEmpty() },
                at = o.optLong("at", 0L),
            )
        }
        if (map.isEmpty()) return
        snapshot = Snapshot(map, prefs(ctx).getLong("at", 0L), false)
    }

    private fun saveCache(ctx: Context, s: Snapshot) {
        val arr = org.json.JSONArray()
        for (it in s.items.values) {
            arr.put(
                JSONObject()
                    .put("platform", it.platform)
                    .put("uid", it.uid)
                    .put("remaining", it.remaining ?: JSONObject.NULL)
                    .put("exceeded", it.exceeded)
                    .put("error", it.error ?: "")
                    .put("at", it.at),
            )
        }
        prefs(ctx).edit().putString(KEY_CACHE, arr.toString()).putLong("at", s.at).apply()
    }

    // ------------------------------------------------------------------
    // 刷新
    // ------------------------------------------------------------------

    /**
     * 刷新全部账号的积分。
     *
     * @param force 无视 TTL 强制打上游（用户手动点刷新时用）
     * @param onDone 完成回调（主线程）；中途失败也会回调，附最终快照
     */
    fun refreshAll(ctx: Context, force: Boolean = false, onDone: ((Snapshot) -> Unit)? = null) {
        if (!force && fresh()) {
            onDone?.let { cb -> main.post { cb(snapshot) } }
            return
        }
        if (!inFlight.compareAndSet(false, true)) return   // 已有一轮在跑，连点不重复打

        snapshot = snapshot.copy(loading = true)
        val app = ctx.applicationContext ?: ctx
        thread(name = "account-credits") {
            var next = snapshot
            try {
                next = fetchAll(app)
                saveCache(app, next)
                Log.i(TAG, "积分刷新完成：Trae ${next.known(TRAE).size}/${next.total(TRAE)}，" +
                    "Qoder ${next.known(QODER).size}/${next.total(QODER)}")
            } catch (e: Exception) {
                // 整批炸了也要把 loading 复位，否则 UI 永远转圈
                Log.w(TAG, "积分刷新异常: ${e.javaClass.simpleName}: ${e.message}")
                next = snapshot.copy(loading = false)
            } finally {
                snapshot = next.copy(loading = false)
                inFlight.set(false)
                val s = snapshot
                if (onDone != null) main.post { onDone(s) }
            }
        }
    }

    /**
     * 单账号查积分偶发失败（上游限流/瞬时 5xx）就再试两次。
     *
     * 现场表现是"同一平台里只有一个账号查不到、别的都正常"——多打两次基本就回来了，
     * 比直接在界面上标"查询失败"有用。
     */
    private fun <T> retryFetch(block: () -> T?): T? {
        var last: T? = null
        for (i in 1..3) {
            last = runCatching(block).getOrNull()
            if (last != null) return last
            if (i < 3) {
                try { Thread.sleep(400L * i) } catch (_: InterruptedException) { return null }
            }
        }
        return last
    }

    private fun fetchAll(ctx: Context): Snapshot {
        val map = LinkedHashMap<String, Item>()
        val prev = snapshot.items
        val now = System.currentTimeMillis()

        // 保留上一次的值，本次失败时继续显示（只把 error 标出来）
        fun keep(platform: String, uid: String): Item? = prev[key(platform, uid)]

        // ---- Trae ----
        val traeMetas = try { TraeAuthStore.list(ctx) } catch (_: Exception) { emptyList() }
        for (m in traeMetas) {
            val old = keep(TRAE, m.uid)
            val item = try {
                if (!m.region.isSupported) {
                    // 国际服签到/积分协议未实测：不硬打上游，明确标注而不是显示 0
                    Item(TRAE, m.uid, old?.remaining, false, "该区域积分暂不支持查询", old?.at ?: 0L)
                } else {
                    val cred = TraeAuthStore.loadByUid(ctx, m.uid)
                    if (cred == null) {
                        Item(TRAE, m.uid, old?.remaining, false, "本地凭证缺失", old?.at ?: 0L)
                    } else {
                        // 续期会轮换 token，必须与转发互斥（同 switchTraeToFunded 的纪律）
                        val tok = Gateway.locked {
                            TraeAuthApi.restartWithValidTokenFor(ctx, cred, skewSec = 3600L)
                        } ?: cred.accessToken.takeIf { it.isNotBlank() }
                        if (tok.isNullOrBlank()) {
                            Item(TRAE, m.uid, old?.remaining, false, "token 不可用", old?.at ?: 0L)
                        } else {
                            val bal = retryFetch { TraeUpstream.fetchUsage(tok, cred.uid, m.region) }
                            if (bal == null) {
                                Item(TRAE, m.uid, old?.remaining, false, "查询失败", old?.at ?: 0L)
                            } else {
                                Item(TRAE, m.uid, bal.toDouble(), bal <= 0L, null, now)
                            }
                        }
                    }
                }
            } catch (e: Exception) {
                Item(TRAE, m.uid, old?.remaining, false, e.javaClass.simpleName, old?.at ?: 0L)
            }
            map[key(TRAE, m.uid)] = item
        }

        // ---- Qoder ----
        val qMetas = try { QoderAuthStore.list(ctx) } catch (_: Exception) { emptyList() }
        for (m in qMetas) {
            val old = keep(QODER, m.uid)
            val item = try {
                val cred = QoderAuthStore.loadByUid(ctx, m.uid)
                if (cred == null) {
                    Item(QODER, m.uid, old?.remaining, false, "本地凭证缺失", old?.at ?: 0L)
                } else {
                    // 用 validJobTokenFor（作用于指定账号），**不要**为了查积分去 setActive ——
                    // 那会让"只是看一眼"变成"偷偷换号"。
                    val jt = QoderAuthApi.validJobTokenFor(ctx, cred, 60L * 60L * 1000L)
                        ?: cred.jobToken.takeIf { it.isNotBlank() }
                    if (jt.isNullOrBlank()) {
                        Item(QODER, m.uid, old?.remaining, false, "token 不可用", old?.at ?: 0L)
                    } else {
                        val c = retryFetch { QoderAuthApi.fetchCredits(jt, cred.mode) }
                        if (c == null) {
                            Item(QODER, m.uid, old?.remaining, false, "查询失败", old?.at ?: 0L)
                        } else {
                            Item(QODER, m.uid, c.remaining, c.exceeded, null, now)
                        }
                    }
                }
            } catch (e: Exception) {
                Item(QODER, m.uid, old?.remaining, false, e.javaClass.simpleName, old?.at ?: 0L)
            }
            map[key(QODER, m.uid)] = item
        }

        return Snapshot(map, now, false)
    }

    /** 清空缓存（账号全删时用，避免残留别人的积分）。 */
    fun clear(ctx: Context) {
        snapshot = Snapshot(emptyMap(), 0L, false)
        prefs(ctx).edit().remove(KEY_CACHE).remove("at").apply()
    }

    // ------------------------------------------------------------------
    // 展示
    // ------------------------------------------------------------------

    /** 积分文本：`1234` / `12.5` / `—`（查不到）。 */
    fun format(v: Double?): String {
        if (v == null) return "—"
        return if (v == v.toLong().toDouble()) v.toLong().toString() else String.format("%.1f", v)
    }

    /** 单个账号的展示文本（含失败标注）。 */
    fun label(item: Item?): String {
        if (item == null) return "—"
        val v = format(item.remaining)
        return when {
            item.error != null && item.known -> "$v（${item.error}）"
            item.error != null -> "—（${item.error}）"
            else -> v
        }
    }

    /**
     * 平台合计的展示文本。
     *
     * 只报数字（`1234`）—— 不再附「（2/3 个账号）」括号说明。
     * 而不是"总共就这么多"。
     */
    /** 平台合计：只报数字，不带「(2/3 个账号)」这种括号说明。 */
    fun totalLabel(s: Snapshot, platform: String): String = format(s.sum(platform))

    // ------------------------------------------------------------------
    // 自检
    // ------------------------------------------------------------------

    fun selfCheck() {
        val s = Snapshot(
            mapOf(
                key(TRAE, "a") to Item(TRAE, "a", 100.0, false, null, 1L),
                key(TRAE, "b") to Item(TRAE, "b", 50.0, false, null, 1L),
                key(TRAE, "c") to Item(TRAE, "c", null, false, "查询失败", 1L),
                key(QODER, "q") to Item(QODER, "q", 0.0, true, null, 1L),
            ),
            at = 1L,
            loading = false,
        )
        check(s.total(TRAE) == 3) { "自检失败：Trae 账号总数应为 3" }
        check(s.known(TRAE).size == 2) { "自检失败：Trae 查到数的应为 2" }
        check(s.failed(TRAE) == 1) { "自检失败：Trae 失败数应为 1" }
        check(s.sum(TRAE) == 150.0) { "自检失败：Trae 合计应为 150（只加查到的）" }
        check(!s.complete(TRAE)) { "自检失败：有失败账号时合计不完整" }

        check(s.sum(QODER) == 0.0) { "自检失败：Qoder 合计应为 0（查到就是 0）" }
        check(s.complete(QODER)) { "自检失败：Qoder 单账号全部查到应算完整" }

        // 一个都没查到 → null（不是 0）：0 会被误读成"真的没额度"
        check(s.sum("trae-none") == null) { "自检失败：没查到任何数时合计必须是 null 而不是 0" }
        check(format(null) == "—") { "自检失败：null 应显示 —" }
        check(format(0.0) == "0") { "自检失败：0 应显示 0" }
        check(format(1234.0) == "1234") { "自检失败：整数不应带小数" }
        check(format(12.5) == "12.5") { "自检失败：小数应保留一位" }

        // 合计只报数字，不带「(2/3 个账号)」这种括号
        check(totalLabel(s, TRAE) == "150") { "自检失败：合计应只报数字" }
        check(totalLabel(s, QODER) == "0") { "自检失败：合计 0 应显示 0" }
        check(totalLabel(s, "nope") == "—") { "自检失败：无账号平台应显示 —" }

        // 单账号标签
        check(label(null) == "—") { "自检失败：无条目应显示 —" }
        check(label(Item(TRAE, "x", 88.0, false, null, 1L)) == "88") { "自检失败：正常值展示不对" }
        check(label(Item(TRAE, "x", null, false, "查询失败", 1L)) == "—（查询失败）") {
            "自检失败：从无数据时失败应显示 —（原因）"
        }
        check(label(Item(TRAE, "x", 9.0, false, "查询失败", 1L)) == "9（查询失败）") {
            "自检失败：有旧数据时失败应保留旧值并标原因"
        }
    }
}
