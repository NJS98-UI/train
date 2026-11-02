# Auth Stores 分析：TraeAuthApi / TraeAuthStore / QoderAuthApi / QoderAuthStore

> 目标：达到"不看 Kotlin 也能用 Rust 重写"的精度。
> 源文件（已逐行通读）：
> - `app/src/main/java/com/example/traework2api/TraeAuthApi.kt`（421 行）
> - `app/src/main/java/com/example/traework2api/TraeAuthStore.kt`（492 行）
> - `app/src/main/java/com/example/traework2api/QoderAuthApi.kt`（873 行）
> - `app/src/main/java/com/example/traework2api/QoderAuthStore.kt`（441 行）
> 辅助参照（引用侧常量/数据流）：`TraeRegion.kt`（被 TraeAuthApi 依赖，必须读）、`Gateway.kt`、`TraeUpstream.kt`、`AccountCredits.kt`、`LocalFakeServer.kt`、`MainActivity.kt`、`QoderMachine.kt`、`BatchCheckin.kt`、`CreditMonitor.kt`。

---

## 1. Trae 登录流

### 1.1 区域常量（来自 TraeRegion.kt，登录 URL / OAuth host 的唯一来源）

| 字段 | CN（国服） | SG（国际服） |
|---|---|---|
| `id` | `"cn"` | `"sg"` |
| `consoleHost`（登录页 host） | `https://www.trae.cn` | `https://www.trae.ai` |
| `oauthHost`（ExchangeToken/GetUserInfo host） | `https://api.trae.com.cn` | `https://ug-normal.trae.ai` |
| `clientId` | `"en1oxy7wnw8j9n"` | `"ono9krqynydwx5"` |
| `agentHost`（聊天，非登录链路） | `https://trae-api-cn.mchost.guru` | `https://core-normal.trae.ai` |
| `ugHost`（额度/签到） | `https://api.trae.cn` | `https://ug-normal.trae.ai` |
| `userRegionHeader` | `"CN"` | `""`（空则不发该头） |
| `isSupported` | true | **false**（冻结为未开发，见 §5） |
| `canLogin` | oauthHost/consoleHost/clientId 三者非空才 true；两区当前均 true | |

- `IDE_VERSION = "0.1.43"`（与区域无关，companion 常量）。
- `TraeRegion.current`：`@Volatile` 全局当前区域，默认 CN。持久化 key `trae_region`（prefs `traework2api_prefs`），`load()` 启动恢复、`set()` 切换并落盘，`fromId()`：trim + lowercase 匹配 id，未知/空白/null 一律回退 CN。
- `requireLogin()`：当前区域 `canLogin==false` 时抛 `IllegalArgumentException`，消息：`"区域 {id} 的登录协议尚未接入（缺 OAuth host / ClientID），目前仅支持 cn"`。

### 1.2 登录链接构造（`buildLoginUrl()`）

模板：`{consoleHost}/authorization?{query}`，query 全部 key、value 各做 `URLEncoder.encode(…, "UTF-8")`，用 `&` 连接，**顺序固定**（linkedMap，Rust 侧需用保序序列化或固定拼接顺序）：

```
login_version   = 1
auth_from       = solo
login_channel   = native_ide
plugin_version  = 2.3.62834
auth_type       = local
client_id       = {region.clientId}
redirect        = 0
login_trace_id  = hex(8 字节随机)        → 16 个 hex 字符
auth_callback_url = http://127.0.0.1:18080/authorize   （整体再 URL 编码）
machine_id      = hex(16 字节随机)        → 32 个 hex 字符（与 device 各自独立生成）
device_id       = hex(16 字节随机)        → 32 个 hex 字符
x_device_id     = {与 device_id 同值}
x_machine_id    = {与 machine_id 同值}
x_device_brand  = PC
x_device_type   = PC
x_os_version    = 1.0
x_app_version   = {IDE_VERSION=0.1.43}
x_app_type      = stable
```

精确 CN/SG 成品示例（占位说明）：
- CN：`https://www.trae.cn/authorization?login_version=1&auth_from=solo&…&client_id=en1oxy7wnw8j9n&…`
- SG：`https://www.trae.ai/authorization?…&client_id=ono9krqynydwx5&…`

随机串生成：`SecureRandom` 取 N 字节 → 逐字节 `%02x` 拼接（小写 hex）。Rust：`rand::rng` + `hex::encode`。
注意：登录参数中的 device/machine 与 ug 接口三个设备标识（`deviceIdFor/sessionIdFor/marketUserIdFor`，从 uid 派生）**无关且不可混用**（源码注释明确回退过该尝试）。登录发生在拿到 uid 之前，只能随机。

### 1.3 回调链接识别与解析

**识别（`isCallback`）**：`url.regionMatches(0, prefix, 0, prefix.len, ignoreCase=true)`——整串以 `http://127.0.0.1:18080/authorize`（32 字符，常量拼接 `CALLBACK_HOST=127.0.0.1`、`CALLBACK_PORT=18080`、`CALLBACK_PATH=/authorize`）为前缀，**忽略大小写**，比较长度即前缀长度（32 字符）。非此前缀（如 `http://evil.example.com/authorize?refreshToken=abc`）判 false。

**解析（`extractRefreshToken`，parse_qs 语义）**：
1. `substringAfter('?')` 取 query 段（第一个 `?` 之后的全部；无 `?` → 空串）。
2. 按 `&` 切分，丢弃空段；每段按 `=` 切 **limit=2**（value 内允许再含 `=`）。
3. key/value 各做**一次** `URLDecoder.decode("UTF-8")`，解码异常则保留原值（decode 内部 catch）。
4. 组装成 map：**重复 key 时后者覆盖前者**（`associate` 语义）。
5. 取值顺序（空/空白视为缺失，逐级回退）：
   - `refreshToken`
   - `refresh_token`
   - 兜底：解析 `userJwt` 或 `user_jwt` 参数为 JSON 对象（`parseJson`：直接 `JSONObject(raw)`，失败返回 null；此处 map 值已经历一次 query 解码），取其字符串字段 `RefreshToken`（非空白才算命中）。
6. 返回 `?.takeIf { isNotBlank() }`；全部缺失 → null。

**失败判定（`loginWithCallbackUrl`，唯一登录方式）**：
- 输入 trim 后空白 → `IllegalArgumentException("请粘贴回调链接")`
- `!isCallback(url)` → `IllegalArgumentException("这不是 Trae 回调链接。应形如 http://127.0.0.1:18080/authorize?refreshToken=…")`
- `extractRefreshToken==null` → `IllegalStateException("链接里没有 refreshToken，请确认复制的是登录后跳转的那条完整链接")`
- 用户流程：系统浏览器打开链接 → 登录成功后跳到打不开的 `127.0.0.1:18080/authorize?...` → 用户**手动复制地址栏整条链接粘回 App**。内置 WebView/本地监听均不存在（Tauri 桌面版同样用"复制粘贴"即可，无需起本地 HTTP 服务）。

**辅助函数 `parseJsonParam(raw)`**（公共，供别处取 userInfo/userJwt 用）：对 `raw` 依次尝试 `JSONObject(raw)`、`JSONObject(URLDecoder.decode(raw))`，都失败返回 null——处理可能双重 URL 编码的 JSON 参数。

