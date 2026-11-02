# TraeWork2API —— 签到与积分子系统分析（移植 Tauri/Rust 用）

分析范围（全部逐行通读）：

- 当前工作区 `/app/src/main/java/com/example/traework2api/` 下 8 个指定文件：`BatchCheckin.kt`(418 行)、`AutoCheckin.kt`(213 行)、`CheckinReceiver.kt`(72 行)、`AutoCheckinReceiver.kt`(32 行)、`QoderCheckin.kt`(320 行)、`QoderCheckinReceiver.kt`(58 行)、`CreditMonitor.kt`(296 行)、`AccountCredits.kt`(451 行)
- 为达"不看 Kotlin 也能重写"的接口精度，补充通读了上述文件直接调用的 API 层：`TraeUpstream.kt`（ug 请求头/签到/用量/判定）、`QoderAuthApi.kt`（campaigns/claim/credits/档位阶梯）、`TraeRegion.kt`（host 表）、`QoderMachine.kt`（身份头常量）、`Gateway.kt`（tokenLock）、`GatewayBackend.kt`（平台常量值）

关键共享机制（Rust 移植必须整体保留语义）：

| 机制 | Kotlin 实现 | 语义 |
|---|---|---|
| `Gateway.locked { }` | `synchronized(Any())`，同一把 `tokenLock`（Gateway.kt:34,49） | Trae refresh token 是**轮换式**的：任何两条路径并发续期同一账号，先换到的会被后换到的作废（甚至触发整条令牌族作废）。所有 Trae 续期/批量签到必须持锁。**Kotlin synchronized 可重入**：`runAll` 持锁期间 `checkinOne` 内再次 `locked` 是安全的；Rust `std/tokio Mutex` 不可重入，见 §9 设计注意 |
| `busy` AtomicBoolean | `BatchCheckin.busy` / `QoderCheckin.busy` 各一个 | 同一平台"手动入口"与"定时入口"互斥；CAS 失败即本轮跳过 |
| `TraeUpstream.lastAuthFailed` | `@Volatile Boolean`，每次 `ug()` 调用前复位 | 区分「token 被拒（401/403/业务码 1001）」与「网络失败」，上层据此出不同文案 |
| `CreditMonitor.stale` | `@Volatile Boolean = true` | 账号身份变化后由 UI 置 true，强制下一轮重拉 |

---

## 1. 签到接口完整细节

### 1.1 Trae（国服 `region=CN`；国际服 SG 已冻结）

Host 取自 `TraeRegion`：

| 区域 | ugHost（签到/用量） | agentHost（聊天） | 是否支持签到 |
|---|---|---|---|
| CN | `https://api.trae.cn` | `https://trae-api-cn.mchost.guru` | `isSupported = (this == CN)` → 支持 |
| SG（国际） | `https://ug-normal.trae.ai` | `https://core-normal.trae.ai` | `isSupported = false` → 批量路径直接过滤掉，标注「不支持」**永不发请求**（SG 的 ug 头是按国服包抓的，未实测） |

`canQueryQuota = ugHost.isNotBlank()`（CN/SG 都 true；判 unsupported 用 `!isSupported || !canQueryQuota`）。

三个端点（均为 **POST**，请求体固定为字符串 `{}`，Content-Type `application/json`）：

| 用途 | 路径（拼在 ugHost 后） | 返回解读 |
|---|---|---|
| 查签到状态 | `/trae/api/v2/ug/checkin_credits/status` | 解析 `checked_in`/`credits`/`enable` |
| 执行签到(claim) | `/trae/api/v2/ug/checkin_credits/claim` | 取整型业务码 `code`（缺省 -1） |
| 查积分余额 | `/trae/api/v2/pay/ide_user_ent_usage` | 解析 `usage_summary` |

请求头全集（`ug()`，TraeUpstream.kt:212-234，逐条对齐实测可签到成功的参考实现 luckymiaow/trae-mate）。`{token}` 为 access token、`{uid}` 为账号 uid：

```
Authorization: Cloud-IDE-JWT {token}     ← 注意自定义 scheme，非 Bearer
Accept: */*
Content-Type: application/json
User-Agent: VSCode 1.107.1 (TRAE SOLO CN)
X-Market-Client-Id: VSCode 1.107.1
X-Market-User-Id: marketUserIdFor(uid)   ← 派生，见下
X-Device-Id: deviceIdFor(uid)            ← 派生，15 位纯数字
Vscode-Sessionid: sessionIdFor(uid)      ← 派生，64 位 hex
X-Lgw-Req-Sdk-Type: 3
Package-Type: stable_cn
X-Lscbd-Aid: 787976
X-Lscbd-Platform: windows
App-Version: 0.1.45
X-Tt-Trace-Id: 00-{16位无横线hex}-01     ← 每次随机（uuid 去横线取前 16）
X-Request-Id: {随机 UUID v4}
Sec-Fetch-Dest: empty
Sec-Fetch-Mode: no-cors
Sec-Fetch-Site: none
X-User-Region: CN                        ← region.userRegionHeader，空时填 "CN"
```

三个"配套设备标识"缺一即被上游判为共用设备（→ 9074）。派生算法（`seededStream`）：反复计算 `SHA-256( "{salt}:{uid}" 的 UTF-8 字节 || i 的大端 4 字节 )`，把每轮 32 字节摘要串联，直到够 `nbytes`，截断：

| 标识 | salt | nbytes | 成型规则 |
|---|---|---|---|
| `deviceIdFor` | `"devid"` | 16 | 取 15 字节，每位输出 `((b & 0xFF) % 10)` 的十进制数字字符 → 15 位纯数字（hex 格式会被上游拒） |
| `sessionIdFor` | `"sess"` | 32 | 全部转小写 hex（64 字符） |
| `marketUserIdFor` | `"market"` | 16 | 先修版本/变体位：`b[6] = (b[6]&0x0F)\|0x40`、`b[8] = (b[8]&0x3F)\|0x80`，再格式化为标准 UUID v4（8-4-4-4-12 小写 hex） |

同一 uid 永远得到同一套（确定性、不漂移）；不同账号天然隔离（批量签到可行性的前提）。

HTTP/业务层判定（`ug()` 统一封装，TraeUpstream.kt:235-248）：

1. 每次调用先置 `lastAuthFailed = false`
2. HTTP 401 或 403 → `lastAuthFailed = true`，返回 null
3. HTTP 非 2xx，或响应体为空白 → 返回 null（`lastAuthFailed` 保持 false = 网络/上游异常）
4. 响应体 JSON 解析失败 → 返回 null
5. 业务根对象 `code == 1001` → `lastAuthFailed = true`，返回 null（实测：token 无效时 status 接口回 **HTTP 200 + code=1001 + enable=false**，属于鉴权失败被伪装成"未开放"）
6. 其余情况返回整个 JSON 根对象

状态解析 `parseCheckin(root)`（可离线自检的纯函数）：

- `root.code == 1001` → 返回 null（同上理由）
- `checkedIn = root.checked_in`（布尔，缺省 false）
- `credits = root.credits`（long，缺省 0）
- `enable = root.enable`（布尔，缺省 false）
- 实测真实返回（2026-09-25 dump）：`{"checked_in":true,"code":0,"credits":150,"did_checked_in":false,"enable":true,"extra_credits":50,"message":"success"}`；多余字段忽略、缺字段不抛异常。

claim 结果 `claimCheckin`：返回 `root.optInt("code", -1)`；`-1` 表示响应里根本没有 code 字段。**业务失败也回 HTTP 200 + 业务体**，所以 code 单独不可作成功判据。

成功唯一判据是合取 `checkinSucceeded(before, after, code)`（TraeUpstream.kt:386-387）：

```rust
code == 0 && after.is_some() && after.checked_in && before.map(|b| b.checked_in) != Some(true)
```

即：成功码 ∧ 重查得到状态 ∧ 现在已签 ∧ 之前未签。逐条件动机（自检钉死，TraeUpstream.kt:726-733）：只看 code 会被"HTTP 200 + 业务拒绝"骗；只看跃迁在本地陈旧状态/`code=9004`（今天已签过、积分不变）时误报；`after=null` 一律判未成功；`code=null`（网络失败）或 `-1`（字段缺失）均不算成功；before 已 `checkedIn` 不算本次成功。

