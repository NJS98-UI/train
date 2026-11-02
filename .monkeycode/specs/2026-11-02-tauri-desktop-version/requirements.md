# Requirements Document — TraeWork2API 桌面版（Tauri）

## Introduction

把现有 Android 原生版 TraeWork2API（本地 OpenAI 兼容 API 网关，上游为 Trae 国服/国际或 Qoder/COSY）
移植为 Tauri 桌面应用，功能与 Android 版一一对应。Android 特有的机制（前台服务保活、唤醒锁、
看门狗闹钟、划掉任务自动拉回、Widevine 设备指纹）在桌面端采用等价机制替代：系统托盘常驻、
开机自启、机器指纹派生 Key、系统通知。

与 Android 版的对应关系：

| Android | 桌面版 |
|---|---|
| MainActivity 五页签 UI | Tauri Webview 前端（同结构、同主题/配色） |
| LocalFakeServer（0.0.0.0:8787） | Rust 内置 HTTP 网关（同端口、同路由） |
| SharedPreferences 持久化 | JSON 文件持久化（应用数据目录） |
| 前台服务 + 通知 + 唤醒锁 + 看门狗 | 托盘常驻 + 系统通知 + 关闭窗口后台运行 + 开机自启 |
| AlarmManager 每日签到 | 应用内定时器 + 启动补签（同语义） |
| Widevine+签名指纹派生 API Key | 机器指纹（Windows MachineGuid / machine-id 等）派生 API Key |

## Glossary

- **网关 (Gateway)**: 本机 8787 端口上跑的 OpenAI 兼容 HTTP 服务
- **上游 (Upstream)**: Trae 国服/国际或 Qoder 官方服务器
- **后端 (Backend)**: 网关转发所选择的上游链路，取值 `trae` / `qoder` / `auto`
- **凭证 (Credential)**: 账号的 access_token / jobToken 等访问上游的令牌
- **换号 (Failover)**: 上游未吐正文即失败时切换到另一个账号重试
- **续期 (Refresh)**: 用 refresh token / PAT 换取新的有效凭证（轮换式）
- **签到 (Check-in)**: 向 Trae/Qoder 官方领取每日积分的活动
- **积分 (Credits)**: 账号余额，用于判断限流与换号
- **JOB / 任务**: 一次转发调用产生的用量记录
- **机器指纹 (Machine Fingerprint)**: 操作系统提供的稳定设备标识，用于派生本地 API Key

## Requirements

### R1 本地 OpenAI 兼容网关

**User Story:** 作为桌面用户，我希望本机上有一个 OpenAI 兼容 API，让 Chatbox、Cherry Studio、沉浸式翻译等客户端直接用我的 Trae/Qoder 账号聊天。

#### Acceptance Criteria

1. WHEN 网关启动，the system SHALL 在 `0.0.0.0:8787` 监听并提供 `GET /healthz`、`GET|/v1 /v1/models`（及无前缀 `/models`）、`POST /v1/chat/completions`（及 `/chat/completions`）。
2. WHEN 客户端请求携带正确的本地 API Key（`Authorization: Bearer sk-…`），the system SHALL 放行该请求。
3. IF 请求缺少或携带错误的本地 API Key，the system SHALL 返回 401 及 OpenAI 风格错误体。
4. WHEN 客户端以 `stream=true` 请求聊天补全，the system SHALL 以 SSE 流式转发上游增量内容（含工具调用增量）。
5. WHEN 客户端以 `stream=false` 请求，the system SHALL 聚合完整回复后以 JSON 返回，含 usage 字段。
6. WHEN 请求体含 `tools`/`tool_calls`，the system SHALL 按 OpenAI 协议完整映射到上游协议并回传。
7. WHERE 桌面与手机在同一局域网，the system SHALL 允许局域网内其它设备直连 `http://<本机IP>:8787/v1`，并在概览页展示该地址。
8. IF 8787 端口被占用，the system SHALL 返回明确错误并允许用户重启服务，启动操作具备幂等性。
9. IF 网关已向客户端发出 200 响应头后链路异常，the system SHALL 将异常收敛为流内 `event: error` 帧并以 `[DONE]` 收尾（修复 Android 版已知的流污染缺陷；此为需求方授权的唯一"更正确"偏差）。

### R2 双上游后端与自动路由

**User Story:** 作为同时拥有 Trae 与 Qoder 账号的用户，我希望两个平台都能作为聊天上游，并且系统自动把每个模型派给正确的上游。

#### Acceptance Criteria