### 1.4 ExchangeToken（refresh → access，token 轮换）

- 方法/URL：`POST {region.oauthHost}/cloudide/api/v3/trae/oauth/ExchangeToken`
- 请求头：`Content-Type: application/json`、`Accept: application/json`、`User-Agent: Trae/0.1.43`
- 请求体（key 顺序不敏感）：
  ```json
  {"ClientID":"<region.clientId>","RefreshToken":"<refresh>","ClientSecret":"-","UserID":""}
  ```
  注意大小写：请求体里是 `ClientID`（登录 URL query 里是 `client_id`）。`ClientSecret` 恒为 `"-"`，`UserID` 恒为 `""`。
- 响应判定（`ExchangeResult`）：
  - 非 2xx 或空 body → `IOException("ExchangeToken http {code}: {body前200字}")`
  - 顶层 JSON 取 `Result` 对象；缺失 → `IOException("ExchangeToken 缺 Result")`
  - `Result.Token`（字符串）空白 → `IOException("ExchangeToken 未返回 Token")` → 作为 accessToken
  - `Result.RefreshToken`：非空白则为**轮换后的新 refresh**；缺失/空白 → null（调用方沿用旧值）
  - 到期时间 `expiresAt`（unix **秒**，可 null）：
    1. `Result.TokenExpireAt` 以**字符串**读取（`optString`）再 `toLongOrNull()`；`>0` 时 `normalizeMs`：数值 `> 1_000_000_000_000` 判毫秒 → `/1000`，否则视为秒。
    2. 缺省兜底：`Result.TokenExpireDuration`（`optLong`，数字或字符串均可，JSON 层自动转）`>0` → `now_sec + duration`。
    3. 都没有 → null（落盘时变 0）。
- HTTP 客户端超时：connect 20s、read 30s。
- **区域纪律**：`exchangeToken(refreshToken, region)` 的 region 参数默认取全局当前区域；多账号后台续期/签到**必须显式传该账号自己的区域**，否则跨区打错 host（实测踩坑）。SG 上游协议与 CN 同构（错误结构 `ResponseMetadata.Error.Code=10101/StandardCode=040004`）。

### 1.5 GetUserInfo（access → 账号身份）

- `POST {oauthHost}/cloudide/api/v3/trae/GetUserInfo`
- 请求头：`Content-Type: application/json`、`Accept: application/json`、`User-Agent: Trae/0.1.43`、**`X-Cloudide-Token: {accessToken}`**
- 请求体：`{"ReqSource":"IDE","IDEVersion":"0.1.43"}`
- 响应：顶层取 `Result` 对象（缺失 → IOException）；读取 `Result.UserID`→uid、`Result.ScreenName`→nickname、`Result.EnterpriseID`→enterpriseId（均 optString 缺省 ""）。
- 注意：`getUserInfo` 用的是**当前全局区域的 oauthHost**（属性取值），调用前需保证区域正确。

### 1.6 loginWithCallbackUrl 的组装

```
refresh = extractRefreshToken(url)
ex      = exchangeToken(refresh)                    // 当前区域
user    = getUserInfo(ex.accessToken)
cred    = Cred(
  accessToken  = ex.accessToken,
  refreshToken = ex.refreshToken ?: refresh,        // 轮换优先
  expiresAt    = ex.expiresAt ?: 0,
  uid/nickname = user.uid / user.nickname,
  regionId     = 当前区域 id,
)
TraeAuthStore.save(context, cred)                   // 落盘并设为当前账号
```
enterpriseId **不落盘**（只在此处读取，进入 Cred 的没有该字段）。

---

## 2. Trae 续期：决策树

### 2.1 有效性和阈值

- **时钟源**：一律 `System.currentTimeMillis()/1000`（本地墙钟，unix 秒）。Qoder 用毫秒（见 §4.4），两边单位不同是已知坑，Rust 侧建议类型区分。
- `expiresAt` 语义：Cred 里是 unix **秒**；`0=未知`。
- `expiredWithin(expiresAt, skewSec)`（唯一"要不要续"判据）：
  ```
  expiresAt == null 或 <= 0            → true（判过期，必须续）
  now_sec + skewSec >= expiresAt       → true（临近/已过期）
  否则                                  → false
  ```
  即"有效期剩余 ≤ skewSec"就续。skewSec 现网取值：Gateway 每请求 3600 秒；AccountCredits/CreditMonitor/BatchCheckin 3600 秒；selfCheck 用 3600 验证边界。
- 判错方向后果不对称（源码 selfCheck 注释）：该续不续 → 持续 401；不该续却续 → 白白消耗轮换型 refresh。

### 2.2 `restartWithValidToken(context, skewSec): String?`（作用于"当前账号"，会动全局区域）

```
1. cred = TraeAuthStore.load(context)          // activeUid → loadByUid
   cred==null → return null
2. TraeRegion.current = cred.region             // 临时把全局区域切到账号区域！
3. if cred.accessToken 非空白 且 !expiredWithin(cred.expiresAt, skewSec):
      return cred.accessToken                   // 有效，直接返回（日志：token 有效）
4. // 临近/已过期：
   if cred.refreshToken 空白:
      return cred.accessToken.takeIf{非空白}     // 无 refresh：返回旧的（可能过期）或 null
5. ex = exchangeToken(cred.refreshToken)         // 走步骤2切好的区域；异常直接向上抛
                                                  // （失败时尚未写库 → 不破坏已存数据）
6. TraeAuthStore.save(context, Cred(
      accessToken  = ex.accessToken,
      refreshToken = ex.refreshToken ?: 旧值,     // 轮换：有则写回新值
      expiresAt    = ex.expiresAt  ?: 旧值,       // 兜底：上游没给就保留旧的
      uid/nickname/regionId 原样))                // save 顺带 setActive(同 uid，无副作用)
7. return ex.accessToken
```
写回字段：access、refresh（轮换）、expiresAt。注意这是旧式路径：无竞态守卫、异常裸抛；现在真正的统一入口是 `ensureFreshTokenFor`（Gateway 的 credProvider 仍用它）。

### 2.3 `ensureFreshTokenFor(context, cred, skewSec): String?`（唯一续期入口，防令牌重放）

背景纪律（源码注释，两条缺一不可）：
1. **先读库里的最新凭据**再决定续不续——调用方手里的 cred 可能是几秒前的快照，期间别处已续过；拿旧 refresh 再换会触发服务端"旧令牌重放"判定，整条令牌族作废（症状：签到成功，过一会儿全部 401）。
2. **库里 access 已不同于快照 → 别人续过了，直接复用，一次网络请求都不发。**

```
1. fresh = TraeAuthStore.loadByUid(context, cred.uid)  // 读失败/无此号 → cred 本身
2. if fresh.accessToken 非空白 且 !expiredWithin(fresh.expiresAt, skewSec):
      return fresh.accessToken
3. if reuseAfterRace(cred.accessToken, fresh.accessToken):   // 库里非空 且 != 调用方快照
      return fresh.accessToken                                 // 已被别处续过，复用
4. return refreshLocked(context, cred.uid, staleAccess=fresh.accessToken)
```