已知业务码语义表（历史踩坑结论，移植时照抄）：

| code | 含义 | 来源说明 |
|---|---|---|
| 0 | 成功 | 实测 |
| 1001 | 鉴权失败（token 无效） | status 用它且 HTTP 200 |
| 9004 | 缺设备头 → 参数校验失败（`The submitted order parameters are incorrect`），曾被误读为"今日已签到" | 2026-09-26 实测 |
| 9074 | "操作太过频繁"：设备身份不独立（多账号共用设备 id）被按设备维度限流；修法是补齐三个派生标识 | 参考实现 trae_instance.rs:111 |
| -1 | 响应缺 code 字段 | |
| null | 网络不通 / 非 2xx / 解析失败 | 由 `lastAuthFailed` 进一步二分 |

其它实测事实：重复 claim **幂等**（不扣分、状态不变），只浪费请求，因此 UI 在 `checkedIn=true` 时仍禁点。上游按**设备维度**限制每天领一次。

带自愈的高层封装（批量/单账号路径实际调用的是这两个）：

- `fetchCheckinHealing(ctx, cred)`：先用 `cred.accessToken` 查；返回 null 且 `lastAuthFailed` → `Gateway.locked { TraeAuthApi.forceRefreshTokenFor(ctx, cred) }` 强制续期 → 用新 token 再查一次。（症状：token 未到期但已被另一处登录顶掉，"刚签到成功突然变没权限"）
- `claimCheckinHealing(ctx, cred)`：同构，失败后强制续期再 claim 一次。返回 null 时调用方**必须**用 `lastAuthFailed` 二分文案（"token 失效需重登" vs "网络不通"）。

用量 `fetchUsage(tok, uid, region)`（TraeUpstream.kt:314-321）：`region.canQueryQuota` 为 false → null；POST usage 端点取 `root.usage_summary`；`total_amount` 缺省 -1，<0 → null；返回 `(total_amount - consumed_amount).toLong()`（截断取整，实测 5100−1931.46=3168.54→3168）。字段含义：`total_amount`=当前所有未过期积分包 `credits_limit` 之和（随包过期浮动，8350→5100 见过）；`consumed_amount/total_amount` 与上游 `consumption_ratio` 逐位吻合。

### 1.2 Qoder（国内 mode=`cn` / 国际 mode=`global` 同一套协议）

Host（`openApiBase(mode)`，与聊天网关 `gateway.qoder.com.cn`/`api2.qoder.sh` 是两套域名、勿混）：

| mode | OpenAPI base |
|---|---|
| `cn` | `https://openapi.qoder.com.cn` |
| `global` | `https://openapi.qoder.sh` |

端点：

| 用途 | 方法与 URL | 请求体 |
|---|---|---|
| 活动列表 | `GET {base}/sash/api/v1/me/campaigns?forceRefresh=true` | 无 |
| 领取一条 | `POST {base}/sash/api/v1/me/campaigns/{campaignId}/claim` | 固定空对象 `{}`（幂等） |
| 积分余量 | `GET {base}/api/v2/quota/usage` | 无 |

鉴权：`Authorization: Bearer {jobToken}`（jobToken 来自 PAT exchange/refresh，skew 内自动续，由 `validJobTokenFor` 负责；批量续期在 `QoderAuthApi` 内部按账号串行，走 `Gateway.lockedQoder`——独立的一把 `qoderLock`，签到链路本身**不持 Trae 的 tokenLock**）。

身份头**两档**（`getWithIdentity`/`postClaimRaw`，差异只在 desktop 档追加头）：

| 头 | minimal 档（默认） | desktop 档 |
|---|---|---|
| Authorization | `Bearer {jt}` | 同左 |
| Accept | `application/json` | 同左 |
| Content-Type | claim POST 时 `application/json` | 同左 |
| User-Agent | `traework2api` | `Qoder` |
| Cosy-Version | `1.0.1` | `id.version ?: "1.1.69"` |
| Cosy-ClientType | `10`（`QoderMachine.CLIENT_TYPE`，两档恒发 10） | `10` |
| Cosy-MachineId | 不发 | `sha256Hex(持久化 UUID)` 或抓包回填值 |
| Cosy-MachineToken | 不发 | 官方无 umid 客户端约定：**等于 machineId** |
| Cosy-MachineOS | 不发 | 身份对象字段 |
| Cosy-MachineType | 不发 | 仅当非空白才发 |
| Cosy-MachineHostname | 不发 | 仅当非空白才发 |

档位阶梯（`loadCampaigns`，重要风控逻辑）：

1. 起始档位读 prefs `qoder_campaign_variant_{mode}`（只认 `desktop`/`minimal`，否则默认 `minimal`）
2. 阶梯 = 记住的是 desktop → `[desktop]`；否则 `[minimal, desktop]`
3. 当前档请求成功且**活动非空**：若这档 ≠ 起始档则 `rememberVariant` 落盘；返回
4. 当前档返回**空列表**（HTTP 200 静默隐藏是已知服务端行为，`showCampaign=false + campaigns=[]`）：还有下一档就升级复核；已是末档则按空返回
5. 抛异常且消息含 `"401"` 且尚未重试过：`refreshJt()` 换新 token，**同一档**再来一次
6. 末档仍异常：返回 `CampaignLoad(null, variant, 错误消息)`

响应错误：HTTP 非 2xx → 抛 `IOException("Qoder {code}: {body前200字符}")`。

活动列表解析 `parseCampaigns`：

- 根没有 `campaigns` 数组时尝试下钻一层 `data` 对象
- id 键名三选一按序回落：`campaignId` → `campaignKey` → `id`；全空白则丢弃该条
- `showCampaign`：先查下钻后的 root，再查原始 root，都没有 → null（**false 不能当"真没活动"**，桌面档问出来的空才是真空）
- 每条取 `claimStatus`、`actionType`、`benefit.kind`、`benefit.amount`（benefit 可为 null → 0）
- `claimable = id 非空白 && claimStatus.equalsIgnoreCase("CLAIMABLE")` —— 不再按 actionType 过滤：`VIEW_DETAILS` 类也领（领了不发积分，但状态必须与桌面端一致，否则用户看到"电脑能领、程序不能领"）
- `grantsCredits = benefitAmount > 0`
- `startAt/endAt` 是 unix **秒**（注释明示是社区踩过的坑，解析时不要按毫秒）

claim 响应解析 `parseClaim`：

- `root.status` 为空白且存在 `data` 对象 → 改用 `data`
- `claimed = status.equalsIgnoreCase("CLAIMED")`；`replayed = boolean(replayed, false)`；`amount = data.benefit.amount（long，缺省 0）`
- 返回 `(claimed && !replayed, claimed && !replayed ? amount : 0)` —— replayed=true 表示今天已领过，判"未新领"

积分余量 `fetchCredits(jt, mode)`（QoderAuthApi.kt:298-323，字段名国内外有差异所以**全部累加**）：

- 发 `GET {base}/api/v2/quota/usage`，档位固定 minimal（注释：与桌面端一致指 Cosy-ClientType:10）
- root 不含 `userQuota`/`personalQuota`/`isQuotaExceeded` 任一键且含 `data` → 下钻 `data`
- `remaining = Σ(obj.remaining)` 对键 `userQuota, personalQuota, addOnQuota, addonQuota, extraQuota`（各自可为缺省跳过）
- 对 `orgResourcePackage, organizationQuota`：仅当 `available`（缺省 true）才累加其 `remaining`
- `exceeded = root.isQuotaExceeded`（缺省 false）
- 结果 `remaining.coerceAtLeast(0.0)`；异常 → null
- `QCredits.hasCredit = !exceeded && remaining > 0.0`（余 0 不算有额度；`exceeded=true` 一律不算）

领取编排 `claimDailyForCred` 的完整状态机与**全部文案**（QoderAuthApi.kt:751-803）：

