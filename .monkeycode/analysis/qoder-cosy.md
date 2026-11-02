# Qoder COSY 加密链路分析（移植 Tauri/Rust 用）

来源（已逐行通读，共 1415 行）：
- `当前工作区/app/src/main/java/com/example/traework2api/QoderCosyCrypto.kt`（474 行）
- `当前工作区/app/src/main/java/com/example/traework2api/QoderMachine.kt`（116 行）
- `当前工作区/app/src/main/java/com/example/traework2api/QoderUpstream.kt`（825 行）
- 补充交叉引用：`QoderAuthStore.kt`（machineId/指纹持久化）、`QoderAuthApi.kt`（域名）、`QoderModelCatalog.kt`（resolve/sessionFor）、`LocalFakeServer.kt`（换号判定/非流式聚合）

目标：不看 Kotlin 也能用 Rust 字节级重写。所有"⚠️"标记是错了必然 401/400 且上游不给原因的点位。

---

## 1. QoderCosyCrypto

### 1.0 总体链路（"三段交换"与握手各步的变量）

⚠️ 术语校准：代码中"三段交换"（`swapOuterThirds`）只是 body 编码的最后一步混淆（首尾三分之一对调，自反），**不是**三次网络往返。真正的"三段派生"是每次登录/每次请求的两层材料，全部本地计算、无额外交互步骤：

| 段 | 时机 | 输入 | 派生变量 | 输出/去向 |
|---|---|---|---|---|
| ① 运行时字段 | **每次登录一次**，缓存 | uid, jobToken, orgId, orgTags, dataPolicyAgreed | `uuid_r`(16 随机字节→reverseMask), `keyStr`=uuid_r 前 8 字节 hex 小写(16 ASCII), AES key=IV=`keyStr` UTF-8 字节, `plaintext`=运行时 JSON | `encryptUserInfo` = b64std(AES-CBC-PKCS7(plaintext, keyStr, keyStr))；`key` = b64std(RSA-PKCS1v15(keyStr bytes))。请求头 `Cosy-Key` = key；payload 的 `info` = encryptUserInfo |
| ② 每次请求签名 | 每个 HTTP 请求 | 上游 body 明文, key, signedPath | `requestId`=newRequestId()(⚠️与 body 里的 request_id 是两个不同的 UUID，见 3.4), `unixSeconds`=当前秒(字符串,不补零), `payloadB64`=b64std(payloadJSON), `encoded`=encodeBody(body), `encBodyStr`=String(encoded, US_ASCII) | `Authorization: Bearer COSY.<payloadB64>.<md5hex(payloadB64\nkey\nunixSeconds\nencBodyStr\nsignedPath)>`；HTTP body = `encoded` 原始字节 |
| ③ 响应解信封 | 收 SSE | `data:` 行 JSON 信封 | statusCodeValue, body 字符串 | 解出 OpenAI chunk，注入 `model`，重排 tool_call index，转发 `data: {...}\n\n` |

无网络调用、纯计算的部分均可被 `selfCheck()` 离线钉住（第 388-473 行的断言即"字节级等价测试向量"，移植后应逐条复刻，见 1.9）。

### 1.1 路径与协议常量

```kotlin
const val NATIVE_INFER_PATH   = "/algo/api/v2/service/pro/sse/agent_chat_generation"  // URL 用（带 /algo）
const val COSY_SIGNED_PATH    = "/api/v2/service/pro/sse/agent_chat_generation"       // 签名用（不带 /algo）⚠️
const val NATIVE_INFER_QUERY  = "?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1"
const val NATIVE_MODEL_LIST_PATH = "/algo/api/v2/model/list"   // URL 用
const val MODEL_LIST_SIGNED_PATH = "/api/v2/model/list"        // 签名用
const val COSY_VERSION        = "1.1.74"   // Cosy-Version 头 + payload.cosyVersion + business.version
const val SCENE_CLIENT_TYPE   = "5"        // Cosy-ClientType 头（聊天链路固定 5）
const val SCENE_BUSINESS_PRODUCT = "cli"
const val SCENE_BUSINESS_TYPE    = "agent"
const val SCENE_SCENE            = "assistant"
```

⚠️ URL 路径带 `/algo` 前缀、签名路径**不带**。两条路径分别用于 chat 与 model/list，共 4 个字符串，互相不能混用。

### 1.2 自定义 base64（BODY_ALPHABET）

字符表（64 个字符，逐字符转写，索引 0..63，已校验无重复）：

```
 0=_   1=d   2=o   3=R   4=T   5=g   6=H   7=Z   8=B   9=K  10=c  11=G  12=V  13=j  14=l  15=v
16=p  17=C  18=,  19=@  20=a  21=F  22=S  23=x  24=#  25=D  26=P  27=u  28=N  29=J  30=m  31=e
32=&  33=i  34=*  35=M  36=z  37=L  38=O  39=E  40=n  41=)  42=s  43=U  44=r  45=t  46=h  47=b
48=f  49=%  50=Y  51=^  52=w  53=.  54=(  55=k  56=I  57=Q  58=y  59=X  60=q  61=W  62=A  63=!
```

完整字符串（连续一份，便于直接抄）：
`_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!`

填充字符：`BODY_PAD = '$'`（⚠️ 不是标准 base64 的 `=`）。

#### customBase64Encode(raw: ByteArray): ByteArray 伪代码