- `reuseAfterRace(staleAccess, storedAccess) = storedAccess.isNotBlank() && storedAccess != staleAccess`
- 返回 null = "续不动"（无 refresh / 上游拒绝），调用方如实报错，不再重试。

### 2.4 `refreshLocked(context, uid, staleAccess)`（真正的续期，必须在 `Gateway.locked` 互斥内调用；synchronized 可重入）

```
1. base = loadByUid(uid)；null → return null
2. if reuseAfterRace(staleAccess, base.accessToken) → return base.accessToken   // 二次竞态检查
3. base.refreshToken 空白 → 日志警告，return null
4. ex = runCatching(exchangeToken(base.refreshToken, base.region))
      失败 → 日志 "续期失败: {异常类}: {msg}"，return null（保留库内旧数据不破坏）
5. stored = runCatching{ loadByUid(uid) }.getOrNull()
6. // 写回守卫：shouldWriteBack(usedRefresh=base.refreshToken, storedRefresh=stored.refreshToken)
   //   = storedRefresh 空白 || storedRefresh == usedRefresh
   if stored != null 且 守卫不过（续期期间 refreshToken 又被别处轮换）:
      日志"放弃写回，采用库里那份"
      return stored.accessToken.takeIf{非空白} ?: ex.accessToken
7. newCred = base.copy(
      accessToken  = ex.accessToken,
      refreshToken = ex.refreshToken ?: base.refreshToken,
      expiresAt    = ex.expiresAt  ?: base.expiresAt)
8. TraeAuthStore.updateTokens(newCred)      // 只写两个 token 密文，不碰 active
   TraeAuthStore.updateMeta(newCred.toMeta()) // expiresAt 在索引 meta 里，单独写
9. return ex.accessToken
```

### 2.5 其余续期变体

- `restartWithValidTokenFor(context, cred, skewSec) = ensureFreshTokenFor(...) ?: cred.accessToken.takeIf{非空白}`——给**指定账号**（积分监测扫其他号）用；不改当前账号、不动全局区域；续期失败**回落到已存的（可能过期的）access token**。
- `forceRefreshTokenFor(context, cred)`——"已被拒了才用"（401 救急）：**跳过有效性检查**直接 `fresh=loadByUid?:cred; reuseAfterRace?复用:refreshLocked(uid, cred.accessToken)`；续不动返回 null。
- `selfCheck()` 断言（值得原样移植成 Rust 单元测试）：exp=0/null/负数→判过期；已过去→过期；`now+60` 在 skew=3600 内→续；`now+skew+60`→不续；reuse/writeBack 三组真假；`isCallback` 前缀匹配（裸回调 ok、带 refreshToken ok、外域拒绝）；`extractRefreshToken` 裸回调得 null、`?refreshToken=abc123` 得 `abc123`。

### 2.6 失败后的状态处理 / "被拒"效果

续期路径本身**不写任何"账号作废"标志**（store 中无 banned/rejected 字段）。失败仅表现为：日志 + 返回 null/旧 token + 抛异常（§2.2 的 5 步）。"需重新登录"的用户可见状态由上层产生，见 §5。refreshToken 已轮换成功时，旧 refresh 即刻作废——这是并发纪律（2.3/2.4）存在的全部原因，Rust 必须保留等价的"库内最新凭据 + 双重检查 + 写回守卫"，并把 `Gateway.locked` 映射为一把全局异步互斥锁（续期与转发互斥）。

---

## 3. TraeAuthStore 数据模型与持久化

### 3.1 结构体（内存模型）

`Cred`（完整凭证，token 只存在于内存）：
| 字段 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `accessToken` | String | 必填 | access token |
| `refreshToken` | String | 必填 | refresh token（可空串=无轮换能力） |
| `expiresAt` | Long | 必填 | **unix 秒**；0=未知 |
| `uid` | String | 必填 | 账号主键 |
| `nickname` | String | 必填 | 显示名（GetUserInfo.ScreenName） |
| `regionId` | String | `"cn"`（TraeRegion.CN.id） | 账号所属区域；转发/续期选 host 的依据 |

`AccountMeta`（索引条目，无任何 token）：`uid:String, nickname:String, regionId:String, expiresAt:Long(秒,0=未知)`；`region` getter = `TraeRegion.fromId(regionId)`。`Cred.toMeta()` 抽 4 个非敏感字段。

### 3.2 SharedPreferences 确切 key 与序列化格式

prefs 文件：`traework2api_prefs`（`MODE_PRIVATE`）。

| key | 类型 | 格式 |
|---|---|---|
| `trae_accounts` | String | **JSON 数组**，逐条 `{"uid":...,"nickname":...,"region":"cn"/"sg","expiresAt":<long秒>}`（键名是 `region` 而非 regionId） |
| `trae_active_uid` | String | 当前账号 uid；删除当前号后写入**剩下第一个**，删空则写**空串** |
| `trae_acc_<uid>_access` | String | 密文：`base64noWrap(IV) + ":" + base64noWrap(ciphertext‖tag)` |
| `trae_acc_<uid>_refresh` | String | 同上 |
| `trae_region` | String | 全局当前区域 id（由 TraeRegion.set / syncRegionToActiveAccount 写） |
| 旧版（仅迁移读）：`trae_access`、`trae_refresh`、`trae_uid`、`trae_nick`、`trae_exp` | String×4 + Long | 见 3.5 |

密文算法（桌面版将整体替换，见 §7）：AndroidKeyStore 内 AES-256-GCM，别名 `trae_solo_aes_key`，`NoPadding`，GCM tag 128 bit，IV 由 cipher 随机生成（12B），Base64 `NO_WRAP`。`decrypt`：按 `:` 拆两段（split 不限制次数，密文含多个 `:` 直接失败返回 null），任何异常 → null。

索引读（`readIndex`）：整个 JSON 解析异常 → 空列表；条目的 `uid` 空白 → 丢弃该条；`nickname` 缺省 `""`；`region` 缺省 `"cn"`；`expiresAt` 缺省 0。
索引写（`writeMeta`）：读出→按 uid 原位替换，不存在则**追加末尾**→整体序列化回写。**uid 是主键，顺序即列表顺序。**

### 3.3 API 精确语义

- `list(ctx)`：先 `migrateLegacyIfNeeded`（进程内一次性标志 `migrated`），再 `repairMislabeledRegions`，返回 `readIndex`。
- `activeUid(ctx)`：同样先 migrate+repair；索引空 → null；`trae_active_uid` 存的值**必须仍在索引里**才用，否则回退 `metas.first().uid`。
- `load(ctx)` = activeUid→loadByUid；`loadByUid(uid)`：meta 不存在→null；access 密文缺失/解密失败/明文空白→**null**（access 是硬性条件）；refresh 缺失→**空串**（软条件）；expiresAt/nickname/regionId 全部来自 meta。
- `save(ctx, cred)`：加密写 access+refresh → `writeMeta` → `setActive(uid)`。同 uid 重登=覆盖，无重复条目，不影响别的账号。
- `setActive(uid)`：uid 不在索引 → 忽略；否则写 `trae_active_uid`。
- `updateTokens(ctx, cred)`：账号在索引才写两个 token 密文；**不改 active、不写 meta**（expiresAt 更新要调用方另外 `updateMeta`——refreshLocked 正是这么成对调用的）。
- `updateMeta(ctx, meta)`：账号在索引才 `writeMeta`。
- `remove(ctx, uid)`：索引过滤该条并回写；删两个 token key；**若删的是当前账号**：active 写剩下第一个，删空写 `""`。
- `pickFailoverCandidate(metas, failedUid)（internal，纯函数）`：
  ```
  others = metas.filter{ uid != failedUid }              // 保持索引顺序
  return others.firstOrNull{ TraeRegion.fromId(it.regionId).isSupported }  // 优先受支持区域（当前=CN）
      ?: others.firstOrNull()                              // 没有受支持的→退而换任意别的
      // others 为空 → null（只有一个账号时不换）
  ```
  语义：国际服已冻结未开发，兜底优先给国服；两个国服时换另一个国服（selfCheck 逐条钉死，见 3.6）。