| 分支 | status | amount | message |
|---|---|---|---|
| 拿不到有效 jobToken（含空白） | `fail` | 0 | `没有有效 token，请重新粘贴 PAT 登录` |
| 列表加载失败（load.list=null） | `fail` | 0 | `拉取活动失败：{load.error ?: "未知错误"}` |
| 无可领活动（详见下） | `already` | 0 | 四种（见下） |
| 逐条 claim：至少 1 条新领且总额>0 | `ok` | Σamount | `""` |
| 至少 1 条新领但全是 0 额度（查看类） | `ok` | 0 | `领取 {fresh} 条（无 Credits）` |
| 无新领 | `already` | 0 | `lastErr` 存在时 `领取未生效：{lastErr}`，否则 `今日已领取` |

无 claimable 时四种 already 文案（`creditCampaigns = 有 Credits 奖励的活动条数`）：

1. `creditCampaigns > 0 && 全部活动 CLAIMED` → `已领取 {creditCampaigns} 条`
2. `creditCampaigns == 0 && 列表非空` → `无积分活动（{campaigns.size} 条均为查看类，不发 Credits）`
3. `列表非空`（有其他未 CLAIMED 但不可领的）→ `无待领活动（{campaigns.size} 条均已处理）`
4. 列表为空 → `今日无可领活动（上游返回空列表）`

实测事实：「每天领 100 Credits」活动为 `CLAIM_BENEFIT`，**每日 10:00（UTC+8）重置**；列表随日期滚动动态出现（凌晨可能一条 CLAIM_BENEFIT 都没有），所以调用方必须靠定时重试捡漏（对应 `AutoCheckin` 的 10:05 开窗）。逐条 claim 单条异常只记 `lastErr` 继续下一条（一人失败不拖垮整批）。`claimDailyAll` 对全部账号串行，账号间隔 `Thread.sleep(600)`（注意：与 Trae 批量的 800ms 不同值）。

---

## 2. BatchCheckin —— 批量签到编排

`object BatchCheckin`（BatchCheckin.kt），SharedPreferences 文件名 `traework2api_checkin`。

作用域澄清（用户任务里问"对全部账号（国服/国际/Qoder）逐个签到的顺序"）：**BatchCheckin.runAll 只覆盖 Trae 平台账号**，且第一步就 `filter { it.region.isSupported }` —— 国际服账号被过滤出候选集（一个请求都不发，界面由别处标"不支持"）；Qoder 账号完全走平行的 `QoderCheckin.runAll`。两条链路唯一汇合点在 `AutoCheckin.runAllOnce`（跨平台"开窗补签"，见 §3.3）。

三个入口共用 `runAll` 一条编排：账号页手动按钮、每日定时闹钟（CheckinReceiver）、进 App 补签（maybeCatchUp）。

`runAll(ctx): BatchResult`（阻塞，必须后台线程）逐账号流程：

1. 全程持 `Gateway.locked`（含每个账号内部的续期再锁 = Kotlin 可重入；Rust 移植注意见 §9）
2. `metas = TraeAuthStore.list(ctx).filter { it.region.isSupported }`；按 list 顺序逐个
3. `i > 0` 时先 `Thread.sleep(800ms)`（PER_ACCOUNT_DELAY_MS；上游按设备维度风控，连发易撞 9074）
4. 每账号 `checkinOne`，单账号任何异常捕获后转 fail 结果——**错误隔离**：一人失败不拖垮整批
5. 结束后落盘 `last_run_date = today()`（`yyyy-MM-dd`，`SimpleDateFormat(Locale.US)`，设备本地时区）、`last_results = JSON 数组`
6. 通知由调用方发（定时路径发 `notifyIfScheduled`，手动路径不发——用户就在屏幕前）

`PerAccount(uid, nickname, status, credits, message)`，status ∈ `ok / already / disabled / unsupported / fail`；`credits` 成功/已签时为今日可领积分数，其余 `-1`。持久化 JSON 字段名逐字：`uid` `nickname` `status` `credits` `message`。

`BatchResult(results, date)`；`allOk = results 非空 && 每个 ∈ {ok, already, disabled}`（`disabled`=无签到权限也算"处理完了"；**unsupported 不在 allOk 白名单内**——理论上批量路径过滤后不会出现）。

`checkinOne` 判定顺序与全部 message 文案（BatchCheckin.kt:211-257）：

| 序 | 条件 | status | credits | message |
|---|---|---|---|---|
| 0 | `!region.isSupported 或 !canQueryQuota` | unsupported | -1 | `该区域未支持签到` |
| 1 | `loadByUid` 返回 null | fail | -1 | `凭据读取失败` |
| 2 | accessToken 空白 或 `expiredWithin(expiresAt, 3600)`（exp≤0/null 当已过期）→ 走 `Gateway.locked { ensureFreshTokenFor(ctx, cred, skewSec=3600) }`，续回空白 | fail | -1 | `token 已过期且续不回来，需重新登录` |
| 2' | 续期成功 | 继续 | | 只刷新内存副本（入口内部已写回库；**绝不自己拿快照 refreshToken 换**——并发旧令牌重放会作废整条令牌族，即"签到成功后 token 全废"根因） |
| 3 | `fetchCheckinHealing(before)` = null | fail | -1 | `拉取签到状态失败（网络或上游异常）` |
| 4 | `before.checkedIn` | already | before.credits | `""` |
| 5 | `!before.enable` | disabled | -1 | `该账号无签到权限` |
| 6 | claim → 再查 after → `checkinSucceeded(before, after, code)` | ok | `after!!.credits` | `""` |
| 7 | 否则 `after?.checkedIn == true`（跃迁已发生但合取没过，如 code 异常） | already | after.credits | `""`（按已签归档，不报错） |
| 8 | `code == null` | fail | -1 | `lastAuthFailed ? "token 被服务端拒绝，需重新登录" : "网络不通，签到请求未送达"` |
| 9 | `code == 9074` | fail | -1 | `上游判定操作过于频繁（9074），请稍后手动重试` |
| 10 | 其他 code | fail | -1 | `签到未生效（code={code}）` |
| 11 | 任何 Exception | fail | -1 | `签到异常：{e.message ?: 异常类名}` |

状态→人话 `statusLabel`（账号页每行；ok 行含正数加分号）：

```
ok          → "✓ 签到成功 +{credits}"
already     → "已签到"
disabled    → "未开放签到"
unsupported → "该区域未支持签到"
fail/其他    → "失败：{message}"
```

读回接口：`lastResults(ctx)` —— `last_results` 或 `last_run_date` 缺失/JSON 损坏 → null（每元素按字段名取，status 缺省 "fail"、credits 缺省 -1）；`lastRunToday(ctx) = last_run_date == today()`。

续期入口契约（从调用侧归纳，TraeAuthApi 内实现）：`ensureFreshTokenFor(ctx, cred, skewSec)` 到期前 skew 秒内自动续并写回库返回新 access token（失败 null）；`restartWithValidTokenFor(ctx, cred, skewSec)` 同族入口（CreditMonitor/AccountCredits 用）；`forceRefreshTokenFor(ctx, cred)` 无条件强续。**写回一律走 `TraeAuthStore.updateTokens` 语义，不碰 active 指针**——`TraeAuthStore.save` 会把保存账号设为当前账号，后台批量续期会偷偷切号（设计决策注释明确）。

`busy` 互斥语义：`maybeCatchUp` 与 CheckinReceiver 都 `compareAndSet(false,true)`，抢不到即跳过本轮（两个入口撞同一时刻时手动优先；结果由先到者产出）。

selfCheck 钉死的不变量（移植后建议原样做成 Rust 单元测试，BatchCheckin.kt:386-417）：exp=0/null 判过期；未来 7200s 的 exp 在 skew=3600 下判未过期；结果 JSON 往返全等（uid 是主键，串了会把 A 结果显示给 B）；五种 status 文案齐全；allOk 判据四例（全成功 true / 全 already true / 有 fail false / 空列表 false——空判 false 防"0 账号时误去重"）。

---

## 3. 定时调度与补签（BatchCheckin 调度 + CheckinReceiver + AutoCheckin + AutoCheckinReceiver）

项目里"每日定时 + 没跑成下次打开补签"的完整语义分布在**两套并行机制**：