1. WHEN 后端选择为 `trae`，the system SHALL 将全部请求转发到 Trae 上游（按当前账号所在区域选国服/国际 host）。
2. WHEN 后端选择为 `qoder`，the system SHALL 将全部请求走 Qoder COSY 签名链路。
3. WHEN 后端选择为 `auto` 且仅登录了 Qoder 账号，the system SHALL 将全部请求路由到 Qoder。
4. WHEN 后端选择为 `auto` 且两个平台都有账号，the system SHALL 按模型名路由：命中 Trae 模型表走 Trae，命中 Qoder 目录（key/展示名/别名任一）走 Qoder，其余走 Trae。
5. WHEN 后端选择为 `auto` 且两平台都有账号，the system SHALL 在 `/v1/models` 返回两平台模型的并集（Trae 在前、按 id 去重、每项标注 `owned_by`）。
6. THE system SHALL 保存后端选择（`trae`/`qoder`/`auto`）并在 UI 提供切换，默认 `auto`。

### R3 多账号管理与故障换号

**User Story:** 作为拥有多个 Trae/Qoder 账号的用户，我希望一次保存所有账号、一键切换，并在账号失效时自动换号重试。

#### Acceptance Criteria

1. THE system SHALL 支持保存多个 Trae 国服账号、多个 Trae 国际账号与多个 Qoder 账号（PAT，`pt-` 开头）。
2. WHEN 用户在账号页选择某个账号，the system SHALL 将其设为当前账号并持久化，区域随当前账号同步。
3. IF 上游在返回正文前失败（401/token 失效、429 限流、5xx），the system SHALL 切换到下一个可用账号并重试一次；换号结果持久化。
4. IF 换号后仍失败，the system SHALL 将当前账号回滚为原账号后返回错误。
5. THE system SHALL 对 Trae 与 Qoder 各用一把独立的串行锁执行"取凭证+续期+换号"，两条链路互不阻塞。
6. WHEN 账号被服务端拒绝，the system SHALL 在账号页对该账号标记「需重新登录」状态（按 uid 记录）。

### R4 登录与凭证自动续期

**User Story:** 作为长期运行的用户，我希望登录一次后长期免登录（token 自动续期），失效时才需要重新登录。

#### Acceptance Criteria

1. WHEN 用户复制国服/国际登录链接到浏览器完成登录并粘回回调链接，the system SHALL 解析回调换取并保存凭证。
2. WHEN Qoder 用户粘贴 `pt-` 开头的 PAT，the system SHALL 保存并通过 COSY 三段交换/运行时字段流程换取可用凭证。
3. WHEN 转发前凭证距过期不足 60 分钟，the system SHALL 自动续期（refresh token 轮换式更新并持久化）。
4. IF 续期失败且凭证已失效，the system SHALL 触发换号流程；全部账号失效时返回错误并标记账号状态。
5. WHILE 网关长期运行（超过 14 天），the system SHALL 持续通过续期保持免登录有效。

### R5 模型表管理与静默补新

**User Story:** 作为客户端用户，我希望模型列表完整、准确、重启不丢，并且新模型自动出现。

#### Acceptance Criteria

1. WHEN 用户触发刷新模型，the system SHALL 从 Trae 上游拉取真实模型清单并本地持久化（重启后仍可用）。
2. THE system SHALL 按三级降级自动获取 Qoder 模型目录：① COSY 签名 `/algo/api/v2/model/list` ② openapi `/api/v1/models` ③ 内置 20 条快照。
3. THE system SHALL 保存 Qoder 目录每个模型的官方倍率，0 倍率标注「免费」。
4. WHEN 应用启动后进入模型页首次刷新，the system SHALL 静默比对目录补入新模型（每次进程至多一次打上游）。
5. WHEN 模型页渲染，the system SHALL 展示模型数、来源标签（trae/qoder）与「已加载」判定（以当前表是否有模型为准）。

### R6 签到与积分

**User Story:** 作为希望薅满每日免费额度的用户，我希望一键给所有账号签到，并可开启每日自动签到。

#### Acceptance Criteria

1. WHEN 用户点击批量签到，the system SHALL 逐个对全部 Trae 国服/国际与 Qoder 账号执行签到并汇总每个账号的结果。
2. WHEN 定时签到开启（默认关），the system SHALL 按三链路调度：用户设定时刻（Trae 默认 08:00、Qoder 默认 09:00，单发+触发自续）、SGT 10:05 固定开窗补签、应用启动后的补跑；仅定时路径且新增成功数>0 时发系统通知。
3. IF 定时签到在设定时刻未执行成功，WHEN 用户下次打开应用，the system SHALL 自动补签（按账号 `yyyy-MM-dd` 成功日标记防重）。
4. THE system SHALL 在账号页顶部展示 Trae+Qoder 全部账号积分合计，合计只累加查询成功的账号；一个都没查到时显示 `—`，单账号查询失败标注 `—（原因）`（与现版 Android 代码一致）。
5. THE system SHALL 在每张账号卡上展示该账号自己的积分余量（无需切换到该账号）。
6. WHEN 一次转发完成，the system SHALL 只对该次实际使用的那个平台、那个账号刷新额度（内部节流 60 秒）；IF 该次转发失败，the system SHALL 跳过节流立刻判定换号。
7. THE system SHALL 在空闲（无转发）时停止一切对上游的额度轮询。

