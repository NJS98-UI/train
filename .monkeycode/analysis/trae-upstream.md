# TraeUpstream / TraeIntlUpstream / TraeRegion 移植分析

来源文件（已逐行通读，共 1700 行）：

- 当前工作区 `/workspace/app/src/main/java/com/example/traework2api/TraeRegion.kt`（179 行）
- 当前工作区 `/workspace/app/src/main/java/com/example/traework2api/TraeUpstream.kt`（941 行）
- 当前工作区 `/workspace/app/src/main/java/com/example/traework2api/TraeIntlUpstream.kt`（580 行）

辅助参考（用于第 3/4/6 节的调用侧语义，Kotlin 侧实现位于 `LocalFakeServer.kt`、`TraeAuthStore.kt`）：
非流式聚合、换号/重试判定、区域同步入口都在网关层，移植时必须一并对齐，否则三个文件的契约不完整。

约定：本文所有 "exact" 值均为源码字面量，移植时按字符串常量原样落地。日志里绝不出现 token。

---

## 1. TraeRegion —— 区域常量与切换语义

### 1.1 枚举构造参数（7 个字段，全部只读）

| 字段 | 含义 |
|---|---|
| `id` | 内部标识，用于日志与持久化（`"cn"` / `"sg"`） |
| `agentHost` | 聊天 / 模型接口 host（SOLO Agent） |
| `ugHost` | 额度 / 签到接口 host（User Gateway） |
| `userRegionHeader` | 额度/签到请求头的区域标识；空串表示不发该头 |
| `oauthHost` | OAuth（ExchangeToken / GetUserInfo）host |
| `consoleHost` | 登录页 host（构造登录 URL） |
| `clientId` | OAuth ClientID |

### 1.2 两区域全部值（exact）

| 字段 | CN | SG |
|---|---|---|
| `id` | `cn` | `sg` |
| `agentHost` | `https://trae-api-cn.mchost.guru` | `https://core-normal.trae.ai` |
| `ugHost` | `https://api.trae.cn` | `https://ug-normal.trae.ai` |
| `userRegionHeader` | `CN` | `` （空串：国际服未观测到该头，不发明） |
| `oauthHost` | `https://api.trae.com.cn` | `https://ug-normal.trae.ai` |
| `consoleHost` | `https://www.trae.cn` | `https://www.trae.ai` |
| `clientId` | `en1oxy7wnw8j9n`（SOLO stable，实测） | `ono9krqynydwx5` |

关键事实：CN 与 SG 是**两套完全不同的聊天协议**。SG 的 host 上**不存在** `/api/agent/v3/llm_utils_chat`；
把 CN 协议打到 SG host、或把历史上误填的 `a0ai-api-sg.byteintlapi.com` 当入口，都会得到 `4001 param is invalid`。

### 1.3 能力位与说明文案

| 成员 | 定义（exact 逻辑） |
|---|---|
| `canLogin` | `oauthHost.isNotBlank() && consoleHost.isNotBlank() && clientId.isNotBlank()` |
| `canQueryQuota` | `ugHost.isNotBlank()` |
| `isSupported` | `this == CN`（SG 于 2026-09-23 冻结为"未开发"） |
| `unsupportedNote` | CN → `""`；SG → `未开发：登录可通，但聊天不支持多轮上下文与外部工具调用；且免费额度极少（约 $1/月）。建议使用国服。` |

曾有 `canChat` 能力位，因两区域都已实现（协议由 `TraeUpstream.chatStream` 分派）而被删除。
将来若某区域聊天不可用，直接在 `chatStream` 分派处返回一句人话错误即可。

### 1.4 伴生对象常量与全局状态

| 成员 | 值 / 语义 |
|---|---|
| `IDE_VERSION` | `"0.1.43"`；与区域无关（国内实测量）。`User-Agent: Trae/<v>`、`X-Ide-Version` 都用它 |
| `current` | `@Volatile var current: TraeRegion = CN`；当前生效区域，进程级全局 |
| `PREF_NAME` | `"traework2api_prefs"` |
| `KEY_REGION` | `"trae_region"` |

区域切换语义（对应 SharedPreferences，Rust 侧换成任意 KV 持久化）：

| 函数 | 行为 |
|---|---|
| `load(context)` | 启动时调一次：读 `KEY_REGION` → `current = fromId(id)`。缺持久化会让重启回落 CN，导致 SG token 打到 CN host（实测表现为 401/1001 或 4001） |
| `set(context, region)` | `current = region` 并持久化 `region.id` |
| `fromId(id)` | `entries.firstOrNull { it.id == id?.trim()?.lowercase() } ?: CN`；未知/空白一律回退 CN |
| `requireLogin(region = current)` | `require(region.canLogin)`，失败文案：`区域 {id} 的登录协议尚未接入（缺 OAuth host / ClientID），目前仅支持 cn`；返回该 region |
| `selfCheck()` | 钉住不变式：CN 完整可登录且受支持；SG 标为未开发且 unsupportedNote 非空；CN.agentHost 以 `https://` 开头且 `userRegionHeader == "CN"`；SG 三要素齐全且 agentHost 是 https；CN/SG 的 agentHost / oauthHost / consoleHost / clientId 两两不等；CN.consoleHost 含 `trae.cn`、SG.consoleHost 含 `trae.ai`；`fromId` 大小写无关命中且未知/空回退 CN |

上层区域真相同步（`TraeAuthStore.syncRegionToActiveAccount(context)`，移植时属"区域管理"模块）：

```
uid = activeUid(ctx) ?: return          // 无账号时不动，保留用户手动选的区域（登录时还要用）
meta = 账号索引中 uid 对应项 ?: return
TraeRegion.current = meta.region        // 账号自带 region 才是权威
prefs.putString("trae_region", meta.regionId)
```

调用时机：App 启动、切换账号、以及额度监控自动换号（`CreditMonitor`）之后。

### 1.5 区域快照铁律（移植时最容易踩的并发坑）

`TraeUpstream` / `TraeIntlUpstream` 的方法跑在连接线程上，单次请求窗口可达 180s；
`TraeRegion.current` 会被 UI 线程改写（切账号 → `syncRegionToActiveAccount`、复制登录链接 → `TraeRegion.set`）。

规则：**每个对外方法入口只读一次 `current` 存进局部变量 `reg`，之后 host、header、协议分支全部用 `reg`**。
反例后果：判定分支与取 host 是两次独立读取，中间一次翻面会把 CN 协议请求的 CN token 发往 SG host（账号区域串号）。

`TraeUpstream` 里的私有 `region` / `AGENT_HOST` / `UG_HOST` getter 是历史遗留（实际调用点都用快照 `reg`），移植时可以不保留。

---

## 2. TraeUpstream（国服 CN）—— 聊天转发完整协议

### 2.1 端点与常量（全部 exact）

| 常量 | 值 |
|---|---|
| `EP_CHAT` | `/api/agent/v3/llm_utils_chat` |
| `EP_MODELS` | `/api/ide/v1/get_detail_param` |
| `EP_CHECKIN_STATUS` | `/trae/api/v2/ug/checkin_credits/status` |
| `EP_CLAIM` | `/trae/api/v2/ug/checkin_credits/claim` |
| `EP_ENT_USAGE` | `/trae/api/v2/pay/ide_user_ent_usage` |
| `APP_ID` | `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` |
| `IDE_VERSION` | `0.1.43`（= `TraeRegion.IDE_VERSION`） |
| `IDE_VERSION_CODE` | `20260716` |
| `DEVICE_BRAND` | `83DG` |
| `OS_VERSION` | `Windows 11 Pro` |
| `FUNCTION` | `solo_work_remote` |
| `DEFAULT_MODE` | `DeepSeek-V4-Flash` |
| `MODEL_ID_RE` | `^[A-Za-z0-9._-]{1,64}$` |
| `TAG` | `TraeUpstream`（日志只打模型名，绝不打 token） |
| connect timeout | 20 s |
| read timeout | 180 s |

`FUNCTION` 必须为 `solo_work_remote` 的原因（2026-09-25 实测）：新模型（`deepseek-v4.1-flash` / `glm-5.3` /
`step-5-preview`）只挂在 `solo_work_remote` 下，用老的 `solo_work_lite` 打它们一律
`error code=4001 param is invalid`；旧 12 个模型在 `solo_work_remote` 下全部正常且工具调用无回归。

`DEFAULT_MODE` 选 DeepSeek-V4-Flash 是纯成本考虑（默认值被所有未指定模型的客户端共用，贵模型一天能烧掉大量积分）。

### 2.2 聊天请求

```
POST {reg.agentHost}/api/agent/v3/llm_utils_chat
Content-Type: application/json
body = prepareBody(openai_body).toString()   // UTF-8 JSON
```

无 URL query 参数（聊天路径本身不带任何 query）。

### 2.3 SOLO 请求头（chat 与 models 共用；Builder.solo()）

按源码写入顺序，全部字段 exact：

| # | Header | 值 |
|---|---|---|
| 1 | `Accept` | `text/event-stream`（stream=true）/ `application/json`（stream=false） |
| 2 | `Content-Type` | `application/json` |
| 3 | `User-Agent` | `Trae/0.1.43`（`Trae/$IDE_VERSION`） |
| 4 | `Authorization` | `Cloud-IDE-JWT {token}` |
| 5 | `X-Cloudide-Token` | `{token}` |
| 6 | `X-Ide-Token` | `{token}` |
| 7 | `X-Uid` | `{uid}`，**仅当 uid 非空白才发**；缺这个头时上游会接受连接却不返回任何数据（聊天挂死） |
| 8 | `X-App-Id` | `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` |
| 9 | `X-App-Version` | `default` |
| 10 | `X-Ide-Version` | `0.1.43` |
| 11 | `X-Ide-Version-Code` | `20260716` |
| 12 | `X-App-Version-Code` | `20260716` |
| 13 | `X-Ide-Version-Type` | `stable` |
| 14 | `X-Device-Type` | `windows` |
| 15 | `X-OS-Version` | `Windows 11 Pro` |
| 16 | `X-Device-Brand` | `83DG` |
| 17 | `Request-Traffic-Type` | `prod` |

