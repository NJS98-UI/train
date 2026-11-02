# TraeWork2API 分析：模型仓库 / Qoder 目录 / 用量 / 账号备份

> 供 Tauri(Rust) 移植使用。源文件逐行核对范围：
> `ModelsRepo.kt`(105)、`QoderModelCatalog.kt`(561)、`QoderModelCache.kt`(226)、`UsageStore.kt`(97)、`AccountBackup.kt`(61)，
> 以及 `LocalFakeServer.kt` 中模型/健康检查相关片段、三级降级实际实现所在的 `MainActivity.kt` / `QoderUpstream.kt` / `QoderAuthApi.kt` 相关段落。
> Android `SharedPreferences` 在 Rust 端建议映射为单一 JSON 配置文件（如 `~/.local/share/traework2api/*.json`），下文逐处标注 key。

---

## 1. ModelsRepo（Trae 模型表，object 单例）

文件：`app/src/main/java/com/example/traework2api/ModelsRepo.kt`

### 1.1 定位与语义

- **只有上游拉回来的表，没有内置兜底清单**（0.4.3 起删除静态表；0.4.8 起增加磁盘持久化）。
- 未刷新且无磁盘缓存时 `current()` 返回**空数组**（空表是合法状态：`/v1/models` 返回空列表、模型 Tab 显示「尚未加载」，属设计意图）。
- `LocalFakeServer.staticModels()`（`/v1/models` 输出）与「模型」Tab 都读 `current()`。

### 1.2 持久化格式

| 项 | 值 |
|---|---|
| SharedPreferences 名 | `traework2api_models` |
| key | `models_json` |
| value | 上游模型 JSONArray 的 `toString()` 原文（无外壳对象） |

存储的数组元素由 `TraeUpstream.parseModels()` 生成，每项固定 7 个字段（见 1.5）。

### 1.3 状态与缓存语义（`current()`）

```kotlin
private val EMPTY = JSONArray()          // 共享空实例，避免每次 new
@Volatile private var dynamic: JSONArray? = null   // null = 内存里还没刷过
@Volatile private var appContext: Context? = null

fun current(): JSONArray = dynamic ?: EMPTY        // 从不返回 null
val isDynamic: Boolean get() = dynamic != null     // UI 区分"尚未加载/已加载"
```

- `init(ctx)`（冷启动只调一次，须在首个 `/v1/models` 请求前完成，见 `MainActivity.kt:291`）：
  1. `appContext = applicationContext`；
  2. 若 `dynamic != null` 直接返回（幂等）；
  3. 读 prefs 字符串；无值/读取异常 → 返回（保持空状态）；
  4. `JSONArray(json)` 解析失败 → 丢弃当没存过；
  5. **仅当 length > 0** 才装入 `dynamic`。

### 1.4 写入：`applyFetched(arr)`

- **空表一律拒绝覆盖**（`length() > 0` 闸）——刷新失败绝不能把已有清单清空。
- 非空表：置 `dynamic = arr` **并且同步落盘**（`putString(KEY_MODELS, arr.toString()).apply()`），落盘包在 `runCatching` 里失败静默。

### 1.5 拉取流程（Trae 上游）

上游请求（`TraeUpstream.fetchModels`，`TraeUpstream.kt:491`）：

- 区域闸门：`reg.canQueryQuota == false`（`ugHost` 为空）直接返回 null，不拉表。
- `POST {agentHost}/api/ide/v1/get_detail_param`（CN: `https://trae-api-cn.mchost.guru`，SG: `https://core-normal.trae.ai`），带 SOLO 签名头（token 只进内存与请求头）。
- 请求体固定：
  ```json
  {"function":"solo_work_remote","config_names":null,"need_prompt":false,
   "current_config_info":null,"poly_prompt":true,"mode_type":null,"agent_type":null}
  ```
- HTTP 非 2xx / 异常 → null。

解析 `parseModels(root)`（`TraeUpstream.kt:533`）：

1. 取根对象 `config_info_list` 数组，缺 → null。
2. 逐条过滤 `isChatModel(cfg)`（`TraeUpstream.kt:584`），全部满足才保留：
   - `config_source == 1`（只留官方内置，`3` = 用户自接入第三方）；
   - 有 `display_config`，且 `display_config.is_custom_model != true`；
   - `is_invisible_to_user != true`（上游标注对用户隐藏的项，如 `glm-5`）；
   - `config_name` 小写后不以 `custom_model` 开头，且不含词形：`agent`、`subagent`、`summary`、`sagitta`、`aquila`、`file_search`、`explore_`、`computer_use`、`browser_use`。
   - `config_name` 为空跳过。
3. 输出项（`/v1/models` 形状，即 `models_json` 落盘内容）：
   ```json
   {"id": "<config_name>", "name": "<display_config.display_name 或回落 id>",
    "config_name": "<id>", "context_length": <model_detail_list[0].max_input_tokens，缺省回落 131072>,
    "object": "model", "owned_by": "trae-solo", "created": 1753600000}
   ```
4. 结果为空数组 → 返回 null（调用方保留现状）。

### 1.6 静默补新流程（MainActivity）

- `refreshModels(quiet=false)`（`MainActivity.kt:2579`）：
  - 当前后端是 Qoder → 转 `syncQoderCatalog(quiet)`（见 §2.5），Trae 链不走；
  - `auto` 混合模式（Trae+Qoder 都有账号）→ 先顺手 `syncQoderCatalog(quiet=true)`，再继续 Trae 刷新；
  - 无 Trae 凭证 → 非静默时 toast「未登录 Trae」；
  - `modelsLoading` 布尔闸防并发；后台线程 `runCatching { TraeUpstream.fetchModels(...) }`，**无论成败回主线程复位标志**（防"刷新中…"永久卡死）；
  - 拉回 null：保持磁盘缓存顶着，非静默 toast「拉取失败」；
  - 成功：`ModelsRepo.applyFetched(arr)`，非静默 toast 条数变化；最后 `modelsRefreshApply?.invoke()` 原地重画模型页（不 render 整页，保留搜索框/滚动位置）。
