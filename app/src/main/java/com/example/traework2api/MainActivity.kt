package com.example.traework2api

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.NumberPicker
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.concurrent.thread

/**
 * 阶段3：本地"假服务" + Trae 官方登录入口。
 *
 * 单一 Activity + 底部导航结构（移植 WorkBuddy2API-Android 的纯代码 View 组装方式）：
 * 概览 / 账号 / 模型 / 记录 / 更多 五个持久 Tab（`Page.entries` 驱动），
 * 顶部栏 + 卡片风格 + 选中态胶囊指示器。
 *
 * 视觉规则：圆角/间距/字号一律走下面的 design token，不要在页面里再写字面量；
 * 颜色一律走 `tw_*` 资源（`values` / `values-night` 各一套），不要写字面量色值。
 */
class MainActivity : Activity() {
    companion object {
        /** 通知权限请求码（Android 13+ 前台服务的常驻通知要用）。 */
        private const val REQ_NOTIF = 1002
        /** SAF：导出账号 JSON（创建文档）。 */
        private const val REQ_EXPORT = 1003
        /** SAF：导入账号 JSON（打开文档）。 */
        private const val REQ_IMPORT = 1004
        private const val SIXTY_MINUTES_SEC = 3600L
        /** 模型页刷新按钮文案：空状态卡与工具条共用同一份，改一处两边同步。 */
        private const val REFRESH_MODELS_LABEL = "刷新模型目录"

        /** 二级页「保活设置」的 onSaveInstanceState 标记值。 */
        private const val SUB_KEEP_ALIVE = "keepalive"
        /** 二级页「外观风格」的 onSaveInstanceState 标记值。 */
        private const val SUB_STYLE = "style"
        private const val STATE_PAGE = "page"
        private const val STATE_SUB = "sub"

        // ---- design tokens ----
        // 圆角/间距/内容宽度**不在这里**：它们跟着「外观风格」走，见下面按风格读取的属性。
        // 这里只留不随风格变的字号 token。
        private const val FS_TITLE = 22f     // 顶部栏标题
        private const val FS_SECTION = 14    // 区块标题（小号灰色，与卡片标题拉开层级）
        private const val FS_CARD_TITLE = 17 // 卡片标题
        private const val FS_METRIC = 24     // 主指标数值
        private const val FS_BODY = 15
        private const val FS_VALUE = 14
        private const val FS_CAPTION = 13
        private const val FS_CHIP = 12

        /** 本地 API Key 打码占位（概览页默认显示它，避免截屏直接泄露）。 */
        private const val MASKED_KEY = "sk-••••••••••••••••••••••••"

        // ---- 版权信息 ----
        // 概览页与更多页各出现一次，**共用这一份常量**：改一处两边同步，
        // 不会出现"概览页写了新邮箱、更多页还是旧的"。文案按「标签 + 值」成对给，
        // 排版在页面内直接拼，不再走统一的版权行函数。
        private const val AUTHOR_NAME = "玖念"
        private const val AUTHOR_EMAIL = "2820184772@qq.com"
        private const val AUTHOR_LABEL = "作者"
        private const val CONTACT_LABEL = "联系邮箱"

        /** 「使用引导」列出的端点（顺序即展示顺序）。复制时拼成完整 URL。 */
        private val ENDPOINTS = listOf(
            "POST" to "/v1/chat/completions",
            "GET" to "/v1/models",
            "GET" to "/healthz",
        )
    }

    // ------------------------------------------------------------------
    // design token：圆角 / 间距 / 内容宽度 —— **跟随当前风格**（多风格本轮新增）
    // 值只写在 AppStyle.Style 里；页面与 helper 只读这些属性，别在页面里写字面量。
    // 之所以做成读属性的函数而不是 const：切风格时"形态"也要跟着变，const 改不了。
    // ------------------------------------------------------------------
    /** 容器类（卡片/搜索框）圆角。 */
    private val RADIUS_CARD get() = AppStyle.current.cardRadius
    /** 控件类（按钮）圆角。 */
    private val RADIUS_CONTROL get() = AppStyle.current.controlRadius
    /** 胶囊（chip / 导航指示器）圆角：给一个大值即为全圆角。 */
    private val RADIUS_PILL get() = AppStyle.current.pillRadius

    /** 卡片之间的标准间距；页面里不再手传 8/10/12 混用。 */
    private val GAP get() = AppStyle.current.gap
    /** 页面左右内边距。 */
    private val PAGE_PAD_H get() = AppStyle.current.pagePadH
    /** 页面上下内边距。 */
    private val PAGE_PAD_TOP get() = AppStyle.current.pagePadTop
    private val PAGE_PAD_BOTTOM get() = AppStyle.current.pagePadBottom
    /** 卡片内边距。 */
    private val CARD_PAD get() = AppStyle.current.cardPad
    /** 顶栏上下内边距。 */
    private val BAR_PAD_TOP get() = AppStyle.current.barPadTop
    private val BAR_PAD_BOTTOM get() = AppStyle.current.barPadBottom
    /** 内容最大宽度（dp）：手机竖屏不受影响，平板/横屏下不再让一行文字横跨整屏。 */
    private val CONTENT_MAX_DP get() = AppStyle.current.contentMaxDp

    private enum class Page(val title: String, val subtitle: String, val icon: Int) {
        OVERVIEW("概览", "本地服务运行状态", R.drawable.ic_dashboard),
        ACCOUNT("账号", "账户登录状态", R.drawable.ic_account),
        MODELS("模型", "本地可用的模型表", R.drawable.ic_models),
        RECORDS("记录", "调用用量与记录", R.drawable.ic_history),
    }

    private lateinit var contentHost: FrameLayout
    private lateinit var pageTitle: TextView
    private lateinit var pageSubtitle: TextView
    /** 顶栏里的「← 返回」，只在二级页可见（见 [render] / [openKeepAlive] / [openStylePage]）。 */
    private lateinit var backAction: TextView
    private lateinit var navItems: Map<Page, TextView>
    private var currentPage = Page.OVERVIEW
    /**
     * 是否正停在二级页（保活设置）。
     *
     * 做成二级页而不是第 6 个 Tab：底部导航已有 5 项，再加会挤；且保活是"设置一次就不管"的
     * 低频页面，不值得占一个常驻 Tab。
     */
    private var showKeepAlive = false
    /** 是否正停在外观风格二级页（与保活设置共用同一套二级页机制）。 */
    private var showStyle = false
    private val apiKey: String by lazy { Gateway.apiKey(this) }
    /** 账号页积分/额度缓存（null=未拉到），展示用，不存凭据。 */
    private var accountBalance: Long? = null   // 当前可用积分（= total_amount − consumed_amount）
    private var accountCheckin: TraeUpstream.CheckinStatus? = null   // 签到状态（含 enable）
    /** 签到请求进行中（防连点：实测重复 claim 会白发请求）。 */
    private var checkinBusy = false
    /** 本地 API Key 明文默认打码（防止截图/旁观泄露），点「显示」才展开。 */
    private var apiKeyVisible = false
    /** 服务启停进行中（"启动中…" / "停止中…"），null = 空闲。启停是异步的，中间得给个状态。 */
    private var serviceBusy: String? = null
    /** 积分刷新进行中：数值位显示「…」，否则用户点了「刷新」会觉得没反应。 */
    private var quotaLoading = false

    /** 账号选择弹窗里每行固定高度（dp）。 */
    private val PICKER_ROW_H = 56

    /** 账号选择弹窗最多露出几行，超出滚动。 */
    private val PICKER_VISIBLE_ROWS = 5

    /**
     * 「这个账号被服务端拒了」按 **uid** 记，不按页面记。
     *
     * 之前是一个全局 boolean：切到坏账号 → 置 true → 再切回好账号，**没人清它**，
     * 于是好账号也显示「需重新登录」。状态必须跟着账号走，不能跟着页面走。
     */
    private var authFailedUid: String? = null

    /** 该账号是否已被服务端拒绝（本页实测 + 积分条目两个来源，都按 uid 查）。 */
    private fun authFailedFor(uid: String?): Boolean {
        if (uid.isNullOrBlank()) return false
        if (authFailedUid == uid) return true
        // 积分条目本来就是按 uid 存的，弹窗里显示的也是它 —— 两处必须同源，否则又对不上
        return AccountCredits.current().trae(uid)?.error == "需重新登录"
    }
    /** 待写入 SAF 的备份 JSON（导出账号点「保存」后暂存，onActivityResult 拿到 Uri 再落盘）。 */
    private var pendingExportJson: String? = null
    /** 当前 Qoder 账号积分余量（监测器回填，仅展示）。null=未拉到/无账号。 */
    private var qoderRemaining: Double? = null
    /** Qoder 每日 Credits 领取进行中（手动，挡住重复点击）。 */
    private var qoderClaimBusy = false

    /** 「积分与用量」/「Qoder 账号」卡里会被监测器原地刷新的控件。 */
    private var monitorApply: (() -> Unit)? = null

    // ---- 账号页「按账号积分」（AccountCredits）----
    /** 每个账号卡里的"积分"文字 + 它的取值函数；重建页面时清空。 */
    private val creditViews = ArrayList<Pair<TextView, () -> String>>()
    private var creditTotalTrae: TextView? = null
    private var creditTotalQoder: TextView? = null
    private var creditUpdated: TextView? = null
    /** Qoder 当前账号的积分大字（监测器原地刷新它）。 */
    private var qoderCreditView: TextView? = null
    /**
     * 「积分与用量」卡里那几个会被刷新改掉的控件 —— 刷新时**原地改文字**，不重建页面。
     *
     * 为什么不用 `render()`：重建会把滚动位置弹回页首，用户就看不到自己刚点的那张卡在变
     * （实测：加载态的「…」因此根本看不见）。原地更新也顺带没有闪烁。
     */
    private var quotaApply: (() -> Unit)? = null

    /**
     * 「刷新模型」完成后**原地更新模型 Tab**（重建卡片 + 更新计数），不整页 render。
     *
     * 与 [quotaApply] 同一个理由：`render()`/`redraw()` 会 `removeAllViews()`，
     * 把搜索框内容与滚动位置一起丢掉（这个坑项目里踩过，见 MEMORY「积分刷新」）。
     * 每次 [modelsPage] 构建时重新登记，离开本页后作废。
     */
    private var modelsRefreshApply: (() -> Unit)? = null
    /** 本次进程是否已自动同步过 Qoder 官方模型目录（避免每次进模型页都打上游）。 */
    private var qoderCatalogAutoTried = false

    /** 模型表刷新进行中：按钮置灰 + 文案变「刷新中…」，否则用户点了没反馈会连点。 */
    private var modelsLoading = false

    /** 版本号取自 PackageManager，避免依赖 AGP 的 BuildConfig 开关。 */
    private val versionName: String by lazy {
        runCatching { packageManager.getPackageInfo(packageName, 0).versionName }.getOrNull() ?: "-"
    }

    /**
     * 外观偏好比任何控件都早一步生效：先把「亮/暗」落到 uiMode，
     * 这样 `values-night` 资源、平台对话框、光标颜色都跟着用户的选择走。
     */
    /**
     * 启动自检**不许崩 App**。
     *
     * 教训（0.7.9 与 0.7.15 各一次）：改了展示文案/纯函数，却忘了同步 `selfCheck()` 里的断言，
     * 结果是"App 打不开、用户只看到闪退"，现场还拿不到任何日志。
     * 自检的价值是**早发现**，不是**让用户打不开**：失败就记日志 + 界面上一句人话，功能照常跑。
     */
    private val selfCheckFailures = ArrayList<String>()

    private fun selfCheck(name: String, block: () -> Unit) {
        runCatching(block).onFailure {
            android.util.Log.e("SelfCheck", "$name 自检失败: ${it.message}", it)
            selfCheckFailures.add("$name：${it.message}")
        }
    }