鉴权头的 token 就是账号的 `access_token`（`TraeAuthStore.Cred.accessToken`），同时作为 Bearer 风格前缀头
`Cloud-IDE-JWT ` 与两个裸值头 `X-Cloudide-Token` / `X-Ide-Token` 出现。刷新 token（refresh token）不出现在这里，
它只用于 `TraeAuthApi.forceRefreshTokenFor` 换取新的 access_token。

无签名/无摘要类自定义头（没有 hash、没有 HMAC、没有 trace id）。
`Builder.solo(...)` 的参数列表里有 `region`，但**函数体完全没用它**：SOLO 头集合与区域无关，区域只决定 host；
`X-User-Region` 头只在 UG 接口出现（见 2.9）。移植时保持这个事实，别给聊天请求发明区域头。

### 2.4 prepareBody：OpenAI 请求体 → Trae 请求体（原地改写，返回同一对象）

输入是客户端 `/v1/chat/completions` 的 JSON body。逐条规则：

1. `stream` ← `true`（无条件覆盖；上游只认流式）。调用方必须在改写**之前**读原始 `stream` 值
   （Kotlin 侧 `wantsStream` 就是这个用途）。
2. `function` ← `solo_work_remote`。
3. `messages` 数组（若存在且为数组，否则 `getJSONArray` 会抛 JSONException —— 上层必须捕获并回 400）：
   逐个元素处理，非对象的条目 `continue`（跳过）：
   - **assistant 且带 `tool_calls`**：对每个 tool_call 元素
     1. `function` → 重命名为 `function_call`（`put("function_call", fn)` + `remove("function")`）；
     2. 若 `function_call.name` 去空白后为空串 → **丢弃该条**；
     3. 保留条目放入新数组。
     全部丢完 → `remove("tool_calls")`；否则用新数组覆盖 `tool_calls`。
   - **`content` 是字符串且非 null** → 改写为多段数组
     `content = [{"type":"text","text": <原字符串>}]`。
     `content` 已是数组（多模态）或其它类型则原样保留。
4. 模型名（信任边界）：
   - `model = raw.optString("model","").trim()`；空 → `DEFAULT_MODE`。
   - 不匹配 `^[A-Za-z0-9._-]{1,64}$` → 记 warn（`模型名非法，回落默认: {取前40字}`）并回落 `DEFAULT_MODE`。
     这层校验挡住的是 `"model":"a\",\"x\":\"…"` 这类出站 JSON 结构篡改与控制字符污染日志。
   - 合法名同时写入两个字段：`config_name = model`、`model = model`（同值双写，Trae 认 `config_name`）。
5. `normalizeToolChoice(raw)`，见 2.5。
6. `normalizeTools(raw)`，见 2.6。
7. 其余字段（`temperature`、`max_tokens`、`stop`、`user`、任意自定义字段等）**原样透传**，源码对它们零处理。

字段级汇总：新增 `stream`、`function`、`config_name`、`model`（覆盖）；重命名 `messages[].tool_calls[].function`
→ `function_call`；结构转换 `messages[].content`(string) → array；`tools[].function.parameters`(object) → string；
`tool_choice` 归一为 string 或删除。无任何字段被"重命名成别的名字"以外的情形。

### 2.5 normalizeToolChoice（`tool_choice` 归一）

先取副作用闭包 `suppress = 删除 tools 且删除 functions`。

无 `tool_choice` → 直接 return（什么都不做）。按值的运行时类型分派：

| 输入 | 结果 |
|---|---|
| String `"none"`（忽略大小写、trim） | 删 `tool_choice` + `suppress()` |
| String 其它值（如 `"auto"`、`"required"`） | 原样保留（不动） |
| JSONObject，`type`（trim+lowercase）== `none` | 删 `tool_choice` + `suppress()` |
| JSONObject，`type` == `auto` / `required` | `tool_choice` ← 该 type 的小写 trim 字符串 |
| JSONObject，`type` == `function` | 名字取 `tc.function.name`，为空回退 `tc.name`，再 trim；非空 → `tool_choice = name`，空 → `tool_choice = "auto"` |
| JSONObject，其它/缺失 `type` | 删 `tool_choice`（tools 保留） |
| 其它 JSON 类型（数组、数字、布尔、null） | 删 `tool_choice` |

### 2.6 normalizeTools

无 `tools` → return。`tools` 非数组（`optJSONArray` 得 null）→ return（原样留着）。空数组 → 删 `tools`。
否则逐条：

- 条目非对象 → 丢弃。
- 取 `function` 子对象；缺失 → 丢弃该条。
- `function.parameters` 若**已是 JSONObject** → `fn.parameters = params.toString()`（序列化成 JSON 字符串）；
  已经是字符串或其它类型 → 不动。
- 保留整条 `{"type":"function","function":{...}}` 结构放入新数组。
- 新数组为空 → 删 `tools`；否则覆盖。

### 2.7 chatStream 控制流

```
fun chatStream(prepared, traeToken, uid = "", out: (String) -> Unit,
               onResult: (usage: JSONObject?, err: String?) -> Unit)
```

回调契约：

- `out(s)`：每一块要写给客户端的**完整** SSE 片段（已含 `data: ` 前缀与结尾的 `\n\n`）。
- `onResult(usage, err)`：整轮结束时恰好调用一次（`settled` 布尔保证幂等）。`err == null` 表示成功；
  `usage` 是上游 `token_usage` 事件的最后一条原文（可为 null）。
  上游 HTTP 出错时**也**通过 `out` 写出 `event: error` 帧 + `data: [DONE]`，再回调 `onResult`。

流程：

1. `reg = TraeRegion.current`（一次性快照）。
2. `if (reg != CN) { TraeIntlUpstream.chatStream(...同一批参数...); return }` —— 区域分派点。
3. CN：构造并执行请求（见 2.2/2.3）。
4. `!resp.isSuccessful`：
   - `err = resp.body?.string().orEmpty().take(200)`
   - `out("event: error\ndata: " + jsonEscape("upstream http {code}: {err}") + "\n\n")`
     （`jsonEscape` = `JSONObject.quote`，即整个文案是一个 JSON 字符串字面量）
   - `out("data: [DONE]\n\n")`
   - `onResult(null, "http:{code}")` （错误码格式见第 6 节）
   - return
5. 2xx：`BufferedReader(InputStreamReader(body.byteStream, UTF_8))` → `streamSse(br, out, onResult)`。
6. **异常处理事实**：`chatStream` 自身对网络执行**没有 try/catch**。连接失败 / readTimeout(180s) 触发的
   IOException 会向上抛到调用方（Kotlin 网关在 `streamingChat` 外层捕获，且只有在"还没写过任何字节"时
   才补 502 JSON —— 流式响应头早已发出时只能让连接自然断开）。Rust 侧两种做法都可，但必须保证
   "写流之前抛错 = 结构化错误响应；写流之后抛错 = 只断流并有日志"。

### 2.8 流式解析：Trae SSE → OpenAI chat.completion.chunk

#### 2.8.1 SSE 行状态机（SseState.scan）

逐行喂入（`BufferedReader.readLine()`，读到 null 即流末）：

| 行形状 | 处理 |
|---|---|
| 空行 | 事件闭合：若 `event` 为空 → `reset()` 并返回 null（丢弃孤立空行）；否则 `parse(event, data)` 并 `reset()`，返回结果 |
| `event:` 前缀 | `event = line.removePrefix("event:").trim()` |
| `data:` 前缀 | `data.append(line.removePrefix("data:"))`（**多条 data 行直接拼接，无分隔符**；前导空格留在缓冲里，JSON 解析可容忍） |
| `:` 开头 | 注释行，忽略 |
| 其它（如 `id:`） | 忽略 |

`parse(eventName, dataLine)`：

- `r.event = eventName.trim()`；`dataLine` 为空 → 直接返回只有 event 名的结果。
- `JSONObject(dataLine)` 解析失败 → 返回同样的"空结果"（该事件被静默丢弃）。
- 按 event 名取字段（见 2.8.2）。

#### 2.8.2 Trae 分片格式（逐字段）

| event | data JSON 字段 | 取值方式与注意 |
|---|---|---|
| `output` | `response` (string) | `optString("response","")` |
| | `reasoning_content` (string 或 null) | **必须 `(raw.opt(...) as? String).orEmpty()`**。用 `optString` 会把 JSON null 变成字面量 `"null"` 发给客户端（实测踩过：每帧挂着 `"reasoning_content":"null"`）；等价 Go reference 的 `raw["reasoning_content"].(string)` 类型断言，非字符串一律落空 |
| | `tool_calls` (array 或 object 或 null) | 仅当 `has && !isNull` 才取，保留原始类型交给 `stripSoloTools` |
| `token_usage` | 整个对象 | `usage` = 该 data 对象本身（不再钻子字段）。原样透传，因此 `prompt_tokens` / `completion_tokens` / `total_tokens` 等形状完全由上游决定 |
| `done` | `finish_reason` (string) | `optString("finish_reason","")` |
| `error` | `code` (number), `message` (string) | `optLong("code",0)`、`optString("message","")` |

#### 2.8.3 映射回 OpenAI chunk

`id = "chatcmpl-" + System.currentTimeMillis()`（十进制毫秒，流开始时生成一次，整轮复用）。

`writeChunk(delta, finish?)` 输出 `data: {chunk}\n\n`，chunk 结构：

