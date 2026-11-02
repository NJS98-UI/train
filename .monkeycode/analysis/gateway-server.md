# TraeWork2API 网关层分析（LocalFakeServer / Gateway / GatewayService 及上游链路）

> 目标精度：不看 Kotlin 源码即可用 Rust（Tauri 桌面版）重写。
> 主分析文件：
> - `app/src/main/java/com/example/traework2api/LocalFakeServer.kt`（921 行，全文通读）
> - `app/src/main/java/com/example/traework2api/Gateway.kt`（255 行，全文通读）
> - `app/src/main/java/com/example/traework2api/GatewayService.kt`（360 行，全文通读）
> 语义确认引用的其他文件：`TraeUpstream.kt`、`QoderUpstream.kt`、`TraeIntlUpstream.kt`、`GatewayBackend.kt`、`TraeAuthStore.kt`、`QoderAuthStore.kt`、`TraeAuthApi.kt`、`QoderAuthApi.kt`、`ModelsRepo.kt`、`QoderModelCatalog.kt`、`QoderMachine.kt`、`QoderCosyCrypto.kt`（常量）、`UsageStore.kt`、`CreditMonitor.kt`、`TraeRegion.kt`、`MainActivity.kt`（selfCheck/自动启动调用点）。

---

## 1. HTTP 服务（LocalFakeServer）

### 1.1 监听地址 / 端口 / 线程模型

| 项 | 值 | 说明 |
|---|---|---|
| 绑定地址 | `0.0.0.0`（`InetAddress.getByName("0.0.0.0")`） | 局域网可达，不只回环 |
| 端口 | `8787`（`const val PORT`） | 注释记录历史：8788 太常见；84772 超 TCP 上限；4772 是特权端口，最终落到 8787 |
| backlog | `50`（`ServerSocket(PORT, 50, …)`） | |
| accept 线程 | 单线程，命名 `LocalFakeServer-accept`，daemon | 循环 `ss.accept()`，`running` 为 false 时退出 |
| 连接处理 | **每连接一个线程**，命名 `LocalFakeServer-conn`，daemon，无上限 | 连接内异常只 `Log.w` + 关 socket，不影响进程 |
| 启停控制 | `running: AtomicBoolean`（`compareAndSet` 保证幂等 start）；`stop()` 置 false 并 `server.close()`（会使 accept 抛异常退出循环）；不 join 存量连接线程 | |
| `isRunning` | `running.get()` | |

### 1.2 Socket 选项

- `soTimeout = 15_000` ms：防止慢客户端永久占线程。**注意**：`ServerSocket.setSoTimeout` 未设置，只设置在每个 accepted socket 上；该超时同样作用于阻塞写。
- `tcpNoDelay = true`：SSE 小包场景必须关 Nagle（Android 上延迟合包最长 ~200ms，用户感知"转半天不吐字"）。**Rust 移植必须保留**（`set_nodelay(true)`），且回环上无副作用。

### 1.3 请求解析（手写 HTTP/1.x 解析器）

- 请求行/头采用**逐字节读**（`readLineRaw`：单字节 `input.read()` 累积到 `\n`，去掉尾部 `\r`；EOF 时若无内容返回 null）。刻意不用 `BufferedReader`——它会预读缓冲吃掉 body 字节。
- 请求行：跳过空行后按空格切分，`parts[0]=method`，`parts[1]=path`（含查询串原样）；`parts.size < 2` 直接静默关连接（无任何响应）。HTTP 版本不校验。
- 只解析 3 个请求头（前缀匹配大小写不敏感，同头重复时**最后一个值生效**）：
  - `Authorization:` → 取第一个 `:` 之后全部内容 trim；
  - `Content-Length:` → `toIntOrNull() ?: 0`；
  - `Transfer-Encoding:` 且值含 `chunked`（忽略大小写）→ `chunked=true`。
- 其余头（Host、Origin、Accept…）全部忽略，不做任何处理。**没有 Host 校验，没有 HTTP 100-continue，没有 URI 解码**。

### 1.4 路径归一化（决定路由匹配）

```
query = 原始path按'?'切前段
      → 连续多个 '/' 折叠为一个（正则 /{2,}）
      → 若长度>1 则去尾部 '/'（去空则退化为 "/"）
```
即 `/v1//models/` → `/v1/models`；`/` 保持 `/`；`/v1/` → `/v1`。不做大小写折叠、不做模糊匹配。

### 1.5 访问日志

- 每个请求一行 INFO：`→ $method $query auth=${authHeader!=null} len=$contentLength chunked=$chunked`。**只记录去查询串后的路径**（部分客户端把 key 放 `?key=`，禁止记录查询串）。
- `/v1/models` 记录响应模型数；404 记录 WARN（含方法与 query）。

### 1.6 CORS / keep-alive 行为

- **所有非 SSE 响应**（`writeRaw`）固定附带：
  ```
  Access-Control-Allow-Origin: *
  Access-Control-Allow-Methods: GET, POST, OPTIONS
  Access-Control-Allow-Headers: *
  Access-Control-Max-Age: 86400
  ```
- **SSE 流式响应头手写**，只带 `Access-Control-Allow-Origin: *` 与 `Access-Control-Allow-Headers: *`（无 Methods/Max-Age）。注释明确：流式头不走 writeRaw，CORS 头必须补写一份，否则 WebView 系客户端能拉模型但发不出对话。
- `OPTIONS`：最先分支、任何路径都答，`204 No Content` + 上述全套 CORS 头 + `Content-Length: 0`，空 body，**不看鉴权**。这是为 Chatbox（浏览器系）预检设计。
- **keep-alive：全部断开**。`writeRaw` 与 SSE 头都写 `Connection: close`；一个连接只处理一个请求；无 HTTP/1.0/1.1 差异处理；无 chunked **响应**编码（响应体只靠 Content-Length 或 EOF 界定长度）。

### 1.7 响应写出的精确格式（writeRaw / writeJson）

```
HTTP/1.1 <status> <reason>\r\n
Content-Type: <ct>\r\n
<CORS 4 个头逐行，每行以 \r\n 结束>
Content-Length: <utf8字节数>\r\n
Connection: close\r\n
\r\n
<body 原始 UTF-8 字节>
```
reason 映射：200→`OK`，204→`No Content`，401→`Unauthorized`，404→`Not Found`，**其余（含 413、502、400）→ `Error`**。`writeJson = writeRaw(status, "application/json", obj.toString())`。

### 1.8 请求体读取（两种传体）

- 带 `Content-Length`：`readBody(input, contentLength)` —— 按**字节**精确读满（循环 `read(bytes, off, len-off)` 直到 EOF）。`contentLength<=0` 返回空串；`> MAX_BODY_BYTES(8MiB)` 抛异常（上层捕获 → 413）。
- `Transfer-Encoding: chunked`（Chatbox 实测这样发）：`readChunkedBody` —— 逐字节读行；长度行取 `;` 前段按 16 进制解析；读该长度字节；吃掉后续 CRLF 行；`0` 块终止（trailer 不处理）；单块读不满（EOF）则截断返回已收部分；全部块拼进 `ByteArrayOutputStream` 后整体 UTF-8 解码。**分块长度是字节数**（自检钉住了中文按字节切块的用例）。
  - ⚠️ 已知缺口：chunked 路径**没有** MAX_BODY_BYTES 上限（`ByteArrayOutputStream` 无限增长）。Rust 移植时应统一加上累计上限并对超限回 413。

---

## 2. 路由行为总表

匹配顺序即 `when` 分支顺序（第一个命中生效）：

| # | 条件 | 行为 | 鉴权 |
|---|---|---|---|
| 1 | `method == OPTIONS`（任意路径） | `writeRaw(204,"text/plain","")`，仅状态码+CORS 头 | 无 |
| 2 | `query=="/healthz" && method==GET` | `200 text/plain`，body 为裸 `ok`（4 字节） | 无 |
| 3 | `query=="/v1/models" \|\| query=="/models"`（**不限制方法**，POST 也返回模型表） | `200 application/json`：`{"object":"list","data":[...]}`；data 来自 `modelsProvider`，异常/空回退 `ModelsRepo.current()`（未刷新时是**空数组**，属设计意图） | 免鉴权（注释：无敏感数据；聊天接口仍必须鉴权） |
| 4 | `(query=="/v1/chat/completions" \|\| query=="/chat/completions") && method==POST` | 见 §3 | Bearer（见下） |
| 5 | 其余 | `404 application/json`，WARN 日志 | — |

（`/v1/models`、`/chat/completions` 的"无前缀别名"是为 Chatbox 这类客户端设计，双斜杠/尾斜杠变体已由归一化覆盖。）

### 2.1 本地鉴权（与上游 token 是两套）

```kotlin
authorized(auth):
  if apiKey.isEmpty() -> true            // 实践中派生 key 恒非空
  prefix="Bearer "(7字符); auth 为 null 或前缀不匹配(忽略大小写) -> false
  auth.substring(7).trim() == apiKey     // 精确相等比较
```

401 响应（HTTP 401 + `writeJson`）：

```json
{"error":{"message":"missing or invalid local API key","type":"api_error","code":"invalid_api_key"}}
```

### 2.2 全部错误体形状（openAIError）