- 进 App 自动补新 `maybeAutoRefreshModels()`（`MainActivity.kt:2633`，onCreate 调用于 `:408`）：
  - 守卫是进程级一次性布尔 `autoModelsTried`（不能用 `isDynamic`——init 装过磁盘缓存后恒 true）；
  - `if (autoModelsTried || modelsLoading) return; autoModelsTried = true; contentHost.post { refreshModels(quiet = true) }`；
  - 静默模式失败无感（离线启动是正常边界，磁盘缓存继续提供上次清单）。
- `selfCheck()`（`:89`）钉住三条行为：未刷新为空、空表拒覆盖、非空可装入且 current 跟随；**必须在 init 之前跑**，且最后把 `dynamic` 复位 null 防止污染。Rust 端可保留为单元测试。

---

## 2. QoderModelCatalog（Qoder 模型目录，object 单例）

文件：`QoderModelCatalog.kt`。与 Trae 本质区别：**模型是协议的一部分**——请求要带 `model_config.key`，上游按 key 路由，所以必须有权威兜底清单 + 宽容的名字解析。

### 2.1 QModel 数据结构（逐字段）

```kotlin
data class QModel(
    val key: String,                    // 请求 model_config 用的内部 key
    val name: String,                   // 展示名（也是客户端可用别名）
    val contextLength: Int,             // 最大上下文 token
    val reasoning: Boolean,             // 是否推理模型（支持 reasoning_effort）
    val priceFactor: Double? = null,    // 官方倍率；0.0=免费；null=倍率未知（诚实显示 "?"，不许瞎填 1.0）
    val aliases: List<String> = emptyList(),     // 旧展示名/英文名别名（旧客户端仍按旧名请求）
    val unverified: Boolean = false,    // key 是推断未经官方验证；被官方目录覆盖后置 false
)
```

派生属性：
- `free: Boolean get() = priceFactor != null && priceFactor <= 0.0`（null 不算免费）。
- `priceLabel: String` — `priceFactor ?: "?"`；整数值去小数尾巴（`0.0→"0"`、`1.0→"1"`、`2.5→"2.5"`）。

Rust 建议：`Option<f64>` + `serde` 序列化时 `price_factor` 为 `null` 或**缺字段**两种都容忍；`free`/`price_label` 做成方法而非字段。

### 2.2 内置快照 BUILTIN

- **顺序即 UI 顺序**；`ALL` 是 `current()` 的兼容旧别名。
- 常量：`DEFAULT_KEY = "auto"`（兜底模型）、`ONE_M_KEY = "ultimate"`（1M 上下文兜底）、`ONE_M_SUFFIX = "[1m]"`。
- 持久化：PREF `traework2api_qoder_models`，key `catalog_json`（**上游目录原文**，非解析结果，便于改解析规则后重解）、`fetched_at`（unix 毫秒 long）。

⚠️ **移植前必须知道的矛盾**：`selfCheck()` 断言 `BUILTIN.size == 20` 且 `BUILTIN.first().key == DEFAULT_KEY`（首个应为 `auto`），但当前源码实际列表有 **21 条**且首条是 `qwen3-coder-plus`（顶部注释还写着"16 个"）。三处互相矛盾，说明清单增改后自检没跟上。以下 JSON 数组**逐条原样转写自源码第 74–96 行**，Rust 端以源码清单为准嵌入即可，自检断言请一并修正（size=21、首条 qwen3-coder-plus），或按产品意图把 `auto` 移回首位。

```json
[
  {"key":"qwen3-coder-plus","name":"Qwen3 Coder Plus","contextLength":180000,"reasoning":false,"priceFactor":null,"aliases":[],"unverified":false},
  {"key":"auto","name":"Auto","contextLength":180000,"reasoning":false,"priceFactor":0.5,"aliases":[],"unverified":false},
  {"key":"ultimate","name":"极致","contextLength":1000000,"reasoning":true,"priceFactor":0.8,"aliases":["Ultimate"],"unverified":false},
  {"key":"performance","name":"快速","contextLength":1000000,"reasoning":false,"priceFactor":null,"aliases":["Performance"],"unverified":false},
  {"key":"efficient","name":"经济","contextLength":180000,"reasoning":false,"priceFactor":null,"aliases":["Efficient"],"unverified":false},
  {"key":"smodel","name":"Sonus","contextLength":180000,"reasoning":true,"priceFactor":8.0,"aliases":["smodel"],"unverified":false},
  {"key":"cmodel","name":"Cantus","contextLength":200000,"reasoning":true,"priceFactor":4.0,"aliases":[],"unverified":false},
  {"key":"qmodel_38max","name":"Qwen3.8-Max 2","contextLength":1000000,"reasoning":true,"priceFactor":0.5,"aliases":["Qwen3.8-Max"],"unverified":false},
  {"key":"qfmodel","name":"Qwen3.8-Flash","contextLength":1000000,"reasoning":true,"priceFactor":0.0,"aliases":["Qwen3.8-Flash-1M"],"unverified":false},
  {"key":"kmodel_latest","name":"Kimi-K3.1-Pro","contextLength":1000000,"reasoning":false,"priceFactor":1.0,"aliases":["Kimi-K3"],"unverified":false},
  {"key":"kmodel","name":"Kimi-K2.8-Preview","contextLength":256000,"reasoning":true,"priceFactor":0.8,"aliases":["Kimi-K2.7-Code"],"unverified":false},
  {"key":"gmodel","name":"GLM-5.3","contextLength":1000000,"reasoning":true,"priceFactor":0.8,"aliases":[],"unverified":false},
  {"key":"gfmodel","name":"GLM-5.3-Flash","contextLength":1000000,"reasoning":true,"priceFactor":0.1,"aliases":[],"unverified":false},
  {"key":"dmodel","name":"DeepSeek-V4-Pro","contextLength":1000000,"reasoning":true,"priceFactor":0.5,"aliases":[],"unverified":false},
  {"key":"dfmodel","name":"DeepSeek-Flash","contextLength":1000000,"reasoning":true,"priceFactor":0.0,"aliases":["DeepSeek-V4-Flash"],"unverified":false},
  {"key":"mmodel","name":"MiniMax-M3.1-Flash","contextLength":1000000,"reasoning":false,"priceFactor":0.1,"aliases":["MiniMax-M3"],"unverified":false},
  {"key":"lite","name":"Lite","contextLength":180000,"reasoning":false,"priceFactor":null,"aliases":[],"unverified":false},
  {"key":"qmodel_latest","name":"Qwen3.7-Max","contextLength":1000000,"reasoning":true,"priceFactor":0.1,"aliases":[],"unverified":false},
  {"key":"qmodel","name":"Qwen3.7-Plus","contextLength":1000000,"reasoning":true,"priceFactor":0.04,"aliases":[],"unverified":false},
  {"key":"q37fmodel","name":"Qwen3.7-Flash","contextLength":1000000,"reasoning":true,"priceFactor":0.1,"aliases":[],"unverified":false},
  {"key":"gm51model","name":"GLM-5.2","contextLength":1000000,"reasoning":true,"priceFactor":0.6,"aliases":[],"unverified":false}
]
```