- **用户可配置时刻**的每日闹钟批量签到：`BatchCheckin`（Trae，§3.1/3.2）与 `QoderCheckin`（Qoder，§4，结构完全同构）。
- **固定开窗时刻**的跨平台补签：`AutoCheckin` + `AutoCheckinReceiver`（§3.3），把 Trae 与 Qoder 当天没签/没领的补齐。

### 3.1 BatchCheckin 的配置项与调度语义

存储（prefs 文件 `traework2api_checkin`）：

| key | 类型 | 默认 | 说明 |
|---|---|---|---|
| `schedule_enabled` | Boolean | **false**（默认关，关着不挂任何闹钟） | 每日定时开关 |
| `schedule_time` | String `"HH:mm"` | `"08:00"`；正则 `^\d{1,2}:\d{2}$` 校验失败回落默认 | 时刻（设备本地时区） |
| `last_run_date` | String `yyyy-MM-dd` | 无 | 上次批量跑完日期（本地时区），补签去重用 |
| `last_results` | String(JSONArray) | 无 | 上次结果（字段见 §2） |

`timeMinutes`：split ":" → h coerce 0..23、m coerce 0..59 → `h*60+m`。

- `enable(ctx, time)`：写 enabled=true + time → 立刻 `scheduleNext`
- `disable(ctx)`：写 false → `cancelAlarm`
- `setTime(ctx, time)`：只存值；仅当开关开着才重挂闹钟（关着改值、等 enable 时生效）

`scheduleNext(ctx)`（幂等，重复调用只更新触发时间）：

1. 开关关 → 直接 return
2. 对齐点：本地时区 Calendar，设到**今天或明天的 HH:mm:00.000**；`timeInMillis <= now` 则 `+1 DAY_OF_YEAR`
3. 单次触发闹钟：`RTC_WAKEUP`；API<31 或 `canScheduleExactAlarms()` → `setExactAndAllowWhileIdle`，抛异常降级 `setAndAllowWhileIdle`；无精确权限 → 直接非精确并 warn「精确闹钟未授权，签到定时降级为非精确（Doze 下可能延迟数小时）」
4. PendingIntent：`getBroadcast(ctx, 8792, Intent→CheckinReceiver action="com.example.traework2api.CHECKIN_ALARM", FLAG_IMMUTABLE|FLAG_UPDATE_CURRENT)`
5. **无周期语义**：一次一条，靠"每次触发后自续"滚动（四条生命周期：开启时挂 → 触发自续 → 进 App 自愈重挂 → 关闭即取消）

`cancelAlarm`：同 request code 8792 重建 PendingIntent 并 `am.cancel`。

已接受边界：**不做 BOOT receiver**——重启手机闹钟全丢，由"进 App 补签"兜底。桌面版必须重新设计（§9）。

**独立 Receiver 的红线**（注释明确禁止合并进 GatewayService）：网关 `onStartCommand` 对任何非 STOP/WATCHDOG intent 无条件 `markShouldRun(true)`，会把用户手动停掉的网关复活；反之加 CHECKIN 分支会让签到继承网关开关；且 Android 12+ 闹钟拉前台服务受限。Rust 对应物 = `tokio::sync::Mutex` + 后台任务 + 桌面通知，天然无这些问题。

### 3.2 CheckinReceiver 执行模型与"补签/通知"语义

`onReceive`（action 不匹配直接忽略）：

1. 若运行时开关已被关（投递路上关的，cancelAlarm 拦不住已发出闹钟）→ 记日志忽略，什么都不做
2. `goAsync()` 延长 receiver 存活（~10s 窗口），实际干活开线程 `checkin-alarm`（goAsync 只是给进程保温）
3. `PARTIAL_WAKE_LOCK "traework2api:checkin"`，non-refcounted，`acquire(120_000ms)`（覆盖 N 账号 × (status+claim+status) × 网络超时的上界，超时兜底释放防漏放）
4. `busy.compareAndSet` 失败 → 跳过本轮（手动正在进行，结果由手动路径产出）
5. 成功抢锁：`runAll` → `notifyIfScheduled` → 释放 busy
6. finally：`scheduleNext`（**触发后自续明天的闹钟**）→ 释放 wakelock → `pending.finish()`

### 3.3 AutoCheckin —— 固定开窗的跨平台补签

`object AutoCheckin`，prefs 文件 **`traework2api_prefs`**（与 BatchCheckin 不同文件！）。

| key | 类型 | 格式/默认 | 说明 |
|---|---|---|---|
| `auto_checkin_done` | String(JSON object) | 默认 `"{}"`；形如 `{"trae:{uid}":"2026-10-25", "qoder:{uid}":"..."}` | **按账号**记"处理完到哪天"；键必须带平台前缀（自检钉死：`"trae:abc" != "qoder:abc"`，防 Trae/Qoder 撞 uid 互顶） |
| `auto_checkin_last_try` | Long | 0 | 上轮尝试时间戳（毫秒） |

常量：`RETRY_COOLDOWN_MS = 600_000`（10 分钟）。设计动机（注释）：活动每天 10:00(UTC+8) 才开，若 9:50 打开 App 记"今天已跑过"，10 点后就永远不领 —— 所以**不能按天记一次，要按账号记 + 允许当日多次重试**。

开窗语义（**新加坡时区**，`TimeZone("Asia/Singapore")`）：`WINDOW_HOUR=10`、`WINDOW_MINUTE=5`（留 5 分钟余量）。`nextWindowAt(now)`：SGT 今天或明天的 10:05:00.000（`<=now` 顺延一天）。闹钟：`setAndAllowWhileIdle`（**故意非精确**：精确闹钟要单独权限，领取窗口有十几分钟余量差几分钟无所谓），request code `8801`，PendingIntent broadcast → `AutoCheckinReceiver`，action `com.example.traework2api.AUTO_CHECKIN`。

三条纪律（决定实现形态）：

1. 按账号记"今天已处理"（新加账号当天会被补上；按天记会导致新号当天永远漏）
2. 只补没签的：先查状态，`checkedIn` 或 `enable=false` 直接 `markDone` 跳过，**绝不硬发 claim**
3. 失败不影响 App：整段 `runCatching`，网络/令牌问题只写日志，账号留到下轮重试（**不 markDone**）

核心函数：

- `pending(ctx)`：遍历 `TraeAuthStore.list` → `("trae", uid)`，遍历 `QoderAuthStore.list` → `("qoder", uid)`，凡 `doneMap[key("{plat}:{uid}")] != today()`（`today()` 同样是 `yyyy-MM-dd` Locale.US 本地时区——注意 done 日期按**设备本地**记，开窗按 SGT，代码即如此，移植照抄）的都算待处理。一旦确认"已签/无权限/已领取"就 markDone → 后续轮**一个请求都不发**（用户要求"只触发未签到的"）
- `markDone(ctx, key)`：读整 map→put(key, today())→整串写回（只动该键）
- `isDone`：`doneMap[key] == today()`；`doneToday(platform, uid)` 供 UI 零请求显示「已签到」
- `alreadyRanToday = pending().isEmpty()`
- `shouldAttempt = pending() 非空 && now - last_try >= 10min`
- `requestRunSoon(ctx)`：`last_try 置 0` 清冷却 —— 新登录/新粘贴账号必须立刻可补签（用户加完号就切走，等不到下一轮）
- `runAllOnce(ctx): String?`（AutoCheckinReceiver 到点调用；进 App 场景由别处按 shouldAttempt 调）：
  1. `!shouldAttempt` → null；先写 `last_try = now`
  2. Trae 部分（`plat=="trae"` 逐项）：loadByUid；accessToken 空白→跳过（不 markDone）；`fetchCheckinHealing` null→跳过；`checkedIn || !enable` → markDone；`claimCheckinHealing` 返回 null→跳过；**成功判定为 `code in 200..299`**——注意：与 BatchCheckin 的 `checkinSucceeded` 合取判据不同，且实测上游成功码是 `0`。后果是正常签到成功的 code=0 不会命中 markDone/parts，但该账号下一轮 `fetchCheckinHealing` 看到 `checkedIn=true` 会走 markDone 分支收敛（行为最终一致；移植时建议统一用 `checkinSucceeded` 或把成功区间并上 0，文档如实记录现状供决策）
  3. Qoder 部分：pending 中存在任何 `"qoder"` 项 → 调**整批** `QoderCheckin.runAll(ctx)`（复用定时领取：含 600ms 间隔、结果落盘、markClaimed），对结果里每条 `status ∈ {ok, already}` → `markDone("qoder:{uid}")`（失败留下轮）
  4. parts 均空 → null；摘要 = `parts.joinToString(" · ")`，逐条文案：Trae 成功 → `Trae {nickname或uid} 已签到`；Qoder → `Qoder ` + 全部结果 `statusLabel` 用 `；` 连接