统一 `{"error":{"message":<msg>,"type":"api_error","code":<code>}}`；`type` 恒为 `"api_error"`；status 数字**不进 body**（代码注释明确只用于兼容 name/url）。

| 状态 | code | message | 触发点 |
|---|---|---|---|
| 401 | `invalid_api_key` | `missing or invalid local API key` | chat 路由鉴权失败 |
| 400 | `invalid_request_error` | `请求体不是合法 JSON` | `parseBodyOr400`（streamingChat / streamingQoderChat 入口） |
| 413 | `invalid_request_error` | `请求体过大（上限 8 MB）` | Content-Length 超限（`MAX_BODY_BYTES/1024/1024` 格式化进文案） |
| 502 | `upstream_unavailable` | `Qoder 账号不可用：jobToken 为空或已失效。请回 App 重新登录 Qoder，或在「聊天转发后端」里切到 Trae。` | Qoder 后端但 `qcred == null \|\| !chatReady`。**刻意不回假回复**（否则用户无法察觉根本没转发） |
| 502 | `upstream_error` | `上游转发失败` | `streamingChat`/`streamingQoderChat` 抛异常且尚未写出任何字节 |
| 502 | `upstream_error` | 上游真实错误文案（humanize 后） | 非流式分支上游失败（§3.5） |
| 404 | `not_found` | `unknown endpoint: $query` | 未知路径 |

注意：Trae 后端**无凭证时不报错**，`200 + fakeCompletion()`（见 §3.3）；Qoder 后端无凭证回 502 —— 两条链路语义不对称，移植时必须保持。

### 2.3 非法请求的其他处理

- 请求行不足 2 段 / 首行 EOF：静默关闭，无响应。
- body JSON 非法：chat 分支先照常按 model 路由（`JSONObject(body).optString("model","")` 解析失败按空字符串，**不致命**）；进入转发后 `parseBodyOr400` 写 400 并返回。路由阶段 body 若是非法 JSON 不会导致 400——400 只出现在流式转发函数内。
- `messages` 字段存在但不是数组：`prepareBody` 的 `getJSONArray` 抛 JSONException → 冒泡回 `handle` 的 catch → 502 JSON（若还没写流式头）。
- 客户端提前断开（Broken pipe）：`writeChunk` 吞 IOException 继续读上游并记录用量。
- 每连接兜底：`handle` 外层任何未捕获异常只记 WARN 并关 socket。

---

## 3. /v1/chat/completions 完整处理流程

### 3.1 主干流程（handle 内）

```
鉴权(401) → 读 body(chunked / content-length；超限413)
→ model = 宽松解析 body.model（失败为 ""）
→ backend = backendForModelProvider?.invoke(model)   // 异常吞掉→null
            ?: backendProvider?.invoke()              // 异常吞掉→null
            ?: "trae"
→ backend=="qoder" 分支:
    qcred = qoderCredProvider()（异常→null）
    qcred==null 或 !chatReady → 502 upstream_unavailable
    否则 streamingQoderChat(out, body, qcred)（异常→502 upstream_error）
    [然后 return]
→ trae 分支:
    fresh = credProvider()（异常→null）
    fresh==null 或 accessToken 空白 → 200 fakeCompletion()
    否则 streamingChat(out, body, fresh)（异常→502 upstream_error）
```

`fakeCompletion()` 200 body（结构真实、内容假的兜底响应）：

```json
{"id":"chatcmpl-fake-local","object":"chat.completion",
 "created":<now秒>,"model":"glm-5.2",
 "choices":[{"index":0,"message":{"role":"assistant","content":"这是本地假服务返回的测试响应。阶段2第2步已跑通：本地 OpenAI 兼容接口可用，下一步再接真实 TraeWork 上游转发。"},"finish_reason":"stop"}],
 "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}
```

### 3.2 后端路由语义（GatewayBackend.routeFor / resolved）

用户选择存 prefs `traework2api_prefs.gateway_backend` ∈ {`trae`,`qoder`,`auto`}（非法值按 `auto`）。

- `routeFor(model)`：显式选 `QODER`/`TRAE` 直接返回；`auto` 时：
  - 只有 Qoder 账号 → `qoder`；只有 Trae 账号 → `trae`；
  - 两边都有 → 模型名命中 **Trae 目录（id 忽略大小写）→ trae（同名 Trae 优先）**；命中 Qoder 目录（key/展示名/别名忽略大小写）→ `qoder`；都不命中 → `trae`；
  - 都没账号 → `trae`。
- `resolved()`（backendProvider）：`auto` 且"有 Qoder 无 Trae"→ `qoder`，否则 `trae`（展示/兜底用）。
- 模型表（modelsProvider）：`QODER`→`QoderModelCatalog.openAiModels()`；`TRAE`→`ModelsRepo.current()`；`auto` 两平台都有 → **并集**：Trae 表在前，各条复制并补 `owned_by: "trae"`，Qoder 独有项（按 id 去重）追加并补 `owned_by:"qoder"`（⚠️ 必须复制对象，不可污染共享缓存）；其余情况返回单表。

### 3.3 stream 与非 stream 分支的公共骨架（streamWithFailover）

`streamingChat`（Trae）与 `streamingQoderChat`（Qoder）都把控制流委托给同一个私有骨架函数：

```kotlin
private fun streamWithFailover(
    out, wantsStream, model, backend,
    usedUid: () -> String?,
    runUpstream: (emit: (String)->Unit, done: (JSONObject?, String?)->Unit) -> Unit,
    tryFailover: (err: String) -> Boolean,
    onRollback: () -> Unit,
    refreshSameAccount: ((err: String) -> Boolean)? = null,
)
```

执行逻辑（逐字节语义）：

1. **若 wantsStream**：先写流式响应头（无 Content-Length、无 Transfer-Encoding，纯 `Connection: close` + EOF 界定）：
   ```
   HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n
   Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nConnection: close\r\n\r\n
   ```  然后 flush。非流式**什么都不写**，`collected=.ArrayList` 攒帧。
2. `writeChunk(s)`：非流式→只 append 进 collected；流式→`out.write(s.toByteArray(UTF_8)); flush()`，IOException 吞掉。
3. while(true) 每轮（attempt 从 0 起）：
   - 局部 `pending=ArrayList`、`streaming=false`、`usage=null`、`err=null`，记 `t0`；
   - 调 `runUpstream(emit, done)`。emit 回调实现"**成功闸门**"：上游帧先入 pending；一旦某帧字符串包含 `"choices"` 即认定成功 → 置 `streaming=true` 并把 pending 全部 flush（流式实时转发 / 非流式入 collected）；
   - **同账号续期重试**（仅一次，`didRefresh`）：`!streaming && err!=null && !didRefresh && authRelatedError(err)` → `refreshSameAccount?(err)`；true 则 continue；
   - **换号重试**：`!refreshed && !streaming && err!=null && attempt<=1 && retryableUpstreamError(err)` → `tryFailover(err)`；true 则 continue（即最多 1 次续期 + 2 次换号，总上游尝试≤4）；
   - 均不重试 → 收尾：把 pending（此时正常只剩错误帧）flush → 调 `usageRecorder(model, usage, now-t0, err==null, err, backend, usedUid()?:"")` → 若 `attempt>0 && err!=null` 调 `onRollback()` → **非流式**：`extractUpstreamError(collected)`，有错误或 err≠null → `502 openAIError("upstream_error", upErr ?: humanizeUpstreamError(err))`，否则 `200 aggregateChunks(collected, model)` → return。流式则响应早已逐帧写完。
   - continue 时记日志"换号后重试（原错误: …）"。
4. ⚠️ 已知缺陷（Rust 版应修复并保持对外语义）：
   - `runUpstream` 本体（`TraeUpstream.chatStream` 的 OkHttp `execute()`）抛出的 IOException 不在 streamWithFailover 内捕获，会冒到 `handle` 的 catch；流式模式下响应头已发出，补写的 502 会变成 SSE 流里的垃圾字节。Qoder 的 `chatStream` 内部已捕获传输异常，Trae CN/Intl 未捕获。移植时应在 runUpstream 内把连接期异常转成错误事件（等价 `emitError` 帧）再 settle。
   - chunked 请求体无大小上限（§1.8）。

### 3.4 错误分类（换号/续期判定，静态纯函数）

- `authRelatedError(err)`：`queued:` 前缀 → false；取 `':'` 后最后段解析整数，`401|403` → true；解析不出 → false。**只有鉴权类错误才触发"同账号强制续期重试"**（避免 429/5xx/排队白付一次 ExchangeToken 往返）。
- `retryableUpstreamError(err)`：`queued:` 前缀 → true（排队按账号判定，换号有效；且是**立即**返回不 sleep）；取状态码 `401|403|429|>=500` → true；解析不出（网络异常、非 http 类）→ **保守 true**；400/404/422 等 → false（请求本身问题，换号白烧额度并隐藏真错误）。
- 错误码字符串约定（errType）：
  - Trae CN：`http:<status>`、`solo:<上游业务code>`、`interrupted`（流提前 EOF）；
  - Trae Intl：`intl:empty_user_message`、`intl_create:exception|<http码>|no_session`、`intl_commit:<码>|exception`、`intl_events:<码>|exception`；
  - Qoder：`body:<异常>`、`prepare:<异常>`、`transport:<异常名>`、`queued:<modelKey>`、`http:<status>`、`cosy:<流内错误消息>`、`exhausted`。