- `syncRegionToActiveAccount(ctx)`：
  ```
  uid = activeUid ?: return
  meta = 索引中该 uid ?: return
  TraeRegion.current = meta.region
  prefs["trae_region"] = meta.regionId       // 权威=账号自带区域；手动选的会切号后残留错值
  ```
  无账号时**什么都不动**（保留用户手动选的区域供登录用）。启动与切号/故障转移后必须调用。

### 3.4 导出/导入 JSON（桌面版可直接沿用该格式做备份）

外壳：`{"format":"traework2api-accounts","version":1,"activeUid":"…","accounts":[{"uid","nickname","region","expiresAt","accessToken","refreshToken"}]}`（`toString(2)` 带缩进）。⚠️ 明文 token（KeyStore 绑机，密文导出无意义）；等价账号钥匙，只走本地文件/剪贴板，绝不联网、绝不写日志。
`importJson`：优先按外壳取 `accounts`；非 JSON 对象则尝试顶层数组（老格式兼容）。逐条：uid 空白跳过；accessToken 空白跳过；refresh 缺省 ""；expiresAt 缺省 0；`region` 缺省 `"cn"`。逐条用 `importOne`（写 token+meta，**不 setActive**）。成功后 `imported==0` → 抛 `"JSON 里没有可导入的账号（缺 uid 或 accessToken）"`；外壳带 `activeUid` 且命中导入结果才 `setActive`。

### 3.5 旧版迁移 + 区域修补（Android 历史包袱，桌面版建议只保留语义、不保留启发式）

- `migrateLegacyIfNeeded`（进程内一次）：无 `trae_access` → 跳过；解密 access，uid 取 `trae_uid`；access 或 uid 无效 → **仅清除** 5 条旧 key；有效 → 重新加密写入 `trae_acc_<uid>_*`、建单条索引（region 用 uid 长度推断，nick 取 `trae_nick`，exp 取 `trae_exp` 数值）、`trae_active_uid=uid`，删 5 条旧 key。
- `repairMislabeledRegions`（每次 list/activeUid 都跑）：对每条 meta 重算 `inferRegionFromUid(uid)`（取 uid 中的数字字符，**长度 ≥19 → SG，否则 CN**；实测样本 CN=16 位、SG=19 位），不一致就**覆写 region**（幂等）。⚠️ 移植决策：这是为一个迁移 bug 写的一次性修补，且会无差别覆盖区域字段；桌面版存储全新、导出 JSON 自带 `region`，**建议不移植**（或只在"从 Android 迁移数据"通道里跑一次）。

### 3.6 selfCheck 断言清单（建议 1:1 变 Rust test）

索引 JSON 往返无损；`region` 解析 CN/SG 各对；16 位 uid→CN、19 位→SG；failover 四场景（SG 挂→换 CN；只剩 SG→给 SG；单号→null；CN+CN2+SG 换掉 CN→给 CN2）。

---

## 4. Qoder 侧

### 4.1 host / 模式 / header 基元

| 项 | cn | global |
|---|---|---|
| OpenAPI base（所有非聊天端点） | `https://openapi.qoder.com.cn` | `https://openapi.qoder.sh` |
| infer（聊天 COSY `/algo/`，QoderUpstream 用，非本文件实现） | `https://gateway.qoder.com.cn` | `https://api2.qoder.sh` |

两套 host 绝不能混：国内 jobToken 打国际 infer 一律 401。mode 常量：`MODE_CN="cn"`、`MODE_GLOBAL="global"`；未识别 mode 一律按 cn。

端点：`/api/v1/jobToken/exchange`、`/api/v1/jobToken/refresh`、`/api/v1/userinfo`、`/api/v2/quota/usage`、`/sash/api/v1/me/campaigns`（campaigns 带 `/sash` 前缀，与 usage 不同链路）、`/api/v1/models`、`/api/v1/model/list`。

HTTP 客户端：connect 20s / read 30s。

三套头（POST/GET 公共小函数）：
- `post()`（exchange/refresh 用，无鉴权头）：`Content-Type: application/json`、`Accept: application/json`、`User-Agent: traework2api`、`Cosy-Version: 1.0.1`、`Cosy-ClientType: 5`。
- `getBearer()`（userinfo、模型目录 `rawRequest` 用）：`Authorization: Bearer <t>`、`Accept`、`UA: traework2api`、`Cosy-Version: 1.0.1`、`Cosy-ClientType: 5`。非 2xx → `IOException("Qoder {code}: {body前200字}")`。
- `getWithIdentity()/postClaimRaw()`（campaigns/claim，另见 4.6 档位）：`Authorization: Bearer <jt>`；UA：desktop 档 `Qoder`，minimal 档 `traework2api`；`Cosy-Version`：desktop `id?.version ?: "1.1.69"`（QoderMachine.COSY_VERSION），minimal `1.0.1`；`Cosy-ClientType: 10`（QoderMachine.CLIENT_TYPE，**两档都发 10**）；desktop 再叠加 `applyMachineHeaders`：`Cosy-MachineId`、`Cosy-MachineToken`、`Cosy-MachineOS`，以及非空才发的 `Cosy-MachineType`、`Cosy-MachineHostname`；id 为 null 时机器头整体不发。

### 4.2 PAT 校验与使用

- PAT 外形约定：以 `pt-` 开头（仅 UI 提示语 `粘贴 Qoder 个人访问令牌 pt-…`；**代码里没有前缀强校验**）。
- 真正的"校验"= 服务端 exchange：`loginWithPat` 流程 trim → 非空白（否则 `IllegalArgumentException("请粘贴 PAT")`）→ `exchangeJobToken` → `fetchUserInfo` → uid 空白则 `IllegalStateException("Qoder 未返回账号 ID，请确认 PAT 与区域（国内/国际）匹配")`（mode 选错是首要嫌疑）→ 组 `QCred`（见 4.5 字段）→ `QoderAuthStore.save`（设为当前）。
- PAT 不能直接调任何 API；一切调用都走 jobToken。

### 4.3 三段交换 / 凭证换取 API

**exchange（PAT → jobToken）**：
- `POST {openApi(mode)}/api/v1/jobToken/exchange`，头见 `post()` 基元。
- 体：`{"personal_token":"<pat>"}`
- 响应字段：`token`（空白 → `IOException("Qoder exchange 未返回 token")`）→ jobToken（`jt-` 前缀，仅作约定）；`refresh_token`（可为空串）；到期见下文 `parseExpiry`。