字段说明补充：
- 前 16 条 key 全部来自官方目录快照（catalog-v6 解密导出），不是靠命名规律猜的（源码注释 0.7.x）。
- 第 17–21 条（`lite`、`qmodel_latest`、`qmodel`、`q37fmodel`、`gm51model`）是官方 UI 已下架但 key 仍有效的老模型，专为兼容旧客户端保留。
- `lite` 未写 `priceFactor` 即默认 null（倍率未知）。
- 免费模型恰 2 个：`qfmodel`、`dfmodel`（`priceFactor == 0.0`）。
- `smodel` 的别名 `"smodel"` 与其 key 同名（历史遗留，无害；匹配时 key 精确匹配已先命中）。

### 2.3 动态目录状态与合并

```kotlin
@Volatile private var dynamic: List<QModel>? = null   // null = 用内置
@Volatile private var fetchedAt: Long = 0L
fun current(): List<QModel> = dynamic ?: BUILTIN
fun hasDynamic(): Boolean; fun dynamicFetchedAt(): Long
fun setDynamic(entries: List<QModel>?) {               // null/空 = 退回内置（绝不置空目录）
    dynamic = if (entries.isNullOrEmpty()) null else merge(entries)
}
```

`merge(entries)`（`:145`，合并语义，Rust 端逐条复刻）：
1. `entries` 空 → `BUILTIN`。
2. 按内置顺序逐条处理；同 key 有上游条目 `d` 时 `copy`：
   - **`name` 保留内置值**（官方 APP 显示名；上游是英文名+基础价，拿来覆盖会"和官方 APP 对不上"）；
   - `contextLength`：内置 ≤0 用上游；上游更大用上游；否则内置（纯增益规则）；
   - `reasoning = b.reasoning || d.reasoning`（上游缺省 false 不得冲掉内置 true）；
   - `priceFactor = b.priceFactor ?: d.priceFactor`（内置非 null 优先，内置未知才采上游）；
   - `aliases = (b.aliases + d.name).distinct().filter { 非空白且 != b.name }`（上游名留作别名）；
   - `unverified = false`。
3. 上游新 key 依上游出现顺序**追加到末尾**。
   结果：`auto` 永远在内置原位（首条除外——见 2.2 的矛盾说明），客户端默认值不漂。

持久化：
- `load(ctx)`：读 `catalog_json` → `parseCatalogJson`，解析空则保持内置；否则 `fetchedAt = getLong("fetched_at", 0)`，`setDynamic(parsed)`。冷启动调用（`MainActivity.kt:294`）。
- `save(ctx, rawCatalogJson, at=now)`：存**原文** + `fetched_at` 毫秒。
- `clear(ctx)`：remove 两 key，`dynamic=null`、`fetchedAt=0`。

### 2.4 目录 JSON 容错解析 `parseCatalogJson(raw)`

三种根形状（解析异常/不认 → 空列表，调用方必须"空 = 保留原清单"）：
1. `{"chat":[...], "byok_enterprise":[...], "byok_teams":[...]}`（CLI 缓存格式）；
2. `[...]` 扁平数组；
3. `{"data":[...]}` 或任意嵌套（OpenAI 风格/未知包装）。

流程：
- trim 后按首字符 `[`/`{` 判定，其余直接空列表。
- 对象根：**先按 `SCENES = ["chat","byok_enterprise","byok_teams"]` 固定顺序**消费已知场景数组（其余分组键忽略）；`seen` 按 key 去重；只要命中过任一场景且非空即返回。
- 否则通用递归扫描 `collect()`：JSONObject 若 `looksLikeModel` 则转条目，不然下钻所有值；JSONArray 逐元素下钻。
- `looksLikeModel(o)`：(`has("key") || has("model_key")`) **且** 带任一元字段：`price_factor` / `display_name` / `max_input_tokens` / `context_config` / `is_reasoning` / `format`。

单条转换 `toModel(o)` → QModel?：
- `key = o.key 若非空白否则 o.model_key`，trim；空 → null。
- `enable`：仅显式 `false` 或数字 `0` 排除；缺省视为启用。
- `name = display_name → name → key` 逐级回落。
- 上下文：`max_input_tokens`>0 → 否则 `maxContextFrom(context_config)`（遍历各档取 `token_count` 最大值，如 `{"default":{"token_count":N},"1m":{"token_count":M}}`）→ 否则默认 **180000**。
- `price_factor`：**字段不存在或为 JSON null → null（未知）**；存在则 `optDouble`，NaN/Infinite → null。缺省写成 1.0 会把免费模型洗成收费——移植时保持这个"缺字段=未知"语义。
- `reasoning = is_reasoning`（默认 false）。别名/unverified 输出用默认值（空、false）。

### 2.5 三级降级拉取链（实现在 `MainActivity.syncQoderCatalog`，`MainActivity.kt:1242`）

前提：`cred = QoderAuthStore.load(this)`；无 Qoder 账号 → 直接返回（非静默 toast「先添加账号」）。后台线程 `qoder-catalog-sync` 内：