    override fun attachBaseContext(newBase: Context) {
        AppStyle.init(newBase)
        super.attachBaseContext(AppStyle.applyNightOverride(newBase))
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // 外观偏好必须在**建控件之前**加载并刷新亮暗，否则第一帧会用旧风格画一遍。
        // （init 在 attachBaseContext 里已经调过一次，这里是幂等的二道保险。）
        AppStyle.init(this)
        AppStyle.refreshNight(this)
        // 风格表自检：色表缺键 / 色值透明 / 对比度 < 4.5:1 在启动时就暴露出来
        // （记日志 + 一句提示，不崩 —— 见 selfCheck() 的注释）。
        selfCheck("AppStyle") { AppStyle.selfCheck() }
        removeLegacyPlaintextToken()
        // 请求体按字节读的自检：中文 body 曾因按字符读而挂死，这里早失败早暴露。
        selfCheck("LocalFakeServer") { LocalFakeServer.selfCheck() }
        // 区域常量自检：CN 是已实测链路，不能被后续改动弄坏。
        selfCheck("TraeRegion") { TraeRegion.selfCheck() }
        // 多账号索引自检：uid 是主键，序列化错了会串账号。
        selfCheck("TraeAuthStore") { TraeAuthStore.selfCheck() }
        // Qoder 账号索引 + OpenAPI 端点 + 积分判据自检（uid 串了会换错号）。
        selfCheck("QoderAuthStore") { QoderAuthStore.selfCheck() }
        selfCheck("QoderAuthApi") { QoderAuthApi.selfCheck() }
        // Qoder 定时领取：结果往返/去重聚合自检。
        selfCheck("QoderCheckin") { QoderCheckin.selfCheck() }
        // 备份文件（Trae+Qoder 合并）格式识别自检：认错了会把 Trae 备份喂给 Qoder 解析。
        selfCheck("AccountBackup") { AccountBackup.selfCheck() }
        // 积分监测换号阈值边界自检。
        selfCheck("CreditMonitor") { CreditMonitor.selfCheck() }
        // 续期判定边界自检：服务现在每个请求都靠它决定要不要续期，判错就是持续 401。
        selfCheck("TraeAuthApi") { TraeAuthApi.selfCheck() }
        // 国际服 plan_item 累积快照 → 增量的 diff 自检（错则客户端看到内容重复）
        selfCheck("TraeIntlUpstream") { TraeIntlUpstream.selfCheck() }
        // 上游模型表解析自检：空 config_name 必须跳过、非聊天项必须过滤、name/context 回落规则
        // （解析错会让"刷新模型"把好好的清单覆盖成垃圾）。
        selfCheck("TraeUpstream") { TraeUpstream.selfCheck() }
        // 模型表自检：空表绝不允许生效 —— 那会让所有客户端看不到任何模型（本任务最贵的失败面）。
        selfCheck("ModelsRepo") { ModelsRepo.selfCheck() }
        // COSY 签名原语自检：编码/换行/填充/UUID 掩码/RSA 长度错一个都只会得到 401，
        // 且上游不告诉你哪步错 —— 必须在本地就钉死。
        selfCheck("QoderCosyCrypto") { QoderCosyCrypto.selfCheck() }
        // Qoder 模型目录 + 解析器自检：模型名解析错会把请求路由到错的 key。
        selfCheck("QoderModelCatalog") { QoderModelCatalog.selfCheck() }
        // Qoder 上游请求体自检：system 抽取、tool_calls 的 content、cache 标记、参数透传。
        selfCheck("QoderUpstream") { QoderUpstream.selfCheck() }
        // 装入磁盘模型缓存（必须在 selfCheck 之后、任何 /v1/models 请求之前）：
        // 客户端在 App 刚重启、还没手动刷模型时也能拉到上次刷到的清单。
        ModelsRepo.init(this)
        // 装入上次同步到的 Qoder 官方模型目录（含真实倍率）。
        // 必须在任何 /v1/models 请求之前：否则客户端会先拿到内置 16 个，倍率也不对。
        QoderModelCatalog.load(this)
        // 恢复上次选择的区域：不恢复的话重启后会回落 CN，
        // 国际服 token 就会被发到国内服 host（实测 401/1001 或 4001）。
        TraeRegion.load(this)
        // 但**当前账号自带的区域才是权威**：它可能和上次手动选的区域不一致
        // （例：手动选了国服、当前账号却是国际服）→ 必须以账号为准。
        TraeAuthStore.syncRegionToActiveAccount(this)
        // 本地 API Key 稳定性自检：key 一旦"每次取值都变"，所有已配置的客户端全 401。
        selfCheck("Gateway") { Gateway.selfCheck(this) }
        // 后端选择自检：auto 在"只登 Qoder"时必须解析成 Qoder，否则聊天会拿空 Trae 凭证。
        // 必须放在区域恢复之后：TraeAuthStore.load 的结果参与 auto 判定。
        selfCheck("GatewayBackend") { GatewayBackend.selfCheck(this) }
        // 按账号积分的展示规则自检（合计只加查到的、失败要暴露成下界、null ≠ 0）。
        selfCheck("AccountCredits") { AccountCredits.selfCheck() }
        // CLI 模型缓存解密自检（HKDF-SHA256 对齐 RFC 5869 + AES-GCM 往返 + 错 uid 必须解不开）。
        selfCheck("QoderModelCache") { QoderModelCache.selfCheck() }
        // 装入按账号积分缓存：进账号页立刻有数，后台刷新再原地更新。
        AccountCredits.loadCache(this)
        // 启动补签的日期闸门自检（写错就变成"每次前台都签"，会一直打上游）。
        selfCheck("AutoCheckin") { AutoCheckin.selfCheck() }
        selfCheck("QoderMachine") { QoderMachine.selfCheck() }
        // 保活页的 ROM 识别自检：SystemProperties 是隐藏 API（靠反射），读不到也不能崩。
        selfCheck("KeepAlive") { KeepAlive.selfCheck() }

        if (selfCheckFailures.isNotEmpty()) {
            toast("自检警告（不影响使用）：${selfCheckFailures.first()}")
        }

        // 网关由 Gateway（业务）+ GatewayService（保活）持有，**Activity 不再拥有它**：
        // 这样 Activity 被销毁时端口不会跟着断，用户才能"网关开着、切到别的客户端用"。
        // 0.4.8 起**进入 App 默认启动**（用户要求）：除非用户主动停止过（shouldRun=false），
        // 否则一进 App 就把前台服务拉起来 —— 省掉每次手动点「启动服务」。
        autoStartServiceIfWanted()

        val root = FrameLayout(this).apply {
            setBackgroundColor(color("background"))
            fitsSystemWindows = true
        }
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(col, FrameLayout.LayoutParams(-1, -1))

        // 顶部栏（标题 + 副标题 + 1dp 分隔线）
        // 背景用 `background` 而不是 `surface`：让顶栏"退"到页面底色里、卡片"凸"出来，
        // 层次只由卡片承载（旧版顶栏和卡片同色，整屏看着一样平）。
        // 外层全宽撑背景，内层限宽居中，这样平板/横屏下标题与内容左边缘是对齐的
        col.addView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(color("background"))
            addView(LinearLayout(this@MainActivity).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(dp(PAGE_PAD_H), dp(BAR_PAD_TOP), dp(PAGE_PAD_H), dp(BAR_PAD_BOTTOM))
                // 二级页才出现的返回入口。不靠系统返回键：纯小白不一定知道那个键，
                // 顶栏给一个看得见的出口。放标题**上方**（不是左侧），
                // 这样标题左边缘始终与内容区对齐。
                addView(linkAction("← 返回") { render(Page.OVERVIEW) }
                    .also { backAction = it }
                    .apply { visibility = View.GONE })
                addView(TextView(this@MainActivity).apply {
                    textSize = FS_TITLE
                    setTextColor(color("on_surface"))
                    typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
                }.also { pageTitle = it }, top(6))
                addView(TextView(this@MainActivity).apply {
                    textSize = FS_CAPTION.toFloat()
                    setTextColor(color("on_surface_variant"))
                }.also { pageSubtitle = it }, top(4))
            }, LinearLayout.LayoutParams(contentWidth(), -2).apply { gravity = Gravity.CENTER_HORIZONTAL })
        })
        col.addView(divider())

        // 内容区
        contentHost = FrameLayout(this)
        col.addView(contentHost, LinearLayout.LayoutParams(-1, 0, 1f))

        // 底部导航（5 Tab，由 Page.entries 驱动；选中态 = 居中胶囊指示器 + 主色）
        val navBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            setBackgroundColor(color("background"))
        }
        navItems = Page.entries.associate { p ->
            val tv = TextView(this).apply {
                text = p.title
                textSize = FS_CHIP.toFloat()
                gravity = Gravity.CENTER
                setCompoundDrawablesRelativeWithIntrinsicBounds(0, p.icon, 0, 0)
                compoundDrawablePadding = dp(3)
                setPadding(dp(18), dp(6), dp(18), dp(6))
            }
            // 外层撑满 1/5 宽（保证点击区足够大），内层 TextView 包住图标+文字，
            // 选中胶囊因此永远贴合内容：手机竖屏和 3440px 平板横屏都好看。
            navBar.addView(LinearLayout(this).apply {
                gravity = Gravity.CENTER
                setOnClickListener { render(p) }
                addView(tv, LinearLayout.LayoutParams(-2, -2))
            }, LinearLayout.LayoutParams(0, dp(64), 1f))
            p to tv
        }
        col.addView(divider())
        col.addView(navBar)

        setContentView(root)
        // 重建时回到原处：切风格/切亮暗（recreate）与系统夜间模式切换都会走到这里，
        // 不恢复的话用户切一下风格就被弹回「概览」，体验很跳。
        val restored = savedInstanceState?.getString(STATE_PAGE)
            ?.let { name -> Page.entries.firstOrNull { it.name == name } } ?: Page.OVERVIEW
        render(restored)
        when (savedInstanceState?.getString(STATE_SUB)) {
            SUB_KEEP_ALIVE -> openKeepAlive()
            SUB_STYLE -> openStylePage()
        }
        loadAndRefreshToken()
        // 进 App 静默补新模型表：客户端拉 /v1/models 的数据源重启后来自磁盘缓存，
        // 这里补一次刷新保证新鲜（不弹 toast，失败无感）。
        maybeAutoRefreshModels()
    }

    // ------------------------------------------------------------------
    // Tab 路由
    // ------------------------------------------------------------------

    private fun render(page: Page) {
        currentPage = page
        // 点任意底部 Tab 都算"退出二级页"，否则会卡在保活页/外观风格页上出不去。
        showKeepAlive = false
        showStyle = false
        backAction.visibility = View.GONE
        // 页面要重建了，旧的"原地刷新"句柄作废（accountPage / modelsPage 构建时会重新登记）
        quotaApply = null
        modelsRefreshApply = null
        monitorApply = null
        pageTitle.text = page.title
        pageSubtitle.text = page.subtitle
        contentHost.removeAllViews()
        contentHost.addView(when (page) {
            Page.OVERVIEW -> overviewPage()
            Page.ACCOUNT -> accountPage()
            Page.MODELS -> modelsPage()
            Page.RECORDS -> recordsPage()
        })
        // 账号页**自动拉一次**积分/额度：不拉的话三个数值位永远显示「—」，
        // 用户会以为功能坏了（实测就是这个现象），得先手动点「刷新」才有数据。
        // 判据只看 accountCheckin：它是一份拉取里**最后一个**回填的字段，
        // 拿它当"这个账号是否已经拉过"的单一标志（旧写法要求三个都为 null，
        // 只要余额先回填成功就会整体跳过 → 签到状态可能一直是 null，按钮变"未登录"却可点）。
        if (page == Page.ACCOUNT && accountCheckin == null && !quotaLoading) {
            contentHost.post { refreshQuota() }
        }
        navItems.forEach { (key, item) ->
            val active = key == page
            item.setTextColor(color(if (active) "primary" else "on_surface_variant"))
            item.typeface = if (active) Typeface.create("sans-serif-medium", Typeface.NORMAL) else Typeface.DEFAULT
            // 选中态：贴在图标+文字上的胶囊（宽度由内容决定，见 onCreate 的 navBar 组装）
            item.background = if (active) shape("primary_container", RADIUS_PILL) else null
            item.compoundDrawables.filterNotNull().forEach { it.mutate().setTint(color(if (active) "primary" else "on_surface_variant")) }
        }
    }

    /** 打开保活设置二级页（顶栏换成"返回"，Tab 高亮保持在「更多」）。 */
    private fun openKeepAlive() {
        showKeepAlive = true
        pageTitle.text = "保活设置"
        pageSubtitle.text = "让服务在后台长期活着"
        backAction.visibility = View.VISIBLE
        contentHost.removeAllViews()
        contentHost.addView(keepAlivePage())
    }

    /** 打开「外观风格」二级页（顶栏换标题，Tab 高亮保持在「更多」）。 */
    private fun openStylePage() {
        showStyle = true
        pageTitle.text = "外观风格"
        pageSubtitle.text = "配色与形态，随你挑"
        backAction.visibility = View.VISIBLE
        contentHost.removeAllViews()
        contentHost.addView(stylePage())
    }

    /**
     * 按"现在所在的位置"重绘：Tab 页或二级页都还原。
     *
     * ⚠️ 不能拿 `render(currentPage)` 当"刷新界面"用：那会把正停在二级页（保活设置 /
     * 外观风格）的用户踢回 Tab 页。实测：切完风格重建后状态恢复是对的，但 token 加载
     * 线程随后又 `render(currentPage)` 一次，人就被送回「更多」了。
     */
    private fun redraw() {
        when {
            showKeepAlive -> openKeepAlive()
            showStyle -> openStylePage()
            else -> render(currentPage)
        }
    }

    /** 系统返回键：停在任一二级页时先退回「更多」，而不是直接退出 App。 */
    @Deprecated("Activity.onBackPressed 已废弃，但本项目未引 androidx（无 OnBackPressedDispatcher）")
    override fun onBackPressed() {
        if (showKeepAlive || showStyle) render(Page.OVERVIEW) else @Suppress("DEPRECATION") super.onBackPressed()
    }

    /**
     * 记住当前 Tab 与二级页。
     *
     * 两个场景都靠它才不会"跳页"：① 切风格/亮暗走 [recreate]；
     * ② 系统夜间模式切换时本 Activity 会被系统重建（manifest 未声明 configChanges）。
     */
    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putString(STATE_PAGE, currentPage.name)
        outState.putString(STATE_SUB, when {
            showKeepAlive -> SUB_KEEP_ALIVE
            showStyle -> SUB_STYLE
            else -> ""
        })
    }

    // ------------------------------------------------------------------
    // 概览
    // ------------------------------------------------------------------

    private fun overviewPage(): View = scroll {
        val running = GatewayService.running
        val busy = serviceBusy
        addView(when {
            busy != null -> chipInfo(busy)
            running -> statusChip("服务运行中", "success", true)
            else -> statusChip("服务已停止", "error", false)
        })

        // 后台常驻：只留状态 + 两个入口（用户要求去掉全部说明文字）
        addView(card(tone = CardTone.SOFT) {
            val pending = KeepAlive.check(this@MainActivity).count { !it.ok }
            addView(row {
                addView(text(if (running) "后台常驻中" else "服务未启动", FS_CARD_TITLE, true), weighted())
                addView(if (pending == 0) chipOk("保活就绪") else chipWarn("$pending 项待开启"))
            })
            addView(row(8) {
                addView(action(if (pending == 0) "保活设置" else "去设置保活（$pending 项）", outlined = true) {
                    openKeepAlive()
                }, weighted())
                addView(action("外观", outlined = true) { openStylePage() }, weighted(start = 8))
            }, top(12))
        }, top(GAP))

        // 服务地址 + API Key 合并成一张卡（用户要求：API key 移上来和服务地址放在一起）
        val lanIp = lanAddress()
        addView(card(tone = CardTone.EMPHASIS) {
            addView(caption("本机地址"))
            addView(row(8) {
                addView(mono("http://127.0.0.1:${LocalFakeServer.PORT}/v1", FS_BODY, true, "primary"), weighted())
                addView(linkAction("复制", overContainer = true) { copyServerUrl() })
            }, top(4))
            if (lanIp != null) {
                addView(caption("局域网地址"), top(10))
                addView(row(8) {
                    addView(mono("http://$lanIp:${LocalFakeServer.PORT}/v1", FS_BODY, true, "primary"), weighted())
                    addView(linkAction("复制", overContainer = true) { copyLanUrl(lanIp) })
                }, top(4))
            }
            addView(divider(), top(12))
            addView(caption("API Key"))
            addView(row(8) {
                addView(text(if (apiKeyVisible) apiKey else MASKED_KEY, FS_VALUE).apply {
                    setTextIsSelectable(apiKeyVisible)
                }, weighted())
                addView(linkAction(if (apiKeyVisible) "隐藏" else "显示", overContainer = true) {
                    apiKeyVisible = !apiKeyVisible
                    render(currentPage)
                }, lp(-2, -2, start = 8))
                addView(linkAction("复制", overContainer = true) { copyApiKey() }, lp(-2, -2, start = 8))
            }, top(4))
        }, top(GAP))

        addView(action(busy ?: if (running) "停止服务" else "启动服务", full = true, outlined = running || busy != null) { toggleService() }
            .apply {
                if (busy != null) { isEnabled = false; alpha = 0.6f }
            }, top(GAP))

        addView(card { addTraeCard() })
    }

    private fun LinearLayout.addTraeCard() {
        val cred = TraeAuthStore.load(this@MainActivity)
        if (cred == null) {
            addView(text("未登录 Trae", FS_CARD_TITLE, true))
            return
        }
        val hasToken = !cred.accessToken.isBlank()
        addView(row {
            addView(text(cred.nickname.ifBlank { "Trae 账号" }, FS_CARD_TITLE, true), weighted())
            addView(if (hasToken) statusChip("转发 已启用", "success", true) else statusChip("转发 未配置", "warning", false))
        })
        if (cred.uid.isNotBlank()) addView(caption("UID: ${cred.uid} · ${regionLabelFull(cred.region)}"), top(6))
        if (!cred.region.isSupported) addView(caption(TraeRegion.SG.unsupportedNote), top(4))
        if (cred.expiresAt > 0L) addView(caption("Token 到期: ${formatExp(cred.expiresAt)}"), top(4))
    }

    // ------------------------------------------------------------------
    // 账号
    // ------------------------------------------------------------------

    private fun accountPage(): View = scroll {
        // 本页会重建，旧的积分控件句柄必须清掉，否则刷新回来会写进已分离的 View
        creditViews.clear()

        // ---- 积分总计（无说明）----
        addView(card {
            addView(row {
                addView(text("积分总计", FS_CARD_TITLE, true), weighted())
                // 这是**用户主动点**的刷新，允许全量（只有这条路径会遍历所有账号）
                addView(linkAction("刷新") {
                    toast("正在查询全部账号积分…")
                    AccountCredits.refreshAll(this@MainActivity, force = true) { refreshCreditViews() }
                })
            })
            creditTotalTrae = text("—", FS_METRIC, true, "primary")
            creditTotalQoder = text("—", FS_METRIC, true, "primary")
            addView(row(12) {
                addView(creditBlock("Trae", creditTotalTrae!!), weighted())
                addView(creditBlock("Qoder", creditTotalQoder!!), weighted(start = 12))
            }, top(10))
            creditUpdated = caption("")
            addView(creditUpdated!!, top(8))
        }, top(GAP))

        // ---- TRAE 账号 ----
        addView(section("TRAE 账号"))
        val traeMetas = TraeAuthStore.list(this@MainActivity)
        val traeActive = TraeAuthStore.activeUid(this@MainActivity)
        val traeCred = TraeAuthStore.load(this@MainActivity)
        if (traeMetas.isEmpty()) {
            addView(card(tone = CardTone.SOFT) {
                addView(text("未添加账号", FS_CARD_TITLE, true))
            })
        } else {
            // 第一张：当前使用的账号
            addView(card {
                addView(row {
                    addView(text(traeCred?.nickname?.ifBlank { null } ?: "TRAE 账号", FS_CARD_TITLE, true), weighted())
                    addView(chipOk("当前使用"))
                })
                if (traeCred != null) {
                    addView(labelValue("区域", regionLabelFull(traeCred.region)), top(10))
                    addView(labelValue("UID", traeCred.uid), top(4))
                    addView(labelValue("Token", tokenRenewLabel(traeCred.uid, traeCred.expiresAt)), top(4))
                    addView(creditLabel(AccountCredits.TRAE, traeCred.uid), top(6))
                    // 卡上直接签到：不用滚到页面底部的一键签到
                    addView(row(8) {
                        // 文案走 checkinActionLabel（含本地落库判断），和按钮的实际可点状态同源。
                        // 以前这里自己写 `accountCheckin?.checkedIn == true`：打开 App 时该值是 null，
                        // 于是已签到的号也显示「签到」——本地明明记着今天签过了。
                        val btn = action(checkinActionLabel(traeCred != null), outlined = true) { claimCheckin() }
                        addView(btn, weighted())
                        // 建好之后立刻按真实状态修正文案/可点性（refreshCheckinButton 内部会调 checkinActionLabel）
                        btn.post { if (!isFinishing && !isDestroyed) refreshCheckinButton(btn, traeCred != null) }
                    }, top(12))
                }
            })
            // 第二件：横起来的账户选择（点开弹窗，一排一个账号）
            addView(accountBar("账户选择") { showTraeAccountPicker() }, top(8))
        }
        addView(row(8) {
            addView(action("登录账号") { startTraeLogin() }, weighted())
            addView(action("粘贴回调链接", outlined = true) { promptPasteCallback() }, weighted(start = 8))
        }, top(10))

        // ---- Qoder 账号 ----
        addView(section("Qoder 账号"))
        val qMetas = QoderAuthStore.list(this@MainActivity)
        val qActive = QoderAuthStore.activeUid(this@MainActivity)
        val qActiveMeta = qMetas.firstOrNull { it.uid == qActive }
        if (qMetas.isEmpty()) {
            addView(card(tone = CardTone.SOFT) {
                addView(text("未添加账号", FS_CARD_TITLE, true))
            })
        } else {
            qoderCreditView = text(qoderRemainingLabel(), FS_METRIC, true, "primary")
            addView(card {
                addView(row {
                    addView(text(qActiveMeta?.name?.ifBlank { null } ?: "Qoder 账号", FS_CARD_TITLE, true), weighted())
                    addView(chipOk("当前使用"))
                })
                if (qActiveMeta != null) {
                    addView(labelValue("区域", if (qActiveMeta.mode == QoderAuthApi.MODE_GLOBAL) "国际版" else "国内版"), top(10))
                    addView(labelValue("UID", qActiveMeta.uid), top(4))
                    if (qActiveMeta.email.isNotBlank()) addView(labelValue("邮箱", qActiveMeta.email), top(4))
                }
                addView(caption("积分"), top(10))
                addView(qoderCreditView!!, top(2))
                // 卡上直接领取（只领这一个账号，不动其它账号）
                if (qActiveMeta != null) {
                    addView(row(8) {
                        addView(
                            action(
                                when {
                                    qoderClaimBusy -> "领取中…"
                                    qoderClaimedToday(qActiveMeta.uid) -> "已领取"
                                    else -> "领取"
                                },
                                outlined = true,
                            ) {
                                claimQoderForUid(qActiveMeta.uid)
                            },
                            weighted(),
                        )
                    }, top(12))
                }
                // 设备身份：**所有 Qoder 账号都显示入口**（以前只对国际版显示，
                // 用户在国内版上找不到它）。
                //   · 国际版领取会带这套身份（有抓包真货就用真货，否则用本机派生）
                //   · 国内版刻意不带（实测国内带指纹反而 no_campaign）
                if (qActiveMeta != null) {
                    val captured = QoderAuthStore.fingerprint(this@MainActivity, qActiveMeta.uid) != null
                    val isGlobal = qActiveMeta.mode == QoderAuthApi.MODE_GLOBAL
                    addView(linkAction(
                        (if (captured) "设备身份：抓包指纹" else "设备身份：本机派生") +
                            (if (isGlobal) "" else "（国内版不发送）")
                    ) {
                        promptQoderFingerprint(qActiveMeta.uid)
                    }, top(12))
                }
            })
            addView(accountBar("账户选择") { showQoderAccountPicker() }, top(8))
        }
        // 添加 Qoder 账号：两个按钮（登录跳浏览器 / 回填令牌）
        addView(row(8) {
            addView(action("登录 Qoder") { showQoderLoginGuide() }, weighted())
            addView(action("回填令牌", outlined = true) { promptQoderPat() }, weighted(start = 8))
        }, top(10))

        // ---- 聊天转发后端：三个按钮，当前选择绿色高亮 ----
        addView(section("聊天转发后端"))
        run {
            val sel = GatewayBackend.selection(this@MainActivity)
            fun pick(b: String) {
                GatewayBackend.set(this@MainActivity, b)
                toast("转发后端：${GatewayBackend.label(b)}")
                render(currentPage)
            }
            addView(row(8) {
                addView(action("TRAE", outlined = sel != GatewayBackend.TRAE) { pick(GatewayBackend.TRAE) }, weighted())
                addView(action("Qoder", outlined = sel != GatewayBackend.QODER) { pick(GatewayBackend.QODER) }, weighted(start = 8))
                addView(action("自动", outlined = sel != GatewayBackend.AUTO) { pick(GatewayBackend.AUTO) }, weighted(start = 8))
            })
        }

        // ---- 自动换号（可选）：当前号用完时在同一平台内换到还有积分的号 ----
        //
        // 注意这里**没有检测间隔**：定时轮询已彻底删除（会拿账号去刷上游接口）。
        // 换号判定只发生在"转发之后刷新当前账号"那一刻，不额外增加请求。
        addView(section("自动换号"))
        addView(card {
            val auto = CreditMonitor.isAutoSwitchEnabled(this@MainActivity)
            val cur = CreditMonitor.threshold(this@MainActivity)
            addView(row {
                addView(text("当前号用完就换", FS_VALUE, true), weighted())
                addView(action(if (auto) "已开启" else "已关闭", outlined = !auto) {
                    CreditMonitor.setAutoSwitchEnabled(this@MainActivity, !auto)
                    toast(if (!auto) "已开启自动换号" else "已关闭自动换号")
                    render(currentPage)
                })
            })
            addView(row {
                addView(text("换号阈值", FS_VALUE, true), weighted())
                addView(chipInfo("≤ $cur"))
            }, top(12))
            addView(row(8) {
                listOf(5L, 10L, 15L, 20L).forEachIndexed { i, v ->
                    addView(action("$v", outlined = cur != v) {
                        CreditMonitor.setThreshold(this@MainActivity, v)
                        render(currentPage)
                    }, weighted(start = if (i == 0) 0 else 8))
                }
            }, top(10))
            addView(caption("只在同一个平台内换号；换完立刻刷新新号的余额。不做定时检测。"), top(8))
        }, top(8))

        // ---- 账户导出：只留导出 / 导入 ----
        addView(section("账户导出"))
        addView(row(8) {
            addView(action("导出", outlined = true) { exportAccounts() }, weighted())
            addView(action("导入", outlined = true) { importAccounts() }, weighted(start = 8))
        }, top(8))

        // ---- 一键签到（所有账号 / 所有平台）----
        addView(section("一键签到"))
        addView(card {
            val statusView = caption("")
            quotaApply = { statusView.text = checkinStatusText() }
            statusView.text = checkinStatusText()
            // 一个按钮覆盖两个平台的**全部账号**：文案固定，不随单平台登录态变（否则
            // "未登录" 会把"Qoder 还能签"这件事说没了）
            addView(action("一键签到全部账号", full = true) { claimAllCheckin() })
            addView(linkAction("诊断领取链路") { runQoderDiagnose() }, top(8))
            addView(linkAction("诊断 Trae 账号") { runTraeDiagnose() }, top(4))
            addView(statusView, top(10))

            addView(divider(), top(14))
            val on = QoderCheckin.isEnabled(this@MainActivity)
            addView(row {
                addView(text("定时领取", FS_VALUE, true), weighted())
                addView(linkAction(if (on) "已开启" else "已关闭") {
                    if (QoderCheckin.isEnabled(this@MainActivity)) {
                        QoderCheckin.disable(this@MainActivity); toast("已关闭定时领取")
                    } else {
                        QoderCheckin.enable(this@MainActivity, QoderCheckin.timeText(this@MainActivity))
                        toast("已开启，每天 ${QoderCheckin.timeText(this@MainActivity)}")
                    }
                    render(currentPage)
                }, lp(-2, -2, start = 8))
                addView(linkAction(QoderCheckin.timeText(this@MainActivity)) { promptCustomTime() },
                    lp(-2, -2, start = 8))
            }, top(12))
        }, top(8))

        // 只用**本地缓存**画数字，**不发任何请求**（用户要求：进账号页也不许自动刷新）。
        // 数据来源：转发后刷新的结果 + 手动点「刷新」+ 补签落库的状态。
        refreshCreditViews()

        // 登记"监测器要把哪些控件原地刷新"
        monitorApply = {
            qoderCreditView?.text = qoderRemainingLabel()
        }
    }

    // ------------------------------------------------------------------
    // 账号页的交互零件
    // ------------------------------------------------------------------

    /** 横向账户选择条：**只写「账户选择」**（不显示账号名，账号在弹窗里选）。 */
    private fun accountBar(title: String, onClick: () -> Unit): View =
        LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            background = shape("surface", RADIUS_CARD, "outline_variant")
            isClickable = true
            setOnClickListener { onClick() }
            setPadding(dp(CARD_PAD), dp(16), dp(CARD_PAD), dp(16))
            addView(text(title, FS_VALUE, true), weighted())
            addView(text("\u25BE", FS_CARD_TITLE, true, "on_surface_variant"))
        }

    /** 某个 Trae 账号的积分文本（"积分：123" / "积分：—"）。 */
    private fun traeCreditText(uid: String): String =
        "积分：" + AccountCredits.label(AccountCredits.current().trae(uid))

    private fun qoderCreditText(uid: String): String =
        "积分：" + AccountCredits.label(AccountCredits.current().qoder(uid))

    /** Trae 账号选择弹窗：每行 = 用户名 · 区域 · 积分，当前账号后面一个绿勾。顺序不动。 */
    private fun showTraeAccountPicker() {
        val metas = TraeAuthStore.list(this)
        val active = TraeAuthStore.activeUid(this)
        if (metas.isEmpty()) { toast("还没有账号"); return }
        val panel = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        metas.forEach { m ->
            panel.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(6), 0, dp(6), 0)
                // 每行**固定高度**：账号多寡只改变滚动，不改变行高
                layoutParams = LinearLayout.LayoutParams(-1, dp(PICKER_ROW_H))
                isClickable = true
                setOnClickListener {
                    pickerDialogRef?.dismiss()
                    if (m.uid != active) switchAccount(m.uid)
                }
                addView(text(m.nickname.ifBlank { m.uid }, FS_VALUE, true).apply {
                    maxLines = 1
                    ellipsize = android.text.TextUtils.TruncateAt.END
                }, weighted())
                addView(caption(regionLabel(m.region)), lp(-2, -2, start = 8))
                addView(caption(traeCreditText(m.uid)), lp(-2, -2, start = 10))
                if (m.uid == active) {
                    addView(text("\u2713", FS_CARD_TITLE, true, "success"), lp(-2, -2, start = 10))
                }
                addView(text("删除", FS_CHIP, false, "error").apply {
                    setPadding(dp(12), dp(4), dp(2), dp(4))
                    setOnClickListener {
                        confirmDialog(
                            "删除账号",
                            "删除「${m.nickname.ifBlank { m.uid }}」？本地凭证会一起清掉。",
                            "删除",
                            danger = true,
                        ) {
                            TraeAuthStore.remove(this@MainActivity, m.uid)
                            // 只抹掉本地缓存，不触发刷新（删号不需要去问上游）
                            AccountCredits.forget(this@MainActivity, AccountCredits.TRAE, m.uid)
                            pickerDialogRef?.dismiss()
                            toast("已删除")
                            render(Page.ACCOUNT)
                        }
                    }
                }, lp(-2, -2, start = 6))
            })
            panel.addView(divider())
        }
        showPickerDialog("选择 Trae 账号", panel, metas.size)
    }

    /** Qoder 账号选择弹窗：同样的排版与规则。 */
    private fun showQoderAccountPicker() {
        val metas = QoderAuthStore.list(this)
        val active = QoderAuthStore.activeUid(this)
        if (metas.isEmpty()) { toast("还没有账号"); return }
        val panel = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        metas.forEach { m ->
            panel.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(dp(6), 0, dp(6), 0)
                layoutParams = LinearLayout.LayoutParams(-1, dp(PICKER_ROW_H))
                isClickable = true
                setOnClickListener {
                    pickerDialogRef?.dismiss()
                    if (m.uid != active) switchQoderAccount(m.uid)
                }
                addView(text(m.name.ifBlank { m.uid }, FS_VALUE, true).apply {
                    maxLines = 1
                    ellipsize = android.text.TextUtils.TruncateAt.END
                }, weighted())
                addView(caption(if (m.mode == QoderAuthApi.MODE_GLOBAL) "国际版" else "国内版"), lp(-2, -2, start = 8))
                addView(caption(qoderCreditText(m.uid)), lp(-2, -2, start = 10))
                if (m.uid == active) {
                    addView(text("\u2713", FS_CARD_TITLE, true, "success"), lp(-2, -2, start = 10))
                }
                addView(text("删除", FS_CHIP, false, "error").apply {
                    setPadding(dp(12), dp(4), dp(2), dp(4))
                    setOnClickListener {
                        confirmDialog(
                            "删除账号",
                            "删除「${m.name.ifBlank { m.uid }}」？本地令牌会一起清掉。",
                            "删除",
                            danger = true,
                        ) {
                            QoderAuthStore.remove(this@MainActivity, m.uid)
                            AccountCredits.forget(this@MainActivity, AccountCredits.QODER, m.uid)
                            pickerDialogRef?.dismiss()
                            toast("已删除")
                            render(Page.ACCOUNT)
                        }
                    }
                }, lp(-2, -2, start = 6))
            })
            panel.addView(divider())
        }
        showPickerDialog("选择 Qoder 账号", panel, metas.size)
    }

    /** 统一样式的列表弹窗（只有关闭键；行自己处理点击）。 */
    private var pickerDialogRef: AlertDialog? = null

    /**
     * 账号选择弹窗的壳：**固定大小**，最多露出 [PICKER_VISIBLE_ROWS] 行，超出上下滑动。
     *
     * 为什么固定：账号一多，弹窗会被撑到整屏甚至超出屏幕，底部「关闭」按钮跑出可见区，
     * 用户就没法关掉它。固定高度后，"几个账号"只影响滚动条，不影响弹窗尺寸。
     */
    private fun showPickerDialog(title: String, panel: View, rows: Int) {
        val visible = rows.coerceIn(1, PICKER_VISIBLE_ROWS)
        // 行高 * 行数 + 分隔线（行间 1dp）+ 2dp 余量，避免最后一行被裁
        val h = dp(visible * PICKER_ROW_H + (visible - 1) + 2)
        val scroll = ScrollView(this).apply {
            isVerticalScrollBarEnabled = true
            addView(panel, android.widget.FrameLayout.LayoutParams(-1, -2))
        }
        val holder = android.widget.FrameLayout(this).apply {
            addView(scroll, android.widget.FrameLayout.LayoutParams(-1, h))
        }
        val dlg = confirmDialog(
            title = title,
            message = "",
            okText = "关闭",
            cancelText = null,
            content = holder,
        ) { }
        pickerDialogRef = dlg
    }

    /** Trae 登录：复制链接并直接跳浏览器（登录后回到「粘贴回调链接」）。 */
    private fun startTraeLogin() {
        val region = TraeRegion.current
        if (!region.canLogin) { toast("${region.id} 区域未开放"); return }
        TraeRegion.set(this, region)
        val url = TraeAuthApi.buildLoginUrl()
        copyToClipboard("traeLoginUrl", url, "登录链接已复制")
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: Exception) {
            toast("没有可用浏览器，链接已复制，请手动打开")
        }
    }

    /** Qoder 登录：先给登录说明，确认才跳浏览器。 */
    private fun showQoderLoginGuide() {
        confirmDialog(
            title = "登录 Qoder",
            message = "1. 登录后点左侧「服务集成」\n" +
                "2. 在「个人访问令牌」处生成并复制令牌\n" +
                "3. 回到本页点「回填令牌」粘贴",
            okText = "打开浏览器",
        ) {
            try {
                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://qoder.com/account/integrations")))
            } catch (_: Exception) {
                copyToClipboard("qoderUrl", "https://qoder.com/account/integrations", "链接已复制，请用浏览器打开")
            }
        }
    }

    /**
     * Qoder 设备身份（国际版领取必需）。
     *
     * 默认值来自 [QoderMachine]：照官方 CLI 的 `randomUUID → sha256` 回退链路复刻，
     * 服务端看到的就是「没有 umid 的桌面端」，不是凭空编的哈希。
     * 这里额外允许回填**抓包得到的真实指纹**（有真货时优先级更高）。
     */
    private fun promptQoderFingerprint(uid: String) {
        val cur = QoderAuthStore.fingerprint(this, uid)
        val derived = QoderMachine.machineId(this)
        fun field(hint: String, value: String) = EditText(this).apply {
            this.hint = hint
            setText(value)
            isSingleLine = true
            textSize = FS_CARD_TITLE.toFloat()
            setTextColor(color("on_surface"))
            setHintTextColor(color("on_surface_variant"))
            background = shape("surface_container", RADIUS_CONTROL)
            setPadding(dp(12), dp(10), dp(12), dp(10))
        }
        val idField = field("cosy-machineid", cur?.machineId.orEmpty())
        val tkField = field("cosy-machinetoken", cur?.machineToken.orEmpty())
        val tyField = field("cosy-machinetype（默认空）", cur?.machineType.orEmpty())
        val panel = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL
            addView(idField, top(0))
            addView(tkField, top(8))
            addView(tyField, top(8))
            if (cur != null) {
                addView(linkAction("清除抓包指纹，回到本机派生") {
                    QoderAuthStore.clearFingerprint(this@MainActivity, uid)
                    toast("已清除，改回本机派生身份")
                    render(currentPage)
                }, top(12))
            }
        }
        confirmDialog(title = "Qoder 设备身份", message = "", okText = "保存", content = panel) {
            val id = idField.text?.toString()?.trim().orEmpty()
            val tk = tkField.text?.toString()?.trim().orEmpty()
            if (id.isBlank() || tk.isBlank()) {
                toast("两项都留空 = 用本机派生；要覆盖就两项都填")
                return@confirmDialog
            }
            val ty = tyField.text?.toString()?.trim().orEmpty()
            QoderAuthStore.setFingerprint(this, uid, QoderAuthStore.QFingerprint(id, tk, ty))
            toast("已保存抓包指纹")
            render(currentPage)
        }
    }

    /**
     * 定时领取时间：**上下滑动**选小时与分钟（不手打）。
     *
     * 用两个 [NumberPicker] 而不是输入框：手打要处理 "8:5" / "08：05" / 全角冒号等一堆脏输入，
     * 而滚轮天然只有合法值，滑到哪就是哪。
     */
    private fun promptCustomTime() {
        val cur = QoderCheckin.timeText(this@MainActivity).split(":")
        val h0 = cur.getOrNull(0)?.toIntOrNull()?.coerceIn(0, 23) ?: 8
        val m0 = cur.getOrNull(1)?.toIntOrNull()?.coerceIn(0, 59) ?: 0
        val hourPicker = NumberPicker(this@MainActivity).apply {
            minValue = 0
            maxValue = 23
            value = h0
            wrapSelectorWheel = true
        }
        val minPicker = NumberPicker(this@MainActivity).apply {
            minValue = 0
            maxValue = 59
            value = m0
            wrapSelectorWheel = true
        }
        val panel = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER
            addView(hourPicker, lp(0, -2).also { it.weight = 1f })
            addView(text(":", FS_CARD_TITLE, true), lp(-2, -2, start = 6, end = 6))
            addView(minPicker, lp(0, -2).also { it.weight = 1f })
        }
        confirmDialog(
            title = "定时领取时间",
            message = "",
            okText = "确定",
            content = panel,
        ) {
            val t = String.format(Locale.US, "%02d:%02d", hourPicker.value, minPicker.value)
            if (QoderCheckin.isEnabled(this@MainActivity)) {
                QoderCheckin.enable(this@MainActivity, t)
            } else {
                QoderCheckin.setTime(this@MainActivity, t)
            }
            toast("定时领取：每天 $t")
            render(currentPage)
        }
    }

    /** 一键签到：Trae 与 Qoder **所有账号**都跑一遍。 */
    private fun claimAllCheckin() {
        var any = false
        if (TraeAuthStore.activeUid(this) != null) { claimCheckin(); any = true }
        if (QoderAuthStore.list(this).isNotEmpty()) { claimQoderDailyAll(); any = true }
        if (!any) toast("没有可签到的账号")
    }

    /** 一键签到卡下面的状态文字：只报结果，不写解释。 */
    private fun checkinStatusText(): String {
        val parts = ArrayList<String>()
        accountCheckin?.let {
            parts.add("Trae：" + if (it.checkedIn) "已签到" else if (!it.enable) "无签到权限" else "可签到")
        }
        val last = QoderCheckin.lastResults(this)
        if (last != null) {
            val ok = last.results.count { it.ok }
            parts.add("Qoder：" + if (ok > 0) "领取 $ok 个" else "无可领")
        }
        return parts.joinToString("  ·  ")
    }

    // ------------------------------------------------------------------
    // 按账号积分（AccountCredits）的界面胶水
    // ------------------------------------------------------------------

    /** 倍率文本：`0` / `1` / `2.5`（去掉无意义的小数尾巴）。 */
    private fun priceText(v: Double): String =
        if (v == v.toLong().toDouble()) v.toLong().toString() else v.toString()

    /** 一个可原地刷新的"积分"文字（登记到 [creditViews]，刷新回来只改 text，不重建页面）。 */
    private fun creditLabel(platform: String, uid: String): TextView {
        val tv = text("", FS_CAPTION, false, "on_surface_variant")
        val value: () -> String = {
            val snap = AccountCredits.current()
            val item = if (platform == AccountCredits.TRAE) snap.trae(uid) else snap.qoder(uid)
            "积分：${AccountCredits.label(item)}"
        }
        creditViews.add(tv to value)
        tv.text = value()
        return tv
    }

    /** 总览里的一个竖排小块（标题 + 大数字）。 */
    private fun creditBlock(title: String, value: TextView): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        addView(caption(title))
        addView(value, top(2))
    }

    /** 把 [creditViews] 与两个合计、更新时间**原地**重画（不 render，滚动位置不丢）。 */
    private fun refreshCreditViews() {
        for ((tv, value) in creditViews) tv.text = value()
        val snap = AccountCredits.current()
        creditTotalTrae?.text = AccountCredits.totalLabel(snap, AccountCredits.TRAE)
        creditTotalQoder?.text = AccountCredits.totalLabel(snap, AccountCredits.QODER)
        creditUpdated?.text = when {
            snap.loading -> "正在查询全部账号…"
            snap.at <= 0L -> "尚未查询（点右上「刷新积分」）"
            else -> "更新于 ${clockText(snap.at)}"
        }
    }

    /** `HH:mm:ss`。 */
    private fun clockText(ms: Long): String =
        java.text.SimpleDateFormat("HH:mm:ss", java.util.Locale.getDefault()).format(java.util.Date(ms))

    /**
     * 手动导入 Qoder CLI 的模型目录缓存（`catalog-v7` / `catalog-v6`）。
     *
     * 为什么要这条路：上游目录接口的响应体形状没有公开文档，实测只能确认路由存在（401）。
     * CLI 缓存是**同一份数据**，且解密算法已知（HKDF 派生 key + AES-GCM），
     * 所以让用户把文件内容粘进来，就能拿到**准确的 key 和官方倍率**。
     */
    private fun promptImportCatalog() {
        val uid = QoderAuthStore.load(this)?.uid.orEmpty()
        val edit = EditText(this).apply {
            hint = "粘贴 catalog-v7 文件的全部内容（base64），或一段明文目录 JSON"
            isSingleLine = false
            maxLines = 8
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
            setTextColor(color("on_surface"))
            setHintTextColor(color("on_surface_variant"))
            background = shape("surface_container", RADIUS_CONTROL)
            setPadding(dp(14), dp(12), dp(14), dp(12))
        }
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(edit)
            addView(caption("在装过 Qoder CLI 的机器（比如代码空间）上执行："), top(12))
            addView(mono(QoderModelCache.cliHint(uid)), top(4))
            addView(caption("把输出的整段内容粘贴到上面即可。解密用当前账号的 uid，你不需要自己解。"), top(8))
        }
        confirmDialog(
            title = "导入官方模型目录",
            message = "拿到准确的模型 key 和官方倍率（含 0 倍率的免费模型）。",
            okText = "导入",
            content = panel,
        ) {
            val text = edit.text?.toString().orEmpty()
            if (text.isBlank()) { toast("没有粘贴内容"); return@confirmDialog }
            val r = QoderModelCache.importBlob(text, uid)
            if (!r.ok) { toast(r.error ?: "导入失败"); return@confirmDialog }
            // 落盘必须存**明文**：存密文的话下次启动 parseCatalogJson 解不出来，等于白导
            r.plain?.let { QoderModelCatalog.save(this@MainActivity, it) }
            modelsRefreshApply?.invoke()
            toast("已导入 ${r.models} 个模型" + (if (r.decrypted) "（已解密）" else "") +
                "，共 ${QoderModelCatalog.current().size} 个，免费 ${QoderModelCatalog.freeCount()} 个")
        }
    }

    /** 把上次拉模型目录的**响应原文**摊开看（排查"对不上"用）。 */
    private fun showLastCatalogRaw() {
        val raw = QoderAuthApi.lastCatalogRaw
        val err = QoderAuthApi.lastCatalogError
        val body = when {
            raw.isNullOrBlank() && err.isNullOrBlank() -> "还没有拉取过。点「同步官方目录」试一次。"
            raw.isNullOrBlank() -> "没拿到响应体。\n失败信息：$err"
            else -> "长度 ${raw.length}\n\n" + raw.take(4000) + if (raw.length > 4000) "\n…（已截断）" else ""
        }
        val tv = TextView(this).apply {
            text = body
            setTextIsSelectable(true)
            setTextColor(color("on_surface"))
            textSize = FS_CHIP.toFloat()
            background = shape("surface_container", RADIUS_CONTROL)
            setPadding(dp(12), dp(12), dp(12), dp(12))
        }
        val scroll = ScrollView(this).apply { addView(tv) }
        confirmDialog(
            title = "上次拉取的原文",
            message = if (err.isNullOrBlank()) "接口回了下面这些内容。" else "失败：$err",
            okText = "关闭",
            content = scroll,
        ) { }
    }

    /**
     * 从 Qoder 官方拉模型目录（含真实倍率、以及内置清单里没有的免费模型）。
     *
     * 失败**不清空**现有清单（内置清单永远兜底）：模型列表空了聊天直接废。
     * 成功后按 key 合并进内置清单并落盘，重启不丢。
     */
    private fun syncQoderCatalog(quiet: Boolean = false) {
        val cred = QoderAuthStore.load(this)
        if (cred == null) {
            if (!quiet) toast("先添加一个 Qoder 账号，才能拉官方模型目录")
            modelsRefreshApply?.invoke()
            return
        }
        if (!quiet) toast("正在拉取 Qoder 官方模型目录…")
        thread(name = "qoder-catalog-sync") {
            val jt = QoderAuthApi.validJobTokenFor(this, cred, 60L * 60L * 1000L)
                ?: cred.jobToken.takeIf { it.isNotBlank() }
            var raw: String? = null
            var parsed: List<QoderModelCatalog.QModel> = emptyList()
            if (!jt.isNullOrBlank()) {
                // ① 首选：COSY 签名的动态目录（与桌面版模型选择器同源，"此刻"最准）
                raw = QoderUpstream.fetchModelListRaw(this, cred, jt)
                parsed = if (raw == null) emptyList() else QoderModelCatalog.parseCatalogJson(raw)
                if (parsed.isEmpty()) {
                    // ② 退一步：openapi 的 /api/v1/models（响应形状没实测过，能出就用）
                    val alt = QoderAuthApi.fetchModelCatalog(jt, cred.mode)
                    val altParsed = if (alt == null) emptyList() else QoderModelCatalog.parseCatalogJson(alt)
                    if (altParsed.isNotEmpty()) {
                        raw = alt
                        parsed = altParsed
                    } else if (raw == null) {
                        raw = alt
                    }
                }
            }
            if (raw != null && parsed.isNotEmpty()) {
                QoderModelCatalog.setDynamic(parsed)
                QoderModelCatalog.save(this, raw)
            }
            val total = QoderModelCatalog.current().size
            val free = QoderModelCatalog.freeCount()
            val dynamic = QoderModelCatalog.hasDynamic()
            runOnUiThread {
                modelsRefreshApply?.invoke()
                if (!quiet) {
                    if (dynamic) {
                        toast("官方目录已同步：$total 个模型（$free 个免费）")
                    } else {
                        val why = QoderAuthApi.lastCatalogError?.take(120)
                        toast("拉取失败，继续用内置 $total 个模型" + (if (why != null) "\n$why" else ""))
                    }
                }
            }
        }
    }

    /**
     * Qoder 积分余量数值位：没拉到/无账号显示「—」。
     *
     * 取数优先级：**按账号快照**（[AccountCredits]，覆盖全部账号、带时间戳）
     * → 监测器快照（[CreditMonitor]，只盯当前账号、2 秒一次）。
     * 前者更新更全，后者更实时，都拿不到才显示「—」。
     */
    private fun qoderRemainingLabel(): String {
        val uid = QoderAuthStore.activeUid(this)
        if (uid != null) {
            // 走同一个 label()：卡片和账号弹窗必须显示**同一个**读数与同一种失败原因，
            // 否则会出现"卡片 600、弹窗 —（需重新登录）"这种自相矛盾。
            val item = AccountCredits.current().qoder(uid)
            if (item != null) return AccountCredits.label(item)
        }
        return qoderRemaining?.let { "%.1f".format(it) } ?: "—"
    }

    // ------------------------------------------------------------------
    // Qoder：粘贴 PAT 登录 / 切号 / 删除
    // ------------------------------------------------------------------

    private fun promptQoderPat() {
        val edit = EditText(this).apply {
            hint = "粘贴 Qoder 个人访问令牌 pt-…"
            isSingleLine = false
            maxLines = 4
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
            setTextColor(color("on_surface"))
            setHintTextColor(color("on_surface_variant"))
            background = shape("surface_container", RADIUS_CONTROL)
            setPadding(dp(14), dp(12), dp(14), dp(12))
        }
        // 区域选择：**默认国际版**（绝大多数用户用的是 qoder.sh），选中项实心高亮。
        var mode = QoderAuthApi.MODE_GLOBAL
        val modeRow = row(8) {}
        fun paintMode() {
            modeRow.removeAllViews()
            fun btn(label: String, value: String) {
                val on = mode == value
                modeRow.addView(
                    action(label, outlined = !on) { mode = value; paintMode() },
                    if (modeRow.childCount == 0) weighted() else weighted(start = 8),
                )
            }
            btn("国际版", QoderAuthApi.MODE_GLOBAL)
            btn("国内版", QoderAuthApi.MODE_CN)
        }
        paintMode()
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(edit)
            addView(caption("令牌属于哪个区域？"), top(12))
            addView(modeRow, top(6))
        }
        confirmDialog(
            title = "粘贴 Qoder PAT 登录",
            message = "在 Qoder 账号集成页创建个人访问令牌后粘贴。",
            okText = "登录",
            content = panel,
        ) {
            val pat = edit.text?.toString().orEmpty()
            if (pat.isBlank()) { toast("没有粘贴内容"); return@confirmDialog }
            toast("正在换取 jobToken…")
            thread(name = "qoder-pat-login") {
                val r = runCatching { QoderAuthApi.loginWithPat(this, pat, mode) }
                runOnUiThread {
                    if (isFinishing || isDestroyed) return@runOnUiThread
                    r.onSuccess { c ->
                        toast("登录成功：${c.name.ifBlank { c.email.ifBlank { c.uid } }}")
                        // 新账号要马上补签：清掉冷却，别等下一轮
                        AutoCheckin.requestRunSoon(this)
                        qoderRemaining = null
                        CreditMonitor.stale = true
                        render(currentPage)
                    }.onFailure { e ->
                        toast("登录失败：${e.message}")
                        android.util.Log.w("QoderLogin", "PAT 登录失败", e)
                    }
                }
            }
        }
    }

    private fun switchQoderAccount(uid: String) {
        QoderAuthStore.setActive(this, uid)
        qoderRemaining = null
        CreditMonitor.stale = true
        Toast.makeText(this, "已切换到 Qoder ${uid}", Toast.LENGTH_SHORT).show()
        // 切号**不发请求**校准（用户要求），只按本地标记重画
        render(currentPage)
    }

    private fun confirmRemoveQoder(meta: QoderAuthStore.QMeta) {
        confirmDialog(
            title = "删除 Qoder 账号",
            message = "将删除「${meta.name.ifBlank { meta.uid }}」的本地凭据（含 PAT）。\n不影响 Qoder 服务端账号本身。",
            okText = "删除",
            danger = true,
        ) {
            QoderAuthStore.remove(this, meta.uid)
            qoderRemaining = null
            render(currentPage)
        }
    }

    // ------------------------------------------------------------------
    // Qoder：领取每日 Credits（campaigns/claim）
    // ------------------------------------------------------------------

    private fun qoderClaimLabel(): String = if (qoderClaimBusy) "领取中…" else "领取每日 Credits"
    private fun qoderBatchClaimLabel(): String =
        if (qoderClaimBusy) "领取中…" else "一键领取全部账号每日 Credits"

    /** 领取当前账号。阻塞操作在后台线程；成功后清余量并让监测器重查。 */
    private fun claimQoderDaily() {
        if (qoderClaimBusy) return
        val uid = QoderAuthStore.activeUid(this) ?: run { toast("没有 Qoder 账号"); return }
        qoderClaimBusy = true
        render(currentPage)
        thread(name = "qoder-claim") {
            // 与定时/补签共用 busy：同一账号 token 轮换，并发续期会互相作废。
            if (!QoderCheckin.busy.compareAndSet(false, true)) {
                runOnUiThread {
                    qoderClaimBusy = false
                    if (isFinishing || isDestroyed) return@runOnUiThread
                    toast("定时/补签正在进行，稍后再试")
                    render(currentPage)
                }
                return@thread
            }
            val r = runCatching { QoderAuthApi.claimDailyForUid(this@MainActivity, uid) }
            QoderCheckin.busy.set(false)
            runOnUiThread {
                qoderClaimBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                r.onSuccess { toast(qoderClaimToast(it)) }
                    .onFailure { e -> toast("领取失败：${e.message}") }
                qoderRemaining = null
                CreditMonitor.stale = true
                render(currentPage)
            }
        }
    }

    /** 领取全部账号。 */
    private fun claimQoderDailyAll() {
        if (qoderClaimBusy) return
        if (QoderAuthStore.list(this).isEmpty()) { toast("没有 Qoder 账号"); return }
        qoderClaimBusy = true
        render(currentPage)
        thread(name = "qoder-claim-all") {
            if (!QoderCheckin.busy.compareAndSet(false, true)) {
                runOnUiThread {
                    qoderClaimBusy = false
                    if (isFinishing || isDestroyed) return@runOnUiThread
                    toast("定时/补签正在进行，稍后再试")
                    render(currentPage)
                }
                return@thread
            }
            val items = QoderAuthApi.claimDailyAll(this@MainActivity)
            QoderCheckin.busy.set(false)
            runOnUiThread {
                qoderClaimBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                toast(qoderBatchClaimToast(items))
                qoderRemaining = null
                CreditMonitor.stale = true
                render(currentPage)
            }
        }
    }

    /** 这个账号今天领到过没有（用于按钮显示「领取 / 已领取」）。 */
    private fun qoderClaimedToday(uid: String): Boolean = QoderCheckin.claimedToday(this, uid)

    /** 只领**一个** Qoder 账号（账号卡上的「领取」按钮用）。 */
    private fun claimQoderForUid(uid: String) {
        if (qoderClaimBusy) return
        qoderClaimBusy = true
        render(currentPage)
        thread(name = "qoder-claim-one") {
            val r = runCatching { QoderAuthApi.claimDailyForUid(this, uid) }
                .getOrElse { QoderAuthApi.QClaimResult("fail", 0, "${it.javaClass.simpleName}: ${it.message}") }
            // ok = 这次领到了；already = 今天已领过 → 两种都算"今天收工"，都要落库，
            // 否则界面下次打开还显示「领取」，用户点了才发现已领取。
            if (r.status == "ok" || r.status == "already") QoderCheckin.markClaimed(this, uid)
            runOnUiThread {
                qoderClaimBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                toast(qoderClaimToast(r))
                qoderRemaining = null
                CreditMonitor.stale = true
                render(currentPage)
            }
        }
    }

    /** 诊断：跑一遍领取链路并把原文摊在弹窗里，可一键复制。 */
    /** Trae 单账号诊断：逐个账号把两个额度接口的原始返回摊出来（不签到、不改状态）。 */
    private fun runTraeDiagnose() {
        val metas = TraeAuthStore.list(this)
        if (metas.isEmpty()) { toast("没有 Trae 账号"); return }
        toast("诊断中…")
        thread(name = "trae-diagnose") {
            val dump = runCatching {
                metas.joinToString("\n" + "─".repeat(30) + "\n") { m ->
                    val cred = TraeAuthStore.loadByUid(this, m.uid)
                    if (cred == null) "uid=${m.uid} 凭据读取失败" else TraeUpstream.diagnose(cred)
                }
            }.getOrElse { "诊断异常：${it.javaClass.simpleName}: ${it.message}" }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                val box = android.widget.TextView(this).apply {
                    setText(dump)
                    typeface = android.graphics.Typeface.MONOSPACE
                    textSize = 10f
                    setTextIsSelectable(true)
                    setPadding(dp(14), dp(12), dp(14), dp(12))
                }
                val scroll = android.widget.ScrollView(this).apply { addView(box) }
                val dlg = android.app.AlertDialog.Builder(this)
                    .setTitle("Trae 账号诊断")
                    .setView(scroll)
                    .setPositiveButton("复制", null)
                    .setNegativeButton("关闭", null)
                    .create()
                dlg.setOnShowListener {
                    dlg.getButton(android.app.AlertDialog.BUTTON_POSITIVE).setOnClickListener {
                        val cm = getSystemService(CLIPBOARD_SERVICE) as android.content.ClipboardManager
                        cm.setPrimaryClip(android.content.ClipData.newPlainText("diagnose", dump))
                        toast("已复制")
                    }
                }
                dlg.show()
            }
        }
    }

    private fun runQoderDiagnose() {
        if (QoderAuthStore.list(this).isEmpty()) { toast("没有 Qoder 账号"); return }
        toast("诊断中…")
        thread(name = "qoder-diagnose") {
            val dump = runCatching { QoderAuthApi.diagnose(this) }
                .getOrElse { "诊断异常：${it.javaClass.simpleName}: ${it.message}" }
            runOnUiThread {
                if (isFinishing || isDestroyed) return@runOnUiThread
                val box = android.widget.TextView(this).apply {
                    setText(dump)
                    typeface = android.graphics.Typeface.MONOSPACE
                    textSize = 10f
                    setTextIsSelectable(true)
                    setPadding(dp(14), dp(12), dp(14), dp(12))
                }
                val scroll = android.widget.ScrollView(this).apply { addView(box) }
                val dlg = android.app.AlertDialog.Builder(this)
                    .setTitle("领取链路诊断")
                    .setView(scroll)
                    .setPositiveButton("复制", null)
                    .setNegativeButton("关闭", null)
                    .create()
                dlg.setOnShowListener {
                    dlg.getButton(android.app.AlertDialog.BUTTON_POSITIVE).setOnClickListener {
                        val cm = getSystemService(CLIPBOARD_SERVICE) as android.content.ClipboardManager
                        cm.setPrimaryClip(android.content.ClipData.newPlainText("diagnose", dump))
                        toast("已复制")
                    }
                }
                dlg.show()
            }
        }
    }

    private fun qoderClaimToast(r: QoderAuthApi.QClaimResult): String = when (r.status) {
        "ok" -> "领取成功 +${r.amount} Credits"
        "already" -> r.message.ifBlank { "今日已领取" }
        "unsupported" -> r.message
        else -> "领取失败：${r.message}"
    }

    private fun qoderBatchClaimToast(items: List<Pair<String, QoderAuthApi.QClaimResult>>): String {
        if (items.isEmpty()) return "没有 Qoder 账号"
        val ok = items.filter { it.second.status == "ok" }
        if (ok.isNotEmpty()) {
            val total = ok.sumOf { it.second.amount }
            return "${ok.size}/${items.size} 个账号领取成功，共 +$total Credits"
        }
        // 全无可领：如有明确失败原因，优先说清楚；否则给统一的「无可领」结论。
        val failed = items.firstOrNull { it.second.status == "fail" }
        return failed?.second?.message ?: "无可领额度（已领或不符）· 共 ${items.size} 个账号"
    }

    /** 定时时刻在常用档位间循环（免输入框，够用即可）。 */
    private fun nextQoderTime(cur: String): String {
        val slots = listOf("08:00", "09:00", "12:00", "20:00", "22:00")
        val i = slots.indexOf(cur)
        return slots[if (i < 0) 0 else (i + 1) % slots.size]
    }

    // ------------------------------------------------------------------
    // 账号导出 / 导入（JSON）
    // ------------------------------------------------------------------

    /** 导出：生成备份 JSON → 弹保存框（SAF 写文件）。写失败会 toast，内容不会丢（留在内存直到本次结果返回）。 */
    private fun exportAccounts() {
        val json = runCatching { AccountBackup.exportAll(this) }
            .getOrElse { toast("导出失败：${it.message}"); return }
        pendingExportJson = json
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "application/json"
            putExtra(Intent.EXTRA_TITLE, "traework2api-accounts.json")
        }
        runCatching { startActivityForResult(intent, REQ_EXPORT) }
            .onFailure {
                // 没有文件选择器时退回剪贴板，至少不空手而归。
                copyToClipboard("accountBackup", json, "未找到保存对话框，已改为复制到剪贴板")
                pendingExportJson = null
            }
    }

    private fun copyAccountsJson() {
        val json = runCatching { AccountBackup.exportAll(this) }
            .getOrElse { toast("导出失败：${it.message}"); return }
        copyToClipboard("accountBackup", json, "账号 JSON 已复制到剪贴板（含明文 token，请妥善保管）")
    }

    private fun importAccounts() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
        }
        runCatching { startActivityForResult(intent, REQ_IMPORT) }
            .onFailure { toast("未找到文件选择器；可用剪贴板方式：先复制 JSON，再…") }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (resultCode != RESULT_OK) return
        val uri = data?.data ?: return
        when (requestCode) {
            REQ_EXPORT -> {
                val json = pendingExportJson
                pendingExportJson = null
                if (json == null) return
                thread(name = "account-export-write") {
                    val ok = runCatching {
                        contentResolver.openOutputStream(uri)?.use { it.write(json.toByteArray(Charsets.UTF_8)) }
                            ?: error("无法打开输出流")
                    }
                    runOnUiThread {
                        if (ok.isSuccess) toast("已导出账号 JSON")
                        else toast("导出失败：${ok.exceptionOrNull()?.message}")
                    }
                }
            }
            REQ_IMPORT -> {
                thread(name = "account-import-read") {
                    val text = runCatching {
                        contentResolver.openInputStream(uri)?.use { it.readBytes().toString(Charsets.UTF_8) }
                            ?: error("无法打开输入流")
                    }
                    val summary = text.getOrNull()?.let { runCatching { AccountBackup.importAll(this, it) }.getOrNull() }
                    val err = text.exceptionOrNull() ?: run {
                        // 读成功但解析失败时给出原因
                        null
                    }
                    runOnUiThread {
                        when {
                            summary != null -> {
                                toast("已导入：Trae ${summary.trae} 个 · Qoder ${summary.qoder} 个")
                                qoderRemaining = null
                                accountBalance = null
                                CreditMonitor.stale = true
                                render(currentPage)
                            }
                            else -> toast("导入失败：JSON 无效或没有可导入的账号${err?.let { "（${it.message}）" } ?: ""}")
                        }
                    }
                }
            }
        }
    }

    /**
     * 「签到」按钮文案：已签/进行中/未开放 时置灰。
     *
     * **必须置灰已签状态**：实测重复 claim 虽不扣积分，但会白发一次请求
     * （上游回 `code=9004`），所以从 UI 上就堵住。
     *
     * `accountCheckin == null` 分两种：真未登录、或已登录但还没拉到状态。
     * 后者若也写「未登录」就是**文案撒谎**（用户明明登录着），故分开措辞。
     * 登录状态由调用方传入（`accountPage()` 已经读过了），这里**不重复解密** KeyStore。
     */
    private fun checkinActionLabel(loggedIn: Boolean): String = when {
        // 一套词，四处（卡片/弹窗/一键签到结果）都用它，别再各写各的
        checkinBusy -> "签到中…"
        !loggedIn -> "未登录"
        accountCheckin == null && authFailedFor(TraeAuthStore.activeUid(this)) -> "需重新登录"
        // 没主动查过就用**本地落库**的事实（补签/手动签到会写），一个请求都不发
        accountCheckin == null -> {
            val uid = TraeAuthStore.activeUid(this)
            if (uid != null && AutoCheckin.doneToday(this, "trae", uid)) "已签到" else "签到"
        }
        accountCheckin?.checkedIn == true -> "已签到"
        accountCheckin?.enable == false -> "无签到权限"
        else -> "签到"
    }

    /**
     * 同步签到按钮的文案与可用性。
     *
     * ⚠️ 可用性判据用**显式 null 判断**，不用 `x?.f != false`：后者在 `x == null` 时
     * 求值为 null，`null != false` 为 true → 读起来像守卫、实际只在非 null 时生效，
     * 后续改动一旦删掉 `!= null` 兜底就会静默放行。这里把判据收成一条显式 null 检查。
     */
    private fun refreshCheckinButton(btn: android.widget.TextView, loggedIn: Boolean) {
        val st = accountCheckin
        val uid = TraeAuthStore.activeUid(this)
        val localDone = uid != null && AutoCheckin.doneToday(this, "trae", uid)
        // 没有远端状态时按**本地落库**决定：已签就不让点，没签允许点（点了才去查/签）
        val actionable = !checkinBusy && when {
            st != null -> !st.checkedIn && st.enable
            else -> uid != null && !localDone && !authFailedFor(uid)
        }
        val done = st?.checkedIn == true || (st == null && localDone)
        btn.text = if (done) "\u2713 " + checkinActionLabel(loggedIn) else checkinActionLabel(loggedIn)
        btn.isEnabled = actionable
        btn.alpha = 1f
        // 三态三种外观，**用颜色和尺寸一起表达语义**，而不是只靠一个 alpha：
        //   可签  = 主色实心按钮（能点，最醒目）
        //   已签  = 缩成 success 小胶囊（是**状态**不是操作，占位小、颜色是"已完成"）
        //   其它  = 浅底灰字（加载中/未登录/未开放，弱化）
        //
        // 原先三种状态都是主色实心、只靠 alpha 0.5 区分，于是**不可点的「今日已签到」
        // 比旁边可点的「刷新」还抢眼**（主次颠倒）。已签还额外占着 48dp 高、5 个字的宽度，
        // 一个纯状态坨在那里像块砖。
        btn.background = when {
            actionable -> shape("primary", RADIUS_CONTROL)
            done -> shape("success_container", RADIUS_PILL)
            else -> shape("surface_variant", RADIUS_PILL)
        }
        btn.setTextColor(color(when {
            actionable -> "on_primary"
            done -> "success"
            else -> "on_surface_variant"
        }))
        // 已签收成一枚小 chip（32dp 高、12sp 粗体）；可签/其它仍是 48dp 主按钮尺寸。
        // 32dp 只用于**不可点**的状态展示 —— 可点时必须 ≥48dp 满足无障碍触控目标下限。
        val h = if (done) 32 else 48
        val pad = if (done) 14 else 18
        btn.minimumHeight = dp(h)
        btn.setPadding(dp(pad), 0, dp(pad), 0)
        btn.textSize = (if (done) FS_CHIP else FS_BODY).toFloat()
        btn.setTypeface(Typeface.DEFAULT, if (done) Typeface.BOLD else Typeface.NORMAL)
    }

    /**
     * 签到按钮右侧的**奖励说明**。
     *
     * 只讲"能领多少"这类按钮文案装不下的补充信息，**不再重复签到状态** ——
     * 按钮本身已经写着「今日已签到」/「签到」/「未开放」，再来一句「已签」就是同义反复。
     *
     * `accountCheckin == null` 返回 null 让调用方显示「—」（没数据就是没数据，不编）。
     */
    private fun checkinRewardLabel(): String? = accountCheckin?.let {
        if (!it.enable) "该账号无签到权限" else "每日可领 +${it.credits}"
    }

    /** 积分数值位：刷新中显示「…」，没拿到显示「—」。 */
    private fun quotaValue(v: String?): String = if (quotaLoading) "…" else v ?: "—"

    /**
     * 账号身份变化时**清掉按账号缓存**的积分/额度/签到状态。
     *
     * 这三个字段是"当前账号"的快照，跟账号走。不清的话切号/删除/登录新号后，
     * 新账号会**继承上一个账号的签到状态** —— 表现就是"明明没签到却显示今日已签到"
     * （同一个误报的另一个入口，与判据无关，是缓存没失效）。
     *
     * 只清缓存、不主动拉取：调用方在页面重建后会经 [render] 的自动加载逻辑补拉，
     * 保持"一个动作只打一次上游"。
     */
    private fun invalidateAccountQuota() {
        accountBalance = null
        accountCheckin = null
        // 注意：**不清 authFailedUid** —— 它按 uid 记，是"那个账号的事实"，
        // 切走再切回来仍应保留（否则要重新撞一次 401 才知道）。
    }

    /** Token 是否需要续期：过期则需重新登录，临近则由后台自动用 refresh 续期。 */
    private fun tokenRenewLabel(uid: String, expiresAt: Long): String {
        // 只看到期时间会说谎：token 可能"没过期但已被服务端拒绝"。
        // 有实测证据（查额度/签到撞 401/1001）时以证据为准 —— 但证据必须是**这个账号**的。
        if (authFailedFor(uid)) return "需重新登录"
        if (expiresAt <= 0L) return "未知（无到期信息）"
        val remainSec = expiresAt - System.currentTimeMillis() / 1000
        return if (remainSec <= 0) "已过期 · 需重新登录"
        else if (remainSec <= SIXTY_MINUTES_SEC) "临近到期 · 将自动续期"
        else "有效"
    }

    // ------------------------------------------------------------------
    // 模型
    // ------------------------------------------------------------------

    private fun modelsPage(): View = scroll {
        // 走同一个 scroll{} 容器：原先这里自己抄了一遍内边距，改 scroll 时会漏掉它
        val edit = searchInput("搜索模型…")
        addView(edit, lp(-1, -2))

        // 刷新按钮 + 计数。计数是"刷新真的起作用了"的证据 —— 否则点了没新增时用户会以为没生效。
        val countView = caption("")
        fun refreshCountLabel() {
            // 只报**当前所选平台真实能用的条数**：以前会额外写"（Trae 12 + Qoder 15，按模型路由）"
            // 这种括号说明，数字和平台对不上时反而更迷惑。
            val n = GatewayBackend.modelsFor(this@MainActivity).length()
            countView.text = "已加载 · $n 个模型"
        }
        val refreshBtn = action(REFRESH_MODELS_LABEL) { refreshModels() }
        // 刷新中：按钮置灰 + 文案变「刷新中…」（照抄记录页/积分刷新那套，别新造轮子）
        fun refreshButtonLabel() {
            refreshBtn.isEnabled = !modelsLoading
            refreshBtn.text = if (modelsLoading) "刷新中…" else REFRESH_MODELS_LABEL
        }
        // 工具条（计数 + 按钮）：只在"已加载"时显示 —— 空状态下列表还没有，
        // 顶上摆一个计数「已加载 · 0 个模型」纯属添乱，刷新按钮统一放到空状态卡里。
        val toolbar = row(top = GAP) {
            addView(refreshBtn)
            addView(countView, weighted(start = GAP))
        }
        addView(toolbar)

        // 未加载：整页只有一张说明卡 + 刷新按钮（用户要的"空状态"，见 CHANGELOG 0.4.3）
        // 空状态只留标题 + 一个刷新按钮：列表区不摆任何"怎么添加/怎么导入"的提示
        val notLoaded = card {
            addView(text("模型目录尚未加载", FS_CARD_TITLE, true))
            addView(action(REFRESH_MODELS_LABEL, full = true) { refreshModels() }, top(12))
        }
        addView(notLoaded)

        // 搜不到时的空态。没有它，用户打错一个字就是**整页空白**，会以为 App 坏了。
        val empty = card(tone = CardTone.SOFT) {
            addView(caption("没有匹配的模型。换个关键词，或清空搜索框看看全部。"))
        }.apply { visibility = View.GONE }
        addView(empty)

        // 卡片容器：刷新时**只清空并重塞它的子项**，不动搜索框/按钮/空态（原地更新，滚动位置不丢）
        val cardHost = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }
        addView(cardHost)

        // Qoder 后端首次进本页：自动拉一次官方目录（含倍率 + 内置清单没有的免费模型）。
        // 每进程只试一次：拉不到就安静地用内置清单，不要每次切页都打上游。
        if (GatewayBackend.hasQoderAccount(this@MainActivity) && !qoderCatalogAutoTried) {
            qoderCatalogAutoTried = true
            syncQoderCatalog(quiet = true)
        }

        // 过滤逻辑收成一个函数：刷新重塞后也要重跑一次，否则过滤会作用在已失效的旧 View 上
        var cards = HashMap<String, View>()
        fun applyFilter() {
            val f = edit.text.toString().trim()
            cards.forEach { (key, v) -> v.visibility = if (f.isBlank() || key.contains(f, true)) View.VISIBLE else View.GONE }
            empty.visibility = if (f.isNotBlank() && cards.values.none { it.visibility == View.VISIBLE }) View.VISIBLE else View.GONE
        }
        fun rebuildCards() {
            cardHost.removeAllViews()
            cards = buildModelCards(cardHost)
            applyFilter()
        }
        /** 未加载时藏起搜索框/工具条/列表，只留说明卡；加载后反过来。 */
        fun applyLoadedState() {
            val loaded = GatewayBackend.modelsLoaded(this@MainActivity)
            notLoaded.visibility = if (loaded) View.GONE else View.VISIBLE
            edit.visibility = if (loaded) View.VISIBLE else View.GONE
            toolbar.visibility = if (loaded) View.VISIBLE else View.GONE
            cardHost.visibility = if (loaded) View.VISIBLE else View.GONE
            if (!loaded) empty.visibility = View.GONE
        }

        // 客户端过滤：改可见性而非重建容器，避免丢失输入框焦点
        edit.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: android.text.Editable?) = applyFilter()
        })

        // 刷新回调交给 build 完之后的代码用：本页重建后句柄要跟着换，不能留旧的
        modelsRefreshApply = {
            rebuildCards()
            refreshCountLabel()
            refreshButtonLabel()
            applyLoadedState()
        }
        rebuildCards()
        refreshCountLabel()
        refreshButtonLabel()
        applyLoadedState()
    }

    /** 用当前内存模型表构建卡片；返回 (匹配词→View) 供过滤。 */
    private fun buildModelCards(container: LinearLayout): HashMap<String, View> {
        val cards = HashMap<String, View>()
        val arr = GatewayBackend.modelsFor(this)
        for (i in 0 until arr.length()) {
            val m = arr.optJSONObject(i) ?: continue
            val id = m.optString("id", "")
            val c = modelCard(m)
            cards[id] = c
            container.addView(c, top(GAP))
        }
        return cards
    }

    private fun modelCard(m: org.json.JSONObject): View = card {
        val id = m.optString("id", "").ifBlank { m.optString("config_name", "") }
        val name = m.optString("name", "").ifBlank { id }
        // `id` 现在是官方显示名；副行展示内部 key（排查/兼容老客户端时要用）
        val key = m.optString("key", "").ifBlank { id }
        // 官方标注的倍率：0 = 免费（Qoder 目录里确实有这种）。
        // 缺字段时**不显示**而不是显示 ×1 —— 显示错误的倍率比不显示更误导。
        val pf = if (m.has("price_factor")) m.optDouble("price_factor", 1.0) else null
        val unverified = m.optBoolean("unverified", false)
        addView(row {
            addView(text(name, FS_CARD_TITLE, true), weighted())
            if (pf != null) {
                if (pf <= 0.0) addView(chipOk("免费"))
                else addView(chipInfo("×" + priceText(pf)))
            } else {
                addView(chipInfo("倍率未知"))
            }
        })
        addView(row(4) {
            addView(mono(key, FS_CHIP, colorName = "on_surface_variant").apply {
                maxLines = 1
                ellipsize = android.text.TextUtils.TruncateAt.END
            }, weighted())
            addView(linkAction("复制") { copyModelId(id) })
        })
        // ⚠️ 必须显式 this@MainActivity：这里是 card { } 的 LinearLayout 接收者作用域，
        //    裸 this 是容器不是 Context，编译期直接报 argument type mismatch。
        // 并集模式下每条模型自带来源（merge 时补的 owned_by）；单后端时按当前后端标
        val backendTag = m.optString("owned_by", "").ifBlank {
            if (GatewayBackend.isQoder(this@MainActivity)) "qoder" else "trae-solo"
        }
        val pfText = if (pf == null) "倍率未知" else if (pf <= 0.0) "免费（0 倍率）" else "倍率 ×${priceText(pf)}"
        val keyNote = if (unverified) " · key 未验证" else ""
        addView(caption("context ${m.optInt("context_length", 0)} · $pfText · $backendTag$keyNote"), top(4))
    }

    private fun copyModelId(id: String) = copyToClipboard("modelId", id, "已复制模型 ID: $id")

    // ------------------------------------------------------------------
    // 记录
    // ------------------------------------------------------------------

    private fun recordsPage(): View = scroll {
        val records = UsageStore.all(this@MainActivity)
        if (records.isEmpty()) {
            addView(section("消耗汇总"))
            addView(card(tone = CardTone.SOFT) { addView(caption("暂无记录 —— 发一次聊天请求后这里会显示用量与汇总")) })
            return@scroll
        }
        addView(card {
            addView(text("模型消耗汇总", FS_CARD_TITLE, true))
            val groups = UsageStore.aggregate(this@MainActivity)
            // 条形按**相对最大的那组**画（不是占总量）：模型一多的话每条都只剩一小截，反而看不出差别
            val max = groups.maxOfOrNull { it.total } ?: 0L
            for (g in groups) {
                addView(row(8) {
                    addView(text(g.model.ifBlank { "?" }, FS_VALUE, true), weighted())
                    addView(mono("总 ${g.total} · 输出 ${g.output}", FS_CAPTION, colorName = "on_surface_variant"))
                }, top(12))
                addView(shareBar(g.total, max), top(6))
            }
        })
        addView(section("调用记录（最近 ${records.size} 条）"))
        for (r in records.asReversed()) addView(recordCard(r))
    }

    private fun recordCard(r: UsageStore.UsageRecord): View = card {
        addView(row {
            addView(if (r.ok) statusChip("OK", "success", true) else statusChip("ERR", "error", false))
            addView(text(r.model.ifBlank { "?" }, FS_VALUE), weighted(start = 10))
            // 时间戳用等宽字体，一列扫下来是对齐的
            addView(mono(SimpleDateFormat("HH:mm:ss", Locale.getDefault()).format(Date(r.ts)), FS_CHIP,
                colorName = "on_surface_variant"))
        })
        val detail = StringBuilder()
            .append("in ${r.prompt} · out ${r.completion} · 总 ${r.total} · ${r.durationMs}ms")
        if (r.errorType.isNotBlank()) detail.append(" · ").append(r.errorType)
        addView(caption(detail.toString()), top(6))
    }

    // ------------------------------------------------------------------
    // 更多
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // 保活设置（二级页）
    // ------------------------------------------------------------------

    /**
     * 保活设置页。
     *
     * 刻意把「能一键的」和「只能自己找的」分开摆：安卓**没有**打开厂商私有页面
     * （自启动 / 省电策略 / 后台弹出）的公开接口，硬装成"一键全搞定"只会让用户
     * 点完以为好了、实际仍被杀。所以能跳的给按钮，跳不了的老实给路径。
     */
    private fun keepAlivePage(): View = scroll {
        val items = KeepAlive.check(this@MainActivity)
        val pending = items.filter { !it.ok }
        val rom = KeepAlive.rom()

        // 顶部总状态（首卡不留上间距，紧跟页面顶部）
        addView(card(topMargin = 0) {
            addView(row {
                addView(text(if (pending.isEmpty()) "保活已就绪" else "还有 ${pending.size} 项待开启",
                    FS_CARD_TITLE, true), weighted())
                addView(if (pending.isEmpty()) chipOk("✓ 全部正常") else chipWarn("⚠ 需要处理"))
            })
            addView(caption(if (pending.isEmpty())
                "网关已能在后台长期运行，切到别的 App、息屏都不会断。"
            else
                "下面标 ⚠ 的项会让服务在真机上被系统清掉，建议逐项开启。"), top(8))
        })

        // 一键配置
        val autoCount = pending.count { it.autoOpenable }
        addView(card {
            addView(text("一键配置", FS_CARD_TITLE, true))
            addView(caption(if (autoCount == 0)
                "能自动跳转的项都已就绪。"
            else
                "有 $autoCount 项可以自动打开系统页面，你点「允许」即可。"), top(8))
            addView(action("开始自动配置${if (autoCount > 0) "（$autoCount 项）" else ""}",
                full = true) { runAutoConfig() }, top(12))
        })

        // 逐项清单
        addView(section("逐项检查"))
        addView(card {
            items.forEachIndexed { i, it ->
                if (i > 0) addView(divider(), top(12))
                addView(row(top = if (i > 0) 12 else 0) {
                    addView(text(if (it.ok) "✓" else "⚠", FS_BODY, true,
                        if (it.ok) "success" else "warning"), lp(-2, -2, end = 8))
                    addView(text(it.title, FS_VALUE, true), weighted())
                    if (!it.ok) addView(if (it.autoOpenable) chipInfo("可一键") else chipWarn("需手动"))
                })
                addView(caption(it.why), top(4))
                if (!it.ok && it.autoOpenable) {
                    addView(action("去开启", outlined = true) { openAutoItem(it.title) }, top(8))
                }
            }
        })

        // 厂商私有路径（系统查不到、跳不了）
        addView(section("本机需要手动确认的项"))
        addView(card {
            addView(text("检测到：${rom.label}", FS_CARD_TITLE, true))
            addView(caption("以下页面系统不开放跳转，需要你按路径自己找一下。"), top(6))
            KeepAlive.romGuide(rom).forEach { line ->
                addView(caption("• $line"), top(6))
            }
            addView(action("复制这些路径", outlined = true) {
                copyToClipboard("keepAliveGuide", KeepAlive.romGuide(rom).joinToString("\n"),
                    "路径已复制，可到系统设置里照着找")
            }, top(12))
        })
    }

    /** 「一键配置」：按顺序跳到**第一个**未就绪且可自动打开的项。 */
    private fun runAutoConfig() {
        val next = KeepAlive.check(this).firstOrNull { !it.ok && it.autoOpenable }
        if (next == null) {
            toast("能自动配置的项都已就绪")
            return
        }
        // 一次只跳一个：连着弹多个系统页面会互相顶掉，用户也来不及看。
        openAutoItem(next.title)
    }

    private fun openAutoItem(title: String) {
        val ok = when (title) {
            "电池优化白名单" -> KeepAlive.openBatteryWhitelist(this)
            "精确闹钟" -> KeepAlive.openExactAlarm(this)
            "通知权限", "通知渠道未被关闭" -> KeepAlive.openAppNotificationSettings(this)
            else -> false
        }
        if (!ok) {
            toast("这个页面在本机打不开，请按下方路径手动设置")
        }
        // 用户可能去系统页面勾选后返回，回来时重绘一次以便显示最新状态。
        // 界面若已销毁就不要再重建视图树（用户此时在系统设置页，回来也不会看这个实例）。
        contentHost.postDelayed({
            if (isFinishing || isDestroyed) return@postDelayed
            if (showKeepAlive) openKeepAlive()
        }, 800)
    }

    // ------------------------------------------------------------------
    // 外观风格（二级页）
    // ------------------------------------------------------------------

    /**
     * 「外观风格」页：风格 + 亮暗两组，全部用**当前风格实时渲染**（选中项主色描边 + ✓）。
     *
     * 点一下即持久化并重建生效 —— 颜色/圆角/间距散在几十个控件上（还有 chip、矢量图标
     * tint、间距），逐个回刷既长又容易漏；重建则一定是完整、正确的一套。
     */
    private fun stylePage(): View = scroll {
        addView(section("风格"))
        AppStyle.STYLES.forEach { s -> addView(styleOption(s), top(GAP)) }

        addView(section("亮暗模式"))
        addView(card {
            AppStyle.Mode.entries.forEachIndexed { i, m ->
                if (i > 0) addView(divider(), top(12))
                addView(modeOption(m), top(if (i > 0) 12 else 0))
            }
        })
        addView(caption("切换后立即生效，并会记住你的选择；重开 App 仍是这套。"), top(12))
    }

    /** 风格卡：左侧色样 + 名称说明，选中项 = 主色 2dp 描边 + ✓ 标记。 */
    private fun styleOption(s: AppStyle.Style): LinearLayout {
        val selected = s.id == AppStyle.current.id
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(CARD_PAD), dp(14), dp(CARD_PAD), dp(14))
            // 选中用主色加粗描边：不靠颜色单独传达（色盲也能从描边粗细 + ✓ 分辨）
            background = shape("surface", RADIUS_CARD,
                if (selected) "primary" else "outline_variant", if (selected) 2 else 1)
            elevation = dp(AppStyle.current.elevation + (if (selected) 1 else 0)).toFloat()
            addView(styleSwatch(s), lp(dp(48), dp(48)))
            addView(LinearLayout(this@MainActivity).apply {
                orientation = LinearLayout.VERTICAL
                addView(text(s.label, FS_CARD_TITLE, true, if (selected) "primary" else "on_surface"))
                addView(caption(s.note), top(3))
            }, weighted(start = 14))
            if (selected) addView(chipInfo("✓ 使用中"), lp(-2, -2, start = 10))
            minimumHeight = dp(56)   // 触控目标 ≥ 48dp
            setOnClickListener { switchStyle(s) }
        }
    }

    /**
     * 色样：用**该风格自己**的主色与圆角画一个小方块，内部一条 on_primary 短胶囊。
     * 不这么做的话，没被选中的风格只能靠文字去猜它长什么样。
     */
    private fun styleSwatch(s: AppStyle.Style): View = FrameLayout(this).apply {
        val dark = AppStyle.isNight
        background = GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            setColor(AppStyle.previewColor(s, "primary", dark))
            // 圆角上限 = 方块边长的一半（24dp）：让"方正"与"大圆角"在色样上就分得出来
            cornerRadius = dp(minOf(s.cardRadius, 24f)).toFloat()
            setStroke(dp(1), AppStyle.previewColor(s, "outline_variant", dark))
        }
        addView(View(this@MainActivity).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.RECTANGLE
                setColor(AppStyle.previewColor(s, "on_primary", dark))
                cornerRadius = dp(minOf(s.controlRadius, 8f)).toFloat()
            }
        }, FrameLayout.LayoutParams(dp(24), dp(7), Gravity.CENTER))
    }

    /** 亮暗模式选项：整行可点（高 ≥ 52dp），右侧圆形单选标记。 */
    private fun modeOption(m: AppStyle.Mode): LinearLayout {
        val selected = m.id == AppStyle.mode.id
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(52)
            addView(LinearLayout(this@MainActivity).apply {
                orientation = LinearLayout.VERTICAL
                addView(text(m.label, FS_BODY, true, if (selected) "primary" else "on_surface"))
                addView(caption(m.note), top(3))
            }, weighted())
            addView(radioDot(selected), lp(dp(22), dp(22), start = 12))
            setOnClickListener { switchMode(m) }
        }
    }

    /** 单选圆点：选中 = 主色实心 + 内圈 on_primary 小点；未选中 = 描边空心。 */
    private fun radioDot(selected: Boolean): View = FrameLayout(this).apply {
        background = if (selected) shape("primary", RADIUS_PILL) else shape("surface", RADIUS_PILL, "outline")
        if (selected) addView(View(this@MainActivity).apply {
            background = shape("on_primary", RADIUS_PILL)
        }, FrameLayout.LayoutParams(dp(8), dp(8), Gravity.CENTER))
    }

    /** 切风格：持久化 → 提示 → 重建。 */
    private fun switchStyle(s: AppStyle.Style) {
        if (s.id == AppStyle.current.id) return
        AppStyle.setStyle(this, s.id)
        toast("已切换为「${s.label}」")
        recreate()
    }

    /** 切亮暗：持久化 → 提示 → 重建（重建会重新走 attachBaseContext 覆写 uiMode）。 */
    private fun switchMode(m: AppStyle.Mode) {
        if (m.id == AppStyle.mode.id) return
        AppStyle.setMode(this, m)
        toast("亮暗模式：${m.label}")
        recreate()
    }

    // ------------------------------------------------------------------
    // 底层数据 / 生命周期（保持原逻辑）
    // ------------------------------------------------------------------

    /**
     * 「去登录」入口：跳到账号页并直接复制国服登录链接。
     *
     * 原先是打开内置 WebView 登录窗（[LoginWebActivity]），**已整体移除**：
     * 那条路依赖页面里的 JS 回调，第三方 SDK 一超时就等不到回调、且部分网络下必失败；
     * 而「浏览器 + 粘贴回调」是不依赖 WebView/页面 JS 的实测可用路径，所以现在只留它。
     */
    private fun openLogin(region: TraeRegion = TraeRegion.current) {
        // **无条件重绘**：原先写成 `if (currentPage != Page.ACCOUNT)`，于是用户已经停在账号页时
        // 点「立即登录」/「复制国服登录链接」什么都不会重建 —— 页面继续显示点之前那份
        // 「未登录 / 加载中…」的快照，刚做完的动作在界面上没有回音。
        currentPage = Page.ACCOUNT
        render(Page.ACCOUNT)
        copyLoginUrl(region)
    }

    // ------------------------------------------------------------------
    // 确认弹窗（统一样式）
    // ------------------------------------------------------------------

    /**
     * 统一风格的确认弹窗。
     *
     * **为什么不直接用 `AlertDialog.Builder(this).setTitle(…).setMessage(…)`**：
     * 那个走平台主题，配色来自 `colorAccent`（写死在 `values/colors.xml` 的**靛蓝**），
     * 所以切到苔绿/落日/石墨之后，弹窗还是靛蓝高亮 + 系统灰底，和 App 完全脱节。
     * 这里标题/正文/按钮全部自己画，窗口底也换成当前风格的 `surface` + 圆角。
     *
     * @param danger 破坏性操作（删除），确定键走 `error` 色描边
     * @param content 需要额外控件时传进来（如粘贴回调的输入框）
     */
    private fun confirmDialog(
        title: String,
        message: String,
        okText: String,
        cancelText: String? = "取消",
        danger: Boolean = false,
        content: View? = null,
        onOk: () -> Unit,
    ): AlertDialog {
        var dialog: AlertDialog? = null
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(CARD_PAD), dp(CARD_PAD), dp(CARD_PAD), dp(CARD_PAD))
            addView(text(title, FS_CARD_TITLE, true))
            addView(text(message, FS_VALUE, false, "on_surface_variant"), top(8))
            content?.let { addView(it, top(14)) }
            addView(row(8) {
                if (cancelText != null) {
                    addView(action(cancelText, outlined = true) { dialog?.dismiss() }, weighted())
                }
                addView(action(okText, danger = danger) {
                    dialog?.dismiss()
                    onOk()
                }, weighted(start = if (cancelText != null) 8 else 0))
            }, top(18))
        }
        val dlg = AlertDialog.Builder(this).setView(panel).create()
        dialog = dlg
        // 窗口底换成当前风格的 surface + 统一圆角，替掉平台那层灰底
        dlg.setOnShowListener { dlg.window?.setBackgroundDrawable(shape("surface", RADIUS_CARD)) }
        dlg.show()
        return dlg
    }

    /**
     * 复制指定区域的登录链接，供用户拿到系统浏览器里打开。
     * 同时把 [TraeRegion.current] 切到该区域——粘贴回调时要按同一区域换 token。
     */
    private fun copyLoginUrl(region: TraeRegion) {
        if (!region.canLogin) { toast("${region.id} 区域常量未配置完整"); return }
        TraeRegion.set(this, region)
        val url = TraeAuthApi.buildLoginUrl()
        copyToClipboard("traeLoginUrl", url, "${regionLabel(region)}登录链接已复制，请用系统浏览器打开并登录")
    }

    /**
     * 切到指定账号：改 active uid，并把**区域切成该账号自己的区域**（转发才会打对 host），
     * 然后重新载入 token（必要时续期）并重绘。
     */
    private fun switchAccount(uid: String) {
        val meta = TraeAuthStore.list(this).firstOrNull { it.uid == uid } ?: return
        // 切到**未开发区域**的账号会让转发走进半残链路（无多轮/无外部工具），先确认。
        if (!meta.region.isSupported) {
            confirmDialog(
                title = "该账号属未开发区域",
                message = "${meta.nickname.ifBlank { uid }}（${regionLabel(meta.region)}）\n\n" +
                    TraeRegion.SG.unsupportedNote + "\n\n仍然切换吗？",
                okText = "仍要切换",
                cancelText = "取消",
            ) { doSwitchAccount(meta) }
            return
        }
        doSwitchAccount(meta)
    }

    /** 真正的切号动作（[switchAccount] 确认后调用）。 */
    private fun doSwitchAccount(meta: TraeAuthStore.AccountMeta) {
        TraeAuthStore.setActive(this, meta.uid)
        // 区域以账号为准并持久化（复用同一处逻辑，别在这里再抄一遍）
        TraeAuthStore.syncRegionToActiveAccount(this)
        // 切号 = 换了身份：上一个账号的积分/签到状态必须作废，
        // 否则新账号会继承"已签到"（这是误报的另一个入口）
        invalidateAccountQuota()
        Toast.makeText(this, "已切换到 ${meta.nickname.ifBlank { meta.uid }}", Toast.LENGTH_SHORT).show()
        loadAndRefreshToken()
    }

    /** 删除账号（二次确认）：连密文一起删，避免残留凭据。 */
    private fun confirmRemoveAccount(meta: TraeAuthStore.AccountMeta) {
        confirmDialog(
            title = "删除账号",
            message = "将删除「${meta.nickname.ifBlank { meta.uid }}」的本地凭据（含加密 token）。\n" +
                "不影响 Trae 服务端的账号本身。",
            okText = "删除",
            danger = true,
        ) {
            TraeAuthStore.remove(this, meta.uid)
            // 删掉的若是当前账号，缓存必须作废（下一个激活账号会不一样）
            invalidateAccountQuota()
            toast("已删除")
            render(currentPage)
        }
    }

    private fun regionLabel(region: TraeRegion): String =
        if (region == TraeRegion.SG) "国际服" else "国服"

    /** 带「未开发」后缀的区域名（用于详情展示，一眼看出该区域能不能用）。 */
    private fun regionLabelFull(region: TraeRegion): String =
        regionLabel(region) + if (region.isSupported) "" else "（未开发）"

    /**
     * 粘贴回调链接完成登录。后台换 token，成功后刷新界面。
     * 注意：回调链接里含 refreshToken（敏感），这里只在内存里传递，不打印、不落盘明文。
     */
    private fun promptPasteCallback() {
        val edit = EditText(this).apply {
            hint = "粘贴登录后跳转的完整链接"
            isSingleLine = false
            maxLines = 4
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE
            setTextColor(color("on_surface"))
            setHintTextColor(color("on_surface_variant"))
            // 跟全站的输入框同一套外观，别用平台那根下划线
            background = shape("surface_container", RADIUS_CONTROL)
            setPadding(dp(14), dp(12), dp(14), dp(12))
        }
        confirmDialog(
            title = "粘贴回调链接",
            message = "用系统浏览器打开登录链接登录后，把地址栏里那串 " +
                "http://127.0.0.1:18080/authorize?... 完整粘进来。",
            okText = "登录",
            content = edit,
        ) {
            val url = edit.text?.toString().orEmpty()
            if (url.isBlank()) { toast("没有粘贴内容，请重新打开本窗口"); return@confirmDialog }
            toast("正在换取 token…")
            thread(name = "trae-paste-login") {
                val result = runCatching { TraeAuthApi.loginWithCallbackUrl(this, url) }
                runOnUiThread {
                    result.onSuccess { cred ->
                        toast("登录成功：${cred.nickname.ifBlank { cred.uid }}")
                        AutoCheckin.requestRunSoon(this)
                        // 新登录的账号可能不是原来那个：缓存作废，否则会显示上一个账号的签到状态
                        invalidateAccountQuota()
                        loadAndRefreshToken()
                    }.onFailure { e ->
                        toast("登录失败：${e.message}")
                        android.util.Log.w("TraeLogin", "粘贴登录失败", e)
                    }
                }
            }
        }
    }

    private fun copyApiKey() = copyToClipboard("apiKey", apiKey, "已复制 API Key")

    /** 复制某个端点的完整地址（提示里把完整 URL 也念出来，用户才知道复制到了什么）。 */
    private fun copyEndpoint(path: String) {
        val url = "http://127.0.0.1:${LocalFakeServer.PORT}$path"
        copyToClipboard("endpoint", url, "已复制 $url")
    }

    private fun copyServerUrl() {
        val url = "http://127.0.0.1:${LocalFakeServer.PORT}/v1"
        copyToClipboard("serverUrl", url, "已复制 $url")
    }

    private fun copyLanUrl(lan: String) {
        val url = "http://$lan:${LocalFakeServer.PORT}/v1"
        copyToClipboard("lanUrl", url, "已复制 $url")
    }

    /**
     * 取本机局域网 IPv4：优先 site-local（10./172.16-31./192.168.），
     * 没有再退到 link-local（169.254.，DHCP 失败时的地址 —— 官方修复点：别把它误过滤掉）。
     * hostAddress 可能带 `%wlan0` scope 后缀，会污染 URL，一律剥掉。检测不到返回 null。
     */
    private fun lanAddress(): String? {
        return try {
            var linkLocal: String? = null
            val ifs = java.net.NetworkInterface.getNetworkInterfaces()
            while (ifs.hasMoreElements()) {
                val nif = ifs.nextElement()
                if (!nif.isUp || nif.isLoopback) continue
                val addrs = nif.inetAddresses
                while (addrs.hasMoreElements()) {
                    val a = addrs.nextElement()
                    if (a !is java.net.Inet4Address || a.isLoopbackAddress) continue
                    val host = a.hostAddress?.substringBefore('%') ?: continue
                    if (a.isSiteLocalAddress) return host
                    if (host.startsWith("169.254.") && linkLocal == null) linkLocal = host
                }
            }
            linkLocal
        } catch (_: Exception) { null }
    }

    /**
     * 版权信息行：`标签 | 值`，标签列与「版本」等 labelValue 统一走 `minWidth 96dp`，
     * 所以「作者 / 联系邮箱」两行的值**左边界严格对齐** —— 这正是"两行字"和
     * "一张排版工整的小表"的区别。邮箱用等宽体：字母数字对齐、`@` 与 `.` 不会被
     * 比例字体挤在一起，读串的概率低一截。
     */
    private fun copyToClipboard(label: String, value: String, tip: String) {
        (getSystemService(CLIPBOARD_SERVICE) as ClipboardManager)
            .setPrimaryClip(ClipData.newPlainText(label, value))
        toast(tip)
    }

    /** 拉积分/额度（后台线程，用解密后的真实 token 打 ug 接口），成功后重绘账号页。 */
    private fun refreshQuota() {
        val cred = TraeAuthStore.load(this)
        if (cred == null || cred.accessToken.isBlank()) { toast("未登录 Trae，请先登录"); return }
        if (quotaLoading) return
        // 数值位立刻变「…」：网络往返有几百毫秒，不给反馈用户会以为没点动。
        // 原地改控件而不是 redraw() —— 重建页面会把滚动位置弹回页首，用户就看不见这张卡了。
        quotaLoading = true
        quotaApply?.invoke()
        thread(name = "trae-fetch-quota") {
            // runCatching 同 refreshModels：抛异常会让线程在复位前死掉，`quotaLoading`
            // 卡 true → 按钮永远「刷新中…」且不再响应点击。
            val pair = runCatching {
                TraeUpstream.fetchUsage(cred.accessToken, cred.uid) to
                    TraeUpstream.fetchCheckinHealing(this, cred)
            }.getOrNull()
            val u = pair?.first
            val c = pair?.second
            runOnUiThread {
                if (isFinishing || isDestroyed) { quotaLoading = false; return@runOnUiThread }
                // 拉失败时**保留旧值**（与 claimCheckin 一致）：网络抖一下就把已显示的
                // 积分/签到状态擦回「—」是净损失，用户会以为数据丢了。
                accountBalance = u ?: accountBalance
                accountCheckin = c ?: accountCheckin
                // 按**这个账号**记：成功就清掉它的标记，失败且是鉴权问题才置上
                if (c != null) {
                    if (authFailedUid == cred.uid) authFailedUid = null
                } else if (TraeUpstream.lastAuthFailed) {
                    authFailedUid = cred.uid
                }
                quotaLoading = false
                toast(if (u == null) "未获取到额度" else "已更新")
                quotaApply?.invoke()
            }
        }
    }

    /**
     * 「签到」：领取今日积分。
     *
     * 流程照 reference `scheduler.go:100` 的顺序：**先看状态再决定发不发 claim** ——
     * 已签 / 账号未开放签到时不发请求（实测重复 claim 会白发一次并回 `code=9004`）。
     *
     * ⚠️ **成功判据是"成功码 AND 状态跃迁"的合取**（见 [TraeUpstream.checkinSucceeded]）。
     * 两个单一判据都会被骗，别退回任一个：
     *  - 旧写法 `when (code) { 0 -> "签到成功" }`：`ug()` 在 HTTP 200 + 业务拒绝时
     *    也会返回带 `code` 的 JSON，于是被拒也报成功 —— 这就是"没签到却提示签到成功"。
     *  - 只看状态跃迁：卡片状态陈旧时（显示"未签"、今天其实早已签过），
     *    `code=9004` + 重查已签会被误报成本次成功，而积分并不会变。
     * 合取之后 `code=9004` 正确落到「今日已签到」，不再冒充本次成功。
     *
     * ⚠️ **状态未知时必须先查再决定**：旧守卫写的是 `accountCheckin?.enable == false`，
     * 而 `?.` 在 null 时求值为 null，`null == false` 是 false → **守卫被静默绕过**，
     * 于是"状态未知"也会直接打 claim。这里用 [known] 显式补查一次堵掉。
     *
     * 签到成功后**重查一次**积分：用户最想看的就是"签完变成多少"。
     */
    private fun claimCheckin() {
        val cred = TraeAuthStore.load(this)
        if (cred == null || cred.accessToken.isBlank()) { toast("未登录 Trae，请先登录"); return }
        if (checkinBusy) return
        val before = accountCheckin
        if (before?.checkedIn == true) { toast("已签到"); return }
        if (before?.enable == false) { toast("该账号无签到权限"); return }
        checkinBusy = true
        quotaApply?.invoke()          // 按钮立刻变「签到中…」
        thread(name = "trae-checkin") {
            // 整段包 try/finally：`checkinBusy` 只在主线程复位，若中途抛异常（KeyStore 解密、
            // 网络层），线程死掉后标志永远为 true → 签到按钮永久卡在「签到中…」。
            val result = try {
                // 状态未知（before == null）先补查：不能在不知道 enable/checkedIn 的情况下打 claim
                val known = before ?: TraeUpstream.fetchCheckinHealing(this, cred)
                if (known?.checkedIn == true) {
                    Triple(known, null, null as Long?)
                } else if (known?.enable == false) {
                    Triple(known, null, null as Long?)
                } else {
                    val code = TraeUpstream.claimCheckinHealing(this, cred)
                    // 签到后重查积分与状态：拿到最新余额，同时把按钮切成「今日已签到」
                    val u = TraeUpstream.fetchUsage(cred.accessToken, cred.uid)
                    val after = TraeUpstream.fetchCheckinHealing(this, cred)
                    Triple(known, Triple(code, after, u), null)
                }
            } catch (e: Exception) {
                android.util.Log.w("MainActivity", "签到异常: ${e.javaClass.simpleName}: ${e.message}")
                Triple(before, null, null as Long?)
            }
            val known = result.first
            val tail = result.second
            runOnUiThread {
                checkinBusy = false
                if (isFinishing || isDestroyed) return@runOnUiThread
                if (tail == null) {
                    accountCheckin = known ?: accountCheckin
                    // 落库：本地记下"今天这个号已处理"，下次打开 App 不用请求就能显示「已签到」
                    val uid = cred.uid
                    if (known?.checkedIn == true) AutoCheckin.markDone(this, "trae:$uid")
                    else if (known?.enable == false) AutoCheckin.markDone(this, "trae:$uid")
                    toast(if (known?.enable == false) "该账号无签到权限" else "已签到")
                    quotaApply?.invoke()
                    return@runOnUiThread
                }
                val (code, after, u) = tail
                accountBalance = u ?: accountBalance
                accountCheckin = after ?: accountCheckin
                // 签到成功 / 已签 → 落库，界面与自动补签共用同一份事实
                if (after?.checkedIn == true || TraeUpstream.checkinSucceeded(known, after, code)) {
                    AutoCheckin.markDone(this, "trae:${cred.uid}")
                }
                toast(
                    when {
                        TraeUpstream.checkinSucceeded(known, after, code) -> "签到成功"
                        after?.checkedIn == true -> "已签到"
                        // 别把"token 被拒"说成"网络不通"：两种原因，两个方向的处置
                        code == null && TraeUpstream.lastAuthFailed ->
                            "签到失败：token 已被服务端拒绝，请重新登录该账号"
                        code == null -> "签到失败：网络不通，请稍后重试"
                        // 9074 = 上游判"操作太过频繁"。参考 trae-mate（trae_instance.rs:111）：
                        // **多账号共用同一设备 id 领取**就会撞上它。走到这里说明重查后仍未签
                        // （重查已签会被上面 after.checkedIn 分支拦下并报「今日已签到」）。
                        code == 9074 -> "签到未生效：上游判定操作过于频繁（code=9074），请稍后再试"
                        else -> "签到未生效（code=$code），今日未签到"
                    }
                )
                quotaApply?.invoke()
            }
        }
    }

    /**
     * 「刷新模型」：从上游拉真实模型表并**原地更新**模型 Tab。
     *
     * 三条必须守住的行为（任务书 §3.2）：
     *  - **原地更新**，不 `render()`/`redraw()` —— 那会丢搜索框内容与滚动位置；
     *  - 网络在后台线程，回主线程改 UI（项目无协程库，用 `Thread{}` + `runOnUiThread`）；
     *  - **禁止静默失败**：拉不到要 toast 说人话（失败时保持空状态，不伪造清单）。
     *
     * [quiet] = 静默模式（进 App 自动补新用）：不 toast、不改按钮态。
     * 失败在静默模式下是**正常边界**（离线启动很常见）—— 磁盘缓存还在顶着，
     * 客户端拿到的仍是上次刷到的清单，所以只打日志，绝不能打扰用户。
     */
    private fun refreshModels(quiet: Boolean = false) {
        // Qoder 后端：拉**官方目录**（含真实倍率与内置清单里没有的免费模型）。
        // 拉不到就继续用内置清单 —— 绝不因为一次网络失败把模型列表清空。
        if (GatewayBackend.isQoder(this)) {
            syncQoderCatalog(quiet)
            return
        }
        // 并集模式：Qoder 目录也顺手刷一次（异步，失败不影响下面的 Trae 刷新）
        if (GatewayBackend.isMixed(this)) syncQoderCatalog(quiet = true)
        val cred = TraeAuthStore.load(this)
        if (cred == null || cred.accessToken.isBlank()) {
            if (!quiet) toast("未登录 Trae，请先登录")
            return
        }
        if (modelsLoading) return
        modelsLoading = true
        if (!quiet) {
            // 立刻切「刷新中…」：网络往返有几百毫秒，不给反馈用户会连点
            modelsRefreshApply?.invoke()
            toast("正在加载模型目录…")
        }
        thread(name = "trae-refresh-models") {
            // ⚠️ 必须 runCatching：`fetchModels` 走真实 OkHttp + JSON 解析，任何 RuntimeException
            // 都会让线程在 `runOnUiThread` **之前**死掉 → `modelsLoading` 永远为 true →
            // `refreshModels()` 在开头直接 return、按钮永久 disabled + 文案卡在「刷新中…」。
            // 这是"唯一会让用户可见控件永久失效"的失败面，所以失败也要把标志捎回来复位。
            val arr = runCatching { TraeUpstream.fetchModels(cred.accessToken, cred.uid) }.getOrNull()
            runOnUiThread {
                if (isFinishing || isDestroyed) { modelsLoading = false; return@runOnUiThread }
                modelsLoading = false
                val before = ModelsRepo.current().length()
                if (arr == null) {
                    // 拉不到：**保持现有状态**（磁盘缓存继续顶），但必须明确告诉用户
                    if (!quiet) toast("拉取失败，请检查网络后重试")
                } else {
                    ModelsRepo.applyFetched(arr)
                    val after = ModelsRepo.current().length()
                    if (!quiet) toast(if (after > before) "已更新：$before → $after 个模型" else "已是最新（$after 个模型）")
                }
                modelsRefreshApply?.invoke()
            }
        }
    }

    /**
     * 进 App 自动补新：模型 Tab / 客户端 `/v1/models` 的数据源重启后来自磁盘缓存，
     * 可能落后于上游 —— 静默刷一次，让用户无感拿到最新表。
     *
     * 判据**不能**用 `ModelsRepo.isDynamic`（init 装过磁盘缓存后它恒为 true，
     * 有缓存的设备将永远补不到新表），用进程级一次性标志：每个冷启动恰好刷一次，
     * recreate（转屏/切主题）不重刷。
     */
    private var autoModelsTried = false

    private fun maybeAutoRefreshModels() {
        if (autoModelsTried || modelsLoading) return
        autoModelsTried = true
        contentHost.post { refreshModels(quiet = true) }
    }

    /**
     * 进入 App 默认拉起网关服务（0.4.8 起）。
     *
     * 以前默认关闭、必须手动点「启动服务」，用户嫌麻烦 —— 现在改成：只要用户**没有主动停过**，
     * 打开 App 就当他在用客户端、需要端口在听。[GatewayService.shouldRun] 持久化了用户意图：
     * 用户点过「停止服务」或通知栏「停止」后 it 为 false，这里就不拉 ——
     * 「除非用户手动停止」的承诺靠它兑现，不会出现"关了又自己开"。
     * 已经在跑（前台服务被系统重建、屏幕旋转重建 Activity 等）则直接跳过。
     */
    private fun autoStartServiceIfWanted() {
        if (GatewayService.running || Gateway.isRunning) return
        // 启动条件：用户表达过意图且希望运行（shouldRun=true），或从未表达过（全新安装，默认开）。
        // 唯一不启动的情形 = 用户**主动停止过**（shouldRun=false 且意图存在）。
        if (GatewayService.hasUserIntent(this) && !GatewayService.shouldRun(this)) return
        GatewayService.start(this)
        // Android 13+ 通知要运行时授权。被拒也能跑（前台服务照常），
        // 只是通知栏看不到"正在运行" —— 那是保活体验的一部分，所以该问就问。
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIF)
        }
    }

    /**
     * 启停本地服务（幂等）。
     *
     * 启动 = 拉起 [GatewayService]（前台服务，负责保活）；停止 = 让它收摊。
     * 状态一律读 [GatewayService.running]，**不本地记**：服务可能被系统回收后由 START_STICKY 重建，
     * 本地字段会和真实情况脱节。
     */
    private fun toggleService() {
        if (serviceBusy != null) return
        val stopping = GatewayService.running
        // 启停是异步的（onStartCommand / onDestroy 都晚于这里的调用），
        // 所以先切到「处理中」，否则这几百毫秒里界面毫无反应，用户会连点。
        serviceBusy = if (stopping) "停止中…" else "启动中…"
        redraw()
        if (stopping) {
            GatewayService.stop(this)
        } else {
            GatewayService.start(this)
            // Android 13+ 通知要运行时授权。被拒也能跑（前台服务照常），
            // 只是通知栏看不到"正在运行" —— 那是保活体验的一部分，所以该问就问。
            if (Build.VERSION.SDK_INT >= 33 &&
                checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
            ) {
                requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIF)
            }
        }
        // 等异步动作落地再重绘，否则会画成旧状态；顺便把结果用 toast 说清楚。
        // ⚠️ 复位**不能只挂在这个 postDelayed 上**：它绑在 `contentHost` 这个视图上，
        // 若 500ms 内界面被销毁（返回/划掉），runnable 不再执行 → `serviceBusy` 残留
        // → 概览按钮停在 disabled 的「启动中…」，且 `toggleService()` 因开头守卫拒绝再进入。
        // 所以真正的兜底是 onDestroy 里的复位（见该处），这里只是正常路径。
        contentHost.postDelayed({
            if (isFinishing || isDestroyed) return@postDelayed
            serviceBusy = null
            toast(if (stopping) "服务已停止" else "服务已启动")
            redraw()
        }, 500)
    }

    /**
     * 主动续期一次并重绘 UI（登录返回 / 切账号时调用）。
     *
     * 注意：**不往网关推 token 快照**。转发时的凭证由 `Gateway` 的 credProvider 每次自取，
     * 这里推会和它重复、且一旦续期轮换就可能是过期的那个。
     * 调用它的意义是：让"切账号/刚登录"立刻反映到界面，并把续期提前到用户等待之外。
     */
    private fun loadAndRefreshToken() {
        thread(name = "trae-load-token") {
            // 与 credProvider **共用同一把锁**（Gateway.locked）：refresh 是轮换的，并发会互相作废。
            Gateway.locked {
                TraeAuthApi.restartWithValidToken(this, skewSec = SIXTY_MINUTES_SEC)
            }
            // 切号**不发请求**：签到状态来自本地落库（AutoCheckin 那一轮 / 手动签到）。
            // 用户要求"切换页也不要自动刷新"，所以这里只重画，不打上游。
            runOnUiThread { redraw() }
        }
    }

    override fun onResume() {
        super.onResume()
        // 有任一账号（Trae 或 Qoder）就把积分监测拉起来，账号页才能"每 2 秒"看到活的积分/自动换号。
        // 与网关启动时拉起的那次幂等（CreditMonitor.start 内部已挡重复启动）。
        // 定时轮询已彻底删除：没有任何后台线程。"没有请求就不刷新"。
        // 唯一的刷新点是转发完成回调（CreditMonitor.noteForwarded，只查刚用到的那个账号）。
        CreditMonitor.stop()
        CreditMonitor.listener = CreditMonitor.Listener { s -> applyMonitorSnapshot(s) }
        // 进页面先回填上一次快照，避免要等最多 2 秒才出数。
        CreditMonitor.currentSnapshot()?.let { applyMonitorSnapshot(it) }
        // 补签：全部平台、全部账号。**没领到会重试**（30 分钟冷却），
        // 所以 10:00 开窗后再打开 App 也能领 —— 不再是"一天只试一次"。
        if (AutoCheckin.shouldAttempt(this)) {
            thread(name = "auto-checkin") {
                val summary = runCatching { AutoCheckin.runAllOnce(this) }.getOrNull()
                // **无论有没有"领到了"的摘要都要重画**：runAllOnce 会把"已签 / 无权限"的账号
                // 也 markDone 落库，界面据此把按钮切成「已签到」。以前只在 summary != null 时重画，
                // 于是"本来就是已签"的号落库了但界面不刷新，按钮一直显示「签到」。
                runOnUiThread {
                    if (isFinishing || isDestroyed) return@runOnUiThread
                    qoderRemaining = null
                    CreditMonitor.stale = true
                    if (summary != null) toast("已自动补签：$summary")
                    render(currentPage)
                }
            }
        }
        // 开窗闹钟：到点自动领，不依赖用户开 App
        AutoCheckin.scheduleWindowAlarm(this)
        // ⚠️ 打开 App / 回前台**不拉任何上游**（用户要求）。
        // 签到状态由 AutoCheckin 那一轮补签落库，界面只读本地结果，不额外发请求。
        // 想看真实余额就点积分卡的「刷新」——那是唯一的手动入口。
    }

    override fun onPause() {
        // ⚠️ 必须解绑监听：监测器是**进程级**对象，比 Activity 活得久，
        // 不清掉会一直持有这个 Activity（内存泄漏），且后台往已销毁的界面回调用会崩。
        CreditMonitor.listener = null
        super.onPause()
    }

    /** 把监测器快照刷进界面（主线程调用，见 CreditMonitor 用 Handler 切了主线程）。 */
    private fun applyMonitorSnapshot(s: CreditMonitor.Snapshot) {
        // 手动「刷新」正在转圈时不被后台覆盖，否则「…」和数字会打架。
        if (!quotaLoading && s.traeBalance != null) accountBalance = s.traeBalance
        qoderRemaining = s.qoderRemaining
        quotaApply?.invoke()
        monitorApply?.invoke()
    }

    override fun onDestroy() {
        // ⚠️ **刻意不在这里停网关**。网关归 GatewayService 管：Activity 销毁（用户划掉最近任务、
        // 系统回收界面）不该把正在服务别的客户端的端口一起掐掉。停止只有两条路 ——
        // 用户在概览页点「停止服务」，或点通知栏的「停止」。
        // （不要补 Gateway.stop()：那会退回"退出界面即断线"，保活就白做了。）
        //
        // 但**进行中的 UI 标志必须清掉**：`serviceBusy` 的正常复位挂在 `contentHost.postDelayed`
        // 上，界面在此被销毁时那个 runnable 永不执行，残留值会让下次进来的概览页显示一个
        // 永远禁用、且 `toggleService()` 拒绝再进入的「启动中…」按钮。
        serviceBusy = null
        quotaLoading = false
        modelsLoading = false
        checkinBusy = false
        super.onDestroy()
    }

    /** 阶段2调试遗留的明文 token key，新链路已不读取；删掉避免明文凭据滞留磁盘。 */
    private fun removeLegacyPlaintextToken() {
        runCatching {
            getSharedPreferences("traework2api_prefs", MODE_PRIVATE)
                .edit().remove("trae_token").apply()
        }
    }

    /** unix 秒 → "yyyy-MM-dd HH:mm（剩余 x 天/即将过期）" 人性化显示。 */
    private fun formatExp(epochSec: Long): String {
        val tf = SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.getDefault())
        val s = tf.format(Date(epochSec * 1000))
        val remainDays = (epochSec - System.currentTimeMillis() / 1000) / 86400.0
        val label = if (remainDays >= 1) "剩 ${"%.1f".format(remainDays)} 天" else "即将过期"
        return "$s（$label）"
    }

    // ------------------------------------------------------------------
    // View 组装 helper（这就是本项目的"组件库"）
    // 改样式只改这一段；圆角/间距/字号只看上面跟随风格的 design token。
    // 颜色**只写语义名**（"primary" / "on_surface" …），色值全在 AppStyle 的风格表里。
    // ------------------------------------------------------------------

    private fun scroll(build: LinearLayout.() -> Unit): ScrollView {
        val sv = ScrollView(this)
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(PAGE_PAD_H), dp(PAGE_PAD_TOP), dp(PAGE_PAD_H), dp(PAGE_PAD_BOTTOM))
            build()
        }
        // 平板/横屏下把内容限宽居中，否则一行文字会横跨整个 3440px 屏幕
        sv.addView(col, FrameLayout.LayoutParams(contentWidth(), -2).apply { gravity = Gravity.CENTER_HORIZONTAL })
        return sv
    }

    /** 手机竖屏 = 屏宽（不受影响）；平板/横屏 = CONTENT_MAX_DP 并居中。 */
    private fun contentWidth(): Int = minOf(resources.displayMetrics.widthPixels, dp(CONTENT_MAX_DP))

    /**
     * 卡片层次（三种，用来拉开信息优先级，而不是每张卡都长一样）。
     *
     * - [NORMAL] `surface` + 1dp 描边 + 阴影 = 常规内容卡，"凸起"在页面之上；
     * - [SOFT] `surface_container` 无描边无阴影 = 说明 / 空态，退到后面；
     * - [EMPHASIS] `primary_container` 无描边无阴影 = 本页最该看的那块（如服务地址），最抢眼。
     *
     * 底色不是 `surface` 的卡，里面的正文/说明对比度由 `AppStyle.selfCheck()` 单独钉过。
     */
    private enum class CardTone(val fill: String, val stroke: String?) {
        NORMAL("surface", "outline_variant"),
        SOFT("surface_container", null),
        EMPHASIS("primary_container", null),
    }

    private fun card(topMargin: Int = GAP, tone: CardTone = CardTone.NORMAL, build: LinearLayout.() -> Unit): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(CARD_PAD), dp(CARD_PAD), dp(CARD_PAD), dp(CARD_PAD))
        background = shape(tone.fill, RADIUS_CARD, tone.stroke)
        // 只有常规卡有阴影：有底色的面板靠颜色分层，再叠阴影会显脏
        elevation = dp(if (tone == CardTone.NORMAL) AppStyle.current.elevation else 0).toFloat()
        build()
        layoutParams = lp(-1, -2, top = topMargin)
    }

    private fun row(top: Int = 0, build: LinearLayout.() -> Unit): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        build()
        layoutParams = lp(-1, -2, top = top)
    }

    private fun text(value: String, size: Int = FS_BODY, bold: Boolean = false, colorName: String = "on_surface"): TextView = TextView(this).apply {
        text = value
        textSize = size.toFloat()
        setTextColor(color(colorName))
        if (bold) setTypeface(typeface, Typeface.BOLD)
        setLineSpacing(0f, 1.12f)
    }

    /** 等宽文本：地址、模型 id、时间戳用它，数字和标识符能对齐。 */
    private fun mono(value: String, size: Int = FS_VALUE, bold: Boolean = false, colorName: String = "on_surface"): TextView =
        TextView(this).apply {
            text = value
            textSize = size.toFloat()
            setTextColor(color(colorName))
            typeface = Typeface.create(Typeface.MONOSPACE, if (bold) Typeface.BOLD else Typeface.NORMAL)
            setLineSpacing(0f, 1.12f)
        }

    private fun caption(value: String): TextView = text(value, FS_CAPTION, false, "on_surface_variant")

    /** 区块标题：小号灰字；原先 16sp 深色，和卡片标题（17sp）几乎分不出层级。 */
    private fun section(value: String): TextView = text(value, FS_SECTION, true, "on_surface_variant").apply {
        letterSpacing = 0.04f
        setPadding(0, dp(18), 0, dp(8))
    }

    /** 1dp 分隔线：顶栏/底栏与内容区分层。 */
    private fun divider(): View = View(this).apply {
        setBackgroundColor(color("outline_variant"))
        layoutParams = LinearLayout.LayoutParams(-1, dp(1))
    }

    /**
     * 状态圆点：**形状 + 颜色双编码**（色盲只靠颜色分不出的状态，靠实心/空心也能分）。
     *
     * 实心 = 成立 / 进行中；空心（描边）= 未成立 / 已停止。
     */
    private fun dot(tone: String, on: Boolean, sizeDp: Int = 9): View = View(this).apply {
        background = if (on) shape(tone, RADIUS_PILL) else shape("surface", RADIUS_PILL, tone, 2)
        layoutParams = lp(dp(sizeDp), dp(sizeDp))
    }

    /**
     * 底层胶囊。语义场景请用 chipOk / chipWarn / chipInfo / statusChip，别在这里手配颜色对。
     *
     * 传了 [dotTone] 会在文字左边加一个状态点（[dotOn] 决定实心还是空心）。
     */
    private fun chip(value: String, backgroundName: String, textName: String, dotTone: String? = null, dotOn: Boolean = false): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        minimumHeight = dp(32)
        setPadding(dp(12), 0, dp(12), 0)
        background = shape(backgroundName, RADIUS_PILL)
        dotTone?.let { addView(dot(it, dotOn), lp(dp(9), dp(9), end = 7)) }
        addView(text(value, FS_CHIP, true, textName))
        layoutParams = lp(-2, -2)   // 显式 wrap：否则在竖直容器里会被拉成整条横幅
    }

    /** 带状态点的语义胶囊：**状态一眼可见**（点 + 字，不只靠颜色）。 */
    private fun statusChip(value: String, tone: String, on: Boolean): LinearLayout =
        chip(value, "${tone}_container", tone, dotTone = tone, dotOn = on)

    private fun chipOk(value: String) = chip(value, "success_container", "success")
    private fun chipWarn(value: String) = chip(value, "warning_container", "warning")
    private fun chipInfo(value: String) = chip(value, "primary_container", "primary")

    /**
     * 键值行：标签列只给最小宽度（不再是死 96dp，长标签能撑开），值可长按复制。
     *
     * [monoValue] 给邮箱这类"标识符型"的值用；
     * [decorate] 让调用方在**不重写这个布局**的前提下附加行为（如整行可点复制）——
     * 比再写一个几乎一样、只多一行 setOnClickListener 的行构建器省事且不会走样。
     */
    private fun labelValue(
        label: String,
        value: String,
        monoValue: Boolean = false,
        decorate: ((TextView) -> Unit)? = null,
    ): View = row {
        addView(caption(label).apply { minWidth = dp(96) }, lp(-2, -2))
        val v = if (monoValue) mono(value, FS_VALUE) else text(value, FS_VALUE)
        v.setTextIsSelectable(true)
        decorate?.invoke(v)
        addView(v, weighted(start = 12))
    }

    /**
     * 占比条：整条 = [value] 占 [max] 的比例（用 weight 分两段，不引任何依赖）。
     * 给"模型消耗汇总"用 —— 两列数字扫不出谁吃得多，加条一眼就有结论。
     */
    private fun shareBar(value: Long, max: Long): View = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        background = shape("surface_variant", RADIUS_PILL)
        val ratio = if (max <= 0L) 0f else (value.toFloat() / max).coerceIn(0f, 1f)
        addView(View(this@MainActivity).apply { background = shape("primary", RADIUS_PILL) },
            LinearLayout.LayoutParams(0, dp(6), ratio))
        if (ratio < 1f) addView(View(this@MainActivity), LinearLayout.LayoutParams(0, dp(6), 1f - ratio))
    }

    /** 搜索框：与卡片同一套白底 + 描边。 */
    private fun searchInput(hintText: String): EditText = EditText(this).apply {
        hint = hintText
        isSingleLine = true
        inputType = InputType.TYPE_CLASS_TEXT
        setTextColor(color("on_surface"))
        setHintTextColor(color("on_surface_variant"))
        background = shape("surface", RADIUS_CARD, "outline_variant")
        setPadding(dp(14), dp(12), dp(14), dp(12))
    }

    /**
     * 小胶囊文字按钮。
     *
     * [overContainer] 用在**强调卡内部**（底色本身就是 `primary_container`）——
     * 那时按钮底要改成 `surface`，否则和卡片同色，会只剩一行字飘在那里。
     */
    private fun linkAction(label: String, overContainer: Boolean = false, click: () -> Unit): TextView = TextView(this).apply {
        text = label
        textSize = FS_CAPTION.toFloat()
        typeface = Typeface.DEFAULT_BOLD
        setTextColor(color("primary"))
        gravity = Gravity.CENTER
        minHeight = dp(48)   // 触控目标 ≥ 48dp（无障碍最低要求），原来 40dp 偏小
        minWidth = dp(48)
        setPadding(dp(14), 0, dp(14), 0)
        background = shape(if (overContainer) "surface" else "primary_container", RADIUS_PILL)
        setOnClickListener { click() }
    }

    /**
     * 主按钮。
     *
     * [danger] 用于**删除**这类破坏性操作：文字与描边走 `error` 色，一眼看出"这个按钮点了会少东西"。
     * 不给实心红底 —— 色板里没有 `on_error`，浅色的 error 配白字在深色风格下会看不清。
     */
    private fun action(label: String, outlined: Boolean = false, full: Boolean = false, danger: Boolean = false, click: () -> Unit): Button = Button(this).apply {
        text = label
        isAllCaps = false
        minHeight = dp(48)
        minWidth = dp(48)
        setPadding(dp(18), 0, dp(18), 0)
        setTextColor(color(if (danger) "error" else if (outlined) "primary" else "on_primary"))
        background = if (danger) shape("surface", RADIUS_CONTROL, "error")
        else if (outlined) shape("surface", RADIUS_CONTROL, "outline")
        else shape("primary", RADIUS_CONTROL)
        setOnClickListener { click() }
        if (full) layoutParams = lp(-1, dp(48))
    }

    /** 所有背景的唯一来源。`strokeDp` 供选中态加粗描边用（如「外观风格」页的选中卡）。 */
    private fun shape(fillName: String, radiusDp: Float, strokeName: String? = null, strokeDp: Int = 1): GradientDrawable = GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE
        setColor(color(fillName))
        cornerRadius = dp(radiusDp).toFloat()
        strokeName?.let { setStroke(dp(strokeDp), color(it)) }
    }

    /**
     * 按语义名取色 —— 风格与亮暗在**运行期**决定。
     *
     * ⚠️ 这里**不能**改回 `getColor(R.color.tw_*)`：资源 id 是编译期常量，
     * 只有 `values/` 与 `values-night/` 两份，换不了风格。
     */
    private fun color(name: String) = AppStyle.color(name)
    private fun toast(value: String) = Toast.makeText(this, value, Toast.LENGTH_SHORT).show()
    private fun dp(value: Int) = (value * resources.displayMetrics.density + 0.5f).toInt()
    private fun dp(value: Float) = (value * resources.displayMetrics.density + 0.5f).toInt()
    private fun lp(width: Int, height: Int, start: Int = 0, top: Int = 0, end: Int = 0, bottom: Int = 0) =
        LinearLayout.LayoutParams(width, height).apply { setMargins(dp(start), dp(top), dp(end), dp(bottom)) }
    private fun weighted(start: Int = 0) = LinearLayout.LayoutParams(0, -2, 1f).apply { marginStart = dp(start) }
    private fun top(value: Int) = lp(-1, -2, top = value)
}