- `AutoCheckinReceiver`：goAsync + 线程 `auto-checkin-alarm`（**无 wakelock**，与 CheckinReceiver 的差异）→ `runAllOnce` → finally `scheduleWindowAlarm`（闹钟一次性，必须自续）。异常只 warn。

### 3.4 通知语义汇总

定时路径**只对"真的有新增成功"发通知**：`okCount == 0`（全失败/全已签/无权限）直接 return —— 避免每天白响。手动路径从不发通知。通知不依赖网关渠道，独立渠道。

---

## 4. QoderCheckin + QoderCheckinReceiver —— Qoder 侧批量领取

`object QoderCheckin`（QoderCheckin.kt），与 BatchCheckin **结构同构但独立**（各一套 prefs key/闹钟/通知/渠道/wakeLock tag/request code），互不参与（Qoder 不走本地转发、不持 Trae tokenLock；Qoder 续期在 QoderAuthApi 内部按账号串行 `lockedQoder`）。

prefs 文件 **`qoder_checkin`**：

| key | 类型 | 默认 | 说明 |
|---|---|---|---|
| `schedule_enabled` | Boolean | false | 每日定时开关（同 BatchCheckin 语义） |
| `schedule_time` | String | `"09:00"`（正则同 `^\d{1,2}:\d{2}$`） | 本地时区 HH:mm |
| `last_run_date` / `last_results` | String | 无 | 同 BatchCheckin；但结果 JSON 字段名是 `name`/`amount`（Trae 用 `nickname`/`credits`），`amount` 缺省 0 |
| `qoder_claimed` | String(JSON object) `{uid: "yyyy-MM-dd"}` | 默认 `"{}"` | **按账号**记今天领没领（见下） |

闹钟/接收器差异：`ALARM_REQUEST=8793`、action `com.example.traework2api.QODER_CHECKIN_ALARM`、receiver `QoderCheckinReceiver`（wakeLock tag `traework2api:qoder-checkin`，其余执行模型与 CheckinReceiver 逐行同构：goAsync/2 分钟 wakelock/busy CAS/runAll/notifyIfScheduled/finally scheduleNext 自续）。`scheduleNext` 里 API31 精确闹钟预检与降级逻辑逐行对齐 BatchCheckin（warn 文案为「精确闹钟未授权，Qoder 定时降级为非精确（Doze 下可能延迟）」）。

`maybeCatchUp(ctx)`：与 BatchCheckin 逐行同构（enabled→先 scheduleNext 自愈；last_run_date==today 且 lastResults.allOk → 跳过；否则 busy CAS + runAll）。

`runAll(ctx)`：**不持 tokenLock**。`QoderAuthApi.claimDailyAll(ctx)`（内部已含 600ms 账号间隔与逐账号错误隔离）→ 映射 `PerAccount(uid, name(库内显示名缺省 uid), status, amount, message)`；每个 `status ∈ {ok, already}` 都 `markClaimed(ctx, uid)`（注释指出 0.7.27 曾有"说明写两种都记、代码只记 ok"的偏差，此处已对齐 → 领过/确认无可领都算收工，不再每天补跑）→ 落盘 last_run_date/last_results → 返回 BatchResult。

`allOk = 非空 && 每项 ∈ {ok, already, unsupported}`。

按账号已领标记（与整批 last_results 解耦——单账号卡片上单独点"领取"不经过 runAll，若只信 lastResults，按钮会一直显示"领取"）：

- `markClaimed(uid)`：读 `qoder_claimed` map → `put(uid, today())` → 写回
- `setClaimed(ctx, uid, claimed)`：按**服务端事实**改写本地标记（切号后用 `QoderAuthApi.claimedTodayFor` 只读校准：true=已领完/false=还有可领/null=查不到不改显示）；claimed=false 时 `remove(uid)`
- `claimedToday(ctx, uid)`：`map[uid] == today()`，损坏/缺省 false

通知（notifyIfScheduled）：`okCount==0` 不发。渠道 id `qoder_checkin`，名称 `Qoder 领取结果`，描述 `Qoder 每日定时领取 Credits 的成功通知`，IMPORTANCE_DEFAULT，无角标/无声/无振动；`NOTIFICATION_ID=8794`；标题 `Qoder 每日领取完成`；单行内容 `{okCount} 个账号领取成功，共 +{Σamount} Credits`；BigText `{okCount} 个账号领取成功：{明细}`，明细 = ok 项 `joinToString("、") { "{name} +{amount}" }`；CATEGORY_EVENT + autoCancel。

`statusLabel`（Qoder 版）：

```
ok && amount > 0 → "✓ 领取成功 +{amount}"
ok && amount = 0 → "✓ 已领取（{message}）"   // message 空则不带括号；查看类活动不发 Credits，禁写 "+0" 骗人
already          → message 非空 ? message : "今日已领取"
unsupported      → "该区域/活动暂不可领（见消息）"
fail/其他         → "失败：{message}"
```

selfCheck 与 BatchCheckin 同构（往返、四态文案、allOk 四例）。

---

## 5. CreditMonitor —— 转发后余额监测与自动换号

`object CreditMonitor`（CreditMonitor.kt），prefs 文件 **`traework2api_prefs`**。

| key | 类型 | 默认 | 说明 |
|---|---|---|---|
| `auto_switch_enabled` | Boolean | **true**（用户 2026-10-25 明确保留） | 自动换号开关 |
| `auto_switch_threshold` | Long | 5，读取与写入都 `coerceAtLeast(5)` | 见底阈值；UI 只有 5/10/15/20 四档；老版本存的 0 读时抬回 5（否则四档全不亮像坏了），**下限固定 5** |
| `monitor_interval_sec` | — | — | 常量 `KEY_INTERVAL` 仍定义，**已无任何读取点**（定时轮询被删，见下），可移植为兼容保留或直接删除 |

结构常量：`DEFAULT_INTERVAL_SEC = 0`（KDoc 叙述里"默认 60 秒"是历史值——**实际值必须为 0**，selfCheck 钉死 `DEFAULT_INTERVAL_SEC == 0`：禁止一切定时轮询。历史教训：曾为 2 秒轮询 `ide_user_ent_usage`，8 小时 14400 次，上游按刷接口处理 → token 被判废，症状"突然查不到积分/没有签到权限"）。

`FORWARD_POLL_MIN_MS = 60_000`（转发后刷新节流窗口；selfCheck 钉 `>= 60_000`）。

**唯一刷新入口** `noteForwarded(context, backend: String, uid: String?, urgent: Boolean = false)`（backend 取 `GatewayBackend` 常量 `"trae"`/`"qoder"`）：

1. **每平台各自节流**（互不压制）：读 `@Volatile lastTraePollAt / lastQoderPollAt`；`!urgent && now - lastAt < 60_000` → 直接 return；通过则**立即写入时间戳**（进窗即记，不等查询完成）
2. `urgent=true`（本次转发**失败**）跳过 60 秒节流——"不能让用户干等节流窗口才换号"；成功时仍节流（成功说明余额还够）
3. 起一次性守护线程 `credit-after-forward`（`isDaemon=true`，无队列无并发多 tick；整段 runCatching）：
   - `backend=="qoder"` → `pollQoder(app, uid, forwardFailed=urgent)` → `Snapshot(null, null, qUid, remaining, autoSwitch, threshold)`
   - 否则 → `pollTrae(...)` → `Snapshot(tUid, bal, null, null, ...)`
   - 置 `latest`、`stale=false`、`main.post { listener.onSnapshot(snap) }`