| 字段 | 值 |
|---|---|
| `id` | 上面那个 `chatcmpl-{millis}` |
| `object` | `"chat.completion.chunk"` |
| `created` | `System.currentTimeMillis() / 1000`（**每帧重新取**；Rust 用 `UNIX_EPOCH.elapsed().as_secs()`） |
| `model` | `""`（恒定空字符串，上游回什么都不管） |
| `choices` | `[{ "index": 0 }]`；`finish != null` 时才写 `finish_reason`；`delta` 字段在 choice 组装后才放入（`choices[0].delta`） |
| `usage` | **仅在 `pendingUsage` 非空时写入本帧并清空**（见下） |

`delta` 内容规则（event=output）：

- `response` 非空 → `delta.content = response`
- `reasoning` 非空 → `delta.reasoning_content = reasoning`
- `tool_calls` 存在且 `stripSoloTools` 返回非 null → `delta.tool_calls = <数组>`，并置 `sawToolCall = true`
- `delta.length() > 0` 才写帧（全空的 output 帧被丢弃）

`pendingUsage` 语义（必须照搬的时序细节）：event=`token_usage` 时 `pendingUsage = usage` 且 `lastUsage = usage`；
真正下发要等**下一个 writeChunk**——即 `usage` 挂在其后的第一帧（正常时序就是 `done` 触发的收尾帧）。
Rust 侧若把 usage 提前写入别的帧会改变客户端看到 usage 的位置（多数客户端只看最后一帧，但 OpenAI 语义要求 usage 在末帧）。

终止与 finish_reason 归一：

- `done`：`fin = if (sawToolCall) "tool_calls" else v.finishReason`（上游带工具调用时也发 `finish_reason:"stop"`，
  而 OpenAI 规范要求此处为 `tool_calls`；Claude Code / Cursor 靠它决定"该执行工具了"，收到 `stop` 会直接结束对话、
  工具永不执行）。然后 `writeChunk({}, fin)` → `writeDONE()` → `sawDone = true` → `settle(lastUsage, null)`。
  注意这一帧 `delta` 是空对象 `{}`。
- `error`：`out("event: error\ndata: " + quote("solo error code={code} msg={message}") + "\n\n")` →
  `data: [DONE]` → `sawDone = true` → `settle(null, "solo:{code}")`。
- `[DONE]`：由网关自己写（`data: [DONE]\n\n`），上游从不用 OpenAI 风格的 `data: [DONE]` 终止；
  Trae 的终止信号是 `event: done`。
- 流末无 `done` / `error`（连接被切断、上游提前闭合）：`if (!sawDone) { writeDONE(); settle(null, "interrupted") }`
  —— 幂等兜底，保证客户端一定收到 [DONE]，并把 `interrupted` 作为错误码交给上层。
- 超时：readTimeout 180 s 到点会抛 IOException 走出 2.7 第 6 条的异常路径（`streamSse` 没有内部捕获）；
  因此 `interrupted` 覆盖的是"干净 EOF"，`exception` 覆盖的是"socket 级超时/重置"。Rust 里应显式合成一个
  `interrupted` 语义以保证换号判定表可复用（第 6 节 `retryableUpstreamError` 对非数字尾串一律判"可换号"，
  两种都算"值得换号"）。

#### 2.8.4 工具调用增量还原 stripSoloTools

入参是 `Any?`（Array / Object / 其它）。其它 → 返回 null（本帧无工具）。Array 直接用；单个 Object 包成一条数组。
逐条（非对象条目跳过）：

1. 若 `function_call` 存在且非 null → `call.function = call.function_call`，删 `function_call`。
2. `function` 子对象（若存在）→ 删除 `namespace`、`partial_arguments`。
3. `id` 为空串 → 删掉 `id` 键；`type` 为空串 → 删掉 `type` 键；`function.name` 为空串 → 删掉 `name` 键。
   原因：上游按 OpenAI 方式分片下发 tool_calls —— 首帧带 `id`/`type`/`name`/`index`，
   后续帧只带 `index` + `function.arguments` 片段；后续帧里的空 `id`/`type`/`name` 若透传，客户端可能
   当成"另一个工具"或覆盖首帧值。空串不是合法分片内容，必须删键而非留空。
4. 结果数组放入输出（`index` 与 `arguments` 原样保留，累加工作交给客户端/聚合器）。

### 2.9 UG（User Gateway）接口族：额度、签到、诊断

这些不属于聊天转发，但都在这三个文件里，桌面版移植时需要一并搬（同一 region/`ugHost` 体系）。

`ug(token, url, uid, region)` —— 共用 POST 小工具（body 恒为 `{}`），失败返回 null：

| Header | 值 |
|---|---|
| `Authorization` | `Cloud-IDE-JWT {token}` |
| `Accept` | `*/*` |
| `Content-Type` | `application/json` |
| `User-Agent` | `VSCode 1.107.1 (TRAE SOLO CN)` |
| `X-Market-Client-Id` | `VSCode 1.107.1` |
| `X-Market-User-Id` | `marketUserIdFor(uid)` |
| `X-Device-Id` | `deviceIdFor(uid)` |
| `Vscode-Sessionid` | `sessionIdFor(uid)` |
| `X-Lgw-Req-Sdk-Type` | `3` |
| `Package-Type` | `stable_cn` |
| `X-Lscbd-Aid` | `787976` |
| `X-Lscbd-Platform` | `windows` |
| `App-Version` | `0.1.45`（注意：与 SOLO 的 `0.1.43` 不同的第二个版本号） |
| `X-Tt-Trace-Id` | `00-` + uuid去横线后的**前 16 字符** + `-01` |
| `X-Request-Id` | 随机 UUID（带横线） |
| `Sec-Fetch-Dest` | `empty` |
| `Sec-Fetch-Mode` | `no-cors` |
| `Sec-Fetch-Site` | `none` |
| `X-User-Region` | `region.userRegionHeader`，空则回落 `"CN"`（区域唯一参与 UG 头的地方） |

判定（每次调用先把 `lastAuthFailed = false`）：

- HTTP 401 / 403 → `lastAuthFailed = true`，返回 null
- 非 2xx 或 body 空白 → null（`lastAuthFailed` 保持 false）
- body 不是 JSON object → null
- `code == 1001` → `lastAuthFailed = true`，null（实测：token 无效时 status 接口回 **HTTP 200 + code=1001 + enable=false**；
  不单独标就会出现"token 失效"被显示为"该账号没有签到权限"的假警报）
- 其它 → 返回该 JSONObject

`lastAuthFailed: Boolean`（`@Volatile`，只读暴露）：让上层区分"网络抖了"和"token 被拒"。

设备身份三元组（从 uid 稳定派生；上游按**设备维度**限签到，多账号共用设备 id 会被判"操作太过频繁" code 9074）：

```
seededStream(seed=uid, salt, nbytes):
  data = "salt:seed".UTF8
  循环 i = 0,1,2,...：SHA-256(data || BE32(i))，把 32 字节追加进输出，直到够 nbytes；截断到 nbytes

deviceIdFor(uid):      bytes = seededStream(uid,"devid",16)   -> 取前 15 字节，每字节 (b & 0xFF) % 10 作为十进制位 -> 15 位纯数字（格式错了上游不认）
sessionIdFor(uid):     bytes = seededStream(uid,"sess",32)    -> 全部转小写 hex -> 64 位 hex
marketUserIdFor(uid):  bytes = seededStream(uid,"market",16)  -> b[6] = (b[6]&0x0F)|0x40 ; b[8] = (b[8]&0x3F)|0x80
                        -> 小写 hex 按 8-4-4-4-12 连字符切分 -> 合法 UUID v4
```

实测向量（移植后必做等价自检；salt 分别为 `devid` / `sess` / `market`）：

```
uid = "1234567890123456"
  deviceIdFor      = 413174708280782
  sessionIdFor     = fbabf7aa1e173b90385c623e1ec49157860cc07a4b01f53f7ca141b17d876eae
  marketUserIdFor  = 746608f9-7f37-4960-b4c7-8553cec6d366
uid = "9876543210"
  deviceIdFor      = 630512735296035
```

其余不变式（源码 selfCheck 已钉）：同 uid 稳定、不同 uid 隔离、三个标识互不相同、换 uid 后三个都变、
device 必须 15 位全数字、sessionid 必须 64 位小写 hex、market 必须长度 36 且 `[14]=='4'` 且 `[19] in "89ab"` 且 4 个连字符。

接口族：

| 函数 | URL | 解析 |
|---|---|---|
| `fetchUsage(tok, uid, region)` | `reg.ugHost + /trae/api/v2/pay/ide_user_ent_usage` | `!reg.canQueryQuota` → null；取 `usage_summary`；`total = total_amount`，`< 0` → null；返回 `(total - consumed_amount).toLong()`（截断取整）。实测 5100 − 1931.46 = 3168.54 → UI 3168。`consumed_amount/total_amount` 与上游 `consumption_ratio` 逐位吻合（0.37871764705882355）。`total_amount` 是"当前所有未过期积分包 credits_limit 之和"，会随包过期/新增变化（见过 8350 → 5100），只适合算"当前可用"，已不再对外展示 |
| `fetchCheckin(tok, uid, region)` | `reg.ugHost + EP_CHECKIN_STATUS` | `parseCheckin` |
| `claimCheckin(tok, uid, region)` | `reg.ugHost + EP_CLAIM` | 返回 `optInt("code", -1)`；网络/解析失败 → null |
| `ugRaw(token,url,uid,region)` | 同 `ug` 但头集合精简（`Authorization` / `Content-Type` / UA / `X-Market-Client-Id` / `X-Market-User-Id` / `X-Device-Id` / `Vscode-Sessionid` / `Package-Type` / `X-User-Region`，**无 Accept、无 `X-Lgw-Req-Sdk-Type`、无 Sec-Fetch、无 X-Request-Id、无 App-Version、无 X-Tt-Trace-Id**） | 返回 `(HTTP码, 响应体字符串)`；请求都没送出去 → `(0, "异常类名: message")` |
| `diagnose(cred)` | 依次 `ugRaw` status 与 usage | 拼人话诊断串：uid/昵称、region + supported + canQueryQuota、access/refresh token 的**长度**（不打印值）、`expiresAt` 及 `MM-dd HH:mm` 与剩余分钟数，然后逐接口 `HTTP {code} authFailed={lastAuthFailed}\n{body.take(400)}` |
| `claimCheckinHealing(ctx, cred)` / `fetchCheckinHealing(ctx, cred)` | — | 先正常调用；失败且 `lastAuthFailed` → `Gateway.locked { TraeAuthApi.forceRefreshTokenFor(ctx, cred) }` 强制续 token（与转发链路互斥，避免互相顶掉 refreshToken 轮换）后重试一次；返回 null 时调用方必须用 `lastAuthFailed` 决定文案 |