### 3.5 SSE 输出格式（对客户端，逐字节）

所有转发器通过 `out: (String) -> Unit` 回调交出**完整帧字符串**；网关原样转 UTF-8 字节写出并 flush。**每帧以 `\n\n` 结尾；前缀 `data:` 后固定一个空格**（例外见下）。**没有任何心跳/注释行/`event: keep-alive` 输出给客户端**；心跳只在上游读取侧被丢弃（Trae `SseState.scan` 对 `:` 开头行忽略；Qoder/intl 解析器忽略非 `event:`/`data:` 行）。

- 正文 chunk（Trae 生成）：
  `data: {"id":"chatcmpl-<毫秒时间戳>","object":"chat.completion.chunk","created":<秒>,"model":"","choices":[{"index":0,"delta":{...},"finish_reason":<仅末帧>}],\n["usage":{...} 仅 token_usage 之后的那一帧]}` + `\n\n`
  注意 `"model:""` 是**空字符串**（Trae CN/Intl 生成帧不填模型名；非流式聚合响应则用请求 model）。`delta` 只含有内容的键：`content`、`reasoning_content`、`tool_calls` 三者任意组合，全空则不发帧。
- 终止：`data: {...,"finish_reason":"stop"|"tool_calls","delta":{}}\n\n` 后紧跟 `data: [DONE]\n\n`。上游没发 done 就断流 → 幂等补 `data: [DONE]\n\n`（err=`interrupted`）。
- 错误帧（流式模式下客户端最终看到的错误形态——HTTP 状态仍是 200，因为头先发了）：
  - Trae CN HTTP 非 2xx：`event: error\ndata: "upstream http <code>: <body前200字符>"\n\n` + `data: [DONE]\n\n`（data 是 **JSON 字符串字面量**，`jsonEscape=JSONObject.quote`）。
  - Trae CN 业务 error 事件：`event: error\ndata: "solo error code=<n> msg=<s>"\n\n` + `data: [DONE]\n\n`。
  - Qoder：`emitError` 连发两帧：`event: error\ndata: "<消息>"\n\n` 与 `data: {"error":{"message":"<消息>","type":"api_error"}}\n\n`，其外层再补 `data: [DONE]\n\n`。**例外**：`transport:` 路径只有两帧错误、无 `[DONE]`。
  - 排队（Qoder）：消息文案 `模型「<modelKey>」当前不可用（上游资源受限，<N> 秒后可重试）。请换其它模型，或在账号页切换 Qoder 账号。`（N = retryAfterSeconds/waitTime×1000/retryAfterMs 中先命中的，floor 1s，兜底 2s）。
- Qoder 成功帧是**上游 OpenAI 格式 chunk 的原样透传**（信封拆开后 `body` 字符串），只做两处改写：ToolIndexer 重排 `delta.tool_calls[].index`；注入客户端模型名——快路径直接字符串拼接 `{"model":<clientModel的JSON引号串>,` + `原body去左花括号`（要求未改写、无 model 字段且以 `{` 开头），慢路径解析后 `put("model", clientModel)` 再序列化。
- 非流式（`stream:false`）：**绝不回 SSE**；全部帧入 collected，最后 `aggregateChunks` 聚合成标准 `chat.completion` JSON（200）或 502 错误 JSON。

### 3.6 aggregateChunks（SSE → 非流式 chat.completion 的聚合规则）

对每个收集帧按 `\n` 切行，只处理 trim 后以 `data:` 开头的行；payload trim 后空/`[DONE]` 跳过；非法 JSON 跳过：

| 目标字段 | 规则 |
|---|---|
| `id` | 第一个非空 `id`，缺省 `"chatcmpl-local"` |
| `created` | 第一个非 0 `created`，否则 `System.currentTimeMillis()/1000` |
| `message.content` | 所有帧 `delta.content` **逐帧拼接** |
| `message.reasoning_content` | `delta.reasoning_content` 逐帧拼接；空则**不写该键** |
| `message.tool_calls` | LinkedHashMap 按 `index`（缺省用数组下标）累积：`id` 首个非空；`function.name` 首个非空；`function.arguments` 逐帧字符串拼接；`type` 恒 `"function"`；有则写数组 |
| `finish_reason` | 最后一个非空且 ≠ 字面 `"null"` 的值，初值 `"stop"`；**若产出 tool_calls 且最终值仍是 stop → 改写为 `"tool_calls"`** |
| `usage` | 最后一个出现 `usage` 对象的帧 |
| 其余 | `object:"chat.completion"`、`model`（函数参数）、`choices:[{index:0,message,finish_reason}]`；有工具调用时 `content` 必须是字符串（可为空串） |

### 3.7 上游错误文本提取与人性化（非流式 502 的 message 来源）

`extractUpstreamError(collected)`：`data:` 行 payload 以 `"` 开头 → 当 JSON 字符串字面量解；否则解析对象取 `error`（String 直接用；JSONObject 取 `message`，退 `type`）；首个非空命中 → `humanizeUpstreamError`。humanize 规则（保留原文，追加 `\n\n` + 提示）：`Billing daily count exceeded` → Qoder 当日计费次数用完文案；`billing`+`exceed` → 额度用尽；`quota`|`insufficient` → 额度不足；`unauthorized`|`401` → jobToken 失效建议重登；否则原文。

### 3.8 usage 提取（供 usageRecorder）

- Trae：上游 SSE 事件 `token_usage` → `usage = 事件 data JSON`；`lastUsage` 在 `done` 时 settle 出去；同时 `pendingUsage` 会**挂到下一帧** chunk 的 `usage` 字段（发一帧后清空）。
- Qoder：每个透传 chunk 若含 `usage` 对象 → `onUsage`（chatStream 外层变量，最后写入者生效）。
- Intl：`Kind.USAGE` 存 `lastUsage`，done 时 `lastUsage ?: r.usage` settle。
- 消费方（Gateway 注入的 recorder）读 `optInt("prompt_tokens"/"completion_tokens"/"total_tokens", 0)`、耗时 ms、ok、errType，写 `UsageStore.record(context, model, prompt, completion, total, durationMs, ok, errorType)`（进程内 JSON 环形缓存，上限 `MAX=100` 条，超限丢最旧，同步落盘 SharedPreferences）。

### 3.9 Trae 链路细节（streamingChat → TraeUpstream）

- `wantsStream` **必须在 prepareBody 之前读**（prepareBody 会原地把 `stream` 强改为 true——上游只认流式）。
- `prepareBody`（Trae CN/Intl 共用，原地改写）：
  - `stream=true`、`function="solo_work_remote"`（新模型只挂这个 function；旧模型实测无回归）；
  - assistant 含 `tool_calls`：每项 `function` 改名 `function_call`；`function_call.name` 空 → 丢弃该项；全丢 → 移除 `tool_calls`；
  - `content` 是非 null 字符串 → 包装为 `[{"type":"text","text":<原串>}]`；
  - `model` trim 空 → `DeepSeek-V4-Flash`；不匹配 `^[A-Za-z0-9._-]{1,64}$`（**信任边界防 JSON 注入**）→ 回落默认并 WARN；`config_name` 与 `model` 写同值；
  - `normalizeToolChoice`：字符串 `none`（忽略大小写）→ 删 tool_choice 并**删 tools 与 functions**；对象 `none` 同上，`auto`/`required` → 归一小写字符串；`function` 型 → 提取函数名（`function.name` 优先，退回顶层 `name`）写成字符串，名字空 → `"auto"`；其他类型/未知 `type` → 删除该字段；
  - `normalizeTools`：每项 `function.parameters` 为对象 → **序列化成字符串**（上游要字符串化 schema）；空数组或全部非对象 → 删除 `tools`。
- HTTP 请求（OkHttp，connect 20s / read 180s）：`POST {agentHost}/api/agent/v3/llm_utils_chat`，头（`Builder.solo`）：
  `Accept: text/event-stream`、`Content-Type: application/json`、`User-Agent: Trae/0.1.43`、`Authorization: Cloud-IDE-JWT <token>`、`X-Cloudide-Token`/`X-Ide-Token` = token、`X-Uid: <uid>`（非空才带；**缺失时上游接受连接却不回数据**）、`X-App-Id: 6eefa01c-…`、`X-App-Version: default`、`X-Ide-Version: 0.1.43`、`X-Ide-Version-Code: 20260716`、`X-App-Version-Code: 20260716`、`X-Ide-Version-Type: stable`、`X-Device-Type: windows`、`X-OS-Version: Windows 11 Pro`、`X-Device-Brand: 83DG`、`Request-Traffic-Type: prod`。
