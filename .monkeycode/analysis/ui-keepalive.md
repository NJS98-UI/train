# TraeWork2API —— UI 完整功能清单（Android 原生 View → Tauri + React 移植分析）

分析范围（已逐行通读，无抽样）：

- 当前工作区 `/app/src/main/java/com/example/traework2api/MainActivity.kt`（3028 行）
- 当前工作区 `/app/src/main/java/com/example/traework2api/AppStyle.kt`（371 行）
- 当前工作区 `/app/src/main/java/com/example/traework2api/KeepAlive.kt`（237 行）
- 当前工作区 `/app/src/main/java/com/example/traework2api/GatewayService.kt`（360 行，为第 5 章保活功能级清单补充）
- 当前工作区 `/app/src/main/res/values/strings.xml`、`values/colors.xml`、`values/styles.xml`、`values-night/colors.xml`、`values-night/styles.xml`
- 当前工作区 `/app/src/main/AndroidManifest.xml`
- 辅助核对展示文案来源：`AccountCredits.kt`、`GatewayBackend.kt`、`TraeRegion.kt`、`QoderAuthApi.kt`、`QoderCheckin.kt`、`CreditMonitor.kt`、`UsageStore.kt`、`LocalFakeServer.kt`

---

## 0. 全局结论（先说最容易移植错的四点）

1. **实际只有 4 个底部页签**：`Page` 枚举为 `OVERVIEW("概览") / ACCOUNT("账号") / MODELS("模型") / RECORDS("记录")`（MainActivity.kt:120-125）。类注释与多处行内注释仍写"概览 / 账号 / 模型 / 记录 / **更多** 五个持久 Tab"（MainActivity.kt:36），以及"Tab 高亮保持在「更多」"（MainActivity.kt:452、462、477、487），这些都是**过期注释**：当前实现中「更多」页签已不存在，它的入口被搬到概览页的「后台常驻」卡里（`保活设置` / `外观` 两个按钮，MainActivity.kt:523-535）。移植时按 4 Tab 做，底部导航 `navBar` 用 `weight=1f` 等分（当前是 4 等分）。
2. **「更多 / 关于页」的整块内容已成死代码**：`AUTHOR_NAME="玖念"`、`AUTHOR_EMAIL="2820184772@qq.com"`、`AUTHOR_LABEL="作者"`、`CONTACT_LABEL="联系邮箱"`（MainActivity.kt:80-83）、`ENDPOINTS`（`POST /v1/chat/completions`、`GET /v1/models`、`GET /healthz`）、`versionName`、`copyEndpoint()`、`openLogin()`、`confirmRemoveAccount()`、`copyAccountsJson()`、`checkinRewardLabel()`、`qoderClaimLabel()`、`qoderBatchClaimLabel()`、`claimQoderDaily()`、`promptImportCatalog()`、`showLastCatalogRaw()`、`nextQoderTime()`、`quotaValue()` —— 全部只有定义处 1 次引用，**界面上不会出现**（MainActivity.kt:76-90、219-221、1126-1131、1402-1405、1173-1234、1586-1590、1763、2214-2221、2316-2330、1614-1618、2386-2389）。移植时这些不进 UI，仅作后端能力保留。
3. **颜色不在资源里决定**：`values/colors.xml` / `values-night/colors.xml` 只是「默认风格（靛蓝）」的备份色板与主题占位；真正的取色入口是运行期 `AppStyle.color("语义名")`（AppStyle.kt:297-300，MainActivity.kt:3020）。因此 React 侧**不要**把 hex 写进组件，要建一层 `tokens(style, mode)`。
4. **两处注释与实现相反，实现为准**：
   - 概览自动启服务：`autoStartServiceIfWanted()`（MainActivity.kt:2648-2661，onCreate 调用 326）—— 除"用户主动停过"外，进 App 即拉起前台服务。
   - 进账号页自动拉积分：MainActivity.kt:439-441 会在 `accountCheckin == null && !quotaLoading` 时 `refreshQuota()`；而第 808 行注释写"进账号页也不许自动刷新"。移植以代码行为为准（进账号页会自动拉一次）。
   - 账号页「积分总计」的按钮文案是 `"刷新"`（MainActivity.kt:603），但更新时间位的引导文案写 `"尚未查询（点右上「刷新积分」）"`（MainActivity.kt:1157）—— 文案与按钮名不完全一致，复刻需原样保留这句提示。

---

## 1. 应用骨架、导航与滚动行为

### 1.1 视图层级（MainActivity.kt:328-395）

```
FrameLayout root（背景 color("background")，fitsSystemWindows=true）
└ LinearLayout col（vertical，铺满）
  ├ LinearLayout 顶栏外层（vertical，背景 background，全宽）
  │ └ LinearLayout 顶栏内层（限宽 contentWidth()，水平居中，gravity=CENTER_HORIZONTAL）
  │     ├ linkAction("← 返回") { render(Page.OVERVIEW) }   // backAction，默认 GONE（348-350）
  │     ├ pageTitle   TextView  FS_TITLE=22sp，on_surface，sans-serif-medium（351-355）
  │     └ pageSubtitle TextView FS_CAPTION=13sp，on_surface_variant（356-359）
  ├ divider()  1dp outline_variant（362）
  ├ FrameLayout contentHost  weight=1f（365-366）← 页面内容/二级页都塞这里
  ├ divider()（392）
  └ LinearLayout navBar（horizontal，gravity=CENTER，背景 background）（369-393）
      └ 每个 Page：LinearLayout(weight=1f, 高 64dp) → 内含 TextView（图标在上 + 文字在下）
```

### 1.2 底部导航（MainActivity.kt:368-391、442-449）

- 项：`Page.entries` 驱动，`text = p.title`、`textSize = FS_CHIP(12sp)`、图标 `setCompoundDrawablesRelativeWithIntrinsicBounds(0, p.icon, 0, 0)`（图标在文字**上方**），`compoundDrawablePadding = 3dp`，`padding = 18dp/6dp`。
- 点击：外层容器 `setOnClickListener { render(p) }`（外层撑 1/4 宽保证点击区，内层 TextView 包住内容使胶囊贴合内容 —— 注释 383-384）。
- 选中态（`render()` 末尾统一刷）：
  - 文字色 `primary`，字体 `sans-serif-medium`；未选中 `on_surface_variant` + 默认字体。
  - 背景 = `shape("primary_container", RADIUS_PILL)`（胶囊贴合内容）；未选中为无背景。
  - 图标 tint 同步 `primary` / `on_surface_variant`（`mutate().setTint`）。
- 页签标题/副标题（Page 枚举，原样）：
  | id | title | subtitle | icon |
  |---|---|---|---|
  | OVERVIEW | 概览 | 本地服务运行状态 | R.drawable.ic_dashboard |
  | ACCOUNT | 账号 | 账户登录状态 | R.drawable.ic_account |
  | MODELS | 模型 | 本地可用的模型表 | R.drawable.ic_models |
  | RECORDS | 记录 | 调用用量与记录 | R.drawable.ic_history |

### 1.3 二级页与返回机制

- 状态位：`showKeepAlive`、`showStyle`（MainActivity.kt:140、142）。
- `openKeepAlive()`（453-460）：标题 `"保活设置"`、副标题 `"让服务在后台长期活着"`、`backAction` 显示、内容 = `keepAlivePage()`；不动底部高亮（所以进二级页时底部仍高亮原 Tab —— 注释里说的"保持更多"已不成立）。
- `openStylePage()`（463-470）：标题 `"外观风格"`、副标题 `"配色与形态，随你挑"`、内容 = `stylePage()`。
- 返回出口两条：
  1. 顶栏 `← 返回` → `render(Page.OVERVIEW)`（348）。
  2. 系统返回键 `onBackPressed()`：`if (showKeepAlive || showStyle) render(Page.OVERVIEW) else super`（489-491）。
- 点任意底部 Tab 即退出二级页（`render()` 首段清 `showKeepAlive/showStyle/backAction`，415-420）。
- `redraw()`（479-485）：按当前位置重绘（二级页仍留在二级页），注释明确禁止用 `render(currentPage)` 当刷新（475-477）。
- 进程/配置重建恢复：`onSaveInstanceState` 存 `STATE_PAGE` + `STATE_SUB`（`"keepalive"` / `"style"` / `""`，常量 55-59），`onCreate` 尾部恢复（398-404）。切风格/切亮暗走 `recreate()`，系统夜间模式切换由系统重建 Activity。

### 1.4 滚动与"原地更新不重建"清单

- `scroll {}` 容器（MainActivity.kt:2810-2820）：`ScrollView` + 竖直 `LinearLayout`，内边距 `PAGE_PAD_H / PAGE_PAD_TOP / PAGE_PAD_H / PAGE_PAD_BOTTOM`；内容宽 `contentWidth() = min(屏宽, dp(CONTENT_MAX_DP))`，`gravity=CENTER_HORIZONTAL`（平板/横屏限宽居中，竖屏不受影响）。
- `render(page)` 走 `contentHost.removeAllViews()` 整页重建（427-433）；以下场景刻意**不**走 render：
  | 句柄 | 注册处 | 原地刷什么 | 理由（源码注释） |
  |---|---|---|---|
  | `quotaApply` | 一键签到卡 781 | `statusView.text = checkinStatusText()` | 重建会把滚动弹回页首，看不到"加载中…"（197-202） |
  | `monitorApply` | 账号页 813-815 | `qoderCreditView.text = qoderRemainingLabel()` | 同上，顺带无闪烁 |
  | `creditViews: List<Pair<TextView, ()->String>>` | `creditLabel()` 1130-1140 | 每个账号卡的"积分：x"文本 | `refreshCreditViews()` 只改 text（1149-1160） |
  | `modelsRefreshApply` | 模型页 1880-1885 | `rebuildCards()`+计数+按钮文案+已加载态 | 保住搜索框内容、焦点与滚动位置（204-211、1839-1841） |
- 被 `render()` 作废的三个句柄在 421-424 置 null；`creditViews` 在 `accountPage()` 开头 `clear()`（596）。
- 例外：**API Key 显示/隐藏走整页 `render(currentPage)`**（558-561），不是原地更新。
- 启停服务、领取、切号等处仍是 `render/redraw` 全量重建（2192、2199、2200、2676、2698、1382、1394、1411、1432、1442、1461、1473、1486、2328、2382 等）。

---

## 2. 逐页签完整清单

约定：`card(tone)` = NORMAL(`surface`+1dp `outline_variant` 描边+阴影) / SOFT(`surface_container` 无描边无阴影) / EMPHASIS(`primary_container` 无描边无阴影)（MainActivity.kt:2834-2848）。`action()` = 主按钮（实心 primary / `outlined` 浅描边 / `full` 满宽 48dp / `danger` error 描边，2992-3004）。`linkAction()` = 胶囊文字按钮（13sp 粗体 primary，`overContainer` 时底色换 `surface`，min 48×48dp，2973-2984）。