`parseCheckin(root)`（纯函数，方便无网络自检）：`code == 1001` → null；否则
`CheckinStatus(checkedIn = root.optBoolean("checked_in", false), credits = root.optLong("credits", 0), enable = root.optBoolean("enable", false))`。
字段缺失一律回落默认值，绝不抛异常。

`checkinSucceeded(before, after, code)` —— **合取判据**（两个方向都会被单判据骗）：

```
code == 0 && after != null && after.checkedIn && before?.checkedIn != true
```

上游真实 status 原文（2026-09-25 真账号 dump，三种请求体返回一致）：

```json
{"checked_in":true,"code":0,"credits":150,"did_checked_in":false,"enable":true,"extra_credits":50,"message":"success"}
```

业务码语义（曾连错两次，别再退回旧写法）：

| code | 真实含义 | 旧误判 |
|---|---|---|
| `0` | 成功 | — |
| `1001` | 鉴权失败（HTTP 仍然是 200，且带 `enable=false`） | 误读为"该账号未开放签到" |
| `9004` | 缺设备头 → 参数校验失败（`The submitted order parameters are incorrect`）；同时也是"今天已签过"语境下的码 | 误读为"今日已签到"，导致"没签到却提示签到成功" |
| `9074` | 设备身份不独立（多账号共用设备 id，或缺 `X-Device-Id`/`Vscode-Sessionid`/`X-Market-User-Id` 三件套之一）→ "操作太过频繁" | 误读为"上游限流、客户端无解" |

幂等性：重复 claim 不会扣积分或改坏状态（`checked_in`/`credits` 不变），只产生无谓请求；UI 仍应在
`checkedIn == true` 时禁止点击。`code` 单独不能当成功判据（上游业务失败也回 HTTP 200 + 业务体）。

---

## 3. 模型清单拉取（fetchModels / parseModels）

请求：

```
POST {reg.agentHost}/api/ide/v1/get_detail_param
headers = solo(token, stream=false, uid, reg)     // Accept: application/json
body:
{
  "function": "solo_work_remote",
  "config_names": null,
  "need_prompt": false,
  "current_config_info": null,
  "poly_prompt": true,
  "mode_type": null,
  "agent_type": null
}
```

无 query 参数。前置门：`if (!reg.canQueryQuota) return null`（沿用了 UG 的能力位；SG 因 `ugHost` 非空也能过这道门，
但 `get_detail_param` 只在 CN 实测过，SG 打到 `core-normal.trae.ai` 会失败 → 返回 null → 上层用静态兜底表。
移植时保留"失败必须返回 null 且不得用空表覆盖静态表"这条即可）。

响应解析（`parseModels(root)`）：

1. `config_info_list` 缺失/非数组 → null。
2. 逐条（非对象跳过）：
   - `id = cfg.optString("config_name","").trim()`；空 → 跳过。
   - `isChatModel(cfg)` 过滤（见下）。
   - `name = cfg.display_config.display_name.trim()`，空则回落到 `id`。
   - `ctx = cfg.model_detail_list[0].max_input_tokens`（`optLong`，取不到为 0）；`ctx <= 0` → `131072`。
3. 输出对象字段（`/v1/models` 形状 + 两个 Trae 专有字段）：

| OpenAI 字段 | 值来源 |
|---|---|
| `id` | `config_name` |
| `name`（展示名） | `display_config.display_name`，空回落 `id` |
| `config_name` | `id`（重复写一份，供客户端/内部路由用） |
| `context_length` | `model_detail_list[0].max_input_tokens`，缺省 `131072` |
| `object` | `"model"` |
| `owned_by` | `"trae-solo"` |
| `created` | 固定 `1753600000` |

4. 结果数组为空 → **返回 null**（空表覆盖会让所有客户端看不到模型，Kotlin 侧 `models()` 用 `?: staticModels()` 兜底）。

失败日志文案：`拉模型表失败: http {code} {body.take(200)}`（warn）、`拉模型表异常: {类名}: {message}`（warn）。

`isChatModel(cfg)` 四条判据（全部来自上游字段；2026-09-25 把 49 项逐项 dump 核对过，不靠名字猜）：

```
1) cfg.config_source == 1                    // 只留官方内置。实测 config_source=3 那批正是用户自己接入的第三方 API（display_name 带「肖恩ai」「ZZ 」等用户前缀）
2) cfg.display_config != null 且 cfg.display_config.is_custom_model != true
3) cfg.is_invisible_to_user != true          // 上游自标不对用户展示：glm-5 / glm-5-turbo / DeepSeek-V4-Flash / DeepSeek-V4-Pro / summary / file_search_agent / sagitta / aquila …
                                             // 注意：同名两条（一条 invisible、一条可见）正是靠这条区分，只按名字过滤会让 UI 出现重复的 DeepSeek-V4-Flash
4) isBuiltinChatName(config_name)            // 词形兜底
```

`isBuiltinChatName(id)`：小写后 `startsWith("custom_model")` → false；
包含 `agent` / `subagent` / `summary` / `sagitta` / `aquila` / `file_search` / `explore_` / `computer_use` / `browser_use` 任一 → false；否则 true。

已知局限（必须原样带进移植说明）：无法排除"官方内置但当前账号无权用"的模型。实测 `Doubao-Seed-Evolving`
（会员专属）在表里且 `config_source=1` 且可见，真实聊天返回 `4001`。上游表没有"当前账号是否有权"字段可判，
所以保留；真要按权限过滤得逐个试聊，成本过高而放弃。

自检里钉住的必须放行名单（防止改判据误伤真模型）：`deepseek-v4.1-flash`、`glm-5.3`、`step-5-preview`、`DeepSeek-V4-Flash`。
必须拦下的：`mistral-small-4` / `gemini-3.1-pro` / `glm-4.5-air`（第三方，src=3+custom=true）、
`custom_model_gpt-6`、`summary`、`file_search_agent`、`browser_use_subagent`、`explore_sub_agent_v2`、`glm-5(invisible=true)`。

---

## 4. 非流式响应聚合路径

上游永远被强制成流式（`prepareBody` 写死 `stream=true`），因此"客户端要非流式"必须在**网关侧**把 chunk 聚合成
一个 `chat.completion`。Kotlin 实现位于 `LocalFakeServer.aggregateChunks(chunks, model)`，
移植时它就是 `chatStream` 的姊妹函数（输入 = `out` 回调收到的原始 SSE 片段列表）。

关键前置：`wantsStream` 必须在 `prepareBody` **之前**读（改写后就看不到客户端原始意图）。

缓冲与"是否已开始输出"的门控（`streamWithFailover` 的 `emit` 包装，与换号判定耦合，必须一起移植）：

```
collected = wantsStream ? null : ArrayList<String>()
每个上游 emit 的 chunk:
  if (!streaming) { pending.add(chunk); if (chunk.contains("\"choices\"")) { streaming = true; 依次写出 pending } }
  else 直接写出（流式）/ 直接 append（非流式）
```

即：**只有出现带 `"choices"` 的帧才认定"这次上游真的成功了"**；在那之前的所有帧（典型是 `event: error`）
都留在 pending 里，给换号/续期留出机会，只有最终失败的收尾才把 pending 原样吐给客户端。
流式分支下写失败（客户端 Broken pipe）被静默忽略，流仍继续并记录用量。

聚合算法（逐帧扫 `data:` 行；`payload` 为空或 `[DONE]` 跳过；JSON 解析失败跳过）：

| 目标字段 | 规则 |
|---|---|
| `id` | 第一个非空 `id`；全都没有 → `"chatcmpl-local"` |
| `created` | 第一个非零 `created`；否则 `now/1000` |
| `usage` | 最后一帧里出现的 `usage` 对象覆盖（非流式的 usage 来源） |
| `finish_reason` | 逐个 choice 取 `finish_reason`，**只在非空且不等于字面 `"null"` 时覆盖**，默认 `"stop"` |
| `content` | 按帧顺序拼接 `delta.content`（只看最后一帧拼内容会丢字） |
| `reasoning_content` | 同上拼接 `delta.reasoning_content`；结果为空则整个键不写 |
| `tool_calls` | `LinkedHashMap<index, acc>`，`index = tc.optInt("index", j)`；acc 初始化为 `{index, type:"function", function:{name:"", arguments:""}}`；`tc.id` 非空则覆盖；`function.name` 只在 acc 里为空时用首个非空填入；`function.arguments` 逐帧累加 |
| `role` | 恒 `"assistant"` |
| 工具与 finish 的一致性 | 有 tool_calls 且 `finish == "stop"` → 改写为 `"tool_calls"`（与流式同样的归一）；带 tool_calls 时 `content` 必须是字符串（可为空） |
| `model` | 用 `prepared` 里的 model 回填（上游 chunk 的 model 恒空串） |
| `object` | `"chat.completion"` |
| `choices` | 单元素：`{index:0, message:{...}, finish_reason}` —— 上游多 choice 会被折叠成 index 0 一条 |