- `chatStream` 入口处**快照 `TraeRegion.current` 一次**（区域可能被 UI 线程中途改写；单次请求可长达 180s，不快照会造成 CN token 发往 SG host 的区域串号）。非 CN → 整段委托 `TraeIntlUpstream.chatStream`。
- 上游 SSE 解析状态机（`SseState`）：`event:` 行覆盖事件名（trim）；`data:` 行**原样追加**（不 trim，可多行合并）；`:` 注释行忽略；空行闭合事件并分发。事件名与载荷：`output`（`response`→content；`reasoning_content` **仅当真是 JSON 字符串**才取——optString 曾把 null 变成字面量 `"null"` 挂进每帧，踩过坑；非空 `tool_calls` 过 `stripSoloTools`）、`token_usage`（整段 JSON 当 usage）、`done`（`finish_reason`；有工具调用则归一 `"tool_calls"`——上游带工具时也发 stop，Claude Code/Cursor 靠 finish_reason 触发工具执行）、`error`（`code` long + `message`）。载荷非法 JSON → 该事件当空丢弃。
- `stripSoloTools`：非数组单对象包装成数组；每项 `function_call`→`function`；删 `function.namespace`、`function.partial_arguments`；删空字符串的 `id`/`type`/`function.name`（OpenAI 分片语义：后续帧只有 index+arguments 片段）。
- 区域 host：CN `https://trae-api-cn.mchost.guru`（agent）/ `https://api.trae.cn`（ug）；SG agent `https://core-normal.trae.ai`、ug `https://ug-normal.trae.ai`；SG 已"冻结为未开发"（isSupported=仅 CN；影响换号候选选择，见 §5）。

### 3.10 Trae Intl 链路三段式（SG）

`POST core-normal.trae.ai/api/remote/v1/chat_sessions`（建会话，可带 tools；带 tools 被拒则**摘掉重试一次**，工具字段尽力而为）→ `POST /{sid}/commit`（title=用户文本前 80 字符；缺它模型不生成）→ `GET /{sid}/events?reply_to_message_id=<mid>`（Accept: text/event-stream）。会话 id 按 `sessionKey(prepared)` 缓存续聊（上限 `SESSIONS_MAX=32`）。事件流只有 `plan_item` 承载正文，且 thought/reasoning 是**累积快照**——解析器 `deltaOf(last, cur)` 只下发新增后缀（前缀匹配取差值，否则整段当新增）；工具调用上游不给增量（id 每帧变、params 恒 null），首次见到 `toolName` 时**整段下发一次**完整 tool_calls 并附 `content:""`；`done` 帧 finish_reason 同样按 sawToolCall 归一。错误 → `event: error` + `[DONE]`，errType `intl:<前60字符>`。空 user 消息直接 `intl:empty_user_message`。输出契约与 §3.5 相同（`data: …\n\n`、id=`chatcmpl-<ms>`、`model:""`）。

### 3.11 Qoder 链路细节（streamingQoderChat → QoderUpstream.chatStream）

- 入口需要 `appContext`，缺失直接 `IllegalStateException`；上游要求 `cred.chatReady`（uid 与 jobToken 均非空白）。
- `clientBody`（原始 OpenAI JSON，未改写）传进 `QoderUpstream.chatStream`；内部 `prepareBody` 抛异常 → 自行 emitError + [DONE] + err=`body:<异常>`（不冒泡）。
- 请求体构造要点（每条都是踩过的坑，移植必须保留）：
  - `system`/`developer` 角色消息合并进顶层 `system`（`\n\n` 连接），**同时**再放进 messages（上游实际读 messages[0]，只给顶层会被静默丢弃）；
  - 消息映射：`name`、`tool_call_id` 透传；非 tool 角色且 content 为非空字符串 → 并行发 `contents:[{type:text,text}]` 结构化副本；assistant 带 `tool_calls` → 保留原数组且 `content` 必须为字符串（null 会让上游解析失败，缺省补 `""`）；
  - `ensureCacheMarker`：全 messages 已有 `cache_control` 则不动；否则在**最后一条非 tool 消息的最后一个 text/image_url part** 上补 `{type:"ephemeral"}`（整段前缀进缓存，长对话不全价）；
  - 顶层常量字段：`stream:true`、`chat_task:"FREE_INPUT"`、`chat_context:{}`、`is_reply:true`、`is_retry:false`、`source:1`、`version:"3"`、`agent_id:"agent_common"`、`task_id:"common"`、`session_type:"qodercli"`、`aliyun_user_type:""`、`custom_model:null`；`request_id=request_set_id=chat_record_id=` 同一 UUID；`session_id=QoderModelCatalog.sessionFor(modelKey)`（按模型分开的进程内持久 id，`"q2a-"+随机 UUID 变形）；
  - `business`：`{product:"cli", version:COSY_VERSION, type:"agent", id:随机UUID, name:最后一条user文本前10字符, begin_at:当前ms, stage:"start"}`（部分模型缺 business 直接节点执行失败）；
  - `model_config`：目录解析（先 key、再展示名、再别名、未知回落 `auto`），发 `{key, format:"openai", source:"system", enable:true, display_name, is_vl:true, is_reasoning:<目录值>, price_factor:<目录真实倍率 ?: 1.0}`；
  - `parameters`：`max_tokens` = `max_completion_tokens`>0 ? 它 : `max_tokens`>0 ? 它 : **32000**；`reasoning_effort` 白名单归一（`minimal`→`low`；`none/low/medium/high/xhigh/max` 保留；其他丢弃）；`temperature`/`top_p` 可转 Double 才带；`stop` 字符串→单元素数组、空丢弃；`tool_choice` 原样透传；`tools` 数组原样（缺省 `[]`）。
- 传输：body 整体过 `QoderCosyCrypto.encodeBody`（自定义 64 字母表 `_doRTgHZ…` + pad `$`，加密细节不在本文范围，移植按原算法或保留同一实现）；`POST {QoderAuthApi.inferBase(mode)} + /algo/api/v2/service/pro/sse/agent_chat_generation + ?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`。签名头每次尝试现生成（防重放按 requestId+date）：`Authorization`（payload=base64(requestId+加密 userInfo)，key=runtime.key，unix 秒，参与签名的是 **ASCII 字符串形态的 encoded body**）、`Cosy-Date`、`Cosy-Key`、`Cosy-User=uid`、`Cosy-Business/Product/ClientType/Data-Policy/MachineOS/Scene/Version`、`Cosy-MachineId`（派生，见 QoderAuthStore.machineId）、`Login-Version: v2`、`X-Model-Key: <modelKey>`、`X-Model-Source: system`、`Accept: text/event-stream`、组织字段有值才带 `Cosy-Organization-Id/-Tags`（逗号连接）。刻意**不发** `User-Agent`、`Cosy-MachineToken`、`Cosy-MachineType`、`Connection`。运行时签名材料按 `uid|jobToken` 进程内缓存（>8 清空重建）。
- 尝试循环 `1..3`（MAX_ATTEMPTS）：
  - 传输异常（DNS/拒连/重置）：非末次 sleep **100ms** 重试；末次 emitError(`上游连接失败: <异常名>`, **无 DONE**) + err=`transport:<异常名>`；
  - HTTP 2xx：进入 SSE 信封读（`readEnvelopeSse`，见下）；流内错误 → emitError + DONE + err=`cosy:<msg>`；正常 → DONE + settle(usage)；
  - `401 && attempt==1 && refreshJobToken!=null` → 强制续期成功则 continue（续完还 401 不再续）；
  - `queueWaitMs(raw)`（**先于普通 retryable 判断**）：原文按字符串扫 `"isQueued":true` / `RESOURCE_LIMIT` / `"code":"10605"` / `10605`（真 403 排队信息藏在**多层转义 JSON 字符串**里，顶层无 queue 字段；同时兼容规规矩矩的 `queue:{isQueued:true,waitTime}` 形状）→ 抠 `retryAfterSeconds`×1000 / `waitTime`×1000 / `retryAfterMs`，floor 1000ms，兜底 2000ms；命中 → emitError(排队文案) + DONE + err=`queued:<modelKey>`，**立即返回不重试不 sleep 不降级模型**（排队按账号判，交给上层 streamWithFailover 换号）；
  - `408|429|>=500` 且非末次 → sleep `min(Retry-After秒×1000 ?: 2^(attempt-1)×1000, MAX_RETRY_WAIT_MS=2000)` 重试；
  - 其余/末次 → emitError(`上游 HTTP <code>: <body前200>`) + DONE + err=`http:<code>`。
- 信封 SSE 解析（`readEnvelopeSse`）：`event:` 多段以空格拼接；`data:` 只剥**一个**前导空格、多行以 `\n` 合并；空行 flush 一帧；`event == "finish"` → 终止（返回 null 错误）；`data` 信封 `{statusCodeValue, body}`：`statusCodeValue ∉ {0,200}` → 错误取 `JSON(body).message`，退 `"upstream error status <n>"`；`body` 解析为 chunk，含 `error.message` → 错误；含 `usage` → 上报；ToolIndexer 重排工具索引后透传；EOF 未 finish 兜底 flush 一次。首帧记录"首字 X ms"日志。
- `ToolIndexer`：上游并行工具调用全用 `index:0`、靠新 `id` 区分；本类遇到非空 `id` 就把计数器 +1（首个 id 保持 0），把所有 tool_call 的 `index` 改写为当前计数（客户端按 index 累加才不会合并不同工具）。

---

## 4. provider 回调接口全表（LocalFakeServer 的 @Volatile 挂点）

全部字段是 `@Volatile var`，由 `Gateway.start` 注入；除 `appContext` 外均为可空 lambda。签名与语义：

| 字段 | Kotlin 签名（Rust 等价 `Fn`） | 调用时机 | 失败/空语义 |
|---|---|---|---|
| `credProvider` | `() -> TraeAuthStore.Cred?` | **每次 Trae 转发前**（不只启动快照——服务长驻、token 14 天会过期） | 返回 null 或 accessToken 空白 → 不转发，`200 fakeCompletion()`；**不回退旧快照**。抛异常被吞→等价 null |
| `usageRecorder` | `(model:String, usage:JSONObject?, elapsedMs:Long, ok:Boolean, errType:String?, backend:String, uid:String) -> Unit` | 每次转发循环收尾（成功/彻底失败都调；含换号/续期后的最终结果），连接线程上执行 | Gateway 实现：`UsageStore.record(…optInt 三键 ?:0…)` + `CreditMonitor.noteForwarded(app, backend, uid, urgent=!ok)`（成功按 60s 节流、失败**跳过节流立刻判定换号**；只查本次实际用到的平台+账号，无定时轮询） |
| `failoverProvider` | `(failedUid:String, err:String) -> TraeAuthStore.Cred?` | 上游**尚未输出任何含 `"choices"` 的帧**就失败、且 `retryableUpstreamError(err)`、attempt≤1 时 | 返回 null = 无可换账号 → 原错误报给客户端。返回的 cred 必须 accessToken 非空且 uid ≠ 当前 uid 才生效（LocalFakeServer 侧 `takeIf` 过滤）。Gateway 实现是"乐观切换"：真的 `setActive(next)` + `syncRegionToActiveAccount`，再 `restartWithValidToken(skewSec=3600)` 取新 token；候选由 `TraeAuthStore.pickFailoverCandidate`（排除失败 uid，优先受支持区域 CN，单账号→null） |
| `failoverRollback` | `(originalUid:String) -> Unit` | **换过号（attempt>0）且最终仍失败**时回调，把当前账号切回用户原选 | 换号成功时**不调**（新账号正是想要的结果）。Gateway 实现：`setActive(original)` + 区域同步。防止账号在请求间来回跳 |
| `modelsProvider` | `() -> JSONArray` | `GET /v1/models`（及模型页） | 抛异常→退 `staticModels()`=`ModelsRepo.current()`（未刷新=空数组，合法状态） |
| `backendProvider` | `() -> String`（`"trae"`/`"qoder"`） | chat 路由中 `backendForModelProvider` 返回 null/异常后的**兜底** | 抛异常→再退常量 `GatewayBackend.TRAE` |
| `backendForModelProvider` | `(model:String) -> String` | chat 路由**最先**调用（`auto` 时按模型名决定上游；两平台都有账号时模型表是并集） | null/异常 → 退 `backendProvider`。Gateway 实现=`GatewayBackend.routeFor` |
| `qoderCredProvider` | `() -> QoderAuthStore.QCred?` | 每次 Qoder 转发前 | null 或 `!chatReady` → `502 upstream_unavailable`（**绝不回假回复**——会让"根本没转发"伪装成"转发成功答案假"）。Gateway 实现：`lockedQoder { load(); validJobTokenFor(app, cred, SIXTY_MINUTES_MS) ?: cred.jobToken; 空白→null }` |
| `qoderFailoverProvider` | `(failedUid, err) -> QoderAuthStore.QCred?` | 同 `failoverProvider`，Qoder 侧 | 需 `chatReady && uid≠cur.uid`。Gateway 实现：`list().firstOrNull { uid != failedUid }`（无区域概念），`lockedQoder { setActive; load; validJobTokenFor; 空白→null }` |
| `qoderRollback` | `(originalUid) -> Unit` | 同 `failoverRollback`，Qoder 侧 | `lockedQoder { QoderAuthStore.setActive(original) }` |
| `appContext` | `android.content.Context?` | Qoder 转发入口与 Trae 续期回调中取（机器标识/续 token 需要 Context） | null：Trae `refreshSameAccount` 直接返回 false（不救）；Qoder 转发抛 `IllegalStateException` |
| （Trae 续期专用闭包）`refreshSameAccount` | `(err) -> Boolean`，由 `streamingChat` 注入 | **401/403 时（authRelatedError）先于换号**，在同一账号上强制续 token 重试一次 | Gateway 实现：`Gateway.locked { TraeAuthApi.forceRefreshTokenFor(ctx, cur) }`，拿到即 `cur=cur.copy(accessToken=t)`，续不动 false→轮到换号。防"token 未到期被判废"时白白切走账号 |

凭证数据结构：

```
TraeAuthStore.Cred { accessToken, refreshToken, expiresAt(unix 秒; 0=未知), uid, nickname, regionId(默认"cn") }
QoderAuthStore.QCred { pat, jobToken, jobRefreshToken, expiresAt(unix 毫秒), uid, name, email, mode,
                       organizationId, organizationName, organizationTags:List<String>,
                       dataPolicyAgreed, isDataPolicyModifiable }
  chatReady = uid.isNotBlank() && jobToken.isNotBlank()