**refresh（jobRefreshToken → 新 jobToken）**：
- `POST {openApi}/api/v1/jobToken/refresh`，体 `{"refresh_token":"<jrt>"}`；jrt 空白先抛 `IllegalArgumentException("缺少 jobRefreshToken，无法续期")`。
- 响应：`token`（空白报错）；`refresh_token` **缺省时沿用传入的 jrt**（`optString("refresh_token", jrt)`——与 Trae 的 null 兜底不同，这里空串也会回落旧值）；到期同 parseExpiry。

**到期解析（parseExpiry，unix 毫秒）**：
```
expires_in = o.optLong("expires_in", 0)      // 实测单位是毫秒（国内/国际 OpenAPI 一致）
>0  → now_millis + expires_in
否则 → now_millis + 24*3600*1000             // 缺省 24h
```
不解析 `expires_at` 字符串（规避 Android java.time 限制；Rust 侧同样只需 `expires_in`）。

**userinfo（GET Bearer）** `GET {openApi}/api/v1/userinfo`：
- `uid = o["uid"] 或 "id" 或 "userID"`（三种历史写法依次回退）；`email`；`name = "name" 或 "username"`；
- COSY 聊天签名材料：`organization_id`、`organization_name`、`organization_tags`（**数组 或 逗号分隔字符串**两种形态都吃）、`data_policy_agreed`（bool，缺省 false）、`is_data_policy_modifiable`（bool，缺省 true）。
- 任何异常 → 返回全空 `QUserInfo("","","")`（调用方靠 uid 判失败）。

### 4.4 jobToken 有效性与 `validJobTokenFor` 决策树（skewMillis 单位=**毫秒**）

与 Trae 的方向相反：这里传的是毫秒，比较用严格 `<`：
```
now = System.currentTimeMillis()                    // unix 毫秒
1. if cred.jobToken 非空白 且 (now + skewMillis < cred.expiresAt):
       return cred.jobToken                          // 未临期（expiresAt: unix 毫秒）
2. renewed = runCatching {
       if jobRefreshToken 非空白 → refreshJobToken(jrt, cred.mode)
       else if pat 非空白        → exchangeJobToken(pat, cred.mode)
       else null
   }.getOrNull()
   null（含异常）→ return cred.jobToken.takeIf{非空白}   // 续不动回落旧 jt（可能已过期）
3. QoderAuthStore.updateTokens(cred.copy(
       jobToken = renewed.token,
       jobRefreshToken = renewed.refreshToken.ifBlank { 旧 jrt },
       expiresAt = renewed.expiresAtMillis))
       // updateTokens 会连 pat 一起重加密写回 + writeMeta（expiresAt 落在索引里）
4. return renewed.token
```
调用现网 skew：Gateway 转发 3_600_000ms（60 分钟）；claim/diagnose/claimedToday 60_000ms（60 秒）；AccountCredits 3_600_000ms。`validJobToken(context, skew)` = load(active)→validJobTokenFor。
**`forceRefreshJobToken(context, cred)`**：跳过第 1 步检查直接走 2→4；区别是续不动返回 **null**（不回落旧 jt）——401 救急用，调用方据此报 401 给客户端，不无限重试。
注意 Qoder 路径**没有** Trae 那套"库里最新凭据/写回守卫"双保险（jrt 不是轮换制令牌族），Rust 可沿用简单模型。

### 4.5 QoderAuthStore 字段与持久化 key

prefs 文件：`qoder2api_prefs`；KeyStore 别名 `qoder_aes_key`；密文格式同 Trae（`base64(iv):base64(ct)`，AES-256-GCM-128-tag）。

| key | 内容 |
|---|---|
| `qoder_accounts` | JSON 数组，条目：`{"uid","name","email","mode","expiresAt"(毫秒),"organizationId","organizationName","organizationTags":[…],"dataPolicyAgreed":bool,"isDataPolicyModifiable":bool}` |
| `q_active_uid` | 当前账号；删空回退顺序同 Trae（first→删当前置 ""） |
| `q_acc_<uid>_pat` / `q_acc_<uid>_jt` / `q_acc_<uid>_jrt` | 三类凭据密文 |
| `q_acc_<uid>_fp` | 抓包真机指纹密文（见 4.7） |
| `q_machine_id` | **明文** UUID；读回条件 `length>=16`，否则重新生成并落盘。必须持久稳定（设备身份 + 参考实现用它前 16 字符做派生密钥） |
| `qoder_campaign_variant_<mode>`（在 **`traework2api_prefs`**！与 Trae 同文件，QoderAuthApi 读写） | `minimal`/`desktop`，上次验证出活动的档位 |

`QMeta`（索引条目）：`uid,name,email,mode("cn"|"global"),expiresAt(long 毫秒,0=未知), organizationId, organizationName, organizationTags:List<String>, dataPolicyAgreed=false, isDataPolicyModifiable=true`（后 5 项 COSY 材料非敏感、明文随索引存）。
`QCred`（内存）：meta 全字段 + `pat, jobToken, jobRefreshToken` + `expiresAt`（毫秒）；`chatReady = uid.isNotBlank() && jobToken.isNotBlank()`。
读索引：`organizationTags` 兼容数组与逗号串（`jsonToStringList`）；uid 空白丢弃；mode 缺省 `"cn"`；整串解析失败 → 空列表。**无旧版单账号迁移、无区域修补**（比 Trae 干净）。
`loadByUid`：meta 无 → null；`(pat 解密后空白) 且 (jt 空白)` → null；其余密文缺失一律**空串**。
`save`=写密文×3+writeMeta+setActive；`updateTokens`=（在索引才）写密文×3+**writeMeta**、不动 active（与 Trae updateTokens 的差异：Qoder 版本**顺带写 meta**）；`remove`=删 `_pat/_jt/_jrt` 三 key+索引回写+active 回退（**不删 `_fp`**——注意 remove 里没有 remove(fpKey)，Rust 移植时可顺手修正）。`selfCheck`：指纹往返、空指纹→null、索引往返（含组织字段）、旧索引无组织字段→读默认。
导出/导入 JSON：外壳 `{"format":"traework2api-qoder-accounts","version":1,"activeUid","accounts":[{uid,name,email,mode,expiresAt,organizationId,organizationName,organizationTags:[…],dataPolicyAgreed,isDataPolicyModifiable,pat,jobToken,jobRefreshToken,fingerprint:<encodeFingerprint 后的 JSON 字符串，可为空串>}]}`。导入：uid 或 pat+jt 双空则跳过；`fingerprint` 非空且可解才 `setFingerprint`（老备份缺字段→当作本机派生）；`imported==0` 抛 `"JSON 里没有可导入的 Qoder 账号（缺 uid 或 pat）"`；activeUid 命中才切。

### 4.6 积分 / campaigns / claim（PAT 的下游用途）