自检钉住的样例语义：三帧 content `"想"/"你"/"好"` → `message.content == "你好"` 且 `reasoning_content == "想"`；
`total_tokens == 7` 的 usage 被带出；`id` 取首帧的 `c1`；tool_calls 的 `name` 在首帧、`arguments` 分片在后帧必须
合并成一条而不是互相覆盖。

非流式的错误优先：收尾时先 `extractUpstreamError(collected)`，只要 `err != null` 或者能抠出上游错误文案，
就写 **HTTP 502 + OpenAI error JSON**（`type: "api_error"`, `code: "upstream_error"`），`message` 取
`upErr ?: humanizeUpstreamError(err)`；只有两者都为空才 `200 + aggregateChunks(...)`。
少了这道判定，`event: error` / `data:{"error":…}` 帧会被聚合成"200 + 空回复"，用户以为是"模型不说话了"，
实际是额度/鉴权问题。

---

## 5.（并入 §4 的调用侧记录）onResult 之后网关做什么

`streamWithFailover` 在最终失败分支调用 `usageRecorder(model, usage, elapsedMillis, success = err==null, err, backend, uid)`。
`elapsed = System.currentTimeMillis() - t0` 覆盖整轮（含换号重试耗时）。Rust 侧对应一个 `UsageRecorder` trait / 回调，
签名建议保留 model、usage、耗时、成功位、错误码、backend、uid 七元组。

---

## 6. 失败分类与错误码协议

### 6.1 `chatStream` 侧产生的错误码字符串（`onResult` 的 err 值）

| 来源 | err 字符串 | 说明 |
|---|---|---|
| CN 非 2xx | `http:{code}` | 例 `http:401` |
| CN `error` 事件 | `solo:{code}` | `code` 是上游业务码 long，例 `solo:4001` |
| CN 流末无 done | `interrupted` | EOF 型中断 |
| CN 网络异常 | 直接抛出（无码），由外层捕获 | 网关补 502 |
| SG 无 user 消息 | `intl:empty_user_message` | 不发起任何上游请求 |
| SG 建会话异常 | `intl_create:exception` | |
| SG 建会话非 2xx | `intl_create:{http_code}` | |
| SG 建会话成功但无 sid | `intl_create:no_session` | |
| SG commit 非 2xx | `intl_commit:{http_code}` | |
| SG commit 异常 | `intl_commit:exception` | |
| SG events 非 2xx | `intl_events:{http_code}` | |
| SG events 异常 | `intl_events:exception` | |
| SG 流内 `error` 事件 | `intl:{errorText 前 60 字}` | |

给客户端的 SSE 侧文案（与 err 是两件事）：

- CN HTTP 失败：`event: error` + `data: "upstream http {code}: {body前200字}"`
- CN solo 错误：`event: error` + `data: "solo error code={code} msg={message}"`
- SG 各步：`event: error` + `data: "国际服建会话失败 http {code}"` / `"国际服 commit 失败 http {code}: {body前200}"` /
  `"国际服事件流失败 http {code}: {body前200}"` / `"国际服建会话异常: {类名}: {msg}"` /
  `"国际服 commit 异常: {类名}"` / `"国际服事件流异常: {类名}: {msg}"` / `"国际服建会话未返回 chat_session_id"` /
  `"请求里没有 user 消息内容"`
- 全部错误分支后面都跟 `data: [DONE]\n\n`

### 6.2 分类函数（网关判定表，移植必须逐条对齐 + 自检）

```rust
// err 形如 "{prefix}:{tail}"
fn auth_related(err: &str) -> bool {
    if err.starts_with("queued:") { return false }
    matches!(err.rsplit(':').next().and_then(|s| s.trim().parse::<i32>().ok()),
             Some(401) | Some(403))
}

fn retryable_upstream(err: &str) -> bool {
    if err.starts_with("queued:") { return true }
    match err.rsplit(':').next().and_then(|s| s.trim().parse::<i32>().ok()) {
        None => true,                      // 非 http 码类错误（exception / interrupted / no_session…）保守判定为可换号
        Some(c) => c == 401 || c == 403 || c == 429 || c >= 500,
    }
}
```

Kotlin 原文用 `err.substringAfterLast(':')`（无冒号时返回整串）+ `toIntOrNull()`，语义与上面的 `rsplit` 等价。

判定后果（源码 selfCheck 逐条钉住，移植后请原样复用这 10 条断言）：

| err | authRelated（=先同账号强制续期） | retryable（=允许换号） |
|---|---|---|
| `http:401` | 是 | 是 |
| `http:403` | 是 | 是 |
| `intl_create:429` | 否 | 是 |
| `http:429` | 否 | 是 |
| `http:503` / `http:500` | 否 | 是 |
| `http:400` | 否 | 否（请求本身的问题，换谁都一样） |
| `http:422` | 否 | 否 |
| `intl_commit:404` | 否 | 否 |
| `queued:dfmodel`（Qoder 排队） | 否（不该续期、不该等待重试） | 是（排队按账号判，换号真有效） |
| `intl_events:exception` | 否 | 是（保守换号） |

注意 `solo:4001` 这类上游业务码尾串是 4 位数字，`>= 500` 成立 → 判为**可换号**（这是有意/既成行为：
4001 常见成因是模型/参数与账号状态相关，换号有实际收益），移植时保持该算术性质即可，不必额外加分支。

### 6.3 重试/换号状态机（一次请求内的优先级）

```
attempt = 0; didRefresh = false
loop {
  跑一次上游（emit 门控见 §4）
  refreshed = (!streaming && err != null && !didRefresh && authRelated(err)) 且 refreshSameAccount() 成功
     -> didRefresh = true；同一账号强制续期重试（token 未到期却被判废时，换号是浪费：换号会真实切走并持久化）
  switched = (!refreshed && !streaming && err != null && attempt <= 1 && retryable(err)) 且 tryFailover(err) 成功
     -> 最多换 2 个号（attempt<=1）。排队/限流这类账号级问题换一次常不够，第一个候选也可能仍是排队号
  if (!switched && !refreshed) { 吐 pending → 记录 usage → attempt>0 且 err 非空则 onRollback 还原原账号 → 按流式/非流式收尾 → return }
  attempt += 1
}
```

换号候选选择（`TraeAuthStore.pickFailoverCandidate`）：排除刚失败的 uid，**优先 `isSupported`（国服）账号**，
没有再退而求其次换任何别的账号（国际服聊天半残，当兜底会把"国服 401"变成"国际服答不上来"）；
只有一个账号 → null（没人可换，原错误照报）。`tryFailover` 还要校验新凭证 `accessToken` 非空且 `uid != cur.uid`。

### 6.4 错误信息文案生成

`extractUpstreamError(frames)`：扫所有 `data:` 行，忽略空与 `[DONE]`；
载荷以 `"` 开头 → 按 JSON 字符串字面量解码，非空则 `humanizeUpstreamError(该串)` 返回；
否则按 JSONObject 解析，取 `error` 字段（String 直接用；对象取 `message`，再退 `type`；其它 `toString()`），
非空白则 `humanizeUpstreamError` 返回；都找不到 → null。

`humanizeUpstreamError(raw)`：**保留原文**，仅在其后追加 `\n\n{hint}`（猜错原因比不猜更糟，原文可用于搜索/求助）。
hint 关键词表：

| 匹配（忽略大小写） | hint |
|---|---|
| `Billing daily count exceeded` | `Qoder 账号今天的计费次数已用完（Billing daily count exceeded）。明天会重置；急着用就换一个 Qoder 账号，或把后端切到 Trae。` |
| 同时含 `billing` 与 `exceed` | `账号额度/计费次数已用尽，明天重置或换账号。` |
| 含 `quota` 或 `insufficient` | `账号额度不足，换账号或等重置。` |
| 含 `unauthorized` 或 `401` | `鉴权失败：Qoder 的 jobToken 可能已失效，回 App 重新登录一次。` |
| 其它 | 空（只回原文） |

Trae 侧的文案目前不经这张表（除上游自带 error 文案透传），hint 表主要为 Qoder 后端准备；移植时整表搬过去即可复用。

`prepareBody` 抛 JSONException（`messages` 存在但不是数组）或 OkHttp 抛 IOException 时，外层必须捕获；
未捕获会让客户端收到零字节、表现成"连接被重置"，既没有状态行也无法诊断。

---

## 7. TraeIntlUpstream 与国服版差异（逐项）

顶层分派：`TraeUpstream.chatStream` 在 `reg != CN` 时把**同一批参数**整体转交
`TraeIntlUpstream.chatStream(prepared, traeToken, uid, out, onResult)`，契约为逐字节等价
（都按 `data: ...\n\n` 出块、都恰好回调一次 `(usage, err)`）。以下只列差异。