```

---

## 5. Gateway.kt（业务持有者）

`object Gateway`（进程级单例）。只持有 `@Volatile server: LocalFakeServer?`；`isRunning = server?.isRunning == true`；`start`/`stop` 标 `@Synchronized`。

### 5.1 锁模型

| 锁 | 对象 | 保护范围 | 关键点 |
|---|---|---|---|
| `tokenLock = Any()`（对外经 `fun <T> locked(block:()->T): T = synchronized(tokenLock){…}`） | Trae：credProvider、failoverProvider、failoverRollback、streamingChat 的 forceRefresh、MainActivity 登录后主动续期、积分/签到自愈续期 | **refresh token 是轮换的**：并发续期会让先换到的失效；"取凭证+续期"必须一段串行。synchronized 可重入，嵌套包裹安全 | |
| `qoderLock = Any()`（`fun <T> lockedQoder`） | Qoder 全链路（jobToken 同样轮换） | **必须与 Trae 锁分开**：两套 token 两套上游，共用一把时一个后端的续期/慢转发会把另一个后端整个卡住 | |

### 5.2 60 分钟阈值（单位陷阱）

- Trae：`SIXTY_MINUTES_SEC = 3600L`（**秒**），传给 `restartWithValidToken(ctx, skewSec)`；判据 `expiredWithin`：`expiresAt<=0 || now/1000 + skew >= expiresAt`（0/负=未知，一律视为需续）。
- Qoder：`SIXTY_MINUTES_MS = 60*60*1000L = 3_600_000L`（**毫秒**），传给 `validJobTokenFor(cred, skewMillis)`；判据 `jobToken 非空 && now + skew < expiresAt`。
- 注释钉住的历史 bug：Qoder 曾误用 3600（=3.6 秒），等于永远拿旧 token 撞 401，续期形同虚设。移植时两处的单位必须分别用 `Duration` 类型表达。

### 5.3 本地 API Key 派生（sk- 算法）

```
apiKey(ctx):
  读 prefs("traework2api_prefs") 若还有旧键 "local_api_key" → 删除（一次性清残值）
  return derivedKey(ctx)   // 进程内 @Volatile derivedCache 缓存

derivedKey:
  deviceId = widevineDeviceIdHex()                       // MediaDrm(WIDEVINE_UUID).getPropertyByteArray("deviceUniqueId") → 每字节 "%02x" 小写拼接
           ?: Settings.Secure.ANDROID_ID ?: "noaid"      // 无 Widevine(模拟器)才退化；恢复出厂会变
  input    = "${signingFingerprint(ctx) ?: "nosig"}|$deviceId"     // '|' 单字符分隔，无空格无转义
  key      = "sk-" + sha256Hex(utf8(input)).take(32)              // QoderMachine.sha256Hex：标准 SHA-256、小写 hex、全长 64 字符；取前 32 个 hex 字符(128 bit)
  derivedCache = key