0. token 准备：`jt = QoderAuthApi.validJobTokenFor(ctx, cred, 60min 提前量)`；拿不到则回落 `cred.jobToken` 非空值。jt 为空 → ①② 都不试，直落 ③。

**① COSY 签名动态目录**（`QoderUpstream.fetchModelListRaw`，`QoderUpstream.kt:421`；`raw` 非空即记录 `lastCatalogRaw/lastCatalogError`）：
- URL：`GET {inferBase(mode)}/algo/api/v2/model/list`，**空 body**。
  - `mode == "global"` → `https://api2.qoder.sh`；否则（`cn`）→ `https://gateway.qoder.com.cn`。
- 签名（与聊天同一套 COSY 方案，三处不同）：body 为空串参与签名；**签路径 `/api/v2/model/list`（不含 `/algo` 前缀）**；无需 `X-Model-Key`/`X-Model-Source`。
  - `Authorization: Bearer COSY.<payloadB64>.<md5hex>`，
    `md5hex = md5(payloadB64 + "\n" + key + "\n" + unixSeconds + "\n" + "" + "\n" + "/api/v2/model/list")`；
  - `payloadB64 = base64({"version":"v1","requestId":<新 requestId>,"info":<encrypt_user_info>,"cosyVersion":"1.1.74","ideVersion":""})`；
  - `requestId = newRequestId() = 16 随机字节→整体反序+UUIDv4 掩码(第7字节 0100xxxx、第9字节 10xxxxxx)→小写 8-4-4-4-12`；
  - 运行时字段 `generateRuntimeFields`（登录级缓存）：随机 uuid 前 8 字节的 hex（16 ASCII 字符）当 AES key/IV；明文 JSON `{uid, security_oauth_token(jobToken 非空才出现该键), organization_id, organization_tags, data_policy_agreed}` AES-CBC-PKCS7 加密 → `encrypt_user_info`（b64）；key 用固定 RSA 公钥 PKCS1v15 加密 → `Cosy-Key`（b64）。
- 请求头全集：`Accept: application/json`、`Authorization`、`Cache-Control: no-cache`、`Cosy-Business-Product: cli`、`Cosy-Business-Type: agent`、`Cosy-ClientType: 5`、`Cosy-Data-Policy: agree|disagree`、`Cosy-Date: <unix秒>`、`Cosy-Key: <key>`、`Cosy-MachineId`、`Cosy-MachineOS`（安卓按 ABI，如 `aarch64_android`；Tauri 桌面应按目标平台取 `goarch_goos` 风格：amd64→x86_64、arm64→aarch64、macos→macos）、`Cosy-Scene: assistant`、`Cosy-User: <uid>`、`Cosy-Version: 1.1.74`、`Login-Version: v2`；可选 `Cosy-Organization-Id`（非空才带）、`Cosy-Organization-Tags`（逗号连接，非空才带）。刻意不带 User-Agent。
- 成功判据：HTTP 2xx → 返回响应原文；非 2xx / 网络异常 / 生成签名材料异常 → null，并把原因写进 `QoderAuthApi.noteCatalogAttempt(raw, error)`（形如 `签名目录 HTTP <code>: <body前200字>`）。
- 拿到原文后 `parsed = parseCatalogJson(raw)`；**以"能解析出 ≥1 条模型"为真正成功判据**（HTTP 200 但回业务壳 `{"code":..}` 解析出 0 条照样算失败，继续 ②）。

**② openapi 回落**（`QoderAuthApi.fetchModelCatalog(jt, mode)`，`QoderAuthApi.kt:156`，仅当 ① 解析为空时尝试）：
- Base：`mode=="global"` → `https://openapi.qoder.sh`，否则 `https://openapi.qoder.com.cn`。
- 候选顺序（笛卡尔）：路径 `["/api/v1/models", "/api/v1/model/list"]` × 方法 `["GET", "POST"]`（POST 体 `{}`）。头：`Authorization: Bearer <jt>`、`Accept: application/json`、`User-Agent: traework2api`、`Cosy-Version: 1.0.1`、`Cosy-ClientType: 5`。
- 非 2xx 不抛、记一条失败摘要（`GET /api/v1/models → HTTP 401`）继续下一候选；2xx 原文首次到达即存 `lastCatalogRaw`（供 UI「上次拉取的原文」排查）；解析出 ≥1 条即返回该原文；全部失败 → `lastCatalogError = 各候选失败原因拼"；"`，返回 null。
- 调用侧接线（`MainActivity.kt:1259-1269`）：② 解析非空 → 用 ② 的 raw/parsed 替换；② 也空但 ① raw 为 null → 把 ② 的 raw 也记下来（只为原文可见性）。

**③ 内置快照兜底**：①② 都没有产出（`raw==null || parsed.isEmpty()`）→ **什么都不改**：`dynamic` 维持磁盘缓存或 null，回退内置 BUILTIN。失败**永不清空**清单。

成功收尾（`:1271`）：`QoderModelCatalog.setDynamic(parsed)`（合并语义）+ `save(raw)` 落盘；回主线程 `modelsRefreshApply`；非静默 toast「官方目录已同步：N 个模型（M 个免费）」或「拉取失败，继续用内置 N 个模型\n<失败摘要前120字>」。

**"本进程只同步一次"的实现**（`MainActivity.kt:213, 1843-1847`）：
- Activity 字段 `private var qoderCatalogAutoTried = false`；
- 渲染模型页时：`if (GatewayBackend.hasQoderAccount(ctx) && !qoderCatalogAutoTried) { qoderCatalogAutoTried = true; syncQoderCatalog(quiet = true) }`；
- 即：**静默自动同步每个进程/Activity 实例只触发一次**，失败后切页不再反复打上游；手动「刷新模型」按钮与 `auto` 混合模式刷新不受此闸限制。Rust 端建议用 `Once`/`AtomicBool` 做进程级守卫。

### 2.6 客户端模型名解析 `resolve(rawModel, maxInputTokens=0)` → Resolved

```kotlin
data class Resolved(val model: QModel, val oneMSuffix: Boolean) {
    val key get() = model.key                                    // 发给上游的 key
    val publicId get() = if (oneMSuffix) model.key + "[1m]" else model.key  // 对外展示 id
}
```