| 维度 | CN（TraeUpstream） | SG（TraeIntlUpstream） |
|---|---|---|
| 协议形态 | 单次 `POST /api/agent/v3/llm_utils_chat` | 三段式：`POST /api/remote/v1/chat_sessions` → `POST .../{sid}/commit` → `GET .../{sid}/events?...`（SSE） |
| host 来源 | 快照 `reg.agentHost` | **文件内私有 `const HOST = "https://core-normal.trae.ai"`**（值与 `TraeRegion.SG.agentHost` 相同，但不读区域枚举；移植时统一为一个 host 常量更安全） |
| OkHttp 客户端 | 自己的实例，connect 20s / read 180s | 另一个独立实例，同样 20s / 180s（注释：事件流是长连接，读超时必须够长） |
| 请求头集合 | SOLO 17 项（`Trae/{ver}` UA、`X-App-Id`、`X-Ide-*`、`X-Uid`…） | web 端 7+2 项：`Authorization: Cloud-IDE-JWT {token}`、`X-Trae-Client-Type: web`、`x-user-region: US`、`Origin: https://work.trae.ai`、`Referer: https://work.trae.ai/`、`User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36`；POST 另加 `Content-Type: application/json` 与 `Accept: application/json, text/plain, */*`；事件流另加 `Accept: text/event-stream` |
| 自定义签名/设备头 | UG 才有设备头 | web 头里 `x-user-region` 恒为 `US`（小写头名，与 UG 的 `X-User-Region` 是两套大小写） |
| URL query | 聊天无 query | `events?reply_to_message_id={mid}`（唯一一处 query 参数） |
| 请求体 | 客户端 body 改写后**整体** POST | **重建**：仅取最后一条 user 消息文本作为 `query`，其余 OpenAI 字段（temperature、max_tokens、messages 历史…）全部丢弃 |
| 历史消息 | 走 `messages`（原生支持多轮） | 上游**不接收** messages。多轮只能靠复用 `chat_session_id`；实测把历史塞进 `initial_message.content` 会让上游**完全不生成**（建会话仍 200 / status 2，事件流只回一个 `status_changed new_status=6` 就结束，无 `plan_item`）。改回空数组立刻恢复 |
| 会话复用现状 | 无状态 | `sessions: ConcurrentHashMap<String,String>` + `sessionKey`（`system(前512字) + '\u0000' + 首条user`，截 512；无 user → 空 key 不复用）；`rememberSession` 上限 `SESSIONS_MAX = 32`，超限按插入序删最早的（非 LRU 够用；只在内存，重启失效，不落盘不含凭据） | 
| 多轮实际效果 | 正常 | **实测未生效**：回填旧 sid 后上游仍返回**全新** sid（`message_index` 恒为 3，非追加），即 `POST /chat_sessions` 只建不续（第二轮问"我叫什么"仍答"不知道"）。要真正支持需抓包确定 web 端发第二条消息的真实请求（很可能不是这个端点，或需要 `reply_to_message_id` / `last_message_id` 之类字段）。一旦抓到正确形状，改 `createBody` 即可生效 |
| `content` 字段 | 转成 `[{type:text,text}]` 后发给上游 | `initial_message.content` **必须留空数组**（同上） |
| 工具：请求方向 | `tools[].function.parameters`（object）序列化为 JSON **字符串**；`tool_choice` 归一为字符串 | `toolSetting` 读的是 **已经过 `prepareBody` 的 `prepared`**，所以 `tools[].function.parameters` 此刻已是 JSON 字符串 → 产出的 `tool_plus_setting` 是"JSON 字符串里再套一层转义字符串"（**双重编码**）。该值整体作为字符串放进请求体；建会话被拒时**摘掉该字段重试一次**（尽力而为，失败不影响聊天）。注：国际服 `tool_call_info.name` 实测恒为空、`params` 恒为 null，从未观察到真实工具调用，形状属推测，双重编码是否合规无法证实 |
| 工具：响应方向 | 分片增量（首帧 id/type/name，后续帧 index+arguments），需 `stripSoloTools` 清空键 | **整段一次性**下发：只在首次见到 `tool_call_info.name` 非空时发一条完整 tool_call（`index:0`、`id:"call_{millis}"`、`type:"function"`、`function.{name,arguments}`），并把 `delta.content` 强制置为 `""`（OpenAI 要求带 tool_calls 的帧 content 为空串） |
| 正文承载事件 | `event: output`，字段 `response` / `reasoning_content` / `tool_calls` | `event: plan_item`，字段 `thought` / `reasoning_content` / `tool_call_info` |
| **累积快照问题** | 上游就是增量，直接透传 | `thought` / `reasoning_content` 是**累积快照**（实测请求"从1数到10"，相邻两帧为 `"1"` 与 `"1,2,3,4,"`，后者包含前者），**必须 diff 出新增后缀**，否则客户端拼出 `11,2,3,4,`。同一 `id` 会跨多帧反复出现，不能用 id 变化判断新片段。`deltaOf(prev, now)`：`now` 空 → `""`；`now == prev` → `""`；`now.startsWith(prev)` → 后缀；否则整段（重写/回退时宁可多也不丢）。按字符而非字节 |
| 其它被忽略的事件 | 只认 4 种（output/token_usage/done/error） | 实测共 11 种：`status_changed / platform_timing / metadata / model_config / session_title_message / session_icon_message / timing_events / plan_item / token_usage / done`（其中只有 `plan_item` 带正文）；`streamEvents` 额外识别 `error`，其余一律 `IGNORE` |
| `done` 语义 | `finish_reason` 取上游值（有工具则归一 `tool_calls`） | 固定 `"stop"`（或 `sawToolCall` 时 `"tool_calls"`）；`usage` 从 `done` 的 `usage` 子对象取，与 `lastUsage` 合流 |
| usage 下发位置 | 挂在 done **之后第一个 chunk**（`pendingUsage` 机制），因此末帧带 usage | `chunk()` **从不写 usage**；usage 只经 `onResult` 上浮给非流式聚合/记账。带流式的客户端可能拿不到 usage（移植后可按需补齐：在末帧写入） |
| usage 原始 JSON | `token_usage` 的整个 data 对象 | `token_usage` 的整个 data 对象（同名同构，字段形状由上游决定） |
| `created` / `id` / `model` | `chatcmpl-{millis}`（流开始一次）/ 每帧 `now/1000` / `""` | 同一套写法 |
| EOF（无 done）处理 | `writeDONE()` + `settle(null, "interrupted")`（视为错误 → 参与换号判定） | `chunk({}, "stop")` + `[DONE]` + `settle(lastUsage, null)`（**成功收尾**，err = null）。移植时保持这个差异或明确统一，它直接影响换号行为 |
| commit 步骤 | 无 | `POST .../{sid}/commit`，body `{"title": userText.take(80)}`；**少这一步模型根本不生成**（事件流只回一个 status_changed） |
| 空 user 消息 | 无特殊检查（照发上游） | 提前判定并直接报错 `intl:empty_user_message`，**不发起任何上游请求** |
| 失败重试 | 无内部重试 | 建会话一次；若带了 `tool_plus_setting` 且失败 → 去掉它再试一次 |
| `NO_SESSION` 哨兵 | 无 | `private const val NO_SESSION = "\u0000NO_SESSION"`（`createSession` 用 `(http码字符串, NO_SESSION)` 表示失败，正常 mid 恒非空不会撞名） |
| prompt 提取 | 由上游自己解析 `messages` | `promptOf`: 从后往前找首条 `role == "user"`；`flattenContent`: String 直接用、数组只拼 `type=="text"` 段的 `text`、其它 → `""`（tool 结果只带 `tool_call_id` 而无 content 时得空串，属已知上限） |
| 自检覆盖 | 设备派生、模型表解析、签到判据、模型名信任边界 | `deltaOf` 六条边界、`promptOf`/`flattenContent`、`content` 必须为空、`sessionKey` 稳定性与区分度（同对话下一轮 key 不变，否则多轮失效）、`toolCall` 形状 |

国际服 common_params 内部常量（与区域无关，web 端固定值）：

```json
{"language":"zh-cn","app_language":"en","quality":"stable","app_version":"1.0.0.63",
 "biz_user_id":"<uid>","user_unique_id":"<uid>","scope":"marscode-us","tenant":"marscode",
 "solo_chat_mode":"work","user_identity":"Free","is_freshman":"0","is_privacy_mode":0,"privacy_mode":"off"}
```

`createBody` 其余固定值：顶层 `mode:"work"`、`environment_id:"default"`、`env:"remote"`、
`auto_create_project:false`、`origin:"web"`；`initial_message` 里 `agent_id:"solo_work_remote"`、
`agent_type:"solo_work_remote"`、`model_selection_strategy:"auto"`、`model_name:<prepared.model>`
（这里传的是 `prepared` 里已经写好/回落的模型名；空串 = 服务端自动选）、`chat_session_id:<复用 sid 或 "">`。
注意国际服的 `agent_id/agent_type` 恰好与国内服的 `FUNCTION` 同名（都是 `solo_work_remote`），语义无关。

抓包修正记录（重要，防止移植时回退到旧文档）：第三方文档写 `query` 是对象、
`agent_type=solo_agent_remote`，实测两者都会 500。`query` 必须是 JSON **字符串**：
`[{"type":"text","data":{"content":"<用户话>"}}]`。

---

## 8. 魔法常量总表