```
encLen = ((len + 2) / 3) * 4          # 向上取整到 3 的组再 ×4，含 pad
out = byte[encLen]
i = 0, o = 0
while i + 3 <= len:
    b0,b1,b2 = raw[i..i+2] & 0xFF
    out[o+0] = ALPHABET[(b0 >> 2) & 0x3F]
    out[o+1] = ALPHABET[((b0 << 4) | (b1 >> 4)) & 0x3F]
    out[o+2] = ALPHABET[((b1 << 2) | (b2 >> 6)) & 0x3F]
    out[o+3] = ALPHABET[b2 & 0x3F]
    i += 3; o += 4
rem = len - i                          # 0 / 1 / 2
rem == 1: out[o+0]=ALPH[(b0>>2)&3F]; out[o+1]=ALPH[(b0<<4)&3F]; out[o+2]=PAD; out[o+3]=PAD
rem == 2: out[o+0]=ALPH[(b0>>2)&3F]; out[o+1]=ALPH[((b0<<4)|(b1>>4))&3F]; out[o+2]=ALPH[(b1<<2)&3F]; out[o+3]=PAD
# rem==0 无尾组；注意输出长度恒为 4 的倍数（encLen 已预分配，包括尾组 pad 与 rem==0 时的空洞？——不会：encLen 恰好覆盖 rem=0/1/2 三种情况的全部字节）
```

要点：与标准 base64 的位分组完全同构，仅换表 + pad 换成 `$`；`shl` 后统一 `and 0x3F` 截断（等价标准 `(b0&3)<<4`）。

#### 解码（encodeBody 的逆）

聊天链路**只做编码，从不解码**（上游响应 SSE 是明文 JSON 信封）。若需解码（调试用）：先用第 1.3 节 swap 还原顺序，再按"ALPHABET.rindex(ch)"建逆表做标准 base64 逆运算，`$` 视为 pad。

### 1.3 swapOuterThirds（三段交换）

```
fun swapOuterThirds(src: ByteArray): ByteArray
q = src.size / 3                        # 整数除法
out[0 .. q-1]       = src[size-q .. size-1]   # 末 q 字节挪到最前
out[q .. size-q-1]  = src[q .. size-q-1]       # 中段原位
out[size-q .. size-1] = src[0 .. q-1]          # 首 q 字节挪到最后
```

性质：**自反**（做两次 = 恒等），所有长度 n 均成立；n<3 时 q=0 → 原样返回。示例钉住：`"abcdef"`(n=6,q=2) → `"efcdab"`。⚠️ 编解码两端必须用同一函数。

### 1.4 encodeBody

```
fun encodeBody(raw: ByteArray): ByteArray = swapOuterThirds(customBase64Encode(raw))
```

⚠️ 返回**字节**。HTTP body 直接写这些字节（Content-Type: application/json）；参与 MD5 签名的是同一字节序列按 US_ASCII 转成的字符串（表内全 ASCII，二者无损等价）。

### 1.5 MD5 签名（authorization）

```
fun authorization(payloadB64, key, unixSeconds, encodedBody, signedPath = COSY_SIGNED_PATH): String
signature = lowercase_hex( MD5( UTF-8( payloadB64 + "\n" + key + "\n" + unixSeconds + "\n" + encodedBody + "\n" + signedPath ) ) )
return "Bearer COSY." + payloadB64 + "." + signature
```

- 分隔符恰为单个 `\n`（LF），共 4 个，**末尾无换行**。⚠️ 多一个 `\n` 必 401。
- `unixSeconds`：十进制秒、无补零、无格式化。
- `encodedBody`：model/list 时是**空串**（GET 无 body，签名里该字段位置保留为空，即连续两个 `\n`）。
- 测试向量（selfCheck 钉死）：
  `authorization("PAYLOAD","KEY","1700000000","BODY")` 的签名 = md5hex("PAYLOAD\nKEY\n1700000000\nBODY\n/api/v2/service/pro/sse/agent_chat_generation")
  目录：`authorization("PAYLOAD","KEY","1700000000","","/api/v2/model/list")` = md5hex("PAYLOAD\nKEY\n1700000000\n\n/api/v2/model/list")

#### authorizationPayload

```
fun authorizationPayload(requestId: String, info: String, cosyVersion = COSY_VERSION): String
json = {"version":"v1","requestId":requestId,"info":info,"cosyVersion":cosyVersion,"ideVersion":""}
return b64_std(UTF-8(json))             # Android Base64 NO_WRAP，标准表（含 '=' pad）
```

`info` 已是 base64 字符串，⚠️ 不可二次编码。requestId 用 `newRequestId()`（1.7）。注意：payload 里的 `requestId` 与请求体 JSON 里的 `request_id`（普通 `UUID.randomUUID()`）**是两个独立 UUID，值不同**。

### 1.6 generateRuntimeFields（运行时字段，登录一次）

```
fun generateRuntimeFields(uid, jobToken, organizationId, organizationTags: List<String>, dataPolicyAgreed: bool) -> RuntimeFields{encryptUserInfo, key}
plaintext  = runtimeFieldInputJson(...)                  # 见下
uuid       = reverseMaskUUID(randomBytes(16))            # 16 随机字节
keyStr     = hex_lower(uuid[0..7])                       # 16 个 ASCII 字符
keyBytes   = keyStr.US-ASCII bytes                       # 同时用作 AES key 和 AES IV
ciphertext = AES-128-CBC-PKCS7( UTF-8(plaintext), keyBytes, keyBytes )
encKey     = RSA-1024-PKCS1v15( keyBytes, 固定公钥 )      # 输出恰 128 字节
return { encryptUserInfo = b64std(ciphertext), key = b64std(encKey) }
```

`b64std` 均为标准 base64（NO_WRAP，pad=`=`，无换行）。

#### runtimeFieldInputJson（明文 JSON）

字段固定顺序：`uid → security_oauth_token → organization_id → organization_tags → data_policy_agreed`。

```
o = {}
o.uid = uid
if jobToken.isNotEmpty(): o.security_oauth_token = jobToken   # ⚠️ omitempty：空串时整个键缺席；写空串上游判 token 无效
o.organization_id = organizationId
o.organization_tags = [ ...tags ]                              # 空也输出 []
o.data_policy_agreed = true|false
```

⚠️ org.json 的字段序在任何语言里都不参与验签正确性判定——**唯一硬约束是"参与 MD5 的字节串与发出去的 body 字节完全一致"**。Rust 用 `serde_json`（MapPreserveOrder / 手工按固定顺序拼串）即可。

### 1.7 UUID / 密钥派生