规则（对齐参考实现 `modelResolver.resolve`）：
1. trim；若以 `[1m]` 结尾（大小写不敏感）记下 `oneM=true` 并剥掉后缀再 trim。
2. `findByIdentifier`：`key.equals(ignoreCase)` → `name.equals(ignoreCase)` → 任一 `alias.equals(ignoreCase)`。
3. 未命中/空名 → 列表中 key=="auto" 条目；列表里连 auto 都没有 → 列表首项。
4. 升级：`oneM && maxInputTokens < 900_000 && model.contextLength < 900_000` → 改用 key=="ultimate" 条目（找不到 ultimate 则维持原模型）。
   `maxInputTokens` 入参来自客户端请求的上下文需求（0 = 未声明）。
5. `byKey(key)`：精确等值（区分大小写）查当前生效列表，不做别名解析。

### 2.7 `openAiModels()` 输出对象结构

Qoder 后端时由 `Gateway` 注入为 `LocalFakeServer.modelsProvider`，逐条 `current()`：

```json
{
  "id":            "<name，name 空白时回落 key>",   // 故意用官方显示名（客户端下拉框只认 id）
  "object":        "model",
  "created":       <本次调用时刻 unix 秒>,           // 整个数组共用同一个值
  "owned_by":      "qoder",
  "name":          "<m.name>",
  "key":           "<m.key>",                        // 内部 key 保留，UI 卡片读它
  "context_length":<m.contextLength>,
  "is_free":       <m.free>,                          // priceFactor<=0.0
  "price_factor":  <m.priceFactor>,                   // 仅当非 null 才写；缺字段=「倍率未知」
  "unverified":    true,                              // 仅当 m.unverified 才写
  "aliases":       ["<key>", ...别名]                  // 仅当非空才写；= ([key]+aliases).distinct() 去掉与 id 相同项
}
```

辅助：`freeCount() = current().count { it.free }`（总览页"免费 N / 总 M"）。

### 2.8 会话 id（进程内缓存，目录的附属状态）

- `sessions: HashMap<String,String>`（`@Synchronized` 方法）：`sessionFor(modelKey)` 每模型一个稳定 `session_id`，getOrPut 生成：
  `"q2a-" + formatLowerUUID(reverseMaskUUID(SecureRandom 16 字节))`，即长度 4+36、`q2a-` 前缀 + 小写 UUIDv4。
- 不持久化：进程重启换新 id 只丢上游历史，不影响功能。**注意与 §2.7 的 `created` 类似，Rust 端用 `Mutex<HashMap>` + rand。**
- `newRequestId()` 转发给 `QoderCosyCrypto.newRequestId()`（`request_id`/`chat_record_id` 每请求新生成）。

---

## 3. QoderModelCache（CLI 目录缓存导入器，object 单例）

文件：`QoderModelCache.kt`。**它自身不落 SharedPreferences**——"缓存"指 Qoder CLI 的服务端目录加密缓存文件；本模块负责解密/识别、解析并交给 `QoderModelCatalog.setDynamic`，实际落盘由调用方完成（见 3.4）。

### 3.1 数据来源

- CLI 文件路径：`<cli 配置根>/.models/<uid>/catalog-v7`（用户侧提示命令由 `cliHint(uid)` 生成：`cat ~/.qoder/.models/<uid>/catalog-v7`；兼容 v6）。
- 文件内容是一段 **base64 文本**（非二进制），解码后是信封字节。

### 3.2 信封格式（对齐参考实现 `native_model_cache.go`）

```
base64( "QMC\x01"(4B 魔数) || nonce[12] || AES-256-GCM(key, nonce, plaintext)||tag[16] )
key = HKDF-SHA256(ikm = utf8(uid), salt = "qoder-model-cache-enc", info = "model-cache-v1", L = 32)
```

- 常量：`MAGIC = [0x51,0x4D,0x43,0x01]`、`NONCE_SIZE=12`、`TAG_BITS=128`、`KEY_LEN=32`。
- HKDF 精简实现（只需 1 块输出）：`PRK = HMAC-SHA256(key=salt(空则 32 零字节), ikm)`；`OKM = HMAC-SHA256(key=PRK, info || 0x01)` 截取 length（length 必须 1..32）。与 Go `hkdf.Key(sha256.New, …)` 等价；自检钉住 RFC 5869 Test Case 1 前 32 字节 `3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf`。Rust 端直接用 `hkdf` crate 或复刻此两处 HMAC 即可。
- `decrypt(blob, uid)` 失败判定（全部返回 null，不抛）：base64 解码失败；字节数 < `4+12+16=32`；前 4 字节不等于 MAGIC；AES-GCM 认证失败（含 uid 不匹配）。

### 3.3 `importBlob(blob, uid)` → Result（导入决策树）

```kotlin
data class Result(ok: Boolean, models: Int = 0, error: String? = null,
                  decrypted: Boolean = false,   // false = 明文直通
                  plain: String? = null)         // 解密/直通后的明文目录 JSON（落盘必须存这个）
```

1. trim；空 → 失败 `"内容为空"`。
2. 明文直通：以 `{` 或 `[` 开头 → `parseCatalogJson`；0 条 → 失败 `"是 JSON，但里面没有能识别的模型条目"`；否则 `setDynamic(parsed)` 返回 `ok, decrypted=false, plain=text`。
3. 加密路径：`uid.isBlank()` → 失败 `"需要先登录 Qoder 账号（解密要用账号 uid）"`；`decrypt` 为 null → 失败 `"解密失败：不是有效的 catalog-v7，或 uid 与该账号不匹配"`；解密产物解析 0 条 → 失败 `"解密成功但目录里没有模型（格式变了？）"`；成功 → `setDynamic(parsed)`、`decrypted=true`、`plain=解密产物`。
- `looksEncrypted(text)`：非 `{`/`[` 开头且 base64 前 8 字符可解码出至少 4 字节且匹配 MAGIC → 供 UI 提示"你贴的是 catalog-v7"。

### 3.4 写回时机与"缓存 key / TTL"结论