4. 用户规则逐字落实："用 trae 只扫描 Trae 当前账号 / 用 qoder 只扫这一个 / 没有请求模型的时候就停止扫描"，平时 0 额外请求。

`start(ctx)` = 只记 appCtx + 一条日志；`stop()` = 空方法。保留空实现只为不破坏调用点（幂等、可重复调）。**没有任何后台线程**，"stop 语义"即彻底无状态可停。

`Snapshot(traeUid, traeBalance: Long?, qoderUid, qoderRemaining: Double?, autoSwitch, threshold)`；`currentSnapshot()` 返回内存 `latest`（进页面立即可见，不等查询）。

`pollTrae(ctx, wantedUid, forwardFailed) -> (uid, balance)`：

- uid = wantedUid 非空白否则 `activeUid`；loadByUid null → `(uid, null)`；region 不支持 → `(uid, null)`
- token：`Gateway.locked { restartWithValidTokenFor(ctx, cred, skewSec=3600) }`，null 回落 `accessToken` 非空白者
- `bal = token 有效 ? fetchUsage(token, cred.uid, cred.region) : null`
- 见底判定（保守）：`exhausted = bal != null && bal <= threshold`；**不可用判定**：`unusable = bal == null && forwardFailed`（查不到数**且**本次由转发失败触发 → 这号大概率本身出问题，死守无益；查得到数时只按阈值判——防网络抖动误换）
- `(exhausted || unusable) && autoSwitch` → `pickTraeFunded(excludeUid=当前)` 成功后：`setActive(next)` + `syncRegionToActiveAccount(next)`（下一个请求即走新号）；再对新号 `locked{restartWithValidTokenFor(3600)}` + `fetchUsage` 拿最新余额，返回 `(next.uid, nBal)`（"换了另外一个号就刷另外一个号"）
- 找不到候选 / 开关关 → 返回 `(uid, bal)`

`pickTraeFunded`：整段在 `Gateway.locked` 内；遍历 `list.filter { uid != exclude && region.isSupported }`，逐个取有效 token `fetchUsage`，**第一个** `bal != null && bal > threshold` 者返回（用到才扫一次，非每轮）。

`pollQoder(ctx, wantedUid, forwardFailed) -> (uid, remaining)` 同构：uid 回落 activeUid；`fetchQoderCredits(cred)` = `validJobTokenFor(ctx, cred, 60_000ms)` 回落存量 jobToken → `QoderAuthApi.fetchCredits(jt, cred.mode)`；见底判定 `credits != null && !credits.hasCredit`（Qoder 余量是 Double、`hasCredit=!exceeded&&remaining>0`）或 `credits == null && forwardFailed`；开关开 → `pickQoderFunded`（遍历其它号 fetchCredits，第一个 `hasCredit` 者）→ **只 `setActive(next)`**（无 region 同步概念）→ 对新号补查 remaining 返回。

`latest` 之外的一切（无 inFlight——noteForwarded 每次起线程即时执行，`inFlight` 字段实际服务于历史 tick 路径，现存代码未用它拦 noteForwarded 并发；Rust 可加 `AtomicBool` 防同平台叠加）。

selfCheck 不变量：余 1>阈值0 不算用完 / 余 0 算 / 负数算；null 绝不触发换号（保守原则）；`DEFAULT_INTERVAL_SEC==0`；`FORWARD_POLL_MIN_MS>=60_000`。

---

## 6. AccountCredits —— 按账号积分查询、合计、缓存

`object AccountCredits`（AccountCredits.kt），prefs 文件 **`traework2api_credits`**；平台常量 `TRAE="trae"`、`QODER="qoder"`；条目主键字符串 `key = "{platform}|{uid}"`。缓存新鲜期 `TTL_MS = 60_000`（手动 force 无视）。

与 CreditMonitor 的分工：CreditMonitor 是转发链路守卫（只盯当前账号、自动换号）；AccountCredits 回答"账号页上每个号各剩多少"，需要遍历全部账号——每 2s 遍历全号打上游是明确滥用，因此**触发方式是事件驱动**：打开账号页 / 手动点刷新（`onlyUid != null` 时只刷一个账号；`null` 全量仅在用户手点"刷新"时允许，用户规则"用哪个平台就只查那个号"）。

数据结构：

```
Item(platform, uid, remaining: Double?, exceeded: Boolean, error: String?, at: Long /*unix毫秒, 0=从未成功*/)
    known = remaining != null
Snapshot(items: Map<String,Item>, at: Long, loading: Boolean)
    trae(uid)/qoder(uid); known(p); failed(p); total(p); complete(p)=total>0 && known数==total
    sum(p) = known 为空 ? null : Σ remaining（?? 0.0）   ← 只累加查到数的账号
```

缓存双态：内存 `@Volatile snapshot` + 磁盘。磁盘格式：prefs 键 `cache_json` = JSONArray，每元素字段 `platform/uid/remaining(null → JSON null)/exceeded/error(缺省"")/at`；批量时间另存 Long 键 **`at`**（同文件内直接字面量）。`loadCache` 装载时 `snapshot.at` 用 prefs 的 `at`；解析逐条防御（坏 JSON 整盘弃、缺 platform/uid 条目跳过）；`saveCache` 每轮刷新后落盘。落盘动机：账号页每次 render 重建视图，只放内存切页回来会闪一片"查询中"；落盘后进页面**立刻显示上次的数**，后台刷新**原地更新**（观感"一直有数"）。

刷新编排 `refreshAll(ctx, force=false, onlyUid=null, onDone)`：

1. `!force && fresh()`（内存 snapshot.at 距今 <60s）→ 不碰上游，直接 main.post 回调当前快照
2. `inFlight.compareAndSet` 防连点多轮
3. `snapshot = snapshot.copy(loading=true)`；后台线程 `account-credits`：`onlyUid==null ? fetchAll : fetchOne(onlyUid)` → `saveCache` → 整批异常兜底 `copy(loading=false)` → finally：复位 loading、释 inFlight、主线程回调
4. 重试 `retryFetch`：单账号最多 3 次，间隔 `400*i ms`（i=1→400ms，i=2→800ms，第三次后不再 sleep），全失败返回 null（针对"同一平台只有一个号查不到"的上游限流/瞬时 5xx）

`fetchAll`：先 Trae 全部后 Qoder 全部（各 list 异常→空表）；每项失败**保留上一次 remaining 与 at**，只写 error——单点失败只影响它自己、不整批作废。`fetchOne(uid)`：按 uid 先查 Trae store 命中则刷 `trae|uid`，否则查 Qoder store 刷 `qoder|uid`；**其它账号条目原样保留（不查≠清零）**；`at=now` 写进批次时间。

Trae 单账号 `fetchTraeItem(ctx, meta, old, now) -> Item` 判定链：

| 条件 | remaining | exceeded | error |
|---|---|---|---|
| region 不支持 | old?.remaining | false | `国际服不支持`（不硬打上游，明确标注而非显示 0） |
| loadByUid null | old | false | `本地凭证缺失` |
| `Gateway.locked { restartWithValidTokenFor(skew=3600) }` 与存量 accessToken 皆无效 | old | false | `需重新登录` |
| `retryFetch{ fetchUsage }` null → 再 `Gateway.locked { forceRefreshTokenFor }`（token 未到期已被判废的常见成因）得 `t2`，t2 有效且 ≠tok → 再 retryFetch；仍 null | old | false | `lastAuthFailed ? "需重新登录" : "查询失败"` |
| 查到 bal | `bal.toDouble()` | `bal <= 0` | null，at=now |
| 抛异常 | old | false | 异常类简名 |

Qoder 单账号 `fetchQoderItem`：无 store 凭证 → `本地凭证缺失`；`validJobTokenFor(ctx, cred, skew=60*60*1000ms即1小时)` 回落存量 jobToken，无效 → `需重新登录`；`retryFetch{ fetchCredits(jt, mode) }` null → `查询失败`；成功 → `remaining=c.remaining, exceeded=c.exceeded, at=now`。**绝不为了"看一眼"触发 setActive**（那会把查看变成偷偷换号）。

展示规则：