```
fun reverseMaskUUID(raw[16]): ByteArray
v[i] = raw[15-i]                       # 整体反序（v[0]=raw[15]…）
v[6] = (v[6] & 0x0F) | 0x40            # 版本位 nibble=4（byte6 高 4 位）
v[8] = (v[8] & 0x3F) | 0x80            # 变体位（byte8 高 2 位 = 10）
```

```
fun formatLowerUUID(v[16]): String     # 8-4-4-4-12，小写 hex，连字符在下标 4/6/8/10 前
遍历 i=0..15：i ∈ {4,6,8,10} 时先输出 '-'，再输出该字节的两位 hex
```

```
fun newRequestId(): String = formatLowerUUID(reverseMaskUUID(randomBytes(16)))
```

```
fun runtimeASCIIKey(uuid[16]): String = hex_lower(uuid[0..7])   # 16 ASCII 字符
```

等价关系（selfCheck 钉住）：`runtimeASCIIKey(uuid) == formatLowerUUID(uuid).去掉'-'的前 16 位`。

测试向量：`raw = [01,02,...,10]`（1..16）→ 反序后 v[0]=0x10；v[6] 高 nibble 强制 0x4，v[8] 高两 bits 强制 10b。

### 1.8 PKCS7 填充 与 AES-CBC

```
fun pkcs7Pad(src, blockSize=16): padLen = 16 - len%16（整块时 padLen=16）；追加 padLen 个值为 padLen 的字节
fun aesCbcEncryptPkcs7(plain, key, iv):
    require key.len ∈ {16,24,32}, iv.len == 16
    ciphertext = AES/CBC/NoPadding(encrypt, pkcs7Pad(plain,16), key, iv)   # 实现是 cipher.init(ENCRYPT) + doFinal(padded)
```

本链路实际只走 key=16（AES-128）、iv=key（同一 16 字节）。

### 1.9 selfCheck 测试向量（Rust 移植必须逐条复刻的断言）

1. `swapOuterThirds` 自反；`"abcdef"→"efcdab"`。
2. `encodeBody([0x66])`：长度 4、恰含 2 个 `$`（⚠️ 个数断言在 swap 后仍成立，但 `$` 位置会变：本例落在下标 0 和 2 附近——selfCheck 特意只数个数）。
3. `encodeBody("abc")`：长度 4、无 `$`、每字符都在 ALPHABET 内。
4. 签名公式：1.5 节两个向量 + 断言"末尾多 `\n` 摘要必须不同" + 断言"换 signedPath 摘要必须不同"。
5. reverseMaskUUID：v[6]&0xF0==0x40、v[8]&0xC0==0x80、v[0]==raw 的末字节。
6. PKCS7：16→32、15→16、0→16 个 0x10。
7. 运行时字段：AES 密文 len%16==0；RSA 密文 len==128；jobToken 非空明文必含 `security_oauth_token`，空必缺席。

---

## 2. QoderMachine（机器身份）

### 2.1 machineId 生成与持久化

| 项 | 事实 |
|---|---|
| 持久原料 | `QoderAuthStore.machineId(context)`：SharedPreferences `qoder2api_prefs`，键 `q_machine_id`；首次 `UUID.randomUUID().toString()`（标准 v4、36 位小写含连字符）落盘（明文）；读取时若已存且 `length>=16` 复用 |
| QoderMachine 派生 | `machineId(ctx) = sha256Hex(上述 UUID 字符串的 UTF-8 字节)` → 64 位小写 hex |
| 稳定性要求 | 设备级永久稳定：漂移 = 新设备 → 风控 |

⚠️ **关键错位（移植必核对）**：`QoderUpstream.buildRequest` 与 `fetchModelListRaw` 的 `Cosy-MachineId` 头**直接用原始 UUID 字符串**（`QoderAuthStore.machineId`），没有做 sha256。`QoderMachine.machineId`（sha256）与 `QoderMachine.identity()` 服务于另一条链路（campaigns/登录，token=machineId = 官方无 umid 退化形态），**不参与 chat 头**。

### 2.2 QoderMachine 常量与 Identity

```
COSY_VERSION = "1.1.69"    # ⚠️ 与聊天链路 QoderCosyCrypto.COSY_VERSION=1.1.74 不同，各用各的
CLIENT_TYPE  = "10"        # campaigns 实测决定性因素（5 则 campaigns 恒空）；聊天头仍固定 5
MACHINE_OS   = "aarch64_darwin"
Identity(machineId, machineToken, machineType, machineOS, hostname, clientType, version, fromCapture)
```

`identity(context, captured)`：captured（抓包回填 QFingerprint）非空 → 原样映射，fromCapture=true；否则 `{id=machineId, token=同 id, type="", os=aarch64_darwin, hostname="", clientType="10", version="1.1.69"}`。

### 2.3 sha256Hex 实现约定

```
hex = SHA-256(UTF-8(input)) 输出为小写、无前导省略（每字节 %02x）
向量：sha256Hex("abc") = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"（selfCheck 钉住）；sha256Hex("").len==64
```

### 2.4 QFingerprint（AuthStore 侧，供 identity 回填）

字段默认：machineType="5", machineOS="aarch64_darwin", hostname="localhost", clientType="10", version="1.1.69"。持久化 JSON 短键 `{id,tk,ty,os,hn,ct,ver}`，加密存储，键 `q_acc_{uid}_fp`。空 `id`/`tk` 视为无效。

---

## 3. QoderUpstream（chat 转发完整协议）

### 3.1 URL

```
base(按 cred.mode):  global → https://api2.qoder.sh      cn / 未知 → https://gateway.qoder.com.cn   ⚠️ 与 openapi 域名（openapi.qoder.sh / openapi.qoder.com.cn）是两套，不可混
chat:  POST {base}/algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1
目录:  GET  {base}/algo/api/v2/model/list
```

### 3.2 请求头清单（buildRequest / fetchModelListRaw 逐条）

聊天 POST（全部 header 按构造值）：