```

- `signingFingerprint` = SHA-256(APK 签名证书字节).hex（API 28± 两条取证书路径；拿不到 → null）。设计意图：同设备+同签名永远同 key（覆盖安装/重装/清数据/恢复出厂都不变），不同签名包不共享。**Widevine UUID 以源码字面量为准**：`UUID(-0x121074568629b532L, -0x5c37d8232ae2e13L)`（源码行内注释写作 `edef8ba9-79d6-4acea-a3c8-27dcd54d21ed`；公开标准值通常写作 `edef8ba9-79d6-4ace-a3c8-27dcd51d21ed`，两处注释与字面量存在出入 —— Rust 侧直接构造同一 128-bit 值即可，别抄注释）。
- **桌面移植替代**：无 MediaDrm/PackageManager。建议 `sk-` + sha256(稳定机器身份 || 安装标识) 前 32 hex，机器身份用 OS 级稳定 id（如 `/etc/machine-id`、IOKit 序列号、注册表 MachineGuid——crate `machine-uid`/`machineid`），或首启生成 UUID 持久化到 app config dir；保持"同机同装恒稳、派生确定性、不落盘 key"三条性质即可。
- 自检 `Gateway.selfCheck(ctx)`（MainActivity.onCreate 注册）：`a 非空 && startsWith("sk-")`；`a == apiKey(ctx)`（钉死"每次取值都变导致全客户端 401"的回归）；`a.length == 35`（`sk-` + 32）。

### 5.4 Gateway.start(ctx) 装配清单（幂等；已在跑直接 return——重复 start 会 EADDRINUSE 崩掉）

1. 取 `app = ctx.applicationContext`（一律只用 applicationContext；捕获 Activity 会常驻泄漏）。
2. `LocalFakeServer(apiKey(app))`；按 §4 注入全部 provider（其中 qoderCredProvider / rollback 等均包在 `lockedQoder` 里，Trae 侧包 `locked`）。
3. `server = s; s.start()`；日志"网关已启动，端口 8787"。
4. `Gateway.stop()`：`CreditMonitor.stop()`（现为空实现）→ `server.stop()` → 置 null，幂等。
5. **不在启动时拉积分监测轮询线程**（历史 bug：2 秒轮询把上游打废 token）；唯一触发点是转发完成回调（§4 usageRecorder）。

### 5.5 LocalFakeServer.selfCheck()（MainActivity 启动时跑、失败记名单提示不崩）

钉住的回归（Rust 版应做成 cargo test / 启动 assert 等价物）：

1. 中文 body 按**字节**读满（`readBody` 用例：bytes.size > json.length 断言 + 回读一致）；
2. `retryableUpstreamError` / `authRelatedError` 的 12 条边界（401 换号+续期、429 换号不续期、5xx 换号不续期、`queued:*` 换号但不续期、非 http 类保守换号、400/404/422 不换号）；
3. chunked 解析：`5\r\n{"a":\r\n9\r\n…"你好"}…` 拼接正确；3 字节中文按**字节**切块不被劈开；
4. aggregateChunks：逐帧拼 content、reasoning_content 聚合、finish_reason 取最后非空、usage 带出、id 保留；
5. 工具调用聚合：name 在首帧 + arguments 分片合并为 1 条；有 tool_calls 时 content 键必须存在。

---

## 6. GatewayService.kt：Android 生命周期绑定清单 → 桌面替代方案

它是"保活外壳"：前台服务 + 通知 + 唤醒锁 + 看门狗闹钟 + 用户意图持久化。逐块判定"可丢弃/需等价"：

| Android 机制（代码位置） | 功能语义 | 桌面版处置 |
|---|---|---|
| `Service` 本体、`onBind`=null、`startForegroundService`（API26 分支） | 让进程独立于 Activity 存活 | **可丢弃**。Tauri 主进程本身长驻；网关作为进程内 tokio 服务随应用生命周期存在 |
| 通知渠道 `notification()`/`createChannel()`：CHANNEL_ID=`"traework2api_gateway"`、NOTIFICATION_ID=`8788`、IMPORTANCE_DEFAULT、无徽标/振动/声音、锁屏公开、`CATEGORY_SERVICE`、`setOngoing(true)`、API31 `FOREGROUND_SERVICE_IMMEDIATE`、小图标取 `ic_launcher_foreground`（白剪影）、标题"TraeWork2API 网关"、正文"本地服务运行中 · 端口 8787"、点开=MainActivity、action=停止(PendingIntent ACTION_STOP) | 可见性证明 + 快捷停止入口 | **功能需等价（形态替换）**：系统托盘（Tauri `tray-icon`）：tooltip 显示端口、右键菜单"停止网关/退出"；通知插件仅用于错误告警。渠道概念整体丢弃 |
| `PARTIAL_WAKE_LOCK`，tag `traework2api:gateway-wakelock`，**非计数**，超时 `WAKELOCK_TIMEOUT_MS=10min`，`onStartCommand`/每次唤醒再 acquire（isHeld 才跳过），onDestroy release | 防 Cached App Freezer 冻结（端口在听但进程不被调度）；带超时防被判异常耗电 | **基本可丢弃**（桌面不会冻结后台服务）。若用户要求"合盖仍服务"再挂 keep-awake（如 `rustup` 生态 `caffeinate`/`powercfg` 等价物，或 tauri-plugin）。逐项标注：默认不做 |
| `START_STICKY` 返回值；用户主动停止路径返回 `START_NOT_STICKY` | 被系统回收后自动重建 | **可丢弃**（单进程模型无"被回收再重建"问题）；保留其语义等价：崩溃恢复交给 systemd/launchtools 级别，不内置 |
| `stopWithTask=false`（manifest） + `onTaskRemoved`：划掉最近任务尝试 re-start，被 Android 12+ 后台限制拦住则挂 **5 秒精确闹钟**兜底 | 划掉任务不销毁 | **可丢弃**。桌面关窗≠退进程：Tauri 配 `onWindowEvent close → 隐藏到托盘`即可等价"窗口关了端口还在" |
| AlarmManager 看门狗：`WATCHDOG_INTERVAL_MS=15min`、请求码 `8789`（看门狗）/`8790`（划掉兜底）、`setExactAndAllowWhileIdle(ELAPSED_REALTIME_WAKEUP)`、API31 `canScheduleExactAlarms` 预检、失败降级 `setAndAllowWhileIdle`（Doze 可能延迟数小时）、`FLAG_IMMUTABLE\|FLAG_UPDATE_CURRENT`、每次进入 `onStartCommand` 都续期、`ACTION_WATCHDOG` 到达但 `!shouldRun` → `cancelWatchdog + START_NOT_STICKY`（处理"在途闹钟拦不住"竞态） | 国产 ROM 强杀后拉回 | **整体可丢弃**（不存在省电强杀）。若做"崩溃自动重启网关"用进程内 supervisor + tokio 任务重启即可；**禁止**把它设计成平台级保活定时器（会被审查视为滥用环境） |
| 用户意图持久化：prefs `traework2api_service` / `should_run`(默认 false)/`hasUserIntent`(contains 判定)；`markShouldRun(true)` 在任何启动路径（含看门狗），`stopNow()` 置 false+撤看门狗+stopForeground；0.4.8 语义：**"从没碰过 → 进 App 默认开；主动停过 → 尊重关闭"**（MainActivity.autoStartServiceIfWanted：`running/isRunning` 已跑→return；`hasUserIntent && !shouldRun`→return；否则 start） | 区分"用户关了"vs"被系统杀了"，防"关了又自己开" | **功能需等价**：Rust `AppState{ autostart_intent: Option<bool> }` 存 config 文件；三态语义（None=从未表达→应用打开即启动；false→保持停止；true→启动）原样保留 |
| `isChannelDisabled`（渠道 IMPORTANCE_NONE 检测，供 UI 引导） | 通知被关 → 服务被弱化清理的盲区自检 | **可丢弃**（无通知渠道概念） |
| 电池优化白名单引导、保活状态自检面板（MainActivity 侧，不在本文件） | 厂商白名单正门 | **可丢弃**；桌面等价物只有"开机自启"选项（见下） |
| 刻意**不做**开机自启：manifest 无 `RECEIVE_BOOT_COMPLETED`/BootReceiver；文档注释"重启后必须手动开" | 用户明确要求 | **等价保留该决策**：Tauri 默认不启用 `tauri-plugin-autostart`；若未来开启必须显式配置开关且默认 off，尊重该用户意图历史 |
| `running` @Volatile 内存态（UI 状态显示用）+ `GatewayService.start(ctx)/stop(ctx)` 静态门面 + `ACTION_STOP="com.example.traework2api.STOP"`、`ACTION_WATCHDOG="…WATCHDOG"` | 服务状态查询与命令 | **需等价**：Rust `GatewayController { start(), stop(), is_running() }` + Tauri command `gateway_status/start/stop`；ACTION 常量整体删除 |

---

## 7. 魔法常量总表

### 7.1 端口/超时/尺寸（LocalFakeServer & 传输）

| 常量 | 值 | 单位/含义 |
|---|---|---|
| `PORT` | 8787 | 监听 TCP 端口 |
| backlog | 50 | accept 队列 |
| `soTimeout` | 15000 | ms，连接读超时 |
| `MAX_BODY_BYTES` | 8*1024*1024 | 单请求体上限（安全边界：防 Content-Length 谎报 → 2GB 分配 OOMError） |
| OkHttp connect / read（Trae 与 Qoder client） | 20s / 180s | 上游超时 |
| `CORS Max-Age` | 86400 | 秒 |
| `Authorization prefix` | `"Bearer "`（7 字符） | |
| fakeCompletion `id`/`model` | `"chatcmpl-fake-local"`/`"glm-5.2"` | |
| 流式成功闸门特征串 | `'"choices"'`（含引号子串匹配） | |
| 日志前缀方向符 | `→` / `←` | |

### 7.2 阈值/节流/周期（Gateway / CreditMonitor / UsageStore）

| 常量 | 值 | 含义 |
|---|---|---|
| `SIXTY_MINUTES_SEC` | 3600（**秒**） | Trae token 续期提前量 |
| `SIXTY_MINUTES_MS` | 3_600_000（**毫秒**） | Qoder jobToken 续期提前量（units 陷阱历史 bug：曾错填 3600） |
| CreditMonitor 节流 `FORWARD_POLL_MIN_MS` | 60s（内部节流，注释） | 成功后刷新节流；失败 urgent=true 跳过 |
| UsageStore `MAX` | 100 条 | 用量环形缓存上限 |
| runtime 签名材料缓存上限 | 8 项（`runtimeCache.size > 8` 清空） | |
| Intl 会话缓存 `SESSIONS_MAX` | 32 | |

### 7.3 QoderUpstream

| 常量 | 值 |
|---|---|
| `MAX_ATTEMPTS` | 3 |
| `MAX_RETRY_WAIT_MS` | 2000（曾照抄参考实现 30s → 排队白等 90s 的"转发变慢"主因） |
| `DEFAULT_MAX_TOKENS` | 32_000 |
| 传输抖动 sleep | 100 ms |
| 指数退避基准 | `2^(attempt-1) * 1000ms`（cap 2000） |
| `Retry-After` | 只认秒（×1000） |
| 排队识别串 | `"isQueued":true`、`RESOURCE_LIMIT`、`10605` |
| 排队等待兜底 | floor 1000ms；默认 2000ms |
| HTTP 2xx 白名单 status | `statusCodeValue ∈ {0, 200}` |
| 错误截断 | body 取前 200 字符 |
| NATIVE_INFER_PATH | `/algo/api/v2/service/pro/sse/agent_chat_generation` |
| 签名路径（不含 /algo） | `/api/v2/service/pro/sse/agent_chat_generation` |
| NATIVE_INFER_QUERY | `?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1` |
| 模型目录 | `/algo/api/v2/model/list`（签名路径 `/api/v2/model/list`） |
| COSY_VERSION | 聊天链路（authorizationPayload、Cosy-Version 头、business.version）= `QoderCosyCrypto.COSY_VERSION` = `"1.1.74"`；QoderAuthApi 机器身份/登录头另用 `QoderMachine.COSY_VERSION` = `"1.1.69"`（两套版本共存，别混用） |
| SCENE 头 | ClientType=`5`，BusinessProduct=`cli`，BusinessType=`agent`，Scene=`assistant`；`Login-Version: v2`；`X-Model-Source: system`；encodeBody 字母表 `_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!` pad `$` |
| body 固定字段 | chat_task=`FREE_INPUT`，agent_id=`agent_common`，task_id=`common`，session_type=`qodercli`，version=`3`，source=`1`，is_reply=`true`，is_retry=`false`，model_config.format=`openai`/source=`system`/is_vl=`true`，session_id 前缀 `q2a-`，price_factor 缺省 `1.0`，business.name 取文本前 10 字符 |
| `DEFAULT_KEY`（回落模型） | `"auto"` |
| effort 白名单 | `none/low/medium/high/xhigh/max`（`minimal`→`low`） |

### 7.4 TraeUpstream

| 常量 | 值 |
|---|---|
| EP_CHAT | `/api/agent/v3/llm_utils_chat`；EP_MODELS `/api/ide/v1/get_detail_param`；EP_CHECKIN_STATUS `/trae/api/v2/ug/checkin_credits/status`；EP_CLAIM `/trae/api/v2/ug/checkin_credits/claim`；EP_ENT_USAGE `/trae/api/v2/pay/ide_user_ent_usage` |
| APP_ID | `6eefa01c-1036-4c7e-9ca5-d891f63bfcd8` |
| IDE_VERSION / IDE_VERSION_CODE | `0.1.43` / `20260716` |
| function | `solo_work_remote`（新模型只挂它；老 lite 已全局弃用） |
| DEFAULT_MODE | `DeepSeek-V4-Flash`（便宜，用户要求不默认贵模型） |
| 模型名正则 | `^[A-Za-z0-9._-]{1,64}$` |
| 头集合 | `Authorization: Cloud-IDE-JWT <token>`；`X-Cloudide-Token`、`X-Ide-Token` = **裸 token**（无 scheme 前缀）；UA `Trae/0.1.43`；X-App-Version `default`；X-Ide-Version-Type `stable`；X-Device-Type `windows`；X-OS-Version `Windows 11 Pro`；X-Device-Brand `83DG`；Request-Traffic-Type `prod`；X-Uid 非空才带 |
| ug 头 | UA `VSCode 1.107.1 (TRAE SOLO CN)`、X-Market-Client-Id `VSCode 1.107.1`、X-Lgw-Req-Sdk-Type `3`、Package-Type `stable_cn`、X-Lscbd-Aid `787976`、X-Lscbd-Platform `windows`、App-Version `0.1.45`、Sec-Fetch-* 三件套、X-User-Region（region 值，空→CN） |
| 设备身份派生 | seed=uid；salt `devid`/`sess`/`market`；SHA-256(`salt:seed` ‖ 计数器大端4B) 流；device_id=15 位纯数字（字节 %10）；sessionid=64 hex；market=UUIDv4（修 version/variant 位）；自检向量：`deviceIdFor("1234567890123456")=="413174708280782"` 等 |
| 鉴权业务码 | HTTP 401/403 或 `code==1001` → lastAuthFailed；context_length 缺省 `131072`；created 缺省 `1753600000`；chunk id=`chatcmpl-<sysTimeMillis>` |
| isChatModel 判据 | `config_source==1` && `!display_config.is_custom_model` && `!is_invisible_to_user` && display_config 存在 && 名字不含黑名单词（`custom_model*` 前缀；`agent/subagent/summary/sagitta/aquila/file_search/explore_/computer_use/browser_use` 包含） |
| 上游错误截断 | body 取前 200；Intl errType 取前 60；commit title 前 80 |
| 签到业务码 | 成功合取 `code==0 && after.checkedIn && !before.checkedIn`；`9004`=参数缺失/今天已签歧义码；`9074`=共用设备 |
| Intl | HOST `https://core-normal.trae.ai`；EP_SESSIONS `/api/remote/v1/chat_sessions`；NO_SESSION 哨兵 `"\u0000NO_SESSION"`；三段式 create→commit→events |