**fetchCredits(jt, mode)**：`GET {openApi}/api/v2/quota/usage`（`getWithIdentity` minimal、无机器身份）。响应若根上无 `userQuota/personalQuota/isQuotaExceeded` 且有 `data` → 下钻一层。余量累加：`userQuota,personalQuota,addOnQuota,addonQuota,extraQuota` 各对象的 `remaining`；`orgResourcePackage,organizationQuota` 仅当 `available`(缺省 true) 才算。`isQuotaExceeded`(bool) 照抄；remaining clamp≥0。`hasCredit = !exceeded && remaining>0`。异常→null。

**campaigns（每日 Credits 领取）**：
- 列：`GET {openApi}/sash/api/v1/me/campaigns?forceRefresh=true`；领：`POST …/campaigns/{id}/claim`，体 `{}`（幂等），**claim 与 list 必须同档头**（同一 variant/identity）。
- `parseCampaigns`：根无 `campaigns` → 下钻 `data`；`showCampaign` 三态（root 有→bool；原始 root 有→bool；都无→**null**——只有桌面档的空列表才是"真没活动"，minimal 空是被静默隐藏）。id 键依次 `campaignId`→`campaignKey`→`id`，空白丢弃。可领判据：`id.isNotBlank() && claimStatus.equals("CLAIMABLE", ignoreCase)`（**不再限定 CLAIM_BENEFIT**——VIEW_DETAILS 类也要领以对齐桌面端）。
- 档位阶梯 `loadCampaigns`：起点 = 记忆档位（默认 minimal）；记忆为 desktop 则只打 desktop；否则 `[minimal, desktop]`，**空列表才升档**；非空且升档成功→记住该档（按 mode 分别记）。任意档请求含 `401` 且未重试过 → 调 `refreshJt`(=forceRefreshJobToken) 换 token 后**同档**重发一次；末档仍失败 → `CampaignLoad(list=null, variant, error=msg)`。
- `parseClaim`：`status` 空白且有 `data` → 下钻；`claimed = status=="CLAIMED`(ignoreCase)`；`newly = claimed && !replayed`；额度 `benefit.amount`（缺 0；非新领计 0）。
- `claimDailyForCred` 汇总语义：jt 拿不到 → `fail "没有有效 token，请重新粘贴 PAT 登录"`；列表拉不到 → `fail "拉取活动失败：{err}"`；无可领 → `already`，文案四分支（有 Credits 类且全 CLAIMED→"已领取 N 条"；全为查看类→"无积分活动…不发 Credits"；均处理→"无待领活动…"；空列表→"今日无可领活动（上游返回空列表）"）；逐条 try claim → `ok`（额度>0 报入账，0 报条数）、失败全吞记 lastErr → `already "领取未生效：{err}"` 或 "今日已领取"。
- `claimDailyAll`：**阻塞**函数，逐号 `Thread.sleep(600ms)` 间隔（风控），单号异常→fail 条目。`claimedTodayFor` 只读判"今天领没领完"（true/false/null）。`diagnose(ctx)` 拼接两档原始响应+逐条 claim 的排障文本（移植建议保留，UI 排障依赖）。

**模型目录 fetchModelCatalog**：候选 `/api/v1/models`→`/api/v1/model/list`，每路径 GET→POST `{}`；成功判据 = `QoderModelCatalog.parseCatalogJson(text)` 非空集（HTTP 200 但内容不是目录=失败）；非 2xx **不抛**，继续下一候选。失败细节进 `lastCatalogError`（如 `GET /api/v1/models → HTTP 401`）/`lastCatalogRaw`（两个 `@Volatile var`，UI 诊断用）。jt 空白直接 null，不发请求。

### 4.7 QFingerprint（抓包真指纹）

存在必要：国际版 campaigns 只下发给带真实 `cosy-machinetoken` 的请求；该 token 由阿里 umid SDK 签发且服务端校验来源，自造无效（单变量对照：自造→no_campaign）。故"只能存抓来的真货"。
`QFingerprint(machineId, machineToken, machineType="5", machineOS="aarch64_darwin", hostname="localhost", clientType="10", version="1.1.69")`；密文内 JSON 短键：`id/tk/ty/os/hn/ct/ver`；解码：`id` 或 `tk` trim 后空白 → null；各字段缺省/空白回退到上述默认值。QoderAuthApi.campaignIdentity(desktop 档) = `QoderMachine.identity(ctx, fingerprint(ctx, uid))`（有抓包用抓包，无则本机派生同构身份）。

---

## 5. "账号被服务端拒"标记与重新登录提示的数据流

共识：**存储层（两个 Store）没有任何持久化的 rejected/banned 标志**。"被拒"是运行期派生状态，链路如下（移植时保持同构即可）：

**Trae 检测源**：`TraeUpstream.ug()`（quota/签到一类 GET）内部 `@Volatile var lastAuthFailed`——每次调用先置 false，命中 `HTTP 401/403` 或 **HTTP 200 + 业务体 `code==1001`** 时置 true（ug 返回 null）。上层用 `lastAuthFailed` 区分"网络抖动" vs "token 被拒"。

**Trae 标记/提示链**：
1. `AccountCredits` 逐号刷积分：`restartWithValidTokenFor` 后 token 仍空 → Item.error=`"需重新登录"`；`fetchUsage` 失败 → 先 `Gateway.locked{forceRefreshTokenFor}` 强制续一次再试；仍失败，`if (TraeUpstream.lastAuthFailed) "需重新登录" else "查询失败"` 写进该 uid 的积分条目（内存，按 uid）。
2. `MainActivity`：`authFailedUid: String?`（页面手动实测失败时按 **uid** 记，绝不记全局 bool——历史 bug：切走坏账号不清标志，好账号也显示需重新登录）。判定统一收口 `authFailedFor(uid) = authFailedUid==uid || AccountCredits.current().trae(uid)?.error=="需重新登录"`；UI 文案 `tokenStatusFor` 优先返回 `"需重新登录"`，其次看剩余秒数出 `"已过期 · 需重新登录"`。
3. `BatchCheckin`（签到）：`ensureFreshTokenFor` null → `"token 已过期且续不回来，需重新登录"`；签到失败且 `lastAuthFailed` → `"token 被服务端拒绝，需重新登录"`；签到接口 200+业务拒绝时不许误报成功（0.7.x 修复）。
4. 网关自动换号（`Gateway.failoverProvider`+`LocalFakeServer.streamWithFailover`）：上游**吐正文前**失败（401/429/5xx）→ 先 `refreshSameAccount`（=forceRefreshTokenFor）同账号自救 → 仍失败 `pickFailoverCandidate` 选候选 → **真切换并持久化** `setActive + syncRegionToActiveAccount + restartWithValidToken` → 候选仍失败 `failoverRollback` 切回原 uid + 同步区域（防请求间来回跳）。

**Qoder 检测/提示链**：转发侧 `QoderUpstream.chatStream` 收一个 `refreshJobToken` 回调，401 时先 `forceRefreshJobToken` 强续重试；续不动 → `LocalFakeServer` 对人话报错：401 出口文案 `"鉴权失败：Qoder 的 jobToken 可能已失效，回 App 重新登录一次。"`；cred 缺 jt → `"Qoder 账号不可用：jobToken 为空或已失效。请回 App 重新登录 Qoder…"`。Qoder 网关换号 `qoderFailoverProvider` 用 `QoderAuthStore.list().firstOrNull{uid!=failed}`（**无 isSupported 加权**，只有 mode 概念）。积分条目同 Trae：jt 双空 → "需重新登录"，否则"查询失败"（Qoder 没有 lastAuthFailed 等价物——fetchCredits 吞异常返回 null，无法区分，移植可选增强）。claim 失败文案 `"没有有效 token，请重新粘贴 PAT 登录"` 直给 UI。

---

## 6. 全部常量表

| 常量 | 值 | 出处/用途 |
|---|---|---|
| `TraeRegion.IDE_VERSION` | `"0.1.43"` | UA `Trae/0.1.43`、X-Ide-Version、GetUserInfo.IDEVersion、x_app_version |
| `CALLBACK_HOST/PORT/PATH` | `127.0.0.1` / `18080` / `/authorize` | 回调前缀 |
| `EP_EXCHANGE` | `/cloudide/api/v3/trae/oauth/ExchangeToken` | |
| `EP_USER_INFO` | `/cloudide/api/v3/trae/GetUserInfo` | |
| 登录参数 `plugin_version` / `login_version` / `auth_from` / `login_channel` / `auth_type` / `redirect` / `x_device_brand` / `x_device_type` / `x_os_version` / `x_app_type` | `2.3.62834` / `1` / `solo` / `native_ide` / `local` / `0` / `PC` / `PC` / `1.0` / `stable` | buildLoginUrl |
| ExchangeToken 体常量 | `ClientSecret="-"`、`UserID=""` | |
| 毫秒→秒阈值 | `1_000_000_000_000`（>则 /1000） | normalizeMs |
| device/machine 随机 | hex 16 字节（32 字符）×2；trace 8 字节（16 字符） | SecureRandom |
| hex 格式 | 小写 `%02x` | |
| clientId CN / SG | `en1oxy7wnw8j9n` / `ono9krqynydwx5` | |
| CN hosts | console `https://www.trae.cn`；oauth `https://api.trae.com.cn`；ug `https://api.trae.cn`；agent `https://trae-api-cn.mchost.guru` | |
| SG hosts | console `https://www.trae.ai`；oauth `https://ug-normal.trae.ai`；ug 同左；agent `https://core-normal.trae.ai` | |
| region 推断 | 数字长度 ≥19 → SG，否则 CN；默认 CN | inferRegionFromUid |
| Trae 现网 skew | Gateway 转发 `skewSec=3600`；积分/监测/签到 `3600`（秒）；selfCheck 用 3600 | |
| auth client 超时 | connect 20s / read 30s（TraeAuthApi、QoderAuthApi 相同；TraeUpstream 另有 read 180s 非本域） | |
| prefs 名 | Trae+全局 `traework2api_prefs`；Qoder `qoder2api_prefs` | |
| KeyStore 别名 | `trae_solo_aes_key` / `qoder_aes_key` | 桌面版移除 |
| 密文分隔符 / Base64 | `":"` / NO_WRAP | |
| GCM | AES-256、tag 128 bit、IV=cipher 默认 12B | |
| `EXPORT_FORMAT` | `"traework2api-accounts"` / `"traework2api-qoder-accounts"` | version=1 |
| Qoder OpenAPI/infer hosts | cn `https://openapi.qoder.com.cn`,`https://gateway.qoder.com.cn`；global `https://openapi.qoder.sh`,`https://api2.qoder.sh` | |
| Qoder mode | `cn`/`global`；未识别按 cn | |
| Qoder jt 到期缺省 | `24h`（expires_in 缺省/≤0，单位**毫秒**） | parseExpiry |
| Qoder 现网 skew | 转发 `SIXTY_MINUTES_MS=3_600_000L`；领号/诊断 `60_000L`；积分 `3_600_000L`（全毫秒）；Trae 的 `SIXTY_MINUTES_SEC=3600L` 同文件并存、单位各异——移植时禁止交叉传 | |
| Qoder 基础头 | `UA: traework2api`、`Cosy-Version: 1.0.1`、`Cosy-ClientType: 5`（post/getBearer/目录）；campaigns 两档 `Cosy-ClientType: 10` | |
| `QoderMachine.COSY_VERSION` / `CLIENT_TYPE` | `"1.1.69"` / `"10"` | desktop 档默认 |
| desktop 档头 | `UA: Qoder` + `Cosy-MachineId/Token/OS`（+非空的 `Cosy-MachineType/Hostname`） | applyMachineHeaders |
| 指纹默认值 | `machineType=5, OS=aarch64_darwin, hostname=localhost, clientType=10, version=1.1.69` | QFingerprint |
| `q_machine_id` | 明文 UUID；复用条件 length≥16 | |
| campaigns | 列表 `?forceRefresh=true`；claim 体 `{}`；claimable=CLAIMABLE；startAt/endAt（若用）为**秒** | |
| claim 批量间隔 | `600L` ms/账号 | claimDailyAll |
| 错误截断 | 上游 body 进异常消息一律 `take(200)` | |
| claim 状态 | `ok / already / fail` | |
| 业务鉴权失败码 | Trae HTTP 200 + code `1001` ≡ 401/403 → lastAuthFailed；SG OAuth 错误结构 code `10101`/`040004`（同构可复用） | |