| 头 | 值/构造 |
|---|---|
| Accept | `text/event-stream` |
| Authorization | `QoderCosyCrypto.authorization(...)`，signedPath=COSY_SIGNED_PATH |
| Cache-Control | `no-cache` |
| Content-Type | `application/json` |
| Cosy-Business-Product | `cli` |
| Cosy-Business-Type | `agent` |
| Cosy-ClientType | `5` |
| Cosy-Data-Policy | `dataPolicyAgreed ? "agree" : "disagree"` |
| Cosy-Date | 与签名同一个 `unixSeconds` 字符串 |
| Cosy-Key | 运行时字段 `key`（RSA 密文 b64） |
| Cosy-MachineId | `QoderAuthStore.machineId`（原始持久 UUID，36 位） |
| Cosy-MachineOS | `aarch64_android`/`x86_64_android`/`arm_android`/`x86_android`（按 Build.SUPPORTED_ABIS 映射，默认 aarch64_android；参考实现语义 `goarch_goos`，桌面端对应如 `aarch64_darwin`） |
| Cosy-Scene | `assistant` |
| Cosy-User | `cred.uid` |
| Cosy-Version | `1.1.74` |
| Login-Version | `v2` |
| X-Model-Key | 解析后的模型 key（body.model_config.key） |
| X-Model-Source | `system` |
| Cosy-Organization-Id | 仅 `organizationId` 非空时 |
| Cosy-Organization-Tags | 仅非空时，`joinToString(",")` |

⚠️ 刻意**不发**：`User-Agent`（与签名身份一致性；reqwest 默认带 UA，须显式清空——见第 5 节）、`Cosy-MachineToken`、`Cosy-MachineType`（参考实现发请求前删除）、`Connection: keep-alive`。

GET 目录请求：去掉 Accept 改 `application/json`、无 Content-Type、无 X-Model-*，Authorization 的 `encodedBody` 传空串、signedPath=`/api/v2/model/list`；其余同表。

### 3.3 请求体 prepareBody(src: OpenAI JSON) → 上游 JSON（逐字段）

顶层（按代码插入序转写，org.json 实际不保证序；Rust 固定序即可，硬约束只有"签名串==发送串"）：

| 字段 | 构造 |
|---|---|
| business | `{product:"cli", version:"1.1.74", type:"agent", id:普通UUIDv4, name:lastUserText.take(10字符), begin_at:当前毫秒(long), stage:"start"}`。lastUserText=倒序最后一条 role=user 的非空 content（字符串或 contents[type=text].text） |
| request_id / request_set_id / chat_record_id | **同一个** `java.util.UUID.randomUUID().toString()`（标准 v4；⚠️ 与签名 payload 的 requestId 不同源） |
| session_id | `QoderModelCatalog.sessionFor(modelKey)`：进程内按模型缓存 `"q2a-" + formatLowerUUID(reverseMaskUUID(16 随机))`；重启换新 id 只丢历史 |
| stream | **恒 `true`**（上游只认流式；客户端 stream:false 由上层聚合，见 3.8） |
| chat_task | `"FREE_INPUT"` |
| chat_context | `{}` |
| is_reply | `true` |
| is_retry | `false` |
| source | `1` |
| version | `"3"` |
| agent_id | `"agent_common"` |
| task_id | `"common"` |
| session_type | `"qodercli"` |
| aliyun_user_type | `""` |
| model_config | `{key, format:"openai", source:"system", enable:true, display_name:m.name, is_vl:true(恒真), is_reasoning:m.reasoning, price_factor: m.priceFactor ?? 1.0}`（⚠️ 用目录真实倍率，未知才 1.0） |
| custom_model | `null`（JSON null，键必须存在） |
| system | 所有 role∈{system,developer} 的 content（仅字符串且非空）用 `"\n\n"` 连接 |
| messages | 见 toUpMessage |
| tools | `src.tools ?: []` |
| parameters | 见下 |

toUpMessage(每条非 system/developer 消息)：
```
{ role, [name 非空才带], [tool_call_id 非空才带], content?, contents?, tool_calls? }
- role != "tool" 且 content 为非空字符串 → contents = [{type:"text", text:content}]
- 有 tool_calls 非空数组 → 原样带 tool_calls，且 content 必须是字符串（null→""）⚠️ 否则上游解析失败
- 否则 content 非 null → content 原样（可为 string，不含 null）
```

ensureCacheMarker(msgs)：全量扫描，任一 part 已有 `cache_control` → 不动；否则**从后往前**找最后一条非 tool 消息的最后一个 `contents` part，type∈{text,image_url}，补 `cache_control:{"type":"ephemeral"}`。⚠️ 必须全 prompt 恰好一个，否则前缀不进缓存（长对话全价）。

parameters：
```
max_tokens = max_completion_tokens>0 ? 它 : (max_tokens>0 ? 它 : 32000)
reasoning_effort = normalize(minimal→low; none|low|medium|high|xhigh|max 原样; 其它→整个键省略)
temperature/top_p: 是数字才带（Double）
stop: 字符串→单元素数组；数组非空→原样；空→省略键（上游只认数组）
tool_choice: 存在且非 null → 原样
```

### 3.4 签名与发送（chatStream 主循环）