| 常量 | 值 | 出处 / 用途 |
|---|---|---|
| CN agentHost | `https://trae-api-cn.mchost.guru` | 聊天、模型表 |
| CN ugHost | `https://api.trae.cn` | 额度、签到 |
| CN oauthHost / consoleHost / clientId | `https://api.trae.com.cn` / `https://www.trae.cn` / `en1oxy7wnw8j9n` | 登录链路 |
| CN userRegionHeader | `CN` | UG 头 `X-User-Region` |
| SG agentHost / HOST | `https://core-normal.trae.ai` | 三段式聊天 |
| SG ugHost / oauthHost | `https://ug-normal.trae.ai`（两处同值） | 额度（未实测）/ ExchangeToken |
| SG consoleHost / clientId | `https://www.trae.ai` / `ono9krqynydwx5` | 登录页 / OAuth |
| SG userRegionHeader | `""`（不发该头） | — |
| 曾错的 SG 入口 | `a0ai-api-sg.byteintlapi.com` → 只会 `4001 param is invalid`，权威逆向项目里零出现 | 禁止回填 |
| IDE_VERSION / IDE_VERSION_CODE | `0.1.43` / `20260716` | SOLO 头 |
| UG App-Version | `0.1.45` | 与 SOLO 版本号不同 |
| UG UA / X-Market-Client-Id | `VSCode 1.107.1 (TRAE SOLO CN)` / `VSCode 1.107.1` | |
| APP_ID | `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` | `X-App-Id` |
| DEVICE_BRAND / OS_VERSION / X-Device-Type / X-App-Version / X-Ide-Version-Type / Request-Traffic-Type | `83DG` / `Windows 11 Pro` / `windows` / `default` / `stable` / `prod` | |
| X-Lgw-Req-Sdk-Type / X-Lscbd-Aid / X-Lscbd-Platform / Package-Type | `3` / `787976` / `windows` / `stable_cn` | UG 头 |
| Sec-Fetch-Dest / Mode / Site | `empty` / `no-cors` / `none` | UG 头 |
| FUNCTION | `solo_work_remote` | 聊天体 + 模型表体；也等于 SG 的 agent_id/agent_type |
| DEFAULT_MODE | `DeepSeek-V4-Flash` | model 缺失/非法回落 |
| MODEL_ID_RE | `^[A-Za-z0-9._-]{1,64}$` | 模型名信任边界 |
| SG web UA | `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36` | |
| SG Origin / Referer / X-Trae-Client-Type / x-user-region | `https://work.trae.ai` / `https://work.trae.ai/` / `web` / `US` | |
| SG POST Accept | `application/json, text/plain, */*` | |
| SG 固定体值 | `mode:work`, `environment_id:default`, `env:remote`, `origin:web`, `auto_create_project:false`, `model_selection_strategy:auto`, `language:zh-cn`, `app_language:en`, `quality:stable`, `app_version:1.0.0.63`, `scope:marscode-us`, `tenant:marscode`, `solo_chat_mode:work`, `user_identity:Free`, `is_freshman:"0"`, `is_privacy_mode:0`, `privacy_mode:"off"` | |
| 派生 salt | `devid` / `sess` / `market` | SHA-256 KDF 盐 |
| 设备格式 | device 15 位纯数字；sessionid 64 位小写 hex；market UUID v4 | |
| 时间/超时 | connect 20s；read 180s（两处各一套客户端实例） | |
| 截断长度 | 诊断 body 400；错误 body 200；模型名日志 40；commit title 80；SG 错误码文案 60 | `.take(n)` 字符数 |
| 模型表兜底 | `context_length` 缺省 `131072`；`created` 固定 `1753600000`；`owned_by` = `"trae-solo"` | |
| 词形黑名单 | `custom_model*` 前缀；`agent`,`subagent`,`summary`,`sagitta`,`aquila`,`file_search`,`explore_`,`computer_use`,`browser_use` | |
| 会话表 | `SESSIONS_MAX = 32`；key 取 512 字符；分隔符 `'\u0000'` | |
| `NO_SESSION` 哨兵 | `"\u0000NO_SESSION"` | |
| UG/业务码 | `0` 成功；`1001` 鉴权失败（HTTP 200）；`4001` param invalid；`9004` 参数校验失败 / 今天已签过；`9074` 设备身份不独立（操作太过频繁） | |
| chunk id / created | `chatcmpl-{System.currentTimeMillis()}`；`{millis/1000}` 每帧；`model` 恒 `""` | |
| SSE 终止 | 内部终止事件 `event: done`；网关写 `data: [DONE]` | |
| 换号上限 | `attempt <= 1`（最多换 2 个号）；续期最多 1 次 / 请求 | |

---

## 9. 硬编码 JSON 样例（转写示例各一）

### 9.1 请求转写

客户端（OpenAI）发来：

```json
{
  "model": "glm-5.3",
  "stream": false,
  "temperature": 0.7,
  "messages": [
    {"role": "system", "content": "你是简洁的助手"},
    {"role": "user", "content": "北京天气"},
    {"role": "assistant", "content": null,
     "tool_calls": [
       {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"北京\"}"}},
       {"id": "call_2", "type": "function", "function": {"name": "   ", "arguments": "{}"}}
     ]},
    {"role": "tool", "tool_call_id": "call_1", "content": "晴 12℃"}
  ],
  "tools": [
    {"type": "function", "function": {"name": "get_weather", "description": "查天气",
      "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}}
  ],
  "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
  "presence_penalty": 0.0
}
```

`prepareBody` 之后真正 POST 到 `https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat` 的体
（`org.json.JSONObject` 内部是 HashMap，键序无保证；下面按语义顺序书写，移植时用 `serde_json::Map` 保序或任意序均可，上游不依赖顺序）：

```json
{
  "model": "glm-5.3",
  "stream": true,
  "temperature": 0.7,
  "messages": [
    {"role": "system", "content": [{"type": "text", "text": "你是简洁的助手"}]},
    {"role": "user", "content": [{"type": "text", "text": "北京天气"}]},
    {"role": "assistant", "content": null,
     "tool_calls": [
       {"id": "call_1", "type": "function", "function_call": {"name": "get_weather", "arguments": "{\"city\":\"北京\"}"}}
     ]},
    {"role": "tool", "tool_call_id": "call_1", "content": [{"type": "text", "text": "晴 12℃"}]}
  ],
  "tools": [
    {"type": "function", "function": {"name": "get_weather", "description": "查天气",
      "parameters": "{\"type\":\"object\",\"properties\":{\"city\":{\"type\":\"string\"}},\"required\":[\"city\"]}"}}
  ],
  "tool_choice": "get_weather",
  "presence_penalty": 0.0,
  "function": "solo_work_remote",
  "config_name": "glm-5.3"
}
```

变化点核对：`stream` 被强制 true；`tool_calls[1]` 因 name 去空白为空被丢弃；`function`→`function_call`；
所有字符串 `content`（含 tool 角色）变多段数组，`content: null` 保持 null（`isNull` 判定跳过）；
`tools.parameters` 变 JSON 字符串；`tool_choice` 对象折叠成命名字符串；
末尾新增 `function`、`config_name`；`model` 原样保留（合法名）。
另外 `max_tokens` 等未列字段一律不动。
若客户端把 `model` 写成 `a","x":"1`（注入型，不匹配 `MODEL_ID_RE`）则 `model` 与 `config_name` 都变成 `DeepSeek-V4-Flash`。

国际服对**同一份 prepared** 的体（`userText` 只取最后一条 user = "北京天气"；tools 存在时另附 `tool_plus_setting`）：

```json
{
  "mode": "work",
  "environment_id": "default",
  "initial_message": {
    "chat_session_id": "",
    "content": [],
    "query": "[{\"type\":\"text\",\"data\":{\"content\":\"北京天气\"}}]",
    "model_name": "glm-5.3",
    "agent_id": "solo_work_remote",
    "agent_type": "solo_work_remote",
    "model_selection_strategy": "auto",
    "common_params": "{\"language\":\"zh-cn\",\"app_language\":\"en\",\"quality\":\"stable\",\"app_version\":\"1.0.0.63\",\"biz_user_id\":\"3695871032827027\",\"user_unique_id\":\"3695871032827027\",\"scope\":\"marscode-us\",\"tenant\":\"marscode\",\"solo_chat_mode\":\"work\",\"user_identity\":\"Free\",\"is_freshman\":\"0\",\"is_privacy_mode\":0,\"privacy_mode\":\"off\"}"
  },
  "env": "remote",
  "auto_create_project": false,
  "origin": "web",
  "tool_plus_setting": "{\"tools\":[{\"name\":\"get_weather\",\"description\":\"查天气\",\"parameters\":\"{\\\"type\\\":\\\"object\\\",\\\"properties\\\":{\\\"city\\\":{\\\"type\\\":\\\"string\\\"}},\\\"required\\\":[\\\"city\\\"]}\"}]}"
}
```

（`tool_plus_setting` 里的 `parameters` 是**二次转义**结果，正源是 §7 记录的"prepareBody 已先把 parameters 变字符串"。
它对应的未转义内层内容为：
`{"tools":[{"name":"get_weather","description":"查天气","parameters":"{\"type\":\"object\",\"properties\":{\"city\":{\"type\":\"string\"}},\"required\":[\"city\"]}"}]}`）

### 9.2 响应转写（CN）

上游 SSE 原文（片段，展示一帧带工具的完整序列）：

```
event: output
data: {"response":"北京","reasoning_content":null}

event: output
data: {"response":"今天晴，12℃"}

event: output
data: {"tool_calls":[{"index":0,"id":"call_9","type":"function","function":{"name":"get_weather","arguments":"{\"ci","namespace":"solo_tools","partial_arguments":"..."}}]}

event: output
data: {"tool_calls":[{"index":0,"id":"","type":"","function":{"name":"","arguments":"ty\":\"北京\"}"}}]}

event: token_usage
data: {"prompt_tokens":100,"completion_tokens":8,"total_tokens":108}

event: done
data: {"finish_reason":"stop"}

```

网关 `out` 收到的 OpenAI chunk（`created` 每帧重取，示例用固定值示意；注意第 5 帧起 `reasoning_content` 因是
JSON null 而不再出现，`model` 恒空串，usage 挂在末帧）：

```
data: {"id":"chatcmpl-1762000000000","object":"chat.completion.chunk","created":1762000000,"model":"","choices":[{"index":0,"delta":{"content":"北京"}}]}

data: {"id":"chatcmpl-1762000000000","object":"chat.completion.chunk","created":1762000001,"model":"","choices":[{"index":0,"delta":{"content":"今天晴，12℃"}}]}

data: {"id":"chatcmpl-1762000000000","object":"chat.completion.chunk","created":1762000001,"model":"","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_9","type":"function","function":{"name":"get_weather","arguments":"{\"ci"}}]}}]}

data: {"id":"chatcmpl-1762000000000","object":"chat.completion.chunk","created":1762000001,"model":"","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ty\":\"北京\"}"}}]}}]}

data: {"id":"chatcmpl-1762000000000","object":"chat.completion.chunk","created":1762000001,"model":"","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":8,"total_tokens":108}}

data: [DONE]

```

同一份 chunk 列表走非流式聚合（客户端发的是 `stream:false`）得到：