- `format(v)`：null → `—`；整数值 → 不带小数点（`1234`）；否则 `%.1f`（`12.5`）
- `label(item)`：条目 null → `—`；**任何 error → `—（原因）`，绝不显示旧数字**（曾试过"旧数+括号原因"，屏幕上出现「600（token 失效）」自相矛盾读数被否掉；数字唯一含义是"此刻查到的值"）；否则 format(remaining)
- 合计 `totalLabel(snapshot, platform) = format(sum(platform))` —— **只报数字或 `—`**。"（2/3 个账号）"式标注**已从文案中移除**（selfCheck 钉死：有 1 个账号失败时合计仍显示 `150`，无残留/无账号平台显示 `—`）；历史格式「`{合计}（{known}/{total} 个账号）`」所需的 `known()/total()/complete()/failed()` 计数 API 全部保留，注释语义：有失败时合计是**下界**；一个都没查到 → `sum=null`（显示 `—`，显示 0 会被误读成"真没额度了"）。移植建议：数据层 API 原样保留，若桌面 UI 想恢复下界说明可用 `complete()` + `known/total` 自行拼接。
- `forget(platform, uid)`：删账号时从缓存抹条目（**纯本地，零请求**——旧版在此调全量刷新等于删一号问全号）；`clear()` 全清（prefs remove `cache_json` 和 `at`）。

---

## 7. 常量表 + SharedPreferences 全清单

### 7.1 数值/字符串常量

| 常量 | 值 | 出处 |
|---|---|---|
| Trae 账号间隔 | 800 ms | BatchCheckin.PER_ACCOUNT_DELAY_MS |
| Qoder 账号间隔 | 600 ms | QoderAuthApi.claimDailyAll |
| 转发后刷新节流 | 60 000 ms | CreditMonitor.FORWARD_POLL_MIN_MS |
| 定时轮询间隔 | 0（禁轮询） | CreditMonitor.DEFAULT_INTERVAL_SEC |
| 积分缓存 TTL | 60 000 ms | AccountCredits.TTL_MS |
| 补签重试冷却 | 600 000 ms | AutoCheckin.RETRY_COOLDOWN_MS |
| 开窗时刻 | Asia/Singapore 10:05（窗口定义 10:00 + 5min 余量） | AutoCheckin.WINDOW_HOUR/MINUTE |
| Trae 批量默认时刻 / Qoder 批量默认时刻 | 08:00 / 09:00（本地时区，`HH:mm`） | timeText 缺省值 |
| token 续期 skew | 3600 s（批量/监测/页查统一） | expiredWithin/ensureFreshTokenFor 各处 |
| Qoder jt skew | noteForwarded 内 60 000 ms；AccountCredits 3 600 000 ms；claim 60 000 ms | 各调用点 |
| 查积分重试 | 3 次，间隔 400·i ms（i=1,2） | retryFetch |
| wakelock 超时 | 120 000 ms | 两个 Receiver |
| 见底阈值 | 默认 5，下限 5，档位 {5,10,15,20} | CreditMonitor |
| 闹钟 request code | 网关 8789/8790（外部）；Trae 签到 8792；Qoder 领取 8793；开窗补签 8801 | |
| 通知 id | Trae 签到 8791；Qoder 领取 8794 | |
| HTTP 超时 | connect 20 s / read 180 s | TraeUpstream.client（Qoder 用各自 client，值未在这批文件内另行定义） |
| 日期/时间格式 | `yyyy-MM-dd`（Locale.US）、闹钟日志 `yyyy-MM-dd HH:mm`（Locale.US） | 各处 today() |

### 7.2 Actions / 渠道 / wakeLock tag

| 项 | 精确字符串 |
|---|---|
| Trae 定时 action | `com.example.traework2api.CHECKIN_ALARM` |
| Qoder 定时 action | `com.example.traework2api.QODER_CHECKIN_ALARM` |
| 开窗补签 action | `com.example.traework2api.AUTO_CHECKIN` |
| Trae 通知渠道 | id `traework2api_checkin`，名 `签到结果`，描述 `每日定时签到的成功通知` |
| Qoder 通知渠道 | id `qoder_checkin`，名 `Qoder 领取结果`，描述 `Qoder 每日定时领取 Credits 的成功通知` |
| 渠道属性 | IMPORTANCE_DEFAULT、无角标、无声（null sound）、无振动 |
| wakeLock tag | `traework2api:checkin`、`traework2api:qoder-checkin`（均 PARTIAL_WAKE_LOCK、非引用计数、2 min 超时） |

### 7.3 SharedPreferences 文件 × key（全部精确字符串）

文件 `traework2api_checkin`（BatchCheckin 专用）：

| key | 类型 | 默认 |
|---|---|---|
| `schedule_enabled` | Bool | false |
| `schedule_time` | Str | `08:00`（校验失败回落） |
| `last_run_date` | Str `yyyy-MM-dd` | 无 |
| `last_results` | Str(JSONArray: uid,nickname,status,credits,message) | 无 |

文件 `qoder_checkin`（QoderCheckin 专用）：

| key | 类型 | 默认 |
|---|---|---|
| `schedule_enabled` | Bool | false |
| `schedule_time` | Str | `09:00` |
| `last_run_date` | Str | 无 |
| `last_results` | Str(JSONArray: uid,name,status,amount,message) | 无 |
| `qoder_claimed` | Str(JSON object `{uid: yyyy-MM-dd}`) | `{}` |

文件 `traework2api_prefs`（多模块共用）：

| key | 归属 | 类型 | 默认 |
|---|---|---|---|
| `auto_checkin_done` | AutoCheckin | Str(JSON `{平台:uid → yyyy-MM-dd}`) | `{}` |
| `auto_checkin_last_try` | AutoCheckin | Long | 0 |
| `auto_switch_enabled` | CreditMonitor | Bool | true |
| `auto_switch_threshold` | CreditMonitor | Long | 5（coerceAtLeast 5） |
| `monitor_interval_sec` | CreditMonitor | （常量在、无读写点，轮询已废弃） | — |
| `trae_region` | TraeRegion（这 8 文件间接依赖） | Str | — |
| `qoder_campaign_variant_cn` / `qoder_campaign_variant_global` | QoderAuthApi（KEY_VARIANT 前缀 `qoder_campaign_variant_` + mode） | Str `minimal`/`desktop` | minimal |

文件 `traework2api_credits`（AccountCredits）：

| key | 类型 | 默认 |
|---|---|---|
| `cache_json` | Str(JSONArray: platform,uid,remaining,exceeded,error,at) | 无 |
| `at` | Long | 0 |

### 7.4 全部对外端点汇总（签到/积分域）

```
POST https://api.trae.cn/trae/api/v2/ug/checkin_credits/status   body {}
POST https://api.trae.cn/trae/api/v2/ug/checkin_credits/claim     body {}
POST https://api.trae.cn/trae/api/v2/pay/ide_user_ent_usage       body {}
GET  https://openapi.qoder.com.cn/sash/api/v1/me/campaigns?forceRefresh=true      (mode=cn)
POST https://openapi.qoder.com.cn/sash/api/v1/me/campaigns/{id}/claim             (mode=cn)
GET  https://openapi.qoder.com.cn/api/v2/quota/usage                              (mode=cn)
GET/POST 同上三条路径的 https://openapi.qoder.sh 版本                              (mode=global)
```

---

## 8. 移植时必须照抄的行为陷阱清单