### 2.1 页签「概览」（overviewPage，MainActivity.kt:513-588）

元素顺序 = 卡片数：3 张卡片 + 1 个顶部状态胶囊 + 1 个满宽主按钮。

1. **服务状态胶囊**（非卡片，页面第一行，516-520）
   - `serviceBusy != null` → `chipInfo(busy)`（primary_container/primary）
   - `GatewayService.running` → `statusChip("服务运行中", "success", on=true)`（success_container 底 + 实心 success 圆点）
   - 否则 → `statusChip("服务已停止", "error", on=false)`（error_container 底 + 空心 error 圆点）
2. **卡片 1 —— 后台常驻（SOFT）**，523-535；用户要求"只留状态 + 两个入口（去掉全部说明文字）"
   - 行 1：左 `text(运行中?"后台常驻中":"服务未启动", 17sp bold)`；右 chip：`KeepAlive.check()` 未就绪项数 `pending == 0` → `chipOk("保活就绪")`，否则 `chipWarn("$pending 项待开启")`
   - 行 2（top 12dp，两枚 outlined 按钮各占半宽）：
     - `action(pending==0 ? "保活设置" : "去设置保活（$pending 项）", outlined)` → `openKeepAlive()`
     - `action("外观", outlined)` → `openStylePage()`
3. **卡片 2 —— 服务地址 + API Key（EMPHASIS）**，539-564（用户要求 API Key 与服务地址同卡）
   - `caption("本机地址")`；行：`mono("http://127.0.0.1:8787/v1", 15sp bold, primary)` + `linkAction("复制", overContainer)` → `copyServerUrl()`
     - toast：`"已复制 http://127.0.0.1:8787/v1"`（2391-2394；端口常量 `LocalFakeServer.PORT = 8787`）
   - 条件渲染（`lanAddress() != null`）：`caption("局域网地址")` + `mono("http://<ip>:8787/v1")` + `linkAction("复制")` → `copyLanUrl(ip)`，toast `"已复制 http://<ip>:8787/v1"`
     - LAN 取址规则：优先 site-local IPv4，退化 169.254.（link-local），剥 `%iface` 后缀，检测不到返回 null → 整段不显示（2406-2424）
   - `divider()`（top 12）
   - `caption("API Key")`；行：
     - 值：`apiKeyVisible ? apiKey : MASKED_KEY`，其中 `MASKED_KEY = "sk-••••••••••••••••••••••••"`（24 个 U+2022 圆点，74 行定义）；`setTextIsSelectable(apiKeyVisible)`
     - `linkAction(apiKeyVisible ? "隐藏" : "显示")` → 翻转 `apiKeyVisible` 后 `render(currentPage)`
     - `linkAction("复制")` → `copyApiKey()`，toast `"已复制 API Key"`（值始终取 `Gateway.apiKey(this)` 现读，143-144）
4. **满宽主按钮**（566-569）
   - 文案：`serviceBusy ?: (running ? "停止服务" : "启动服务")`；`outlined = running || busy != null`；busy 时 `isEnabled=false, alpha=0.6f`
   - 点击 `toggleService()`（2670-2700）：先 `serviceBusy = "停止中…"/"启动中…"` + `redraw()` → `GatewayService.stop()/start()`；Android 13+ 未授权通知则 `requestPermissions(POST_NOTIFICATIONS, REQ_NOTIF=1002)`；500ms 后复位 busy + toast `"服务已停止"` / `"服务已启动"` + `redraw()`
5. **卡片 3 —— Trae 账号（NORMAL）**，571 + `addTraeCard()` 574-588
   - `TraeAuthStore.load() == null` → 仅一行 `text("未登录 Trae", 17sp bold)`
   - 有凭据：
     - 行：`text(nickname.ifBlank { "Trae 账号" }, 17sp bold)` + `statusChip(accessToken 非空 ? "转发 已启用"(success,on) : "转发 未配置"(warning,off))`
     - `uid` 非空 → `caption("UID: ${uid} · ${regionLabelFull(region)}")`，区域文案见 §2.2 末
     - `!region.isSupported` → `caption(TraeRegion.SG.unsupportedNote)`，原样："未开发：登录可通，但聊天不支持多轮上下文与外部工具调用；且免费额度极少（约 $1/月）。建议使用国服。"（TraeRegion.kt:99-101）
     - `expiresAt > 0` → `caption("Token 到期: ${formatExp(expiresAt)}")`，格式 `yyyy-MM-dd HH:mm（剩 x.x 天 / 即将过期）`（2796-2802）

### 2.2 页签「账号」（accountPage，MainActivity.kt:594-816）

固定 6 张 card + 5 个 section；另有 2 条 `accountBar`（有账号时）与多组按钮行。顺序如下。

1. **卡片 —— 积分总计（NORMAL）** 598-616
   - 行：`text("积分总计", 17sp bold)` + `linkAction("刷新")` → toast `"正在查询全部账号积分…"` + `AccountCredits.refreshAll(force=true) { refreshCreditViews() }`
   - `row(12)`：两个 `creditBlock`（竖排：`caption(title)` + 大字值，24sp bold primary）：`"Trae"`、`"Qoder"`，初值 `"—"`
   - `creditUpdated`（caption，初值空）取值：`snap.loading` → `"正在查询全部账号…"`；`snap.at<=0` → `"尚未查询（点右上「刷新积分」）"`；否则 `"更新于 HH:mm:ss"`（1155-1159）
   - 合计显示 = `AccountCredits.totalLabel` → `format(sum)`：整数不带小数，否则 `.1f`；无数据 `"—"`（AccountCredits.kt:378-402）
2. **section `"TRAE 账号"`** 619 + 内容
   - 无账号（`TraeAuthStore.list().isEmpty()`）→ `card(SOFT){ text("未添加账号", 17sp bold) }`
   - 有账号 →
     - 当前账号卡（NORMAL）：
       - 行：`text(nickname ?: "TRAE 账号", 17sp bold)` + `chipOk("当前使用")`
       - `labelValue("区域", regionLabelFull(region))`（标签列 minWidth 96dp，值可选中复制，2930-2941）
       - `labelValue("UID", uid)`
       - `labelValue("Token", tokenRenewLabel(uid, expiresAt))`，四态文案（1783-1792）：`"需重新登录"`（有该 uid 的被拒证据）/ `"未知（无到期信息）"` / `"已过期 · 需重新登录"` / `"临近到期 · 将自动续期"`（剩 ≤3600s）/ `"有效"`
       - `creditLabel(TRAE, uid)` → `"积分：" + AccountCredits.label(item)`，`label` 规则：`null`→`"—"`；有 error→`"—（原因）"`；否则数字（1130-1140，AccountCredits.kt:384-393）
       - 签到按钮行（top 12）：`action(checkinActionLabel(已登录), outlined) { claimCheckin() }`，建好后 `btn.post { refreshCheckinButton(btn, ...) }`
         - `checkinActionLabel`（1687-1700）8 态：`"签到中…"` / `"未登录"` / `"需重新登录"` / 本地落库已签 `"已签到"` / `"签到"` / 远端 `"已签到"` / `"无签到权限"` / `"签到"`
         - `refreshCheckinButton`（1709-1748）三态外观：可签 = `primary` 实心 + `on_primary` 字 + 48dp 高 + 18dp 横向 pad + 15sp；已签 = 前缀 `✓ ` + `success_container` 胶囊 + `success` 字 + 32dp 高 + 14dp pad + 12sp bold；其它 = `surface_variant` 胶囊 + `on_surface_variant` 字。可用性：`!checkinBusy && (远端已知? (!checkedIn && enable) : (uid!=null && !本地已签 && !被拒))`
         - `claimCheckin()` 结果 toast（2495-2565）：`"未登录 Trae，请先登录"` / `"已签到"` / `"该账号无签到权限"` / `"签到成功"` / `"签到失败：token 已被服务端拒绝，请重新登录该账号"` / `"签到失败：网络不通，请稍后重试"` / `"签到未生效：上游判定操作过于频繁（code=9074），请稍后再试"` / `"签到未生效（code=$code），今日未签到"`
     - `accountBar("账户选择") { showTraeAccountPicker() }`：`surface`+描边 16dp 内边距的整行可点条，左 `"账户选择"`（14sp bold）右 `"▾"`（17sp）（823-833）
   - 按钮行（top 10）：`action("登录账号") { startTraeLogin() }` + `action("粘贴回调链接", outlined) { promptPasteCallback() }`
     - `startTraeLogin()`（973-984）：`region.canLogin` 为假 → toast `"${region.id} 区域未开放"`；否则 `TraeRegion.set` → 复制 `TraeAuthApi.buildLoginUrl()` toast `"登录链接已复制"` → `ACTION_VIEW` 打开浏览器；无浏览器 → toast `"没有可用浏览器，链接已复制，请手动打开"`
     - `promptPasteCallback()`（2343-2381）：`confirmDialog(title="粘贴回调链接", message="用系统浏览器打开登录链接登录后，把地址栏里那串 http://127.0.0.1:18080/authorize?... 完整粘进来。", okText="登录", content=多行 EditText hint="粘贴登录后跳转的完整链接")`
       - 空 → toast `"没有粘贴内容，请重新打开本窗口"`；否则 toast `"正在换取 token…"`，后台 `TraeAuthApi.loginWithCallbackUrl`；成功 toast `"登录成功：${nickname ?: uid}"` + `AutoCheckin.requestRunSoon` + `invalidateAccountQuota()` + `loadAndRefreshToken()`；失败 toast `"登录失败：${e.message}"`