---

## 7. 建议 Rust 模块 API 草案 + JSON 存储 schema（桌面版替代 SharedPreferences）

### 7.1 模块划分与 API 草案

```rust
// crates/trae/region.rs
pub enum Region { Cn, Sg }                    // is_supported(): 仅 Cn
pub const IDE_VERSION: &str = "0.1.43";
impl Region {
    pub fn console_host(&self) -> &'static str;
    pub fn oauth_host(&self) -> &'static str;
    pub fn client_id(&self) -> &'static str;
    pub fn from_id(id: Option<&str>) -> Region; // trim+lowercase, 兜底 Cn
}
pub static CURRENT: Lazy<Mutex<Region>>;        // 替代 @Volatile TraeRegion.current

// crates/trae/auth.rs
pub struct TraeCred { access_token, refresh_token, expires_at: i64 /*秒*/,
                      uid, nickname, region: Region }
pub struct ExchangeResult { access_token: String, refresh_token: Option<String>,
                            expires_at: Option<i64> }
pub fn build_login_url(region: Region) -> String;
pub fn is_callback(url: &str) -> bool;
pub fn extract_refresh_token(callback: &str) -> Option<String>;
pub async fn exchange_token(refresh: &str, region: Region) -> Result<ExchangeResult>;
pub async fn get_user_info(access: &str, region: Region) -> Result<UserInfo>;
pub async fn login_with_callback_url(store: &Store, url: &str) -> Result<TraeCred>;
pub fn expired_within(expires_at: i64, skew: Duration) -> bool;   // ≤0/null 均为 true（用 Option<i64> 更明确）
pub async fn restart_with_valid_token(store, skew: Duration) -> Option<String>;
pub async fn ensure_fresh_token_for(store, cred, skew) -> Option<String>;  // 唯一入口
pub async fn force_refresh_token_for(store, cred) -> Option<String>;
fn reuse_after_race(stale: &str, stored: &str) -> bool;            // 单测直接移植
fn should_write_back(used: &str, stored: &str) -> bool;
// refreshLocked 逻辑保留，整段包在 store.refresh_lock() (tokio::sync::Mutex) 内，等价 Gateway.locked

// crates/trae/store.rs
pub struct AccountStore { /* 单 JSON 文件 + 内存缓存 + Mutex */ }
impl AccountStore {
    pub fn list(&self) -> Vec<AccountMeta>;
    pub fn active_uid(&self) -> Option<Uid>;
    pub fn load(&self) -> Option<TraeCred>;           // active
    pub fn load_by_uid(&self, uid: &str) -> Option<TraeCred>;
    pub fn save(&self, cred: &TraeCred);              // +设为 active
    pub fn update_tokens(&self, cred: &TraeCred);     // 不碰 active/meta
    pub fn update_meta(&self, meta: &AccountMeta);
    pub fn remove(&self, uid: &str);
    pub fn set_active(&self, uid: &str);
    pub fn sync_region_to_active(&self);              // 写 CURRENT + config.region
    pub fn pick_failover_candidate(&self, failed_uid) -> Option<AccountMeta>;
    pub fn export_json(&self) -> String; pub fn import_json(&self, s: &str) -> Result<usize>;
}

// crates/qoder/auth.rs
pub struct JobToken { token, refresh_token: String, expires_at_ms: i64 }
pub struct QCred { pat, job_token, job_refresh_token, expires_at_ms: i64,
                   uid, name, email, mode: QMode,
                   organization_id, organization_name, organization_tags: Vec<String>,
                   data_policy_agreed, is_data_policy_modifiable: bool }
pub async fn exchange_job_token(pat, mode) -> Result<JobToken>;
pub async fn refresh_job_token(jrt, mode) -> Result<JobToken>;
pub async fn fetch_user_info(jt, mode) -> QUserInfo;          // 永不失败，uid 空=失败
pub async fn login_with_pat(store, pat, mode) -> Result<QCred>;
pub async fn valid_job_token_for(store, cred, skew: Duration /*毫秒域*/) -> Option<String>;
pub async fn force_refresh_job_token(store, cred) -> Option<String>;
pub async fn fetch_credits(jt, mode) -> Option<QCredits>;
pub async fn load_campaigns / claim_daily_for_uid / claim_daily_all / claimed_today_for / diagnose(...) -> String;
pub async fn fetch_model_catalog(jt, mode) -> Option<String>; // + Atomic lastCatalogError/Raw

// crates/qoder/store.rs —— 结构同上；另 machine_id(): 首次 UUIDv4 落盘、fp(get/set/clear) per-uid

// 通用：reqwest Client connect_timeout 20s/read 30s；错误信息一律 truncate(body, 200)；
// 全部 selfCheck() 平移为 #[test]（含 Trae 边界 skews、failover 4 场景、Qoder parseCampaigns/parseClaim 样例 JSON）。
```