```
upstreamBody = prepareBody(clientBody)                    # 失败 → emitError("请求体构造失败:类名", 见 3.7) + [DONE] + onResult(null,"body:类名")
modelKey     = upstreamBody.model_config.key ?? "auto"
jobToken     = cred.jobToken; settled=false
encoded      = encodeBody( UTF-8( upstreamBody.toString() ) )
encStr       = String(encoded, US_ASCII)                  # 仅用于签名
for attempt in 1..3:
    unixSeconds = now_ms/1000 字符串                       # 每次尝试重取
    runtime     = runtimeFieldsFor(cred, jobToken)         # {uid|jobToken: RuntimeFields} 缓存, size>8 清空
    payloadB64  = authorizationPayload( QoderModelCatalog.newRequestId(), runtime.encryptUserInfo )
    authorization = authorization(payloadB64, runtime.key, unixSeconds, encStr)   # 默认 COSY_SIGNED_PATH
    req = buildRequest(...); 异常 → settle(null,"prepare:类名")
    resp = client.execute(req)
    传输异常: attempt<3 → sleep 100ms 重试;  否则 emitError + settle(null,"transport:类名")
    成功(2xx): readEnvelopeSse(...)；流内错误 e≠null → emitError(e)+[DONE]+settle(usage,"cosy:"+e)
               正常 → [DONE] + settle(usage,null)；return
    401 且 attempt==1 且有 refreshJobToken 且续出新 token 非空 → jobToken=新值, continue（同一 body 重签）
    queued=queueWaitMs(错误原文)：非 null → 立即 emitError("模型「{modelKey}」当前不可用（上游资源受限，{max(queued/1000,1)} 秒后可重试）。请换其它模型，或在账号页切换 Qoder 账号。") + [DONE] + settle(null,"queued:{modelKey}"); return   # ⚠️ 不重试/不降级/不 sleep，换号交给上层
    retryable = code==408 || code==429 || code>=500：attempt<3 → wait=(Retry-After秒*1000 ?? 2^(attempt-1)*1000).min(2000)ms → sleep → continue
    否则 → emitError("上游 HTTP {code}: {raw 前 200 字符}") + [DONE] + settle(null,"http:{code}")
落出循环 → settle(null,"exhausted")
```

要点：⚠️ 每次 attempt 都重新生成 payload 的 requestId、unixSeconds（Authorization/Cosy-Date 必须与签名用的值同步替换），**body 密文不变**（同一份 encoded 重复签）。上游按 (requestId, date) 防重放。

### 3.5 SSE 分帧（readEnvelopeSse）

上游帧格式（与 Trae 不同，是双层信封）：
```
event: chat                # 可选；多行事件名用 ' ' 连接
data: {"statusCodeValue":200,"body":"<OpenAI chunk JSON 的字符串>"}
                           # 空行 = 帧边界；多行 data 用 '\n' 拼接，每行剥前缀 "data:" 后仅剥一个前导空格
```