3. **section `"Qoder 账号"`** 660 + 内容
   - 无账号 → `card(SOFT){ text("未添加账号", 17sp bold) }`
   - 有账号 → 当前账号卡（NORMAL）：
     - 行：`text(name ?: "Qoder 账号", 17sp bold)` + `chipOk("当前使用")`
     - `labelValue("区域", mode==MODE_GLOBAL ? "国际版" : "国内版")`、`labelValue("UID", uid)`、`email` 非空再 `labelValue("邮箱", email)`
     - `caption("积分")` + 大字 `qoderRemainingLabel()`（24sp bold primary，`qoderCreditView`，`monitorApply` 原地刷新）：优先 `AccountCredits` 快照 `label`，否则 `CreditMonitor` 余量 `"%.1f"`，否则 `"—"`（1299-1308）
     - 领取按钮行（top 12，仅当前账号非空）：`action(when { qoderClaimBusy→"领取中…"; qoderClaimedToday(uid)→"已领取"; else→"领取" }, outlined) { claimQoderForUid(uid) }`
     - `linkAction((captured ? "设备身份：抓包指纹" : "设备身份：本机派生") + (isGlobal ? "" : "（国内版不发送）"))` → `promptQoderFingerprint(uid)`（所有 Qoder 账号都显示入口，注释 700-703）
       - 弹窗 `confirmDialog("Qoder 设备身份", okText="保存", content=三个 EditText)`：hint 依次 `"cosy-machineid"`、`"cosy-machinetoken"`、`"cosy-machinetype（默认空）"`；`cur != null` 时额外 `linkAction("清除抓包指纹，回到本机派生")` → toast `"已清除，改回本机派生身份"` + `render(currentPage)`
       - 保存校验：id 或 token 空 → toast `"两项都留空 = 用本机派生；要覆盖就两项都填"`；成功 → toast `"已保存抓包指纹"`
   - `accountBar("账户选择") { showQoderAccountPicker() }`
   - 按钮行（top 10）：`action("登录 Qoder") { showQoderLoginGuide() }` + `action("回填令牌", outlined) { promptQoderPat() }`
     - `showQoderLoginGuide()`（987-1001）：`confirmDialog("登录 Qoder", message="1. 登录后点左侧「服务集成」\n2. 在「个人访问令牌」处生成并复制令牌\n3. 回到本页点「回填令牌」粘贴", okText="打开浏览器")` → `ACTION_VIEW https://qoder.com/account/integrations`；失败 → 复制链接并 toast `"链接已复制，请用浏览器打开"`
     - `promptQoderPat()`（1314-1374）：`confirmDialog("粘贴 Qoder PAT 登录", message="在 Qoder 账号集成页创建个人访问令牌后粘贴。", okText="登录", content=多行 EditText(hint="粘贴 Qoder 个人访问令牌 pt-…") + caption("令牌属于哪个区域？") + 两枚区域按钮)`
       - 区域按钮 `"国际版"`（默认选中，实心）/`"国内版"`（选中项实心，未选 outlined），默认 `MODE_GLOBAL`（注释：绝大多数用户用 qoder.sh）
       - 空 → toast `"没有粘贴内容"`；否则 toast `"正在换取 jobToken…"` → `QoderAuthApi.loginWithPat`；成功 toast `"登录成功：${name ?: email ?: uid}"` + `AutoCheckin.requestRunSoon` + 清 `qoderRemaining` + `CreditMonitor.stale=true` + `render`；失败 toast `"登录失败：${e.message}"`
4. **section `"聊天转发后端"`** 724-737 —— 无卡片，一行 3 枚按钮（等宽）
   - `action("TRAE" / "Qoder" / "自动")`，当前选择=实心（`outlined = sel != 该项`）
   - 点击：`GatewayBackend.set` → toast `"转发后端：" + GatewayBackend.label(b)`（label：`"Qoder"`/`"Trae"`/`"自动"`）→ `render(currentPage)`
   （注释写"当前选择绿色高亮"，实际实现是实心 primary 描边/实底，注释过时）