- **无 TTL、无独立缓存层**：导入即生效（内存 `dynamic`），持久化时机由调用方决定——手动导入成功后立刻 `QoderModelCatalog.save(ctx, r.plain)`（`MainActivity.kt:1203`，存**明文目录 JSON**；存密文下次启动解析不出来等于白导）。自动同步链（§2.5）用 `save(raw)` 写上游原文。
- 落盘位置就是 §2.3 的 `traework2api_qoder_models` / `catalog_json` + `fetched_at`；Rust 端复用同一文件即可，导入路径不需要新 key。

---

## 4. UsageStore（用量记录，object 单例）

文件：`UsageStore.kt`。SharedPreferences + JSON 数组，进程内缓存 + 显式锁。

### 4.1 持久化

| 项 | 值 |
|---|---|
| prefs 名 | `traework2api_prefs`（注意：与 App 主 prefs 同名共用） |
| key | `usage_records` |
| value | JSONArray 字符串，每元素字段见下 |
| 上限 | `MAX = 100` 条，超出从**最旧**（下标 0）逐条 `removeAt(0)` 裁剪 |

记录 JSON ↔ 字段映射（`persist`/`fromJson`）：

| JSON 键 | Rust 类型 | 读取缺省 |
|---|---|---|
| `ts` | i64 毫秒 | `now()`（当前时刻） |
| `model` | String | `""` |
| `prompt` | u32 | 0 |
| `completion` | u32 | 0 |
| `total` | u32 | 0 |
| `ms` | i64（durationMs） | 0 |
| `ok` | bool | false |
| `err` | String（errorType） | `""` |

内部结构：`data class UsageRecord(ts, model, prompt, completion, total, durationMs, ok, errorType)`；`lock = Any()` 同步所有读写；`cached: MutableList` + `loaded` 标志，`ensureLoaded` 首次从盘懒加载（解析整体包 `runCatching`，坏数据当空），此后每次 `record` 全量重写 JSON 落盘（`.apply()`）。

### 4.2 写入来源（LocalFakeServer → Gateway 接线）

- `LocalFakeServer.streamWithFailover` 在**一次转发最终落定**（不再换号/续期）时调用 `usageRecorder(model, usageJsonOrNull, elapsedMs, err == null, err, backend, uid)`（`LocalFakeServer.kt:693`）。`usage` 是上游 SSE 帧里的 usage 对象（可为 null）。
- `Gateway.start` 注入回调（`Gateway.kt:148-153`）：`prompt=usage.prompt_tokens ?? 0`、`completion=usage.completion_tokens ?? 0`、`total=usage.total_tokens ?? 0`；`err ?: ""` 存 `errorType`（形如 `http:401`、`queued:<model>`、`exhausted` 等错误类别字符串——**只存 时间/模型/token 数/耗时/状态/错误类型，绝不存任何凭据**）。backend/uid 两个参数不进 UsageStore，转给 `CreditMonitor.noteForwarded`。
- 耗时 `ms` = 单次尝试的 `System.currentTimeMillis() - t0`。

### 4.3 聚合统计 `aggregate(ctx)` → List<ModelAgg>

```kotlin
data class ModelAgg(val model: String, val total: Long, val output: Long)
```

- 用 `LinkedHashMap`（**按记录列表中模型首次出现顺序**，非字典序）；
- `total += r.total`，`output += r.completion`；
- UI（MainActivity 用量页 `:1957`）按此渲染每模型汇总；明细列表直接用 `all()` 全量。
- 读取接口仅两个公开：`record(...)`、`all(...)`、`aggregate(...)`。无删除/清理接口（裁剪只靠 100 条 FIFO）。

Rust 建议：`Mutex<Vec<UsageRecord>>` + `OnceLock` 式懒加载，聚合用 `IndexMap`（保持首次出现序）。

---

## 5. AccountBackup（Trae+Qoder 账号打包备份，object 单例）

文件：`AccountBackup.kt`。把两套账号打进**同一个 JSON 文件**换设备一键登录。

### 5.1 备份文件外壳（导出格式，逐字段）

```json
{
  "format":  "traework2api-backup",
  "version": 1,
  "trae":    { TraeAuthStore.exportJson 的整个对象 },
  "qoder":   { QoderAuthStore.exportJson 的整个对象 }
}
```

`exportAll` 用 `toString(2)`（两空格缩进 pretty print）。两个子对象结构分别如下。

**`trae` 子对象**（`TraeAuthStore.exportJson`，`TraeAuthStore.kt:211`）：
```json
{
  "format": "traework2api-accounts",
  "version": 1,
  "activeUid": "<当前选中 uid，可空串>",
  "accounts": [
    {"uid":"…","nickname":"…","region":"<regionId>","expiresAt":<long ms>,
     "accessToken":"<明文!>","refreshToken":"<明文!>"}
  ]
}
```
- 每个 account 取自 `loadByUid` 的完整 Cred：`uid`、`nickname`、`region`(cred.regionId)、`expiresAt`、`accessToken`、`refreshToken`；导出**明文** token（AndroidKeyStore 绑设备，密文到新机解不开），导入方用本机密钥重新加密落盘（Rust/Tauri 端映射为平台密钥链或自建加密存储）。
- `list()` 里没有凭证的条目 `loadByUid` 返回 null 会被跳过。

**`qoder` 子对象**（`QoderAuthStore.exportJson`，`QoderAuthStore.kt:237`）：
```json
{
  "format": "traework2api-qoder-accounts",
  "version": 1,
  "activeUid": "<当前选中 uid，可空串>",
  "accounts": [
    {"uid":"…","name":"…","email":"…","mode":"cn|global","expiresAt":<long ms>,
     "organizationId":"…","organizationName":"…","organizationTags":["…"],
     "dataPolicyAgreed":<bool>,"isDataPolicyModifiable":<bool>,
     "pat":"<明文!>","jobToken":"<明文!>","jobRefreshToken":"<明文!>",
     "fingerprint":"<encodeFingerprint 的 JSON 字符串，或空串>"}
  ]
}
```
- `fingerprint` 为**字符串字段**（内容又是 JSON 编码），无指纹时空串 ""; 设备身份指纹要随备份走，否则换机后派生变化影响风控。