### 7.5 Region/存储/prefs

| 键 | 值 |
|---|---|
| CN agent/ug/oauth/console/clientId | `trae-api-cn.mchost.guru` / `api.trae.cn` / `api.trae.com.cn` / `www.trae.cn` / `en1oxy7wnw8j9n` |
| SG agent/ug/oauth/console/clientId | `core-normal.trae.ai` / `ug-normal.trae.ai` / `ug-normal.trae.ai` / `www.trae.ai` / `ono9krqynydwx5` |
| prefs 文件 | `traework2api_prefs`（`local_api_key`（读即删残值）、`trae_region`、`gateway_backend`）；`traework2api_service`（`should_run`）；`traework2api_models`（`models_json`，仅非空表落盘） |
| 模型表持久化规则 | 空表**拒绝覆盖**已有；未刷新 `current()=[]` 空数组 |

### 7.6 GatewayService（Android 侧，多数在桌面版废弃）

`CHANNEL_ID="traework2api_gateway"`；`NOTIFICATION_ID=8788`；`WAKELOCK_TAG="traework2api:gateway-wakelock"`；`WATCHDOG_INTERVAL_MS=15*60_000`；`WAKELOCK_TIMEOUT_MS=10*60_000`；`WATCHDOG_REQUEST=8789`；`TASK_REMOVED_REQUEST=8790`；划掉兜底闹钟 delay `5_000ms`；`ACTION_STOP/ACTION_WATCHDOG="com.example.traework2api.STOP"/".WATCHDOG"`；渠道 IMPORTANCE_DEFAULT（低于 DEFAULT 会被 ROM 降级服务优先级）。

---

## 8. Rust（Tauri 桌面版）crate 选型与模块 API 草案

### 8.1 crate 选型

| 用途 | 选型 | 理由（对应原行为） |
|---|---|---|
| HTTP 服务端 | `axum 0.8` + `tokio`（hyper 底层） | 路由表即 §2；SSE 手写帧（**不要用 axum extract 层的自动编码**，需要逐帧 `data: …\n\n` 精确字节 + 每帧 flush，`TcpStream` set_nodelay / set_read_timeout 用 `hyper_util` 或 tower 层暴露） |
| 上游 HTTP client | `reqwest 0.12`（`stream` feature） | 连接 20s / 读 180s、`bytes_stream()` 手搓 SSE 状态机 |
| 序列化 | `serde_json`（`preserve_order` feature 可选） | 帧字段顺序稳定化，便于逐字节测试 |
| JSON 宽松访问 | `serde_json::Value` + 手写 opt_* 扩展（`get_str/get_int默认值`） | 复刻 org.json 的 optString/optInt "缺字段不抛"语义 |
| SHA-256/hex | `sha2` + `hex` | sk 派生、设备身份派生 |
| UUID | `uuid` v4 | request_id 等 |
| 稳定机器身份 | `machine-uid`（回退：首启生成 UUID 存入 config dir） | §5.3 桌面替代 |
| 托盘 | `tauri 2` `tray-icon` + menu | §6 通知/停止按钮等价 |
| 配置存储 | `tauri-plugin-store` 或直接 JSON 文件 | prefs 等价（should_run 三态、后端选择、区域、模型表缓存） |
| 错误/日志 | `anyhow`/`thiserror` + `tracing` | 访问日志（**脱敏查询串**，§1.5） |
| 测试 | `tokio::test` + 自测模块 | §5.5 selfCheck 全部转成单元测试 |

### 8.2 模块划分与函数级 API 草案