1. Trae claim **code=0 才是成功码**；判据必须用合取（§1.1），单看 code、单看跃迁都会重演"没签到却说成功"。
2. 9004/9074 的真实成因分别是缺设备头与设备身份不独立；设备三件套派生算法（§1.1 seededStream）必须逐字节一致，否则全部账号共用设备被判频。
3. AutoCheckin 的 Trae claim 成功判定与 BatchCheckin 不同（`code in 200..299`），实际依赖"下一轮 checkedIn 收敛"；移植时统一判据并保留冷却/按账号 markDone 语义。
4. 一切 Trae token 续期都在全局锁内；批量全程持锁；Rust 里若用非重入 Mutex 需把锁粒度改成"每账号续期一次"或换 ReentrantLock/parking_lot `ReentrantMutex`（§9）。
5. 批量写回 token 用 updateTokens 语义，严禁碰 active 指针（后台续期偷切号的坑）。
6. `done`/`claimed` 标记都是 JSON map `{key: "yyyy-MM-dd"}`；键格式 `trae:{uid}` / `qoder:{uid}`（AutoCheckin）与裸 `{uid}`（qoder_claimed，仅 Qoder 域）两套，别混。
7. 通知只在定时路径、且 `okCount>0` 才发；Qoder 单行文案带总额、Trae 不带。
8. 节流按平台分两份时间戳；urgent 跳节流但仍**更新时间戳**。
9. 余额查询失败保留旧值+error 标记；label 一律 `—（原因）` 不显示旧值；合计一个都没查到是 `—` 而不是 0。
10. 重启闹钟丢失由"进 App 补签"兜底 —— 桌面版无此问题，直接常驻 tokio 任务（见 §9）。

---

## 9. 建议 Rust 模块 API 草案 + 桌面定时方案

### 9.1 模块划分（src-tauri/src/）

```
checkin/
  trae_api.rs      // §1.1：设备派生、ug_post、status/claim/usage、成功合取、last_auth_failed
  qoder_api.rs     // §1.2：两档身份头、campaign 阶梯、claim、credits
  batch.rs         // §2：Trae 批量编排 + PerAccount/BatchResult + 结果落盘
  qoder_batch.rs   // §4：Qoder 批量编排 + claimed 标记
  catchup.rs       // §3.3：AutoCheckin 跨平台开窗补签（冷却、markDone、pending）
scheduler/
  daily.rs         // §3.1/§4：HH:mm 本地时刻闹钟（用户配置，Trae+Qoder 两条独立）
  window.rs        // §3.3：SGT 10:05 开窗任务
  onboot.rs        // 启动补签（取代 AlarmManager 丢失 + 进 App 补签两条）
credits/
  monitor.rs       // §5：note_forwarded 节流 + 换号
  account.rs       // §6：按账号积分快照/缓存/展示
  store.rs         // TokenStore 锁（替代 Gateway.locked）与 auth store update_tokens
notify.rs          // tauri-plugin-notification 封装两个渠道文案
```

### 9.2 API 草案（签名级）

```rust
// trae_api.rs
pub struct DeviceIds { pub market_user_id: String, pub device_id: String, pub session_id: String }
pub fn derive_device_ids(uid: &str) -> DeviceIds;               // §1.1 三件套
pub struct CheckinStatus { pub checked_in: bool, pub credits: i64, pub enable: bool }
pub enum UgFailure { Network, Auth }                            // 对应 lastAuthFailed 二分
pub async fn ug_post(region: Region, path: &str, token: &str, uid: &str)
    -> Result<serde_json::Value, UgFailure>;
pub async fn fetch_checkin(...)  -> Result<CheckinStatus, UgFailure>;
pub async fn claim_checkin(...)  -> Result<i32, UgFailure>;      // -1 = 缺 code 字段
pub fn checkin_succeeded(before: Option<&CheckinStatus>, after: Option<&CheckinStatus>,
                         code: Option<i32>) -> bool;              // 合取，纯函数好测
pub fn fetch_usage_healing(cred, store) -> ...;  pub fn claim_healing(cred, store) -> ...;

// batch.rs / qoder_batch.rs
pub struct PerAccount { uid: String, nickname: String /*qoder 侧字段名 name*/,
                        status: AccountStatus /* Ok|Already|Disabled|Unsupported|Fail
                        (Qoder 无 Disabled) */, credits/*or amount*/: i64, message: String }
pub struct BatchResult { results: Vec<PerAccount>, date: String }
impl BatchResult { pub fn all_ok(&self) -> bool; }
pub async fn run_all(store: &TraeStore) -> BatchResult;           // 全程 guard 续期锁
pub async fn maybe_catch_up(store: &Settings) -> Result<Option<BatchResult>, Busy>;

// credits/monitor.rs
pub fn note_forwarded(backend: Backend, uid: Option<String>, urgent: bool);
    // 内部 tokio::spawn + 每平台 Instant 节流表；Snapshot broadcast 给 UI

// credits/account.rs
pub async fn refresh_all(force: bool, only_uid: Option<&str>) -> Snapshot; // in-flight 单飞
pub fn label(&Item) -> String; pub fn total_label(&Snapshot, Platform) -> String;

// store.rs
pub struct TraeTokenGuard;   // parking_lot::ReentrantMutex 或直接 std::sync::Mutex（同步续期在
                             // spawn_blocking 中）；语义 = runAll 持锁、内部子请求可重入
```

设置持久化建议用 JSON 文件或 SQLite 代替 prefs，键名沿用 §7.3 清单以便数据迁移；`busy` → 每平台一把 `tokio::sync::Mutex<bool>`/`AtomicBool`。

### 9.3 桌面定时方案（替代 AlarmManager + BOOT + 进 App 补签三件套）

Android 侧"闹钟会被杀/重启丢失、靠进 App 自愈重挂 + 补签兜底"的整套复杂度在桌面端消失，收敛为一个常驻 tokio 任务：

```rust
pub fn spawn(sched: SchedulerHandle, store: SettingsStore, cancel: CancellationToken) {
    tokio::spawn(async move {
        catch_up_on_boot(&store).await;                    // 启动即补签：见下
        let mut tk = tokio::time::interval_at(first_tick(), TICK_GRANULARITY);
        loop {
            tokio::select! { _ = tk.tick() => {
                if now_local_hits(&store.trae_time())  { batch::run_all_send_notification().await;
                                                         store.mark_date_today(); }
                if now_local_hits(&store.qoder_time()) { qoder_batch::run_all_notify().await; }
                if now_sgt_hits_10_05()               { catchup::run_all_once().await; }
            }, _ = cancel.cancelled() => break }
        }
    });
}
```

落点对照（Android 语义 → Rust 落点）：

| Android 机制 | Rust/桌面落点 |
|---|---|
| `scheduleNext` 单发闹钟 + 触发自续 | tokio 任务里按 `HH:mm` 本地时刻计算 `Instant`：用 `chrono::Local` 求"今天或明天 HH:00"，到期跑完后重新计算下一日时刻（`interval_at` + 每跳 reset，或每次 `sleep_until` 循环）；**对齐点语义逐条照抄 §3.1**（秒/毫秒清零、≤now 顺延一天） |
| 用户改时刻（setTime） | Notify 通知任务重算下一个 tick，无需重建任务 |
| 闹钟在进程被杀/重启后丢失 → 开机无 BOOT receiver → 进 App `maybeCatchUp` | **开机自启 App（或常驻托盘）+ 启动即跑 `catch_up_on_boot`**：依次执行 `batch::maybe_catch_up()`、`qoder_batch::maybe_catch_up()`（判据原样：enabled && last_run_date==today && all_ok → 跳过；有 fail 再试一轮），顺带覆盖"错过 HH:mm 才启动"的场景；AutoCheckin 的冷却语义（10 min + request_runsoon）原样保留 |
| CheckinReceiver goAsync + wakelock | 不存在该问题：tokio 任务天然存活；仅当系统睡眠需恢复时，用 `tokio-time-unsabotage`/`system-power` 事件后立刻补跑一次 tick（可选加固） |
| 通知 | `tauri-plugin-notification`；文案逐字照抄 §3.1 通知汇总与 §4，成功才发、`ok_count==0` 不发 |
| `Gateway.locked`（可重入 synchronized） | `parking_lot::ReentrantMutex`（批量持锁 + 子续期再锁）或改锁粒度为"仅续期临界区持锁、runAll 循环不全程持锁"——后者要求单账号续期原子性仍成立，并保留"批量期间转发等锁"的语义注释 |
| AtomicBoolean busy | 同结构 `AtomicBool` CAS |
| 设备本地时区 vs SGT 开窗混用 | `chrono` 显式两个 tz：批量用 `Local`，开窗用 `Asia/Singapore` fixed offset；done 标记日期按 `Local`（照抄现状） |

单元测试直接移植三个 selfCheck 的全部断言（§2 末尾、§3.3、§7 陷阱清单），它们是各文件注释里"判错会让用户白丢积分"的不变量全集。