### R7 用量记录

**User Story:** 作为关心消耗的用户，我希望看到每次调用的用量与成败。

#### Acceptance Criteria

1. WHEN 一次转发结束，the system SHALL 记录：模型、prompt/completion/total tokens、耗时、成败、错误信息、后端与账号 uid，并持久化。
2. THE system SHALL 在记录页按模型聚合展示用量与调用统计。

### R8 本地 API Key 稳定性

**User Story:** 作为已把 Key 填进各客户端的用户，我希望升级、重装、清缓存后 Key 保持不变。

#### Acceptance Criteria

1. THE system SHALL 由「机器指纹（操作系统稳定标识）」确定性地派生本地 API Key，格式 `sk-` + 32 位十六进制。
2. THE system SHALL 在多次取值、重启、重装后保持派生 Key 一致（自检验证稳定性与格式）。
3. IF 操作系统稳定标识不可读，the system SHALL 退化使用一个持久化随机文件 ID 并保证此后稳定。
4. THE system SHALL 在界面默认打码显示 Key，并提供明文查看与复制。

### R9 桌面常驻与开机自启（替代 Android 保活）

**User Story:** 作为希望"装好就不用管"的用户，我希望关掉窗口后网关继续跑、开机自动拉起。

#### Acceptance Criteria

1. WHILE 网关运行中，THE system SHALL 在系统托盘持续显示状态图标，右键菜单提供启动/停止/打开主窗口/退出。
2. WHEN 用户点击窗口关闭按钮且「点 X 最小化到托盘」开启（默认开），the system SHALL 隐藏窗口并保持网关运行。
3. THE system SHALL 提供「开机自启」开关（默认关，与 Android 版"刻意不做自启"的历史决策一致，用户显式开启后系统启动时自动拉起应用）；WHEN 应用启动且用户从未手动停止过服务（或意图为开），the system SHALL 自动启动网关（保留 should_run 三态意图语义）。
4. THE system SHALL 在失败/需重新登录/签到完成等事件用系统通知告知用户。
5. THE system SHALL 提供与 Android「保活体检」对应的诊断面板：托盘可用、自启状态、端口监听、上次心跳等逐项体检结果。

### R10 UI 与外观

**User Story:** 作为用户，我希望桌面版与手机版一样的界面结构、亮暗主题与多配色。

#### Acceptance Criteria

1. THE system SHALL 提供 概览 / 账号 / 模型 / 记录 四个持久页签（以现版 Android 代码实况为准，"更多"页为历史死代码，不移植），页面内容与 Android 版逐一对应。
2. THE system SHALL 在概览页展示服务状态、启停开关、API Key、本机与局域网连接地址与快速复制。
3. THE system SHALL 提供二级页：外观设置（亮/暗/跟随系统 + 4 套配色风格）与常驻设置（桌面体检面板），选择立即生效并持久化。
4. WHEN 主题或配色变化，the system SHALL 全窗口即时重绘（等价于 Android  recreate 行为）。

### R11 数据持久化与备份

**User Story:** 作为换机或重装的用户，我希望数据不丢且可迁移。

#### Acceptance Criteria

1. THE system SHALL 把账号、凭证、后端选择、模型表、用量记录、外观与设置项持久化到应用数据目录的 JSON 存储。
2. THE system SHALL 提供账号备份导出与导入（覆盖/合并策略与 Android 版 AccountBackup 一致）。
3. IF 存储文件损坏或缺失，the system SHALL 以默认值自愈启动且不崩溃。

### R12 打包与分发

**User Story:** 作为桌面用户，我希望直接安装一个 exe/msi。

#### Acceptance Criteria

1. THE system SHALL 提供 Tauri v2 工程并通过 `npm run tauri build` 产出 Windows 安装包（NSIS/MSI）。
2. THE system SHALL 支持在 Linux/macOS 上编译运行同一工程（跨平台代码路径一致）。

## 与 Android 版的差异说明（有意为之）

- 前后台服务/唤醒锁/看门狗闹钟/划掉拉回 → 托盘常驻 + 开机自启 + 关窗后台运行（R9）。
- MediaDrm Widevine 与 APK 签名指纹 → 操作系统机器指纹（R8）。
- Android 通知栏常驻通知 → 托盘图标状态 + 系统通知（R9）。
- ROM 白名单/电池优化等 Android 保活体检项 → 桌面等值体检（端口监听、自启、托盘）（R9.5）。
- 其余协议、路由、账号、签到、积分、模型、用量逻辑与 Android 版保持一一对应。