关键移植决策：
- **单位**：Trae `expires_at` 用 `UnixSeconds(i64)`、Qoder 用 `UnixMillis(i64)` 新类型，编译期杜绝 `SIXTY_MINUTES_SEC/MS` 混用事故。
- **时钟**：`System.currentTimeMillis` → `SystemTime::now()`（本地墙钟语义一致即可；若上游按服务端时间验签再考虑 monotonic+校准）。
- `migrateLegacyIfNeeded`/`repairMislabeledRegions`/AndroidKeyStore 整体不移植；`pickFailoverCandidate` 的 uid 长度推断只为"读 Android 导出备份"保留一个 `infer_region_for_legacy(uid)`，正常写入路径以显式 region 为准。
- 并发：一把 `async Mutex`（如 `Gateway.locked`）串行化「续期 vs 转发 vs 批量签到」，`ensure_fresh_token_for` 的双检+写回守卫原样保留。

### 7.2 JSON 文件存储 schema（逐 key 映射提案）

数据目录：`~/.local/share/traework2api/`（Tauri `app_data_dir`）。两个账号文件 + 一个全局配置文件，**明文 JSON + 文件权限 0600**（桌面没有硬件 KeyStore；如需加密可选 `tauri-plugin-stronghold` 或 OS keychain，接口不变）。

`accounts.trae.json`（原子写：tmp+rename；结构复用 `EXPORT_FORMAT` 外壳，向后兼容现有导出文件）：
```json
{ "format": "traework2api-accounts", "version": 2, "activeUid": "…",
  "accounts": [ { "uid": "", "nickname": "", "region": "cn|sg", "expiresAt": 0,
                  "accessToken": "", "refreshToken": "" } ] }
```
`accounts.qoder.json`：
```json
{ "format": "traework2api-qoder-accounts", "version": 2, "activeUid": "",
  "accounts": [ { "uid","name","email","mode","expiresAt"(毫秒),
                  "organizationId","organizationName","organizationTags":[],
                  "dataPolicyAgreed","isDataPolicyModifiable",
                  "pat","jobToken","jobRefreshToken",
                  "fingerprint": { "machineId","machineToken","machineType","machineOS",
                                   "hostname","clientType","version" }  // null=本机派生 } ] }
```
`config.json`：
```json
{ "traeRegion": "cn", "qoderMachineId": "…uuid…",
  "campaignVariant": { "cn": "minimal|desktop", "global": "…" } }
```

逐 key 映射（Android → 桌面 JSON 路径）：

| SharedPreferences key | 新位置 |
|---|---|
| `trae_accounts[i].uid/nickname/region/expiresAt` | `accounts.trae.json → accounts[i].uid/nickname/region/expiresAt` |
| `trae_acc_<uid>_access`（密文） | `accounts.trae.json → accounts[i].accessToken`（明文，per-entry 合并进索引） |
| `trae_acc_<uid>_refresh`（密文） | `accounts.trae.json → accounts[i].refreshToken` |
| `trae_active_uid`（含空串） | `accounts.trae.json → activeUid`（空串→null） |
| `trae_region` | `config.json → traeRegion` |
| `trae_access/trae_refresh/trae_uid/trae_nick/trae_exp` | 无对应（导入一次性迁移，按 §3.5 语义） |
| `qoder_accounts[i].*`（10 字段） | `accounts.qoder.json → accounts[i].*` 同名 |
| `q_acc_<uid>_pat/_jt/_jrt`（密文） | `accounts.qoder.json → accounts[i].pat/jobToken/jobRefreshToken` |
| `q_acc_<uid>_fp`（密文内 JSON 短键 id/tk/ty/os/hn/ct/ver） | `accounts.qoder.json → accounts[i].fingerprint`（展开为全名键；读旧备份时短键映射回来） |
| `q_active_uid` | `accounts.qoder.json → activeUid` |
| `q_machine_id` | `config.json → qoderMachineId` |
| `traework2api_prefs: qoder_campaign_variant_cn/global` | `config.json → campaignVariant.cn/global`（注意它原本在 Trae prefs 文件里） |

`AccountMeta.chatReady` 等派生属性改由结构体 `impl` 提供；`expiresAt=0/""` 等"缺省即未知"约定全部保留（决策树依赖它）。导入合并语义（按 uid 覆盖/追加、activeUid 命中才切、计数校验与错误文案）建议逐字保留，两平台导出格式已天然是这份 schema 的超集。