落盘位置（导入方，不属于备份文件）：Trae prefs `traework2api_prefs`（`KEY_INDEX="trae_accounts"`，`trae_acc_<uid>_access/_refresh`），Qoder prefs `qoder2api_prefs`（`KEY_INDEX="qoder_accounts"`，`q_acc_<uid>_pat/_jt/_jrt`），值经 KeyStore AES-GCM 加密。

### 5.2 导入策略 `importAll(ctx, json)` → `ImportSummary(trae: Int, qoder: Int)`

1. `JSONObject(json)` 失败 → 抛 `IllegalArgumentException("不是有效的账号备份 JSON")`。
2. **备份外壳优先**：根含 `trae` 或 `qoder` 任一对象 →
   - `trae` 节点存在 → `TraeAuthStore.importJson(node.toString())` 计数（内部异常吞掉记 0）；
   - `qoder` 同；
   - 两者计数之和 ≤0 → 抛 `"备份里没有任何可导入的账号（token/PAT 可能缺失或 JSON 损坏）"`；
   - 返回 `ImportSummary(t, q)`。
3. **单份导出兼容**：无外壳 → 按根 `format` 识别：大小写不敏感包含 `"qoder"` → 整包喂 `QoderAuthStore.importJson`；否则（含 format 缺失/null）→ 整包喂 `TraeAuthStore.importJson`。返回对应计数。

**合并语义（覆盖/追加）**——两个 store 的 `importJson` 一致：
- 标准外壳取 `accounts` 数组（无外壳兼容**顶层直接是数组**的老格式）；
- 逐条：Trae 要求 `uid` 非空 **且 `accessToken` 非空**，否则跳过；Qoder 要求 `uid` 非空且 `pat 或 jobToken 至少一个非空`，否则跳过；
- **按 uid 合并**：同 uid 覆盖（token + 元数据重写），新 uid 追加；**导入过程不动"当前选中账号"**（Trae 走 `importOne` 不经 `addWithoutSetActive`；Qoder 直接写 prefs+`writeMeta`）；Qoder 指纹字段缺省即"用本机派生"（老备份没有该字段不报错）；
- 一条都没导成 → 抛异常（消息见各自 `check(imported>0)`）；
- 仅当文件带非空 `activeUid` 且该 uid 在导入后索引中存在 → `setActive(activeUid)`（换设备保持同一个号）。

`selfCheck()`：格式识别三断言（含 qoder 走 Qoder；Trae format 不误判；null format 判 Trae）。Rust 端可保留为单元测试。

安全红线（源码文档强调）：备份文件等价账号钥匙（明文 token/PAT），只走本地文件/剪贴板，绝不联网、绝不写日志。

---

## 6. LocalFakeServer 相关片段（模型表出口与健康检查）

- `PORT = 8787`（8788 太常见、84772 超端口上限、4772 特权端口）。Tauri 端可沿用。
- **路径归一化**（`:473`）：截断 `?` 查询串后，`/{2,}` 合并为单斜杠 + 去尾斜杠（根路径除外）；日志只记归一化路径不记 query（防 `?key=` 泄凭据进日志）。
- 路由（`:482-497`）：
  - `OPTIONS *` → `204`（CORS 预检最先答、任何路径都答，带 `Access-Control-Allow-*` 头）。
  - `GET /healthz`（及归一化别名）→ `200 text/plain "ok"`。
  - `GET|OPTIONS /v1/models` 与 **别名 `/models`**（Chatbox 不带 `/v1`）→ **免鉴权**，响应体：
    ```json
    {"object": "list", "data": <模型数组>}
    ```
    数据源 `models()`（`:596`）= `modelsProvider?.invoke()`（Qoder 后端由 Gateway 注入 `QoderModelCatalog.openAiModels`）异常/为 null 时回落 `staticModels()`（`:111`）= `ModelsRepo.current()`。空数组照常返回（设计意图，客户端看到空清单）。
  - `POST /v1/chat/completions` / `/chat/completions` → 需 `Authorization: Bearer <本地 apiKey>`，鉴权失败 401；按 model 名经 `backendForModelProvider` 路由 `trae`/`qoder`，回落 `backendProvider`，再兜底 `trae`。
- 流式响应头手写并补 CORS（`:626-634`）：`Content-Type: text/event-stream`、`Cache-Control: no-cache`、`Access-Control-Allow-Origin: *`、`Access-Control-Allow-Headers: *`、`Connection: close`。
- 用量回调接线：`usageRecorder` 7 参数见 §4.2；`streamWithFailover` 骨架负责续期/换号判定后统一记录（`retryableUpstreamError`/`authRelatedError` 分类错误串，如 `http:401`、`queued:dfmodel`）。
- `aggregateChunks(chunks, model)`（`:309-376`，非流式聚合）输出 `{id|"chatcmpl-local",object:"chat.completion",created,model:<客户端请求原 model 字符串>,choices:[{index:0,message:{role:"assistant",content,reasoning_content?,tool_calls?},finish_reason}],usage?}`——`model` 字段回填的是**客户端请求里写的名字**（含 `[1m]` 时保持原样），与 Qoder `Resolved.publicId` 语义一致。

---

## 7. 全部常量表（跨文件汇总）

### ModelsRepo.kt
| 常量 | 值 |
|---|---|
| PREFS | `"traework2api_models"` |
| KEY_MODELS | `"models_json"` |

### QoderModelCatalog.kt
| 常量 | 值 | 用途 |
|---|---|---|
| DEFAULT_KEY | `"auto"` | 兜底/默认模型 key |
| ONE_M_KEY | `"ultimate"` | 1M 上下文升级目标 |
| ONE_M_SUFFIX | `"[1m]"` | 客户端显式 1M 后缀 |
| PREF | `"traework2api_qoder_models"` | prefs 名 |
| KEY_CATALOG | `"catalog_json"` | 上游目录原文 |
| KEY_FETCHED_AT | `"fetched_at"` | 拉取时刻（unix ms） |
| SCENES | `["chat","byok_enterprise","byok_teams"]` | 分组解析消费顺序 |
| (隐含) 上下文缺省 | `180_000` | toModel 兜底 |
| (隐含) 1M 升级阈值 | `900_000` | resolve |
| (隐含) session 前缀 | `"q2a-"`（总长 40） | sessionFor |
| (隐含) created | `System.currentTimeMillis()/1000` | openAiModels |