5. **section `"自动换号"`** 743-768 + 卡片
   - 行：`text("当前号用完就换", 15sp bold)` + `action(auto ? "已开启" : "已关闭", outlined=!auto)`；点击 → `CreditMonitor.setAutoSwitchEnabled(!auto)` + toast `"已开启自动换号"`/`"已关闭自动换号"` + `render`
   - 行（top 12）：`text("换号阈值", 15sp bold)` + `chipInfo("≤ $cur")`（`CreditMonitor.threshold` 默认 5，最小 5）
   - 行（top 10）：4 枚等宽阈值按钮 `"5" `"10"` `"15"` `"20"`，选中实心其余 outlined → `setThreshold` + `render`
   - `caption("只在同一个平台内换号；换完立刻刷新新号的余额。不做定时检测。")`
6. **section `"账户导出"`** 771-775 —— 无卡片，一行两枚 outlined 按钮：`"导出"` → `exportAccounts()`；`"导入"` → `importAccounts()`
   - 导出（1597-1612 + onActivityResult 1629-1675）：`AccountBackup.exportAll()` 失败 → toast `"导出失败：${it.message}"`；成功暂存 `pendingExportJson` → `ACTION_CREATE_DOCUMENT`（type=`application/json`，`EXTRA_TITLE="traework2api-accounts.json"`，REQ_EXPORT=1003）；无选择器 → 退化为复制到剪贴板并 toast `"未找到保存对话框，已改为复制到剪贴板"`；写成功 toast `"已导出账号 JSON"`，失败 `"导出失败：${msg}"`
   - 导入（1620-1627）：`ACTION_OPEN_DOCUMENT`（type=`*/*`，REQ_IMPORT=1004）；无选择器 → toast `"未找到文件选择器；可用剪贴板方式：先复制 JSON，再…"`；读入后 `AccountBackup.importAll`：成功 toast `"已导入：Trae ${n} 个 · Qoder ${m} 个"` + 清缓存 + `CreditMonitor.stale=true` + `render`；失败 toast `"导入失败：JSON 无效或没有可导入的账号（原因）"`
7. **section `"一键签到"`** 778-806 + 卡片
   - `quotaApply = { statusView.text = checkinStatusText() }`（原地球柄）
   - `action("一键签到全部账号", full)` → `claimAllCheckin()`（1100-1105）：Trae 有激活号则 `claimCheckin()`，Qoder 有号则 `claimQoderDailyAll()`，两者都无 → toast `"没有可签到的账号"`
     - `claimQoderDailyAll()`（1438-1464）：`qoderClaimBusy` 置位 + `render`；`QoderCheckin.busy` CAS 失败 → toast `"定时/补签正在进行，稍后再试"`；结果 toast `qoderBatchClaimToast`：`"${ok}/${total} 个账号领取成功，共 +$total Credits"` / `失败原因` / `"无可领额度（已领或不符）· 共 N 个账号"`；单项 `qoderClaimToast`：`"领取成功 +${amount} Credits"` / `"今日已领取"`(或 r.message) / `"unsupported→r.message"` / `"领取失败：${msg}"`
   - `linkAction("诊断领取链路")` → `runQoderDiagnose()`（1532-1564）：无号 toast `"没有 Qoder 账号"`；toast `"诊断中…"`；平台 `AlertDialog` 标题 `"领取链路诊断"`，内容等宽 10sp 可选中，按钮 `"复制"`（toast `"已复制"`）/`"关闭"`
   - `linkAction("诊断 Trae 账号")` → `runTraeDiagnose()`（1493-1530）：无号 toast `"没有 Trae 账号"`；同上，标题 `"Trae 账号诊断"`，多账号用 30 个 `─` 分隔
   - `statusView`（caption）文案 `checkinStatusText()`（1108-1119），以 `"  ·  "` 连接：`"Trae：已签到 / 无签到权限 / 可签到"`、`"Qoder：领取 N 个 / 无可领"`
   - `divider()`（top 14）
   - 行（top 12）：`text("定时领取", 15sp bold)` + `linkAction(on ? "已开启" : "已关闭")`（点击 `QoderCheckin.disable/enable` + toast `"已关闭定时领取"` / `"已开启，每天 HH:mm"`）+ `linkAction(QoderCheckin.timeText)` → `promptCustomTime()`
     - `promptCustomTime()`（1059-1097）：`confirmDialog("定时领取时间", okText="确定", content=两个 NumberPicker(0-23 / 0-59, wrap) 中间 `:`)` → 开启中则 `QoderCheckin.enable(t)` 否则 `setTime(t)` + toast `"定时领取：每天 HH:mm"` + `render`；默认时刻 `"09:00"`（默认值来自 QoderCheckin.kt:70-71）
8. 收尾：`refreshCreditViews()`（只画本地缓存，不发请求，808-810）+ 登记 `monitorApply`

**账号选择弹窗（两套同构）**（843-970）

- `showTraeAccountPicker()`：空列表 toast `"还没有账号"`；标题 `"选择 Trae 账号"`
- `showQoderAccountPicker()`：标题 `"选择 Qoder 账号"`
- 每行固定高 `PICKER_ROW_H=56dp`：`text(nickname/name ?: uid, 14sp bold, maxLines=1 ellipsize END)` + `caption(regionLabel(m.region))`（Trae）或 `caption("国际版"/"国内版")`（Qoder）+ `caption("积分：…")` + 当前项 `text("✓", 17sp bold, success)` + 红色 `"删除"`（12sp，error 色）
  - 行点击：关弹窗；`uid != active` → Trae `switchAccount(uid)`（2287-2313）/ Qoder `switchQoderAccount(uid)`（1376-1383）
    - Trae：`!meta.region.isSupported` → `confirmDialog("该账号属未开发区域", message="<昵称>（<区域>）\n\n未开发：…\n\n仍然切换吗？", okText="仍要切换")`；确认后 `setActive` + `syncRegionToActiveAccount` + `invalidateAccountQuota()` + toast `"已切换到 <昵称 ?: uid>"` + `loadAndRefreshToken()`
    - Qoder：`setActive` + 清余量 + `CreditMonitor.stale` + toast `"已切换到 Qoder ${uid}"` + `render`（切号不发请求）
  - 删除：`confirmDialog("删除账号", "删除「<昵称 ?: uid>」？本地凭证会一起清掉。", okText="删除", danger=true)` → `AuthStore.remove` + `AccountCredits.forget` + 关弹窗 + toast `"已删除"` + `render(Page.ACCOUNT)`（Trae 文案"本地凭证"，Qoder 文案"本地令牌"，Qoder 弹窗标题同为"删除账号"）
- 弹窗壳 `showPickerDialog()`（951-970）：高度固定为 `min(rows, PICKER_VISIBLE_ROWS=5) × 56dp + 分隔线 + 2dp`，内部 `ScrollView`；只有 `"关闭"` 键（`cancelText=null`），`message=""`，`onOk` 为空

**区域文案**（2332-2337）：`regionLabel` = `SG ? "国际服" : "国服"`；`regionLabelFull` = 后者 +（不受支持时）`"（未开发）"`

### 2.3 页签「模型」（modelsPage，MainActivity.kt:1798-1942）

控件顺序：搜索框 → 工具条（刷新按钮 + 计数）→ 未加载卡 → 搜索空态卡 → 卡片容器。

1. `searchInput("搜索模型…")`：单行，`surface`+`outline_variant` 描边圆角（2957-2965）；`afterTextChanged → applyFilter()`
   - 过滤：对每张卡的 key（模型 id）做 `contains(关键词, ignoreCase)` 改 visibility（不重建，保焦点）；全部隐藏且关键词非空 → 显示空态卡
2. 工具条 `row(top=GAP)`：`action(REFRESH_MODELS_LABEL="刷新模型目录")` + `countView`（caption）
   - 计数文案：`"已加载 · $n 个模型"`，n = `GatewayBackend.modelsFor(ctx).length()`（1805-1810）
   - 刷新中：按钮 disable + 文案 `"刷新中…"`（1813-1816）
3. 未加载态卡（NORMAL）：`text("模型目录尚未加载", 17sp bold)` + `action("刷新模型目录", full)`（1827-1831）
4. 搜索空态卡（SOFT）：`caption("没有匹配的模型。换个关键词，或清空搜索框看看全部。")`，初值 GONE（1834-1837）
5. 模型卡若干（`modelCard` 1906-1940，NORMAL），每张：
   - 行：`text(name ?: id, 17sp bold)` + chip：`price_factor` 缺失 → `chipInfo("倍率未知")`；`<=0` → `chipOk("免费")`；否则 `chipInfo("×" + priceText(pf))`（`priceText`：整数去尾，如 `0`/`1`/`2.5`）
   - 行(top 4)：`mono(key, 12sp, on_surface_variant, maxLines=1 ellipsize)` + `linkAction("复制")` → `copyModelId(id)` toast `"已复制模型 ID: <id>"`
   - `caption("context ${context_length} · <倍率文案> · <backendTag>[ · key 未验证]")`，倍率文案：`"倍率未知"` / `"免费（0 倍率）"` / `"倍率 ×<v>"`；backendTag = `owned_by` 否则 `isQoder ? "qoder" : "trae-solo"`；`unverified=true` 追加 `" · key 未验证"`
- 显隐规则 `applyLoadedState()`（1863-1870）：已加载 → 隐藏未加载卡，显示搜索框/工具条/卡片容器；未加载反之并强制隐藏搜索空态
- 首次进模型页（有 Qoder 账号且本进程未试过）→ `syncQoderCatalog(quiet=true)` 静默同步官方目录（1845-1848，`qoderCatalogAutoTried`）
- `refreshModels(quiet)`（2579-2621）：Qoder 后端 → 转 `syncQoderCatalog`；混合后端 → 顺手静默刷 Qoder；Trae 未登录 → toast `"未登录 Trae，请先登录"`；`modelsLoading` 守卫；开始 toast `"正在加载模型目录…"`；失败 toast `"拉取失败，请检查网络后重试"`；成功 toast `"已更新：$before → $after 个模型"` 或 `"已是最新（$after 个模型）"`；结果一律走 `modelsRefreshApply`（原地重建卡片区）
- `syncQoderCatalog`（1242-1290）：无 Qoder 账号 → toast `"先添加一个 Qoder 账号，才能拉官方模型目录"`；toast `"正在拉取 Qoder 官方模型目录…"`；成功 toast `"官方目录已同步：$total 个模型（$free 个免费）"`；失败 toast `"拉取失败，继续用内置 $total 个模型"`（+ 原因截 120 字符）
- 进 App 静默自动刷一次：`maybeAutoRefreshModels()`（2633-2637，`autoModelsTried` 每进程一次，`refreshModels(quiet=true)`，无 toast）

### 2.4 页签「记录」（recordsPage，MainActivity.kt:1948-1984）

- 空态（1）：`section("消耗汇总")` + `card(SOFT){ caption("暂无记录 —— 发一次聊天请求后这里会显示用量与汇总") }`
- 非空：
  1. 汇总卡（NORMAL）：`text("模型消耗汇总", 17sp bold)`，然后每个分组（`UsageStore.aggregate`）：
     - 行(top 12)：`text(model ?: "?", 14sp bold)` + `mono("总 ${total} · 输出 ${output}", 13sp, on_surface_variant)`
     - `shareBar(value, max)`：高 6dp 圆角条，`surface_variant` 底 + `primary` 前景，比例 = 该组 / **最大组**（不是总量）（2947-2954）
  2. `section("调用记录（最近 ${records.size} 条）")`
  3. 每条记录一张卡（NORMAL，倒序 `asReversed()`）：
     - 行：`statusChip(r.ok ? "OK"(success,on) : "ERR"(error,off))` + `text(model ?: "?", 15sp)` + `mono("HH:mm:ss", 12sp, on_surface_variant)`
     - `caption("in <prompt> · out <completion> · 总 <total> · <durationMs>ms[ · <errorType>]")`
- 记录字段来源 `UsageStore.UsageRecord(ts, model, prompt, completion, total, durationMs, ok, errorType)`（UsageStore.kt:17-26）

### 2.5 二级页「保活设置」（keepAlivePage，MainActivity.kt:2001-2062）

4 张卡片 + 2 个 section。数据 = `KeepAlive.check()` / `KeepAlive.rom()` / `KeepAlive.romGuide()`。

1. 总状态卡（top 间距 0，NORMAL）
   - 行：`text(pending.isEmpty() ? "保活已就绪" : "还有 ${pending.size} 项待开启", 17sp bold)` + `chipOk("✓ 全部正常")` / `chipWarn("⚠ 需要处理")`
   - `caption`：就绪 → `"网关已能在后台长期运行，切到别的 App、息屏都不会断。"`；否则 → `"下面标 ⚠ 的项会让服务在真机上被系统清掉，建议逐项开启。"`
2. 一键配置卡
   - `text("一键配置", 17sp bold)`
   - `caption`：`autoCount == 0` → `"能自动跳转的项都已就绪。"`；否则 → `"有 $autoCount 项可以自动打开系统页面，你点「允许」即可。"`
   - `action("开始自动配置" + (autoCount>0 ? "（$autoCount 项）" : ""), full)` → `runAutoConfig()`
     - `runAutoConfig()`（2065-2073）：只跳**第一个**未就绪且 `autoOpenable` 项（一次只跳一个，多弹窗会互相顶掉）；全就绪 → toast `"能自动配置的项都已就绪"`
     - `openAutoItem(title)`（2075-2091）：`"电池优化白名单"`→`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`；`"精确闹钟"`→`ACTION_REQUEST_SCHEDULE_EXACT_ALARM`；`"通知权限"`/`"通知渠道未被关闭"`→`ACTION_APP_NOTIFICATION_SETTINGS`；打不开 → toast `"这个页面在本机打不开，请按下方路径手动设置"`；800ms 后若仍在保活页 → `openKeepAlive()` 重绘取最新状态
3. `section("逐项检查")` + 卡：6 项，每项 `✓/⚠` 前缀（success/warning 粗体）+ 标题（14sp bold）+ 未就绪时 chip（`chipInfo("可一键")` 或 `chipWarn("需手动")`）+ 原因 caption + （未就绪且可一键时）`action("去开启", outlined)`；项间 `divider()`
   检查项逐条（KeepAlive.kt:139-170）：
   | title | ok 判据 | why（原样引用） | autoOpenable |
   |---|---|---|---|
   | 电池优化白名单 | `PowerManager.isIgnoringBatteryOptimizations(pkg)` | 没加入白名单时，系统省电策略会在息屏后清掉后台服务 —— 这是真机上最致命的一项。 | 是 |
   | 通知权限 | SDK<33 或 `POST_NOTIFICATIONS` 已授 | 前台服务的常驻通知需要它；通知不可见时系统会弱化甚至清理服务。 | 是 |
   | 通知渠道未被关闭 | `!GatewayService.isChannelDisabled(ctx)`（渠道 importance==IMPORTANCE_NONE 即关） | 渠道被关掉等同于通知不可见，服务会被削弱。 | 否 |
   | 精确闹钟 | SDK<31 或 `AlarmManager.canScheduleExactAlarms()` | 服务被厂商强杀后靠它定时唤醒恢复；被禁用时只能退化成非精确闹钟，Doze 下可能延迟数小时。 | 是 |
   | 自启动 / 后台运行 | 恒 `false`（系统无查询接口） | 系统不提供查询接口，需要你自己确认（路径见下方）。 | 否 |
   | 省电策略 = 无限制 | 恒 `false` | 系统不提供查询接口，需要你自己确认（路径见下方）。 | 否 |
4. `section("本机需要手动确认的项")` + 卡
   - `text("检测到：${rom.label}", 17sp bold)`
   - `caption("以下页面系统不开放跳转，需要你按路径自己找一下。")`
   - 逐行 `caption("• " + line)`（`romGuide`）
   - `action("复制这些路径", outlined)` → 多条路径以 `\n` 连接复制，toast `"路径已复制，可到系统设置里照着找"`
   - ROM 识别（KeepAlive.kt:46-121，优先系统属性，回落品牌名）与引导文案：
     - `MIUI "小米 / Redmi（MIUI / HyperOS）"`：自启动：设置 → 应用设置 → 应用管理 → 本应用 → 自启动（打开）／省电策略：同上页面 → 省电策略 → 选「无限制」／后台弹出：同上页面 → 权限管理 → 后台弹出界面（允许）
     - `COLOR_OS "OPPO / 一加 / realme（ColorOS）"`：完全后台行为：设置 → 电池 → 应用耗电管理 → 本应用 → 允许完全后台行为／自启动：同上页面 → 允许自启动／关闭智能省电：同上页面 → 关闭「智能省电」
     - `EMUI "华为 / 荣耀（EMUI / MagicOS）"`：应用启动管理：设置 → 应用 → 应用启动管理 → 本应用 → 关掉「自动管理」／然后把「自启动 / 关联启动 / 后台活动」三项都打开
     - `ORIGIN_OS "vivo / iQOO（OriginOS）"`：后台高耗电：设置 → 电池 → 后台耗电管理 → 本应用 → 允许后台高耗电／自启动：i 管家 → 应用管理 → 权限管理 → 自启动（允许）
     - `OTHER "其它 Android"`：在「设置 → 应用 → 本应用」里找：自启动 / 后台运行 / 省电策略／把省电策略设为「不优化 / 无限制」

### 2.6 二级页「外观风格」（stylePage，MainActivity.kt:2103-2201）

- `section("风格")` + 4 个风格行（`styleOption`，2118-2138，非 card）：
  - 左：48×48dp **色样**（`styleSwatch` 2144-2160：该风格自己的 `primary` 作底、圆角 = `min(cardRadius, 24)`dp、`outline_variant` 1dp 描边；内部居中一枚 24×7dp `on_primary` 胶囊，圆角 `min(controlRadius, 8)`dp）
  - 中：`text(s.label, 17sp bold, selected?primary:on_surface)` + `caption(s.note)`
  - 右：选中时 `chipInfo("✓ 使用中")`
  - 行底：`surface` + `RADIUS_CARD`，描边 `selected ? primary 2dp : outline_variant 1dp`，elevation = `current.elevation + (selected?1:0)`，`minHeight=56dp`
  - 点击 `switchStyle(s)`：同 id 直接 return；否则持久化 + toast `"已切换为「<label>」"` + `recreate()`
- `section("亮暗模式")` + 卡（`modeOption` 2163-2185）：3 行，每行 `text(m.label, 15sp bold, selected?primary:on_surface)` + `caption(m.note)` + 右侧 `radioDot`（22dp：选中 = primary 实心圆 + 中心 8dp `on_primary` 点；未选 = `surface` 底 + `outline` 描边空心），`minHeight=52dp`，行间 `divider()`
  - `Mode`：`SYSTEM("system","跟随系统","系统开夜间模式时自动变深")`、`LIGHT("light","浅色","始终使用浅色")`、`DARK("dark","深色","始终使用深色")`
  - 点击 `switchMode(m)`：持久化 + toast `"亮暗模式：<label>"` + `recreate()`
- 尾注 `caption("切换后立即生效，并会记住你的选择；重开 App 仍是这套。")`

### 2.7 统一弹窗 `confirmDialog`（MainActivity.kt:2238-2270）

- 自绘（不走平台主题，避免切风格后弹窗仍是靛蓝）：面板 = `cardPad` 内边距竖排；`title` 17sp bold `on_surface`；`message` 14sp `on_surface_variant`；`content` 可选自定义视图（top 14）；按钮行 top 18：`action(cancelText ?: "取消", outlined)` + `action(okText, danger)`（等宽，danger → error 字+error 描边）
- 窗口背景 = `shape("surface", RADIUS_CARD)`（替换平台灰底）
- 破坏性场景（删除账号、设备身份清除等）传 `danger = true`

---

## 3. Toast 全清单（按触发点，全部原样文案）

| 触发 | 文案 |
|---|---|
| 启动自检失败 | `自检警告（不影响使用）：{第一条失败信息}` |
| 点积分总计「刷新」 | `正在查询全部账号积分…` |
| `refreshQuota` 未登录 / 结果 | `未登录 Trae，请先登录` / `已更新` / `未获取到额度` |
| 选后端 | `转发后端：Trae｜Qoder｜自动` |
| 定时领取开关 | `已关闭定时领取` / `已开启，每天 HH:mm` |
| 定时领取时间弹窗 | `定时领取：每天 HH:mm` |
| 打开账号选择弹窗（空列表） | `还没有账号` |
| 弹窗内删除账号 | `已删除` |
| 切 Trae / Qoder 账号 | `已切换到 {昵称或uid}` / `已切换到 Qoder {uid}` |
| Trae 登录按钮 | `登录链接已复制` / `{region.id} 区域未开放` / `没有可用浏览器，链接已复制，请手动打开` |
| `copyLoginUrl` | `{国际服/国服}登录链接已复制，请用系统浏览器打开并登录`、`{region.id} 区域常量未配置完整` |
| 粘贴回调 | `没有粘贴内容，请重新打开本窗口` / `正在换取 token…` / `登录成功：{昵称或uid}` / `登录失败：{msg}` |
| PAT 回填 | `没有粘贴内容` / `正在换取 jobToken…` / `登录成功：{name|email|uid}` / `登录失败：{msg}` |
| 设备身份弹窗 | `两项都留空 = 用本机派生；要覆盖就两项都填` / `已保存抓包指纹` / `已清除，改回本机派生身份` |
| 一键签到无账号 | `没有可签到的账号` |
| Trae 签到 | `已签到` / `该账号无签到权限` / `签到成功` / `签到失败：token 已被服务端拒绝，请重新登录该账号` / `签到失败：网络不通，请稍后重试` / `签到未生效：上游判定操作过于频繁（code=9074），请稍后再试` / `签到未生效（code={code}），今日未签到` |
| Qoder 领取 | `没有 Qoder 账号` / `定时/补签正在进行，稍后再试` / `领取成功 +{amount} Credits` / `今日已领取`(或上游 message) / `领取失败：{msg}` / `{ok}/{n} 个账号领取成功，共 +{total} Credits` / `无可领额度（已领或不符）· 共 {n} 个账号` |
| 诊断 | `诊断中…` / `已复制` / `没有 Trae 账号` |
| 导出/导入 | `导出失败：{msg}` / `未找到保存对话框，已改为复制到剪贴板` / `已导出账号 JSON` / `未找到文件选择器；可用剪贴板方式：先复制 JSON，再…` / `已导入：Trae {n} 个 · Qoder {m} 个` / `导入失败：JSON 无效或没有可导入的账号（{msg}）` |
| 模型 | `正在加载模型目录…` / `拉取失败，请检查网络后重试` / `已更新：{b} → {a} 个模型` / `已是最新（{a} 个模型）` / `未登录 Trae，请先登录` |
| 官方目录 | `正在拉取 Qoder 官方模型目录…` / `先添加一个 Qoder 账号，才能拉官方模型目录` / `官方目录已同步：{n} 个模型（{f} 个免费）` / `拉取失败，继续用内置 {n} 个模型\n{原因}` |
| 复制类 | `已复制 API Key` / `已复制 http://127.0.0.1:8787/v1` / `已复制 http://{lan}:8787/v1` / `已复制 {完整端点 URL}`（copyEndpoint，未被调用） / `已复制模型 ID: {id}` / `账号 JSON 已复制到剪贴板（含明文 token，请妥善保管）`（copyAccountsJson，未被调用） |
| 服务启停 | `服务已停止` / `服务已启动` |
| 保活页 | `能自动配置的项都已就绪` / `这个页面在本机打不开，请按下方路径手动设置` / `路径已复制，可到系统设置里照着找` |
| 外观 | `已切换为「{风格名}」` / `亮暗模式：{模式名}` |
| 后台补签 | `已自动补签：{summary}` |

`Toast.LENGTH_SHORT` 统一（3021）。

---

## 4. AppStyle 主题系统

### 4.1 语义与实现（AppStyle.kt）

- 取色模型：**语义名 → 运行期查表**（`color(name)` 读 `current.palette(night)`），布局代码只写 `color("primary")`；`R.color.*` 编译期常量无法表达"风格"维度（文件头注释 10-21）。取不到键回退 `on_surface`，**绝不返回透明**（297-300）。
- 语义色键共 19 个（`KEYS`，26-31）：`primary, primary_container, on_primary, on_primary_container, secondary, background, surface, surface_container, surface_variant, on_surface, on_surface_variant, outline, outline_variant, success, success_container, warning, warning_container, error, error_container`
- 亮暗三模式：`SYSTEM/LIGHT/DARK`；`applyNightOverride(base)`（283-289）在 `Activity.attachBaseContext` 用 `createConfigurationContext` 强制覆写 `uiMode`（因此平台对话框、光标、`values-night` 资源都跟着走）；`SYSTEM` 原样透传，系统夜间切换会重建 Activity。`refreshNight(ctx)`（271-274）从资源配置回读 `UI_MODE_NIGHT_YES`，**必须在建控件前调用**。
- 持久化：`SharedPreferences("ui_appearance")` 的 `style_id`、`mode_id`；`init()` 幂等一次，读到已删除的 id 退回默认（248-257）。
- 启动自检 `selfCheck()`（322-352，由 MainActivity.onCreate 经 `selfCheck("AppStyle")` 调用，失败**不崩**只 toast）：至少 3 套风格、id 唯一；每套亮/暗 19 键齐全；值必须是 `#RRGGBB` 且不透明；17 组对比度 `≥ 4.5`（WCAG AA 正文）；圆角全 > 0；`cardPad>0 && pagePadH>0 && gap>=0`；`contentMaxDp ∈ 480..900`。
- 对比度钉住的组合（`CONTRAST_PAIRS`，40-58）：`on_surface/`{`surface`,`background`,`primary_container`,`surface_container`}、`on_surface_variant/`{同 4 种底}、`on_primary/primary`、`primary/surface`、`primary/primary_container`、`primary/surface_container`、`on_primary_container/primary_container`、`success/success_container`、`warning/warning_container`、`error/error_container`。
- 风格同时携带**形态**（圆角/间距/限宽/阴影），切换风格时视觉形态一起变（74-100 注释）。

### 4.2 四套配色风格全量色值（顺序即「外观风格」页展示顺序，首个为默认）

#### ① 靛蓝 `id=indigo`，note：`经典靛蓝 · 中圆角 · 标准间距`
形态：cardRadius=16f，controlRadius=12f，pillRadius=100f，gap=12，pagePadH=20，pagePadTop=16，pagePadBottom=24，cardPad=16，barPadTop=14，barPadBottom=12，contentMaxDp=640，elevation=1

| key | light | dark |
|---|---|---|
| primary | #4F46E5 | #BEC2FF |
| primary_container | #E6E7FF | #34358B |
| on_primary | #FFFFFF | #20205F |
| on_primary_container | #17115B | #E1E0FF |
| secondary | #2563A9 | #8AB8E8 |
| background | #F7F7FC | #101016 |
| surface | #FFFFFF | #19191F |
| surface_container | #F0F0F8 | #23232B |
| surface_variant | #E7E7F0 | #2E2E38 |
| on_surface | #1B1B22 | #E5E1EA |
| on_surface_variant | #5F5F6B | #B5B3C0 |
| outline | #777783 | #8B8B97 |
| outline_variant | #C7C7D1 | #3A3A44 |
| success | #16794B | #66E3A0 |
| success_container | #D7F6E5 | #1E4A34 |
| warning | #8A5100 | #FFD08A |
| warning_container | #FFE9C2 | #5A3D17 |
| error | #BA1A1A | #FFB4A8 |
| error_container | #FFDAD6 | #5A2B2B |

（与 `values/colors.xml` / `values-night/colors.xml` 完全一致，那两个文件就是 indigo 的 XML 副本）

#### ② 石墨 `id=graphite`，note：`冷灰中性 · 方正小圆角 · 紧凑间距`
形态：cardRadius=6f，controlRadius=6f，pillRadius=8f，gap=10，pagePadH=16，pagePadTop=14，pagePadBottom=18，cardPad=14，barPadTop=12，barPadBottom=10，contentMaxDp=680，elevation=0

| key | light | dark |
|---|---|---|
| primary | #3F4754 | #B8C2D0 |
| primary_container | #DDE2EA | #2A313C |
| on_primary | #FFFFFF | #1A2028 |
| on_primary_container | #191C22 | #DCE2EA |
| secondary | #4A5568 | #9AA6B5 |
| background | #ECEDEF | #101215 |
| surface | #FFFFFF | #17191D |
| surface_container | #F4F5F7 | #1F2227 |
| surface_variant | #E2E4E8 | #272B31 |
| on_surface | #16181D | #E3E5E9 |
| on_surface_variant | #55595F | #A9AEB6 |
| outline | #6E737B | #868C95 |
| outline_variant | #C6C9CE | #34383E |
| success | #14663F | #7FD9A8 |
| success_container | #D2EEDD | #12321F |
| warning | #7A4700 | #F5C87A |
| warning_container | #FBE3BC | #3A2A06 |
| error | #A81B1B | #FFB3AC |
| error_container | #FBDAD8 | #521413 |

#### ③ 苔绿 `id=moss`，note：`自然苔绿 · 大圆角 · 舒展间距`
形态：cardRadius=24f，controlRadius=18f，pillRadius=100f，gap=16，pagePadH=24，pagePadTop=20，pagePadBottom=28，cardPad=20，barPadTop=16，barPadBottom=14，contentMaxDp=600，elevation=2

| key | light | dark |
|---|---|---|
| primary | #2F6C46 | #8FD5A6 |
| primary_container | #D6EFDD | #1D3A28 |
| on_primary | #FFFFFF | #0D2A18 |
| on_primary_container | #0E2416 | #CDEBD8 |
| secondary | #4A7C59 | #A9C9AF |
| background | #F4F7F2 | #0F1410 |
| surface | #FFFFFF | #161C17 |
| surface_container | #EDF3EA | #1E251F |
| surface_variant | #DFE9DD | #262E27 |
| on_surface | #131A15 | #E1E8E1 |
| on_surface_variant | #4F5B52 | #AAB7AC |
| outline | #6C7A6F | #8A978A |
| outline_variant | #C3CFC2 | #333B33 |
| success | #1A6B3C | #8FE0AE |
| success_container | #D9F2E1 | #12331F |
| warning | #86530A | #F7CA80 |
| warning_container | #FCE8C3 | #3B2B06 |
| error | #A8261F | #FFB4AC |
| error_container | #FBDCD7 | #53140F |

#### ④ 落日 `id=sunset`，note：`暖橙落日 · 大圆角 · 宽松内边距`
形态：cardRadius=20f，controlRadius=14f，pillRadius=100f，gap=12，pagePadH=20，pagePadTop=18，pagePadBottom=24，cardPad=18，barPadTop=14，barPadBottom=12，contentMaxDp=640，elevation=1

| key | light | dark |
|---|---|---|
| primary | #A8431A | #FFB59B |
| primary_container | #FFE2D4 | #6E2607 |
| on_primary | #FFFFFF | #571D06 |
| on_primary_container | #3A1406 | #FFDBCB |
| secondary | #8A5A2B | #E0B58C |
| background | #FDF7F3 | #17110D |
| surface | #FFFFFF | #1F1813 |
| surface_container | #F8EFE9 | #291F18 |
| surface_variant | #EFE2DA | #33261D |
| on_surface | #211711 | #EFE1D9 |
| on_surface_variant | #5C4A3E | #C4B0A3 |
| outline | #7D6A5D | #9C8A7C |
| outline_variant | #D6C5B8 | #45372C |
| success | #1A6B3C | #8FE0AE |
| success_container | #D9F2E1 | #12331F |
| warning | #8A5100 | #F7CA80 |
| warning_container | #FFE9C2 | #3B2B06 |
| error | #B3261E | #FFB4AB |
| error_container | #FFDAD5 | #5C1412 |

（本表色值逐格抄自 AppStyle.kt:206-227，未做换算。）

### 4.3 不随风格变的字号 design token（MainActivity.kt:64-71）

| 常量 | 值 | 用途 |
|---|---|---|
| FS_TITLE | 22f | 顶部栏标题 |
| FS_SECTION | 14 | 区块标题（灰，与卡片标题拉开层级），外加 `letterSpacing=0.04f`、上下 padding 18/8dp |
| FS_CARD_TITLE | 17 | 卡片标题 |
| FS_METRIC | 24 | 主指标数值（积分大字） |
| FS_BODY | 15 | 正文 |
| FS_VALUE | 14 | 键值/次级数值 |
| FS_CAPTION | 13 | 说明文字（顶栏副标题也是此号，以 Float 传入） |
| FS_CHIP | 12 | 胶囊/导航项/等宽小字 |

其它固定尺寸（非风格变量）：

| 用途 | 值 |
|---|---|
| 底部导航项高度 | 64dp |
| 导航项内边距 | 左右 18dp、上下 6dp，图标-文字间距 3dp |
| chip 最小高 / 横向 pad | 32dp / 12dp |
| 状态圆点直径 | 9dp（`dotTone` 左侧，end margin 7dp） |
| 分隔线高度 | 1dp |
| 主按钮最小高 / pad | 48dp / 左右 18dp；`full` 时高固定 48dp |
| linkAction 最小尺寸 | 48×48dp，pad 左右 14dp |
| labelValue 标签最小宽 | 96dp；值左间距 12dp |
| 占比条高度 | 6dp |
| 搜索框内边距 | 14dp/12dp |
| 输入框内边距 | 14dp/12dp（弹窗内），设备身份输入框 12dp/10dp |
| 色样 / 内胶囊 | 48×48dp / 24×7dp |
| 单选圆点 | 22dp 容器，8dp 内点 |
| 弹窗行高 / 最大可见行 | `PICKER_ROW_H=56dp` / `PICKER_VISIBLE_ROWS=5` |
| 风格行、亮暗行最小高 | 56dp / 52dp |
| 行距 | `setLineSpacing(0, 1.12f)` |
| Toast 时长 | LENGTH_SHORT |
| `MASKED_KEY` | `sk-` + 24 个 `•` |
| 端口 | 8787（`LocalFakeServer.PORT`）；回调链接示例端口 18080 |
| dp() 换算 | `value * density + 0.5f` 取整 |

### 4.4 Android 主题（values/styles.xml / values-night/styles.xml）

- `Theme.TraeWork2API` parent `android:style/Theme.Material.Light.NoActionBar`，`fontFamily=sans`
- light：`windowLightStatusBar=true`、`windowLightNavigationBar=true`；night：两者 `false`
- 两者共有：`windowDrawsSystemBarBackgrounds=true`、`statusBarColor`/`navigationBarColor` 透明、`windowBackground=@color/tw_background`、`colorAccent=@color/tw_primary`、`colorControlHighlight=@color/tw_primary_container`、`windowActionModeOverlay=true`
- `strings.xml` 仅 `app_name = TraeWork2API`，全站文案均硬编码在 Kotlin 中，桌面版可直接沿用同一份"文案表"策略。

---

## 5. KeepAlive + GatewayService：保活功能清单（功能级别，供桌面等价设计）

### 5.1 两层保活模型（GatewayService.kt:23-49 文件头）

第一层（标准 Android）：
1. 前台服务 + 常驻通知（Android 8+ 唯一不被随手回收的服务形态）
2. `PARTIAL_WAKE_LOCK`（tag `traework2api:gateway-wakelock`）—— 持锁进程被 Cached App Freezer 跳过（冻结豁免）；不持锁会出现"端口在监听但进程不被调度、请求全挂"（第 27 行注释 + manifest:10-12）
3. `START_STICKY` 返回（166），进程被回收后系统重建
4. manifest `android:stopWithTask="false"`（manifest:64），划掉最近任务不随任务栈销毁

第二层（国产 ROM 适配，真机命门）：
5. `onTaskRemoved`（180-198）：若 `shouldRun` 为假不拉起；否则立即 `startForegroundService` 拉回；被 Android 12+ 后台启动限制拦住 → 退化挂 **5 秒后的精确闹钟**（`TASK_REMOVED_REQUEST=8790`）兜底，并续看门狗
6. **看门狗闹钟**（230-255）：`WATCHDOG_INTERVAL_MS = 15 * 60_000`（刻意 15 分钟 —— 更密会被 ColorOS 判异常唤醒而拦截），请求码 `WATCHDOG_REQUEST=8789`，动作 `ACTION_WATCHDOG = com.example.traework2api.WATCHDOG`；用 `setExactAndAllowWhileIdle`，`canScheduleExactAlarms()` 预检，未授权退化为 `setAndAllowWhileIdle` 并打日志（Doze 下可能延迟数小时）
7. `should_run` 持久化用户意图（PREFS=`traework2api_service`，KEY=`should_run`）：`markShouldRun/shouldRun/hasUserIntent`（92-107）—— 区分"用户主动关了"与"被系统杀了"，避免"关了又自己开"；`ACTION_WATCHDOG` 且 `!shouldRun` → 取消看门狗并 `START_NOT_STICKY`（151-155）

唤醒锁细节（282-298）：`WAKELOCK_TIMEOUT_MS = 10 * 60_000`（**短于看门狗 15 分钟**）；非计数模式；已持有且未过期则跳过；无限期死锁会被 ROM 判"异常耗电"优先清理，故靠看门狗周期续期实现"始终持锁"。

### 5.2 通知与渠道

- CHANNEL_ID `traework2api_gateway`，NOTIFICATION_ID `8788`（54-55）
- 渠道（342-359）：名 `"本地网关"`，描述 `"常驻运行本地 OpenAI 兼容网关，供其它应用调用"`，`IMPORTANCE_DEFAULT`（刻意不用 LOW —— 低重要度会被部分 ROM 降级），已存在则**不覆盖**用户手改设置，无角标、无关振动、无声、锁屏可见 `VISIBILITY_PUBLIC`
- 通知体（304-340）：`setSmallIcon(R.drawable.ic_launcher_foreground)`（必须纯白剪影，系统只取 alpha 染色），`contentTitle="TraeWork2API 网关"`，`contentText="本地服务运行中 · 端口 8787"`，点按打开 MainActivity（`PendingIntent.getActivity` req=0），action 按钮 `android.R.drawable.ic_media_pause` + `"停止"` → 服务 `ACTION_STOP`（req=1）；`setOngoing(true)`、`CATEGORY_SERVICE`、`VISIBILITY_PUBLIC`、`PRIORITY_DEFAULT`、API 31+ `FOREGROUND_SERVICE_IMMEDIATE`（避免延迟 10 秒被 ROM 判隐形后台服务）
- `isChannelDisabled()`：`importance == IMPORTANCE_NONE` → 界面侧"通知渠道未被关闭"检查项

### 5.3 生命周期与网关所有权

- `onStartCommand`（141-167）：`ACTION_STOP` → `stopNow()` + `START_NOT_STICKY`；否则 `markShouldRun(true)` → `startForeground`（必须数秒内，否则 ANR 杀服务）→ `acquireWakeLock()` → `Gateway.start(appContext)` → `running = true` → `scheduleWatchdog()`
- `onDestroy`（200-205）：`Gateway.stop()` + 释放锁 + `running=false`
- `stopNow()`（207-217）：`running=false`、`markShouldRun(false)`、**必须 `cancelWatchdog()`**（否则 15 分钟后被莫名拉起）、停网关、放锁、`stopForeground(STOP_FOREGROUND_REMOVE)`、`stopSelf()`
- 网关归 `Gateway`（业务）+ `GatewayService`（保活）持有，**Activity 不再拥有**，MainActivity.onDestroy 刻意不停网关（MainActivity.kt:2771-2785），只清 `serviceBusy/quotaLoading/modelsLoading/checkinBusy` 四个 UI 标志
- manifest 权限：`INTERNET`、`FOREGROUND_SERVICE`、`FOREGROUND_SERVICE_SPECIAL_USE`（服务 `foregroundServiceType="specialUse"`，`PROPERTY_SPECIAL_USE_FGS_SUBTYPE="User-started on-device OpenAI-compatible API gateway"`）、`POST_NOTIFICATIONS`、`WAKE_LOCK`、`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`、`SCHEDULE_EXACT_ALARM`、`USE_EXACT_ALARM`
- **刻意没有** `RECEIVE_BOOT_COMPLETED`/BootReceiver：用户要求"重启手机后必须自己打开 App 手动开"（manifest:20-21、GatewayService.kt:43-44）
- 已注册闹钟 receiver（均 `exported="false"`，独立于服务 intent，避免 onStartCommand 无条件 `markShouldRun(true)` 复活用户手动停掉的网关）：`.CheckinReceiver`（每日定时签到）、`.QoderCheckinReceiver`（Qoder 每日 Credits 定时领取）、`.AutoCheckinReceiver`（开窗补签，新加坡 10:05）
- `LoginWebActivity` 已整体移除（manifest:40-41），登录只保留「浏览器 + 粘贴回调」
- Activity：`windowSoftInputMode="adjustResize"`，`exported=true`（LAUNCHER）；application：`allowBackup=false`、`usesCleartextTraffic=false`、`networkSecurityConfig=@xml/network_security_config`、`supportsRtl`

### 5.4 桌面版（Tauri）等价映射建议

| Android 机制 | 桌面等价 |
|---|---|
| 前台服务 + 常驻通知 | Tauri 后台线程/tray 图标 + tray tooltip 显示"运行中 · 端口"；tray 菜单 `Stop` |
| PARTIAL_WAKE_LOCK / 冻结豁免 | 无需（桌面不休眠进程）；若需防休眠可 `caffeinate`(mac)/`SetThreadExecutionState`(win)/systemd-inhibit(linux) |
| START_STICKY / onTaskRemoved 拉回 / 15min 看门狗闹钟 | 不需要；改为"应用退出即停服务"+"窗口关闭时最小化到托盘（可配置）" |
| `should_run` 持久化意图 | 保留（同一语义）：存 config 文件，启动时按 `has_user_intent` + `should_run` 决定是否自动拉起 |
| 电池优化白名单 / 精确闹钟 / 通知权限 引导 | 桌面侧替换为"开机自启（可选，用户此前明确要求不做）"、"防火墙/端口占用提示"、"系统通知权限" |
| ROM 私有页面引导 | 删除整节（或替换为"操作系统专属注意事项"文案） |

---

## 6. MainActivity 与后端交互点全清单

按"触发条件 → 调用 → 结果展示"整理，全部为 UI 层直接依赖。

| # | 触发点（UI） | 调用 | 结果展示 |
|---|---|---|---|
| 1 | onCreate 22 项 `selfCheck(...)` | AppStyle/LocalFakeServer/TraeRegion/TraeAuthStore/QoderAuthStore/QoderAuthApi/QoderCheckin/AccountBackup/CreditMonitor/TraeAuthApi/TraeIntlUpstream/TraeUpstream/ModelsRepo/QoderCosyCrypto/QoderModelCatalog/QoderUpstream/Gateway/GatewayBackend/AccountCredits/QoderModelCache/AutoCheckin/QoderMachine/KeepAlive 的 selfCheck | 有失败则 `toast("自检警告（不影响使用）：{第一条}")`，不崩（234-241、318-320） |
| 2 | onCreate | `removeLegacyPlaintextToken()`（prefs `traework2api_prefs` 删 `trae_token`）、`ModelsRepo.init`、`QoderModelCatalog.load`、`TraeRegion.load`、`TraeAuthStore.syncRegionToActiveAccount`、`AccountCredits.loadCache`、`autoStartServiceIfWanted()`、`maybeAutoRefreshModels()`、`loadAndRefreshToken()` | 无 toast 的直接生效；启服务后 Android13+ 可能请求通知权限 |
| 3 | 概览：服务状态/启停 | `GatewayService.running`、`Gateway.apiKey(ctx)`、`GatewayService.start/stop`、`LocalFakeServer.PORT`、`lanAddress()` | chip 三态、按钮态与 busy、toast `服务已启动/停止` |
| 4 | 概览：显示/隐藏/复制 Key | `Gateway.apiKey`（每次现读）+ ClipboardManager | 明文可选中，打码 `MASKED_KEY`，toast `已复制 API Key` |
| 5 | 概览：保活徽章/入口 | `KeepAlive.check()`（未就绪项数）| `保活就绪`/`N 项待开启` chip 与按钮文案 |
| 6 | 概览：Trae 卡 | `TraeAuthStore.load`、`TraeRegion.current`/`isSupported` | 未登录/昵称+转发态 chip+UID/区域/到期 |
| 7 | 账号页进入 | `accountCheckin==null && !quotaLoading → refreshQuota()` | `TraeUpstream.fetchUsage` + `fetchCheckinHealing`；结果 toast `已更新/未获取到额度`；失败保留旧值；按 uid 记 `authFailedUid` |
| 8 | 「刷新」（积分总计） | `AccountCredits.refreshAll(force=true){refreshCreditViews()}` | toast `正在查询全部账号积分…`；合计与更新时间原地更新 |
| 9 | 积分显示 | `AccountCredits.label/totalLabel/format` | `—`、`—（原因）`、整数或 `.1f`；更新时间 `更新于 HH:mm:ss` |
| 10 | 「登录账号」/「粘贴回调链接」 | `TraeRegion.canLogin/set`、`TraeAuthApi.buildLoginUrl`、剪贴板+浏览器；`TraeAuthApi.loginWithCallbackUrl` | §2.2 的 toast 序列；成功后 `AutoCheckin.requestRunSoon` + `invalidateAccountQuota` + `loadAndRefreshToken` |
| 11 | 账户选择弹窗（切号/删号） | `TraeAuthStore.setActive/remove`、`syncRegionToActiveAccount`、`QoderAuthStore.setActive/remove`、`AccountCredits.forget` | `已切换到 …`、`已删除`、未开发区域二次确认；`render` |
| 12 | Trae 卡「签到」 | `claimCheckin()` → `fetchCheckinStatus 先行 / TraeUpstream.claimCheckinHealing / fetchUsage` + `AutoCheckin.markDone("trae:$uid")` | `✓ 签到…` 文案 + toast 8 态；按钮三态外观；`quotaApply` 原地 |
| 13 | 「一键签到全部账号」 | `claimAllCheckin()` → `claimCheckin()` + `claimQoderDailyAll()`→`QoderAuthApi.claimDailyAll` | `checkinStatusText()` 原地更新 + 批量 toast |
| 14 | 「定时领取」开关 / 时间链接 | `QoderCheckin.isEnabled/enable/disable/timeText/setTime`（内部落 AlarmManager+receiver） | `已开启/已关闭` 链接文字 + `HH:mm`；toast 确认 |
| 15 | 「登录 Qoder」 | `showQoderLoginGuide` → 浏览器 `https://qoder.com/account/integrations` | 三步说明弹窗；无浏览器 → 复制+toast |
| 16 | 「回填令牌」(PAT) | `QoderAuthApi.loginWithPat(pat, mode)`（mode 按钮 国际版/国内版，默认 GLOBAL） | `正在换取 jobToken…` → `登录成功：…`/`登录失败：…`；成功后清余量、`CreditMonitor.stale=true`、requestRunSoon |
| 17 | Qoder 卡「领取」 | `claimQoderForUid` → `QoderAuthApi.claimDailyForUid` + `QoderCheckin.markClaimed`（ok/already 都落库） | `领取中…/已领取/领取`；toast 结果；`qoderRemaining=null` + `stale` + `render` |
| 18 | 「设备身份」链接 | `QoderAuthStore.fingerprint/setFingerprint/clearFingerprint`、`QoderMachine.machineId` | 三字段弹窗 + toast；入口名 `设备身份：抓包指纹｜本机派生（国内版不发送）` |
| 19 | 「诊断领取链路」/「诊断 Trae 账号」 | `QoderAuthApi.diagnose`、`TraeUpstream.diagnose`（逐账号，`─×30` 分隔） | 只读诊断弹窗（等宽 10sp，`复制`/`关闭`），不改变状态 |
| 20 | 「聊天转发后端」三按钮 | `GatewayBackend.selection/set/label` | toast `转发后端：…` + `render`；影响模型页与 `/v1/models` 数据源 |
| 21 | 「自动换号」开关与阈值 | `CreditMonitor.isAutoSwitchEnabled/setAutoSwitchEnabled/threshold/setThreshold` | `已开启/已关闭`、`≤ N` chip、4 档按钮 |
| 22 | 「导出」/「导入」 | `AccountBackup.exportAll/importAll` + SAF（`ACTION_CREATE_DOCUMENT`/`OPEN_DOCUMENT`，`onActivityResult`） | §2.2 第 6 项与 §3 的 toast；导入成功清缓存 + render |
| 23 | 模型页「刷新模型目录」/自动同步 | `GatewayBackend.modelsFor/modelsLoaded/isQoder/isMixed/hasQoderAccount`、`TraeUpstream.fetchModels`、`ModelsRepo.applyFetched/current`、`syncQoderCatalog`→`QoderUpstream.fetchModelListRaw` / `QoderAuthApi.fetchModelCatalog` / `QoderModelCatalog.setDynamic/save/current/freeCount/hasDynamic` | `刷新中…` 置灰、计数文案、结果 toast、原地重建卡片区 |
| 24 | 模型卡「复制」 | `copyToClipboard("modelId", id, "已复制模型 ID: …")` | toast |
| 25 | 记录页 | `UsageStore.all/aggregate`（`UsageRecord` 字段） | 汇总卡 + shareBar + 记录卡 |
| 26 | onResume | `CreditMonitor.stop/listener/currentSnapshot`、`AutoCheckin.shouldAttempt/runAllOnce`、`AutoCheckin.scheduleWindowAlarm` | `已自动补签：{summary}` 或无 toast；快照原地刷 `qoderCreditView` 等 |
| 27 | onPause / onDestroy | `CreditMonitor.listener=null`；复位 4 个 busy 标志 | 无 UI |
| 28 | 保活页「去开启」/「开始自动配置」 | `Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` / `ACTION_REQUEST_SCHEDULE_EXACT_ALARM` / `ACTION_APP_NOTIFICATION_SETTINGS` | 跳系统页；800ms 后回本页重绘取最新状态 |
| 29 | 通知栏「停止」/看门狗唤醒 | `GatewayService` `ACTION_STOP`/`ACTION_WATCHDOG`（非 UI 入口但影响状态显示） | 概览 chip 与按钮随之变 |

---

## 7. Android 图标资源清单（供桌面端找等价 SVG）

`res/drawable/`（7 个矢量 XML，导航图标均 24dp/viewport 24，运行时按 `primary`/`on_surface_variant` tint）：

| 文件 | 用途 | 备注 |
|---|---|---|
| `ic_dashboard.xml` | 概览页签图标 | 需等价 SVG（dashboard/gauge） |
| `ic_account.xml` | 账号页签图标 | person/account |
| `ic_models.xml` | 模型页签图标 | layers/cube list |
| `ic_history.xml` | 记录页签图标 | clock/history |
| `ic_more.xml` | **当前未被引用**（原「更多」页签，注释 36 行仍写 5 Tab） | more/ellipsis；移植默认不用 |
| `ic_launcher_background.xml` | 启动图背景层 108dp/viewport 108，`fillColor=#4F46E5` | indigo 主色底 |
| `ic_launcher_foreground.xml` | 启动图前景层 108dp，白色双向箭头（`#FFFFFF` ×2） | 同时用作通知 `setSmallIcon`（白剪影） |

其它：`mipmap-anydpi/ic_launcher.xml`、`mipmap-anydpi-v26/ic_launcher.xml`（自适应图标配置），`xml/network_security_config.xml`。系统图标 `android.R.drawable.ic_media_pause`（通知「停止」按钮）。

---

## 8. 移植建议

### 8.1 React 组件树建议

```
<App>                      // 提供 styleId/mode → CSS 变量注入 + recreat(=整树重渲染)
 ├ <TopBar>                // 返回链(仅二级页) + title + subtitle + 下边框 1px
 ├ <ContentHost>           // <OverviewPage/> <AccountPage/> <ModelsPage/> <RecordsPage/> | <KeepAlivePage/> <StylePage/>
 │    └ 通用原子组件（对应 helper 区，§1.4/2.x）
       ├ <ScrollPage>       // scroll{}：padding 由风格的 pagePad*，maxWidth=contentMaxDp 居中
       ├ <Card tone=normal|soft|emphasis>
       ├ <SectionTitle>、<Row>、<Caption>、<LabelValue>、<Divider>
       ├ <Text size=FS_*>、<Mono size=FS_*>
       ├ <Chip kind=ok|warn|info>、<StatusChip tone dotOn>、<Dot>
       ├ <ActionButton variant=filled|outlined|full|danger|state=done|muted>   // 含 refreshCheckinButton 三态规则
       ├ <LinkPill overContainer>、<AccountBar>、<SearchInput>、<ShareBar>
 ├ <BottomNav items=Page.entries>          // 4 项，选中胶囊 + tint
 └ <Overlay>
     ├ <ConfirmDialog danger content>       // 唯一弹窗原子，所有弹窗基于它
     ├ <PickerDialog rows fixedHeight=max5*56dp>
     ├ <NumberTimePicker>
     └ <DiagnoseDialog mono scrollable copy>
```

页面级组件对应：`OverviewPage`（状态胶囊/常驻卡/地址+Key卡/启停按钮/Trae卡）、`AccountPage`（CreditsTotalCard + TraeSection + QoderSection + BackendRow + AutoSwitchCard + ExportRow + BatchCheckinCard）、`ModelsPage`、`RecordsPage`、`KeepAlivePage`、`StylePage`。

### 8.2 状态管理建议

- **服务端/进程态**（Rust 侧，前端只读 + 订阅）：`gateway_running`、`should_run`、`has_user_intent`、`api_key`、`service_busy` 不进全局 store，用 Tauri command + event（等价 `running` / `markShouldRun`）。桌面版可删除看门狗/唤醒锁/通知渠道三族逻辑，保留 `shouldRun` 意图语义以免"关了又自己开"。
- **前端全局 store**（Zustand/Jotai 一类）建议切片：
  - `uiStyle`：`styleId`、`modeId`、`isNight`、派生 CSS 变量 + 形态 token；写入即整树重渲染（对应 `recreate()`）。
  - `nav`：`currentPage`、`sub: 'keepalive'|'style'|null`（对应 `showKeepAlive/showStyle` + `STATE_SUB`）。
  - `traeAccounts` / `qoderAccounts`：列表、`activeUid`、`authFailedUid`（按 uid，不按页面）。
  - `credits`：`AccountCredits` 快照（含 `loading`、`at`）、`quotaLoading`、`qoderRemaining`、`checkinBusy`、`qoderClaimBusy`。
  - `service`：`running`、`busyLabel`。
  - `apiKeyVisible`、`modelsLoading`、`qoderCatalogAutoTried`、`autoModelsTried`。
  - `keepAliveChecks`：桌面版重定义检查项，保留 `Item{title,ok,why,autoOpenable}` 形状。
- **"原地更新不重建"要在 React 里天然实现**：把 `quotaApply/monitorApply/modelsRefreshApply/creditViews` 的语义翻译成"局部 state + memoized 子树"，滚动位置由 `<ScrollPage>` 的 ref 保持；**绝不用整页 remount** 表达刷新（React key 不要绑定 refreshCount）。注意 API Key 显隐在 Android 侧是整页 render，桌面版可就地 state 切换。
- 异步守卫与源码等价：`quotaLoading/checkinBusy/qoderClaimBusy/modelsLoading/serviceBusy` 都要有 `finally` 复位（Android 靠 `runCatching` + try/finally 与 onDestroy 兜底，2504-2524、2448-2453、2601-2607、2771-2785）。

### 8.3 样式 token 迁移表（Android → CSS）

| Android | CSS/React |
|---|---|
| `AppStyle.color("primary")` | `var(--tw-primary)`（由 `styleId + isNight` 计算出 19 个变量） |
| `RADIUS_CARD / RADIUS_CONTROL / RADIUS_PILL` | `--radius-card:16px; --radius-control:12px; --radius-pill:100px`（pill 建议 `min(pillRadius, 9999px)`；graphite 的 pillRadius=8 要原样数值） |
| `GAP` | `--gap: 12px`（卡片默认 `marginTop`） |
| `PAGE_PAD_H/TOP/BOTTOM`、`CARD_PAD`、`BAR_PAD_TOP/BOTTOM` | `--page-pad-*`、`--card-pad`、`--bar-pad-*` |
| `CONTENT_MAX_DP` | `--content-max: 640px`（`max-width` + `margin-inline:auto`） |
| `elevation` | `--card-elevation`（`box-shadow: 0 {n}px {2n}px rgba(...)`，NORMAL 卡才有；SOFT/EMPHASIS 为 0） |
| `FS_TITLE/SECTION/CARD_TITLE/METRIC/BODY/VALUE/CAPTION/CHIP` | `--fs-title:22px …`（原 24sp/sp 数值直接作 px 基准；注意 Android `dp/sp` 在桌面按 CSS px 1:1） |
| `card(tone)` | `.card--normal{bg var(--tw-surface);border 1px solid var(--tw-outline-variant);shadow}` / `--soft{bg surface_container,无描边无阴影}` / `--emphasis{bg primary_container}` |
| `shape()` 的 strokeDp（选中 2dp） | `data-selected` → `border-width:2px; border-color:var(--tw-primary)` |
| `setLineSpacing(0,1.12f)` | `line-height:1.12`；`letterSpacing=0.04f` → `letter-spacing:0.04em` |
| 无障碍触控下限 | `min-height:48px`（link/action/行/按钮）；已签状态 chip 允许 `32px`（仅不可点） |
| `dp()` 换算 | 移除（CSS 逻辑像素） |
| 状态圆点实心/空心 | `.dot--solid{bg var(--tw-<tone>)} .dot--hollow{border:2px solid var(--tw-<tone>);bg var(--tw-surface)}` |
| `Toast.LENGTH_SHORT` | 3s 自动消失的 toast 栈（右下角或顶部） |
| `values-night` 资源切换 | `mode=system` 时用 `prefers-color-scheme`；light/dark 强制覆盖（等价 `applyNightOverride`） |
| 字体 `sans-serif-medium` / `Typeface.MONOSPACE` | `font-family: system-ui` + `font-weight:500`；等宽 `ui-monospace, monospace` |
| 图标 tint | 引入 SVG 后用 `currentColor` 或 CSS `filter`，保持"图标+文字同色随选中变" |

---

## 9. 复刻验收要点（自查清单）

1. 4 个页签而非 5；二级页仅 2 个（保活设置、外观风格），返回出口在标题**上方**而非左侧。
2. 概览 3 卡 + 1 胶囊 + 1 满宽按钮；账号页 6 卡 + 5 section + 2 条账户选择条；记录页空态 1 卡；保活页 4 卡；外观页 4 行 + 1 卡。
3. `serviceBusy/quotaLoading/checkinBusy/qoderClaimBusy/modelsLoading` 五处"进行中"文案：`启动中…/停止中…/…/签到中…/领取中…/刷新中…` 与置灰规则逐一对应。
4. 积分数值三态：`…`、`—`、`—（原因）`；合计 `format()` 的整数/一位小数规则；`更新于 HH:mm:ss`。
5. Trae 签到按钮 8 种文案 × 3 种外观（实心 48dp / success 胶囊 32dp / 灰胶囊），`✓` 前缀只在已签。
6. API Key 默认打码 24 个 `•`，明文才可长按选中。
7. 模型卡倍率三态（`免费`/`×n`/`倍率未知`）+ `key 未验证` 后缀 + `context N`；缺 `price_factor` 时**不显示**倍率数字。
8. 保活体检 6 项文案（其中两项恒为未就绪、标 `需手动`），一键配置"一次只跳一个"。
9. 4 套风格 × 亮暗 = 8 张色表、每套 19 键，全部 hex 与 §4.2 一致；对比度 ≥ 4.5:1 作为 CI 校验项复刻 `AppStyle.selfCheck`。
10. 不要移植的死代码：`§0` 第 2 条列出的 15 个符号（关于页/版权/端点/剪贴板导出/Qoder 单号领取按钮/导入目录弹窗/原文弹窗/轮播时间/`quotaValue`/`openLogin`/`confirmRemoveAccount`/`ic_more`）。