逐帧处理（flush）：
1. 帧的 event == `"finish"` → **正常终止**（stopped=true，无错误）。
2. `payload`（trim 后）不是合法 JSON → 跳过该帧。
3. `statusCodeValue ∉ {0,200}` → 错误：`body` 按 JSON 解析取其 `message`；错误串=`message`，取不到则 `"upstream error status {code}"`；终止。
4. `body` 为空串 → 跳过。
5. `body` 非合法 JSON → 跳过。
6. `chunk.error.message` 非空 → 错误串=该 message，终止。
7. `chunk.usage` 对象存在 → 回调 onUsage（覆盖式记录，最终 settle 用**最后一个** usage）。
8. ToolIndexer.remap(chunk)（见 3.6）。
9. 写本地帧：
   ```
   快路径（未被 remap 且 chunk 本来没有 model 字段且 body.trim() 以 `{"` 开头）:
       wire = "{\"model\":" + JSONquote(clientModel) + "," + trimmed[1..]     # 字符串级注入，省一次序列化
   慢路径: chunk.put("model", clientModel); wire = chunk.toString()
   out("data: " + wire + "\n\n")
   ```
10. 整条流结束（正常或错误）后由 chatStream 写终止符 `data: [DONE]\n\n`。

行解析细节：`\r` 剥除（trimEnd('\r')）；空行触发 flush；`event:` 前缀行剥 `event:`，trim 后追加（多行以空格连接）；其它前缀行忽略（兼容注释 `:`、`id:` 等）。连接提前断开（readLine 异常/EOF）→ 末尾补一次 flush 再返回。

首字延迟有日志埋点（首帧时 ms）。

### 3.6 tool_call 映射（ToolIndexer）

上游对**并行**工具调用都发 `index:0`、只靠新 `id` 区分；OpenAI 客户端按 index 累积会把两个并工具合并。规则：
```
cur=0; seen=false
对 chunk.choices[*].delta.tool_calls[*]（保持数组序）:
    if tc.id 非空: seen ? cur++ : (seen=true)     # 第一个 id 仍属 index0
    若 tc.index != cur 记 changed；强制 tc.index = cur
返回 changed（决定走慢路径）
```
⚠️ 增量帧里只有分片续写（无 id）时 index 沿用 cur，自然聚合到正确槽位。

### 3.7 OpenAI chunk 透传映射总表

上游 `body`（信封里的 chunk JSON）字段结构即 OpenAI chat.completion.chunk（id/object/created/model(缺)/choices/delta/finish_reason/usage），网关仅做三件事：注入 `model`（客户端原始 model 字符串）、重排 tool_call index、把 `usage` 抄给上层计量。其余字段不做改名不做删减。错误帧统一转成：
```
event: error
data: "<message 原样 JSON 字符串>"
                     (空行)
data: {"error":{"message":"<msg>","type":"api_error"}}
                     (空行)
data: [DONE]
```

### 3.8 排队 / 倍率相关

`queueWaitMs(raw: String): Long?` —— 处理 403 资源受限的多层转义响应（实测样例见注释）：
```
命中判据（纯文本扫整串）：含 "isQueued":true / RESOURCE_LIMIT / "code":"10605" / 10605 任一条
    从转义文本抠秒（正则 \*"(key)"\s*:\s*(\d+)，key 依次 retryAfterSeconds → waitTime → retryAfterMs）:
        前两者 ×1000 且下限 1000ms；retryAfterMs 直接下限 1000ms；都没有 → 2000ms
未命中判据：按规整 JSON 兜底：顶层 queue.isQueued==true → waitTime>0?waitTime:2000
其它 → null（不是排队错误）
```
命中排队 → HTTP 层面直接 settle("queued:{modelKey}")，**不等待、不降级、不换模型**。

倍率：`model_config.price_factor` 取自目录真实值（null→1.0）。目录 `/algo/api/v2/model/list` 用签名 GET（3.1/3.2），数据源优先级：网关动态 > 本机缓存 > 冻结快照；`resolve` 规则：去 `[1m]` 后缀→按 key/展示名/别名（大小写不敏感）匹配→落空回 `auto`；带 `[1m]` 且输入 tokens<900k 且模型上下文<900k → 升级 `ultimate`(ONE_M_KEY)。

### 3.9 错误码分类（settle 的 errType 标签）

| 本地标签 | 触发 | 换号?（LocalFakeServer.retryableUpstreamError） | 同账号续期?（authRelatedError） |
|---|---|---|---|
| `body:类名` | prepareBody 抛异常 | 是（非数字码保守换） | 否 |
| `prepare:类名` | buildRequest 抛异常 | 是 | 否 |
| `transport:IO类名` | 3 次传输全失败 | 是 | 否 |
| `http:401/403/429/5xx/408...` | 最终 HTTP 状态 | **401/403/429/≥500 → 换；400/404/422 → 不换** | 仅 401/403 |
| `cosy:{streamErr}` | 流内信封错误 | 取决于 message 里数字；非数字保守换 | — |
| `queued:{modelKey}` | 排队/RESOURCE_LIMIT | **换（账号级排队，立即、秒级）** | 否（显式排除） |
| `exhausted` | 循环兜底 | 是 | 否 |

chatStream 自身的传输重试语义：401 只在 attempt==1 续期一次（换新 jobToken 后同一 body 重签重发）；408/429/5xx 退避 ≤2000ms；排队**零重试**。401 若续期成功仍失败则落入后续 attempt 正常路径。

### 3.10 非流式聚合路径

上游恒流式（body stream=true 硬编码）。客户端 `stream:false` 时由 `LocalFakeServer.streamWithFailover` 攒帧（`collected`）后走 `aggregateChunks(frames, model)`；且**先判错**（error 帧会被聚合成空回复 → 必须转 502 + openAIError("upstream_error", msg)）。聚合规则：
- 逐帧只认 `data:` 行；跳过空 payload 与 `[DONE]`
- `delta.content`/`delta.reasoning_content` 拼接 → `message.content`/`reasoning_content`
- `delta.tool_calls` 按 index 累积：name 取首个非空，arguments 逐帧累加，id 最后出现的非空覆盖
- `finish_reason` 取最后一个非空非 "null"；有 tool_calls 且 finish=="stop" → 改 "tool_calls"
- `usage` 取最后一帧带 usage 的
- id 取首个非空（缺省 `"chatcmpl-local"`），created 缺省当前秒
- 输出标准 `chat.completion`：`{id,object,created,model,choices:[{index:0,message,finish_reason}],usage?}`

---

## 4. 常量总表 + 硬编码样例请求

| 常量 | 值 | 出处 |
|---|---|---|
| MAX_ATTEMPTS | 3 | QoderUpstream |
| MAX_RETRY_WAIT_MS | 2000 | QoderUpstream |
| DEFAULT_MAX_TOKENS | 32000 | QoderUpstream |
| connect/read timeout | 20s / 180s | OkHttpClient |
| 传输异常固定退避 | 100ms | chatStream |
| runtimeCache 清空阈值 | 8 条 | runtimeFieldsFor |
| 排队默认等待 / 下限 | 2000ms / 1000ms | queueWaitMs |
| RSA 密文长度 | 128 字节（1024-bit） | selfCheck |
| AES key/IV | 16 ASCII 字符（uuid 前 8 字节 hex） | crypto |
| BODY_PAD | `$` | crypto |
| COSY_VERSION(crypto/聊天) | 1.1.74 | crypto |
| COSY_VERSION(machine/campaigns) | 1.1.69 | QoderMachine |
| CLIENT_TYPE(machine) | 10 | QoderMachine |
| SCENE_CLIENT_TYPE(聊天头) | 5 | crypto |
| MACHINE_OS(machine) | aarch64_darwin | QoderMachine |
| infer base | api2.qoder.sh / gateway.qoder.com.cn | QoderAuthApi |
| prefs 文件名 / machine key | qoder2api_prefs / q_machine_id | QoderAuthStore |
| DEFAULT_KEY / ONE_M | auto / [1m]→ultimate | QoderModelCatalog |
| LocalFakeServer MAX_BODY_BYTES | 8MB | LocalFakeServer（本地入站防护） |

### 样例：prepareBody 输出（客户端 `{"model":"Ultimate","messages":[{"role":"system","content":"你是助手"},{"role":"user","content":"你好"}],"max_tokens":123,"reasoning_effort":"minimal","temperature":0.3,"top_p":0.9,"stop":["\n"],"tool_choice":"auto","stream":false}`）

```json
{
  "business": {"product":"cli","version":"1.1.74","type":"agent","id":"3f8a1b2c-4d5e-6f70-8192-a3b4c5d6e7f8","name":"你好","begin_at":1762000000000,"stage":"start"},
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "request_set_id": "550e8400-e29b-41d4-a716-446655440000",
  "chat_record_id": "550e8400-e29b-41d4-a716-446655440000",
  "session_id": "q2a-b1a2c3d4-5e6f-7081-9a2b-3c4d5e6f7a8b",
  "stream": true,
  "chat_task": "FREE_INPUT",
  "chat_context": {},
  "is_reply": true,
  "is_retry": false,
  "source": 1,
  "version": "3",
  "agent_id": "agent_common",
  "task_id": "common",
  "session_type": "qodercli",
  "aliyun_user_type": "",
  "model_config": {"key":"ultimate","format":"openai","source":"system","enable":true,"display_name":"Ultimate","is_vl":true,"is_reasoning":true,"price_factor":1.0},
  "custom_model": null,
  "system": "你是助手",
  "messages": [
    {"role":"user","content":"你好","contents":[{"type":"text","text":"你好","cache_control":{"type":"ephemeral"}}]}
  ],
  "tools": [],
  "parameters": {"max_tokens":123,"reasoning_effort":"low","temperature":0.3,"top_p":0.9,"stop":["\n"],"tool_choice":"auto"}
}
```

（uuid 值为示意；三个 request_* 字段实际同值同请求内；`begin_at` 为毫秒；`business.name` 截 10 个 **Unicode 字符**。整体 `toString()` → UTF-8 → encodeBody 即 wire body 与签名字节。）

### 样例：一次完整 chat 请求（示意头，值已占位）

```
POST /algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1 HTTP/1.1
Host: gateway.qoder.com.cn
Accept: text/event-stream
Authorization: Bearer COSYeyJ2ZXJzaW9uIjoidjEiLCJyZXF1ZXN0SWQiOi...<payloadB64>.<32hex>
Cache-Control: no-cache
Content-Type: application/json
Cosy-Business-Product: cli
Cosy-Business-Type: agent
Cosy-ClientType: 5
Cosy-Data-Policy: agree
Cosy-Date: 1762000000
Cosy-Key: <172字符 RSA 密文 b64（128 字节）>
Cosy-MachineId: 7c9e6679-7425-40de-944b-e07fc1f90ae7
Cosy-MachineOS: aarch64_android
Cosy-Scene: assistant
Cosy-User: <uid>
Cosy-Version: 1.1.74
Login-Version: v2
X-Model-Key: ultimate
X-Model-Source: system
[无 User-Agent；无 Cosy-MachineToken；无 Cosy-MachineType]

<body = encodeBody(上述请求体 JSON) 的原始字节>
```

---

## 5. 内置 RSA 公钥（逐字节事实）

PKIX/SubjectPublicKeyInfo DER 的 base64（Android 以 `Base64.NO_WRAP` 解码 + `X509EncodedKeySpec` 解析），原样（拼接后单行）：

```
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17XcW+ML9FoCI6AOvOzwIDAQAB
```

本地 `openssl rsa -pubin -inform DER -text` 解析验证：1024-bit，公开指数 65537；modulus 128 字节 hex（前导符号字节 00 已剔除）：

```
c0f22307e5cd362e296bb04470f6de8fbf935ce24e8fcf511a0e2701329769c4
a76e499bb938036a52af1eaf818cf79a2600620e3ce87e371d2ca6d85803606a
1b3fa5e874643c9ed2db7e85673ef7227fca56e2e7c08f0927609bb896a9f24b
e1782099a66016a5bfdc3f1ff756bfc9e88d7b5dc5be30bf45a0223a00ebcecf
```

Rust 可直接以 modulus/exponent 构造 `rsa::BigUint`（避开 SPKI 解析），或嵌入同一 base64 用 `spki` 解析。

⚠️ RSA padding 细节（rsaEncryptPkcs1v15，对齐 Go `rsaEncryptPKCS1v15Exact`）：
- 手动构造 EM：`0x00 ‖ 0x02 ‖ PS ‖ 0x00 ‖ M`，PS = 长度 `k - len(M) - 3` 的**非零随机**（逐字节抽、抽到 0 就重抽该字节，不做整段重抽）。
- `c = EM^e mod N`，大端输出**左补零到恰好 k=128 字节**（`fillBytes` 处理 top bit 导致 `toByteArray()` 带 0x00 前缀多一字节的情形：多则截头，少则右对齐填充）。
- 校验：k>=11、len(M) <= k-11、m<N。
- 刻意绕开 `Cipher RSA/ECB/PKCS1Padding`（Android 实现对 PS 零字节处理存在版本差异）。Rust 等价：`rsa::Pkcs1v15Encrypt` 加密（同样合法 PKCS#1 v1.5，随机填充本就每次不同；服务端只解密校验合法性，等价性成立，但 PS 生成建议同样"非零随机字节"语义以保持行为画像一致）。
- 加密对象：`keyStr` 的 16 ASCII 字节（**不是** AES 密文也非 16 个原始 uuid 字节）。

## Android API 依赖清单与 Rust 等价物

| Kotlin/Android 依赖 | 位置 | Rust 等价 |
|---|---|---|
| `android.util.Base64`（NO_WRAP/decode） | crypto b64Std、公钥解码 | `base64::engine::general_purpose::STANDARD`（编解码含 `=` pad，无换行） |
| `javax.crypto AES/CBC/NoPadding` + `SecretKeySpec`/`IvParameterSpec` | aesCbcEncryptPkcs7 | `aes::cipher::...`：`cbc::Encryptor<aes::Aes128>`（key=16）+ 手写 `pkcs7Pad`（或 `aes::...buf` + `Pkcs7Padding`） |
| `MessageDigest("MD5"/"SHA-256")` | md5Hex、QoderMachine | `md-5`、`sha2` crate |
| `KeyFactory RSA` + `X509EncodedKeySpec` + `BigInteger.modPow` | rsaEncryptPkcs1v15 | `rsa` crate（`RsaPublicKey::new(n, e)` + `Pkcs1v15Encrypt`）自填 modulus/exponent hex |
| `SecureRandom` | 全部随机 | `rand::rngs::OsRng`（rsa crate 内置兼容） |
| `android.content.Context` + SharedPreferences | AuthenticStore/Upstream | Tauri：`tauri-plugin-store` 或直接向 `configDir()/traework2api/` 写 JSON/plaintext（machineId 非机密，明文可） |
| `android.os.Build.SUPPORTED_ABIS` | MACHINE_OS lazy | `std::env::consts::ARCH`/`TARGET`：arm64→`aarch64_<os>`、x86_64→`x86_64_<os>`；Tauri 桌面按参考实现语义用 goos 名（macos/linux/windows 风格自定，需与服务端画像一致） |
| `android.util.Log` | 各 catch 分支 | `log`/`tracing`（⚠️ token 永不入日志：Kotlin 全程只打异常类名，保留该纪律） |
| `okhttp3.*`（无 UA、180s read） | QoderUpstream | `reqwest`：`connect_timeout(20s)`、`read_timeout(180s)`/`client.read_timeout`，stream 响应体；⚠️ **必须清空 UA**：`UserAgent::from_str("")` 或显式 `headers.insert(USER_AGENT, HeaderValue::from_static(""))` 且 `danger_accept_invalid_certs(false)` 保持默认 |
| `org.json.JSONObject`（无序 map） | 全部 body/payload | `serde_json`：签名与发送必须复用**同一份 Vec\<u8\>**（一次 `to_writer` 结果既当 body 又参与 MD5），杜绝二次序列化 |
| `java.util.UUID.randomUUID()`（body request_id 与 business.id） | prepareBody | `uuid::Uuid::new_v4().to_string()`（小写 36） |
| `BufferedReader.readLine` SSE | readEnvelopeSse | `resp.bytes_stream()` + 自维护行切分（\n/\r\n），帧空行 flush，语义与 3.5 一致 |
| `Thread.sleep` | 退避 | `tokio::time::sleep` |
| `@Synchronized`（machineId、sessionFor、runtimeFieldsFor） | 并发保护 | `std::sync::Mutex` / `parking_lot::Mutex` |

---

## 6. 建议 Rust 模块 API 草案（crate: aes/rsa/md5/sha2/base64/reqwest）

```rust
// qoder_cosy::crypto —— 纯计算，无 IO，可脱离网络跑 test_vectors.rs（复刻 1.9 selfCheck）
pub const BODY_ALPHABET: [u8; 64];              // _doRTgHZ... 逐字节
pub const BODY_PAD: u8 = b'$';
pub const NATIVE_INFER_PATH: &str; pub const COSY_SIGNED_PATH: &str; pub const NATIVE_INFER_QUERY: &str;
pub const NATIVE_MODEL_LIST_PATH: &str; pub const MODEL_LIST_SIGNED_PATH: &str;
pub const COSY_VERSION: &str = "1.1.74";        // 聊天
pub const SCENE: (ClientType="5", Product="cli", BusinessType="agent", Scene="assistant");

pub struct RuntimeFields { pub encrypt_user_info: String, pub key: String } // key = b64(RSA 128B)

pub fn custom_b64_encode(raw: &[u8]) -> Vec<u8>;                       // 1.2
pub fn swap_outer_thirds(src: &[u8]) -> Vec<u8>;                       // 1.3
pub fn encode_body(raw: &[u8]) -> Vec<u8>;                             // 1.4
pub fn pkcs7_pad(src: &[u8], block: usize) -> Vec<u8>;                 // 1.8
pub fn aes_cbc_encrypt_pkcs7(plain: &[u8], key: &[u8], iv: &[u8]) -> Vec<u8>;  // aes 128/192/256 断言
pub fn rsa_encrypt_pkcs1v15(msg: &[u8]) -> Vec<u8>;                    // 内嵌 modulus hex，128B 输出
pub fn reverse_mask_uuid(raw: &[u8; 16]) -> [u8; 16];
pub fn format_lower_uuid(v: &[u8; 16]) -> String;
pub fn runtime_ascii_key(uuid: &[u8; 16]) -> String;                   // 前 8 字节 hex
pub fn new_request_id(rng: &mut OsRng) -> String;
pub fn runtime_field_input_json(uid,&str, job_token, org_id, tags, agreed) -> String // 固定字段序+omitempty
pub fn generate_runtime_fields(uid: &str, job_token: &str, org_id: &str, tags: &[String], agreed: bool) -> RuntimeFields;
pub fn authorization_payload(request_id: &str, info: &str, cosy_version: &str) -> String; // b64std(JSON)
pub fn authorization(payload_b64:&str, key:&str, unix_seconds:&str, encoded_body:&str, signed_path:&str) -> String; // md5(五段 \n 连接)
pub fn b64_std(bytes: &[u8]) -> String;
pub fn md5_hex(s: &str) -> String;

// qoder_machine
pub fn sha256_hex(input: &str) -> String;                     // 小写、%02x；向量 "abc"
pub struct MachineStore { path: PathBuf }                     // configDir/q_machine_id（明文 uuid）
pub fn machine_uuid(store) -> String;                         // ≥16 校验，首次 v4 落盘
pub fn machine_id_sha256(store) -> String;                    // ⚠️ 仅 campaigns 身份链；聊天头用原始 uuid
pub fn identity(captured: Option<Fingerprint>, store) -> Identity;

// qoder_upstream
pub struct PreparedBody(pub serde_json::Value);               // prepare_body(&openai_json, &catalog) -> Result<PreparedBody>
pub async fn chat_stream(
    client: &reqwest::Client,                                 // UA 空、connect 20s、read 180s
    base: &str,                                               // infer_base(mode)
    prepared: &PreparedBody, cred: &QCred, runtime: &RuntimeFields,
    machine_uuid: &str, model_key: &str,
    sink: &mut impl FnMut(String),                            // OpenAI SSE 帧（与 3.7 [DONE] 一致）
    refresh_job_token: &mut impl FnMut() -> Option<String>,
) -> (Option<serde_json::Value> /*usage*/, Option<String> /*err 标签，前缀同 3.9 表*/);
pub async fn fetch_model_list(client, base, cred, job_token) -> Option<String>;
pub fn queue_wait_ms(raw: &str) -> Option<u64>;
pub fn retry_after_ms(header: Option<&str>) -> Option<u64>;
pub struct ToolIndexer;  pub fn remap(&mut self, chunk: &mut Value) -> bool;
pub fn read_envelope_sse(byte_stream) -> impl Stream<Item=Frame>; // 3.5 状态机
pub fn aggregate_chunks(frames: &[String], model: &str) -> serde_json::Value; // 3.10（LocalFakeServer 侧）

// localfake_server 的换号判定纯函数（一并移植）
pub fn retryable_upstream_error(err: &str) -> bool;    // queued→true, 数字码∈{401,403,429,≥500}→true, {400,404,422}→false, 非数字→true
pub fn auth_related_error(err: &str) -> bool;          // queued→false, 数字码∈{401,403}
```

移植验收顺序建议：crypto（复刻 selfCheck 全部向量）→ prepareBody（复刻 QoderUpstream.selfCheck 断言）→ SSE/ToolIndexer（构造含 statusCodeValue/finish/并行 tool 的假流）→ 真实网关冒烟。