```rust
// ==== crate: gw_server（对应 LocalFakeServer.kt）====
pub struct ServerHandle { /* running: AtomicBool, shutdown_tx */ }
pub async fn start(cfg: ServerConfig) -> Result<ServerHandle>;   // 幂等；bind 0.0.0.0:8787, backlog 50
pub async fn stop(&mut self);                                    // 不 join 存量连接，与原逻辑一致

pub struct ServerConfig {
    pub api_key: String,                       // Gateway.api_key()
    pub providers: Arc<ProviderSet>,           // ↓ 全部 11 个回调
    pub max_body_bytes: usize,                 // 8 MiB（补齐到 chunked 路径）
}

// 请求级管线
async fn handle_conn(sock: TcpStream, cfg);    // so_timeout 15s、nodelay、read_line_raw 语义用逐字节读取或限流 BufReader+position 复原
fn  normalize_path(p: &str) -> Cow<str>;       // 折叠 //、去尾 /（root 保留）
fn  authorized(key: &str, header: Option<&HeaderValue>) -> bool;

// 帧与聚合（纯函数，全部有单测 = 原 selfCheck 的移植）
pub fn data_frame(payload: &str) -> Vec<u8>;              // b"data: " + payload + b"\n\n"
pub const DONE_FRAME: &[u8] = b"data: [DONE]\n\n";
pub fn error_event(msg: &str) -> Vec<u8>;                 // "event: error\ndata: "<json串>"\n\n"
pub fn chunk_json(id, created, model, delta, finish, usage) -> String;
pub async fn read_body(cl: Option<u64>, chunked: bool, max: usize) -> Result<String, BodyErr>; // 超限 Err→413
pub fn aggregate_chunks(frames: &[String], model: &str) -> Value;
pub fn extract_upstream_error(frames: &[String]) -> Option<String>;
pub fn humanize_upstream_error(raw: &str) -> String;
pub fn openai_error(code: &str, msg: &str) -> Value;      // {"error":{message,type:"api_error",code}}

// ==== provider 回调 trait（§4 一一映射；均返回 Result/Option）====
pub struct ProviderSet {
    pub trae_cred:    Box<dyn (Fn() -> Option<TraeCred>) + Send + Sync>,
    pub usage_rec:    Box<dyn (Fn(&UsageRecord) -> () ) + Send + Sync>,   // record{model,usage,ms,ok,err,backend,uid}
    pub trae_failover:Box<dyn (Fn(&str,&ErrKind)->Option<TraeCred>) + Send + Sync>,
    pub trae_rollback:Box<dyn (Fn(&str)->()) + Send + Sync>,
    pub models:       Box<dyn (Fn()->Vec<Value>) + Send + Sync>,
    pub backend:      Box<dyn (Fn()->Backend) + Send + Sync>,
    pub backend_for:  Box<dyn (Fn(&str)->Option<Backend>) + Send + Sync>,
    pub qoder_cred:   Box<dyn (Fn()->Option<QoderCred>) + Send + Sync>,
    pub qoder_failover: …, pub qoder_rollback: …,
    pub app: Arc<AppPaths>,                                 // 替代 appContext
}

// ==== 编排核心（streamWithFailover 等价；stream/non-stream/换号/续期 全在此）====
pub async fn serve_completion(sock_out, body: String, providers: &ProviderSet, …);
pub trait UpstreamRunner {   // 泛型化两条链路
    async fn run(&mut self, sink: &mut dyn FnMut(Frame)) -> RunOutcome; // RunOutcome{usage:Option<Usage>, err:Option<ErrKind>}
    async fn refresh_same_account(&mut self, err: &ErrKind) -> bool;   // 默认 false
}
fn try_refresh(auth_err: &ErrKind, did_refresh: &mut bool) -> bool;
fn try_failover(err: &ErrKind, attempt: u8, max_attempts: u8) -> FailAction;

// ==== 错误分类（纯函数，§3.4 全表转成单测）====
pub enum ErrKind { Http(u16), Solo(i64), Queued(String), Transport(String), Cosy(String), Body(String), Prepare(String), Intl(IKind), Interrupted, Exhausted }
impl ErrKind { pub fn auth_related(&self) -> bool; pub fn retryable(&self) -> bool; }

// ==== crate: gw_credential（Gateway.kt）====
pub struct KeyLocks { pub trae: Mutex<()>, pub qoder: Mutex<()> }   // tokio::sync::Mutex（跨 await 持有需评估：原实现锁内做网络 IO，等价用 blocking 段或缩小临界区）
pub fn derive_api_key(app: &AppPaths) -> String;    // "sk-"+sha256("<install_fingerprint>|<machine_id>").hex[..32]
pub fn fingerprint(app) -> String;                  // 桌面替代：安装目录哈希 / Tauri 捆绑资源哈希（等价"签名指纹"角色）
pub fn machine_id(app) -> String;                   // machine-uid 或持久 UUID，小写 hex
pub fn start(ctx) -> Result<()>; pub fn stop();     // 幂等闸（EADDRINUSE 防重）
pub mod selftest { fn key_stable_and_shaped(); }    // sk- 前缀、长 35、二次取值一致

// ==== 阈值与续期 ====
pub struct RenewSkew { pub trae: Duration /* 3600s */, pub qoder: Duration /* 3_600_000ms */ }  // 类型上强制单位
pub fn expired_within(expires_at: Option<i64>, skew: Duration) -> bool;

// ==== crate: upstream_trae（TraeUpstream/TraeIntlUpstream）====
pub fn prepare_body(req: &mut Value) -> Result<(), PrepareErr>;  // stream=true/function/solo_work_remote、tool_calls↔function_call、content→parts 数组、model 正则+默认、tool_choice/tools 归一
pub fn solo_headers(token,&stream,uid,&region) -> HeaderMap;     // §3.9 全清单；X-Uid 非空才带
pub async fn chat_stream(body, token, uid, region: &'_ TraeRegion /* 请求级快照！ */, sink, on_result) ; // 区域只读快照一次；非 CN→intl::chat_stream
mod sse { pub struct State; pub fn scan(&mut self, line: &[u8]) -> Option<TrackedEvent>; }  // ":" 注释忽略；多 data 行合并；空行分发
pub fn strip_solo_tools(raw: &Value) -> Option<Value>;
pub async fn fetch_models(token, uid) -> Option<Vec<ModelEntry>>; pub fn is_chat_model(cfg)->bool;

// ==== crate: upstream_qoder（QoderUpstream + QoderCosyCrypto 依赖）====
pub fn prepare_body(client: &Value) -> Value;          // §3.11 全映射（含 system 双写、ensure_cache_marker、parameters…）
pub async fn chat_stream(app, client:&Value, cred:&QCred, refresh: impl Fn()->OptJobToken, sink, on_result): RunOutcome;
   // 3 attempts；401@1 先 refresh；queued 探测 queue_wait_ms(raw)->Option<Dur>；backoff min(2^n*1000, 2000)
mod envelope { pub struct Reader; pub fn feed(&mut self, line) -> Option<Frame>; }
pub struct ToolIndexer { cur: i64, seen: bool }  pub fn remap(&mut self, chunk:&mut Value) -> bool;
pub fn emit_error(sink, msg);                      // 两帧：quoted + {"error":…}（保持现状；顺带修复 transport 路径缺 [DONE]，或保留原样=二选一需产品决策，默认**修复**）

// ==== crate: backend_router / stores / catalog / usage ====
pub fn route_for(app,&ModelName)->Backend; pub fn resolved(app)->Backend; pub fn models_for(app)->Vec<Value>; // 并集去重+owned_by
pub struct TraeStore / QoderStore;  // load/save/set_active/sync_region/pick_failover_candidate(优先 is_supported=CN)
pub struct UsageStore { ring: VecDeque<UsageRecord, 100> }  pub fn record(...)->();
pub struct CreditMonitor { throttle: HashMap<Backend, Instant> } pub fn note_forwarded(backend,uid,urgent); // urgent 跳节流；仅转发触发，无定时器

// ==== crate: service（GatewayService 桌面等价物）====
pub struct GatewayController { state: Arc<AppState> }
impl GatewayController {
    pub fn start(&self); pub fn stop(&self);                // 幂等
    pub fn set_intent(&self, v: Option<bool>);              // 三态：never-touched/true/false（§6 语义原样）
    pub fn autostart_on_open(&self);  // hasUserIntent && !shouldRun → 不启动；否则启动
}
pub fn build_tray(app) -> ();   // "运行中·端口 8787" + 停止菜单 + 关窗隐藏托盘
```

### 8.3 移植优先级备注（易错点清单）

1. **字节级**：body 读取、chunked、UTF-8 一律按字节计数；SSE 帧 `\n` 结束（不是 `\r\n`）；`data:` 只剥一个前导空格。
2. **顺序契约**：`wantsStream` 先于 `prepare_body` 读取；流式头在任何聚合/错误 JSON 之前写出；OPTIONS 分支最先；queued 判定先于普通 retryable；Trae 优先于 Qoder 的同名模型。
3. **异步化改造点**：原 per-connection OS 线程 + 阻塞 IO → tokio task；`streamWithFailover` 的循环里有网络 await，锁必须用异步锁且**临界区内不跨大 await**（原 synchronized 包了整段续 IO，可缩小）；`usedUid()` 的 `cur` 可变捕获改成 `Arc<RwLock<Cred>>` 或循环局部值。
4. **修 bug 保持对外语义**：chunked 上限、Trae 连接异常转错误帧（而非污染 SSE）、transport 路径补 `[DONE]`（建议修复并记录 changelog）。
5. **安全**：api key 不落盘、日志不记查询串、token 不进日志、model 字段走正则白名单、错误信息回显截断（200 字符）——照单全收。