```json
{"id":"chatcmpl-1762000000000","object":"chat.completion","created":1762000000,"model":"glm-5.3",
 "choices":[{"index":0,
   "message":{"role":"assistant","content":"北京今天晴，12℃",
     "tool_calls":[{"index":0,"type":"function","id":"call_9",
       "function":{"name":"get_weather","arguments":"{\"city\":\"北京\"}"}}]},
   "finish_reason":"tool_calls"}],
 "usage":{"prompt_tokens":100,"completion_tokens":8,"total_tokens":108}}
```

国际服对同一轮的上游事件（`thought` 为累积快照，实测形状）：

```
event: status_changed
data: {"new_status":2}

event: plan_item
data: {"id":"p1","task_id":"t1","thought":"北京","reasoning_content":"","timing":{},"tool_call_info":{"id":"x","name":"","params":null,"result":{}}}

event: plan_item
data: {"id":"p1","task_id":"t1","thought":"北京今天晴，12℃","reasoning_content":"查数据","timing":{},"tool_call_info":{"id":"x","name":"","params":null,"result":{}}}

event: done
data: {"usage":{"total_tokens":20}}
```

转换后下发（首帧整段，第二帧只发后缀 `今天晴，12℃`、reasoning 后缀 `查数据`；`done` 帧 finish 为 `stop`，
usage **不**出现在流式帧里，只经 `onResult` 上浮）：

```
data: {"id":"chatcmpl-1762000000500","object":"chat.completion.chunk","created":1762000000,"model":"","choices":[{"index":0,"delta":{"content":"北京"}}]}

data: {"id":"chatcmpl-1762000000500","object":"chat.completion.chunk","created":1762000000,"model":"","choices":[{"index":0,"delta":{"content":"今天晴，12℃","reasoning_content":"查数据"}}]}

data: {"id":"chatcmpl-1762000000500","object":"chat.completion.chunk","created":1762000001,"model":"","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]

```

---

## 10. 建议 Rust 模块 API 草案

分层：`region`（常量与持久化状态）、`trae`（CN 协议）、`trae_intl`（SG 协议）、`wire`（SSE 编解码）、
`device_id`（派生）、`quota`（UG 接口）、`convert`（请求/响应转换，可被两协议复用）、`gateway`（换号/聚合，已有其它 spec 覆盖）。

```rust
// ---- region.rs ----
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region { Cn, Sg }

pub struct RegionInfo {
    pub id: &'static str,
    pub agent_host: &'static str,
    pub ug_host: &'static str,
    pub user_region_header: &'static str,
    pub oauth_host: &'static str,
    pub console_host: &'static str,
    pub client_id: &'static str,
}

impl Region {
    pub const IDE_VERSION: &'static str = "0.1.43";
    pub fn info(self) -> &'static RegionInfo;
    pub fn can_login(self) -> bool;
    pub fn can_query_quota(self) -> bool;
    pub fn is_supported(self) -> bool;          // self == Cn
    pub fn unsupported_note(self) -> &'static str;
    pub fn from_id(id: Option<&str>) -> Region; // trim+lowercase，未知/空 -> Cn
}

/// 全局生效区域：进程内一次读取、一次快照；并发切换用 RwLock 或 ArcSwap
pub fn current() -> Region;
pub fn set_and_persist(cfg: &ConfigStore, r: Region);
pub fn load_persisted(cfg: &ConfigStore);
/// 上层：以"当前账号自带 region"为权威同步（对应 syncRegionToActiveAccount）
pub fn sync_to_active_account(store: &AccountStore, cfg: &ConfigStore);
pub fn require_login(r: Region) -> Result<Region, LocalError>;
pub fn self_check() -> anyhow::Result<()>;

// ---- wire.rs（SSE 行状态机：CN 与 SG 共用一套行解析）----
pub struct SseScanner { event: String, data: String }
pub enum SseEvent {
    Named { event: String, json: Option<serde_json::Value> }, // 空 data / 解析失败 => json = None
}
impl SseScanner {
    /// 每行喂入（不含行尾换行）；只在遇到空行且 event 非空时返回 Some，否则 None
    pub fn feed(&mut self, line: &str) -> Option<SseEvent>;
}

// ---- convert.rs ----
pub fn prepare_body(mut body: serde_json::Value) -> serde_json::Value;
// 规则清单：stream=true、function=solo_work_remote、assistant.tool_calls[].function->function_call 且丢空 name、
// content(String且非null)->[{type:text,text}]、model trim/校验/回落 + 双写 config_name、
// normalize_tool_choice、normalize_tools(parameters object->String)
pub const MODEL_ID_PATTERN: &str = r"^[A-Za-z0-9._-]{1,64}$";
pub fn model_name_allowed(s: &str) -> bool;

// ---- trae.rs（CN）----
pub struct TraeClient { http: reqwest::Client }   // connect 20s, read 180s
pub enum ChatError {
    Http(u16),            // "http:{code}"
    Solo(i64),            // "solo:{code}"
    Interrupted,          // "interrupted"
    Transport(String),    // socket 超时/重置：映射到可换号语义
}
impl ChatError { pub fn wire_tag(&self) -> String; }   // 生成分类用的 err 串

pub trait ChunkSink: FnMut(String) {}                  // 每块已是 "data: ...\n\n"

impl TraeClient {
    pub async fn chat_stream(
        &self,
        prepared: &serde_json::Value,
        access_token: &str,
        uid: &str,                      // 空则不发 X-Uid
        region: Region,                 // 调用方快照；!= Cn 时转交 trae_intl
        chunk: &mut dyn FnMut(&str),
    ) -> Result<Option<serde_json::Value>, ChatError>;   // Ok(Some(usage)) = 成功
    pub async fn fetch_models(&self, access_token: &str, uid: &str) -> Option<serde_json::Value>; // 空表 => None
    pub fn parse_models(root: &serde_json::Value) -> Option<serde_json::Value>;
    pub fn is_chat_model(cfg: &serde_json::Value) -> bool;
}

// ---- trae_intl.rs（SG）----
pub struct TraeIntlClient { http: reqwest::Client }
pub enum IntlError {
    EmptyUserMessage, Create(u16), CreateException, NoSession,
    Commit(u16), CommitException, Events(u16), EventsException, Upstream(String),
}
impl TraeIntlClient {
    pub async fn chat_stream(
        &self, prepared: &serde_json::Value, access_token: &str, uid: &str,
        sessions: &SessionCache, chunk: &mut dyn FnMut(&str),
    ) -> Result<Option<serde_json::Value>, IntlError>;
    pub fn create_body(user_text:&str, model:&str, uid:&str, sid:&str) -> serde_json::Value;
    pub fn tool_setting(prepared:&serde_json::Value) -> Option<String>;   // Option<JSON 字符串>
    pub fn prompt_of(prepared:&serde_json::Value) -> String;
    pub fn flatten_content(msg:&serde_json::Value) -> String;
    pub fn session_key(prepared:&serde_json::Value) -> String;            // "" = 不复用
}
pub fn delta_of(prev: &str, now: &str) -> String;   // chars 语义，非字节
pub struct SessionCache { /* HashMap + 插入序 VecDeque，cap 32 */ }

// ---- device_id.rs ----
pub trait SeedStream { fn seeded_stream(seed:&str, salt:&str, nbytes:usize) -> Vec<u8>; } // SHA-256(salt:seed||BE u32 counter)
pub fn device_id_for(uid:&str) -> String;      // 15 位纯数字
pub fn session_id_for(uid:&str) -> String;     // 64 位小写 hex
pub fn market_user_id_for(uid:&str) -> String; // UUID v4
pub fn self_check_vectors() -> anyhow::Result<()>;   // §2.9 的四组实测值 + 格式断言

// ---- quota.rs ----
pub struct UgClient { http: reqwest::Client }
pub struct CheckinStatus { pub checked_in: bool, pub credits: i64, pub enable: bool }
pub enum UgOutcome<T> { Ok(T), AuthFailed, Failed }       // 取代 lastAuthFailed 全局位
impl UgClient {
    pub async fn fetch_usage(&self, tok:&str, uid:&str, region: Region) -> UgOutcome<i64>;
    pub async fn fetch_checkin(&self, tok:&str, uid:&str, region: Region) -> UgOutcome<CheckinStatus>;
    pub async fn claim_checkin(&self, tok:&str, uid:&str, region: Region) -> UgOutcome<i32>;
    pub fn parse_checkin(root:&serde_json::Value) -> Option<CheckinStatus>;      // code==1001 -> None
    pub fn checkin_succeeded(before:Option<&CheckinStatus>, after:Option<&CheckinStatus>, code:Option<i32>) -> bool;
    pub async fn diagnose(&self, cred:&Cred) -> String;
    // healing 版本由上层组合：Err(AuthFailed) -> force_refresh -> 重试一次
}

// ---- gateway.rs（跨协议公共控制流，对应 streamWithFailover）----
pub fn auth_related(err_tag:&str) -> bool;     // queued: -> false；尾数 401/403 -> true
pub fn retryable_upstream(err_tag:&str) -> bool;
pub async fn stream_with_failover<C: UpstreamChat>( /* 见 §6.3 状态机 */ ) -> Response;
pub fn aggregate_chunks(frames:&[String], model:&str) -> serde_json::Value;   // §4
pub fn extract_upstream_error(frames:&[String]) -> Option<String>;
pub fn humanize_upstream_error(raw:&str) -> String;
```

移植时优先落的自检（等价搬运现有 `selfCheck`，它们是全项目最贵的资产）：
`Region::self_check`、`device_id::self_check_vectors`、`convert::prepare_body` 的模型名信任边界与 tool_calls 丢弃、
`parse_models` 的 8 条过滤/回落、`checkin_succeeded` 的 8 条合取断言、`delta_of` 的 6 条（含中文按字符）、
`session_key` 的 4 条（含"同对话下一轮必须不变"）、`aggregate_chunks` 的 content/reasoning/tool_calls/finish/usage/id，
`auth_related` / `retryable_upstream` 的 13 条判定表。