### QoderModelCache.kt
| 常量 | 值 |
|---|---|
| MAGIC | `51 4D 43 01`（`QMC\x01`） |
| NONCE_SIZE | 12 |
| TAG_BITS | 128 |
| KEY_LEN | 32 |
| SALT | `"qoder-model-cache-enc"` |
| INFO | `"model-cache-v1"` |
| RFC5869 校验向量 | `3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf` |

### UsageStore.kt
| 常量 | 值 |
|---|---|
| PREFS | `"traework2api_prefs"` |
| KEY | `"usage_records"` |
| MAX | 100 |

### AccountBackup.kt / 两个 AuthStore
| 常量 | 值 |
|---|---|
| 备份外壳 format | `"traework2api-backup"`，version 1 |
| TraeAuthStore.EXPORT_FORMAT | `"traework2api-accounts"` |
| TraeAuthStore.PREF_NAME | `"traework2api_prefs"`；KEY_INDEX=`"trae_accounts"`；token key `trae_acc_<uid>_access/_refresh` |
| QoderAuthStore.EXPORT_FORMAT | `"traework2api-qoder-accounts"` |
| QoderAuthStore.PREF_NAME | `"qoder2api_prefs"`；KEY_INDEX=`"qoder_accounts"`；`q_acc_<uid>_pat/_jt/_jrt` |

### QoderAuthApi.kt（目录 ② 相关）
| 常量 | 值 |
|---|---|
| MODE_CN / MODE_GLOBAL | `"cn"` / `"global"` |
| openapi base | `https://openapi.qoder.com.cn` / `https://openapi.qoder.sh` |
| infer base（① 用） | `https://gateway.qoder.com.cn` / `https://api2.qoder.sh` |
| EP_MODELS / EP_MODEL_LIST | `"/api/v1/models"` / `"/api/v1/model/list"` |
| EP_EXCHANGE / EP_REFRESH | `"/api/v1/jobToken/exchange"` / `"/api/v1/jobToken/refresh"` |

### QoderCosyCrypto.kt（① 签名相关）
| 常量 | 值 |
|---|---|
| NATIVE_MODEL_LIST_PATH | `"/algo/api/v2/model/list"` |
| MODEL_LIST_SIGNED_PATH | `"/api/v2/model/list"`（签名不含 `/algo`） |
| NATIVE_INFER_PATH / QUERY | `"/algo/api/v2/service/pro/sse/agent_chat_generation"` / `"?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"` |
| COSY_VERSION | `"1.1.74"` |
| SCENE_CLIENT_TYPE / BUSINESS_PRODUCT / BUSINESS_TYPE / SCENE | `"5"` / `"cli"` / `"agent"` / `"assistant"` |

### TraeUpstream.kt / TraeRegion.kt（§1.5 上游表相关）
| 常量 | 值 |
|---|---|
| EP_MODELS | `"/api/ide/v1/get_detail_param"` |
| FUNCTION | `"solo_work_remote"` |
| agentHost CN / SG | `https://trae-api-cn.mchost.guru` / `https://core-normal.trae.ai` |
| 上下文缺省 / created | `131072` / `1753600000` |
| isChatModel 词形黑名单 | `custom_model`(前缀), `agent`,`subagent`,`summary`,`sagitta`,`aquila`,`file_search`,`explore_`,`computer_use`,`browser_use` |

### LocalFakeServer.kt / GatewayBackend.kt
| 常量 | 值 |
|---|---|
| PORT | 8787 |
| MAX_BODY_BYTES | 8 MiB（超限 413） |
| 后端标识 | `"trae"` / `"qoder"` / `"auto"` |
| openAiModels owned_by | `"qoder"`；Trae 表条目 owned_by `"trae-solo"` |

### MainActivity.kt（流程闸）
| 字段 | 值/语义 |
|---|---|
| `qoderCatalogAutoTried` | 模型页自动同步 Qoder 目录一次性闸（§2.5） |
| `autoModelsTried` | 冷启动静默补新 Trae 表一次性闸 |
| `modelsLoading` | Trae 刷新并发闸 |

---

## 8. Rust 移植要点速记

1. **两套模型体系互不通用**：Trae=纯上游表（空是合法态），Qoder=内置权威快照+上游合并（永不为空）。`current()` 语义、空表闸门（Trae `applyFetched`、Qoder `setDynamic`）必须 1:1 保留。
2. Qoder 名称解析是大小写不敏感三段匹配（key→name→alias），并把 `[1m]` 后缀与 900k 升级规则做完整；`resolve` 的返回同时携带内部 key 和对外 publicId。
3. `parseCatalogJson` 的容错优先级（场景分组 → 递归扫描）与"解析 0 条 = 失败，绝不清空"是全链路的公共底座；成功判据统一为"≥1 条"，HTTP 状态码只作次要线索。
4. 合并语义里**展示名与倍率以内置为准**（官方 APP 显示值优先于上游英文名/基础价），上下文取 max、推理位取 or、上游名进别名、新 key 追加——顺序即 UI 顺序。
5. catalog-v7 导入的密码学参数（MAGIC/HKDF salt+info/nonce 12B/GCM tag 128bit）逐字节对齐，rust 侧 `aes-gcm`+`hmac`/`sha2` 手写 1-block HKDF 即可过自检向量。
6. UsageStore 纯 FIFO 100 条、全量重写落盘；聚合保持首次出现顺序、只累加 `total` 与 `completion`。
7. AccountBackup 文件本身明文含 token（等价账号钥匙）；Rust 端落盘加密应使用平台安全存储，导入策略为按 uid 覆盖合并、非空才动 activeUid。
8. `selfCheck` 断言与内置清单存在历史脱节（§2.2 红字），移植时先定基线（建议：保留源码 21 条现状 + 修断言），避免把 Android 端的陈旧检查照搬成 bug。
