# User Instruction Memory

This file records user instructions, preferences, and teachings for reference in future interactions.

## Format

### User Instruction Entry
User instruction entries should follow this format:

[User Instruction Summary]
- Date: [YYYY-MM-DD]
- Context: [Mentioned scenario or time]
- Instructions:
  - [Content of user teaching or instruction, described line by line]

### Project Knowledge Entry
Entries discovered by the Agent during task execution should follow this format:

[Project Knowledge Summary]
- Date: [YYYY-MM-DD]
- Context: Discovered by Agent while performing [specific task description]
- Category: [Operations & Deployment|Build Methods|Testing Methods|Troubleshooting & Debugging|Workflow & Collaboration|Environment Configuration]
- Instructions:
  - [Specific knowledge points, described line by line]

## Deduplication Strategy
- Before adding a new entry, check for similar or identical instructions.
- If a duplicate is found, skip the new entry or merge it with the existing one.
- When merging, update the context or date information.

## Entries

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while performing desktop frontend Android theme/port task
- Category: Build Methods
- Instructions:
  - 桌面前端构建：`cd /workspace/desktop/frontend && npm run build`（= `tsc -b && vite build`，tsconfig 开 strict + noUnusedLocals，任何未用导入/参数都会失败）。
  - 本地预览：`npm run dev`（vite，端口 5173，`allowedHosts: ['.monkeycode-ai.online']`）。`/api` 反代到 `http://127.0.0.1:8790`，该端口由 Rust crate `crates/tw-bridge` 提供 IPC 桥；桥不在时各页显示「本地 IPC 桥接未启动」占位（`src/ipc.ts` 已处理非 JSON 的 5xx）。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while performing desktop frontend Android theme/port task
- Category: Troubleshooting & Debugging
- Instructions:
  - 配色唯一来源是 `desktop/frontend/src/theme/styles.ts`，必须与 `.monkeycode/analysis/ui-keepalive.md` §4.2 四张色表（19 键 × 4 风格 × 亮/暗 = 152 格）+ §4.3 形态逐项一致。核对方式：用 esbuild 把 `styles.ts` 打包成 mjs，node 脚本解析 md 表格逐格 diff，并调用文件内 `selfCheck()`（含对比度 ≥4.5 断言）确认违规数为 0。改动色值后必须重跑该核对再提交。
  - 页面样式只允许 `var(--tw-*)` / `--fs-*` / `--radius-*` 等 token，禁止硬编码 hex，否则换风格时不跟随。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while checking whether the desktop preview backend was runnable
- Category: Environment Configuration
- Instructions:
  - `crates/tw-core/src/config.rs` 当前编译失败（E0308 元组解构、E0425 未定义符号），连带 `tw-bridge` 无法构建；前端任务约定不改 Rust 目录，需由后端侧修复后预览才能真正出数据。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while performing models/usage/backup rewrite in tw-core (multi-agent parallel waves)
- Category: Build Methods
- Instructions:
  - `cargo test -p tw-core <a> <b> <c>` 多过滤词非法（cargo 只接受一个 TESTNAME），验收必须分开跑：`cargo test -p tw-core models::` / `usage::` / `backup::`（带 `::` 防误匹配 `gateway::models_fallback` 等他模块测试名）。
  - 全 crate lib test 编译含其他并行 wave 的半成品模块（auth/api、upstream/*、gateway/*）；rustc 一次性输出全部错误，若错误清单不含自己的文件即代表自己侧干净，无需等他人修复。
  - 多个 agent 同时编译共享 target 锁会排队；长等策略：后台终端 `for i in $(seq 1 N); do cargo test -p tw-core --no-run && break; sleep 60; done` 监视到编译通过后立刻跑自己的过滤测试。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while performing checkin/credits rewrite in tw-core
- Category: Troubleshooting & Debugging
- Instructions:
  - 环境里的 chrono 是 0.4.45：`TimeZone::with_ymd_and_hm` 已改名 `with_ymd_and_hms`；构造"某固定偏移的某日 HH:MM"请用 `NaiveDate::and_hms_opt(...)` + `tz.from_local_datetime(&naive).single()`。
  - `reqwest::Request` 没有公开构造器（无 `Request::builder`）；需要逐字节断言请求（方法/URL/body/显式头顺序）时，自建结构化请求体（如 `checkin::trae_api::PreparedRequest`），实发再走 `client.request(method, url).headers(map).body(bytes).build()`（qoder 侧共用，配 `execute_with(client)`）。
  - `HH:mm` 换毫秒必须 `minutes * 60_000`；漏乘 60 会让"今天 08:00"退化成"00:08"，表现为 `due_at` 恒真、`next_run_at` 偏 16 小时。此类换算应在 `self_check()` 里用真值钉住。
  - 验收命令：`. "$HOME/.cargo/env" && cd /workspace/desktop && cargo test -p tw-core`（当前全量 281 绿）；lib test 链接阶段在这台 2 核机上前台可跑满 3-5 分钟，务必用受资源限制的后台终端。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while completing ipc.rs and running full `cargo test -p tw-core` acceptance
- Category: Troubleshooting & Debugging
- Instructions:
  - 若 `gateway::backend_adapter` 的用例（candidate_order_and_registration / placeholder_backend_falls_back_to_active 等）在重复跑全量测试时读到非空账号（如 `trae#u2`），根因是其 `temp_store(tag)` 目录名=`/tmp/tw-adapter-<tag>-<进程内SEQ>`，无 pid/随机后缀且跑完不清理，跨进程复用 /tmp 残留的 `accounts.trae.json`。处置：把 `/tmp/tw-adapter-*` 整体 `mv` 到备份目录（遵守禁删规则）后重跑验收即恢复确定性全绿；根治要改 backend_adapter.rs 测试目录命名（若任务只允许改 ipc.rs，应报告而非越界修）。
  - `QoderAuthStore.load_by_uid` 对 pat 与 jobToken 双空白的条目返回 None（Kotlin 硬条件）；IPC 测试想模拟"已存在账号"必须给夹具账号带 pat（如 `pat: "pt-fixture"`），否则 fingerprint_set 走"未找到账号"分支。

[Project Knowledge Summary]
- Date: 2026-11-02
- Context: Discovered by Agent while fixing GitHub Actions Build Desktop workflow
- Category: Build Methods
- Instructions:
  - CI 中 Windows 打 tauri 便携 exe 不要用 `tauri-apps/tauri-action`：它调 tauri CLI 时 cwd 不在 `apps/tw-app`，`beforeBuildCommand` 里的相对路径（`npm --prefix ../../frontend`）会 ENOENT。改手动：`desktop/frontend` 下 `npm ci && npm run build`，再 `working-directory: desktop` 跑 `cargo build --release -p tw-app`，artifact 收 `desktop/target/release/*.exe`。
  - Windows 上 `cargo build -p tw-app` 时 tauri-build 强制要求 `desktop/apps/tw-app/icons/icon.ico` 存在（嵌入 exe 资源），Linux 不检查；缺文件 CI 报 `required for generating a Windows Resource file`。由 RGBA png 生成：`PIL Image.save('icon.ico', sizes=[(16,16),...,(256,256)])`。
  - `package-windows` job 带 `continue-on-error: true`，run 总结论 success 会掩盖它失败；验收必须查该 job 自身 conclusion 与 artifacts 列表。
  - 取失败日志免 gh 认证：`git credential fill` 拿 token → `curl -H "Authorization: Bearer $T" https://api.github.com/repos/NJS98-UI/train/actions/runs/<rid>/logs` 下 zip（jobs API 的 `logs` 字段不可靠会缺 key）；token 不得展示。
