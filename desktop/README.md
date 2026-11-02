# TraeWork2API 桌面版（Tauri）

Android 版 TraeWork2API 的桌面移植：本地 OpenAI 兼容 API 网关（默认 `0.0.0.0:8787`），上游为 Trae（国服/国际服）与 Qoder（国内版/国际版）。业务内核为纯 Rust crate，前端为 React + Vite + TS。

## 目录结构

```
desktop/
├── crates/
│   ├── tw-core/        # 业务内核：auth/crypto/upstream/gateway/models/credits/checkin/usage/backup/ipc（不依赖 tauri）
│   ├── tw-bridge/      # 浏览器预览用 IPC 桥：POST /api/invoke/<cmd> + GET /api/events(SSE)，默认 127.0.0.1:8790
│   └── (tw-app)        # Tauri 壳（托盘/关窗进托盘/自启开关），Windows 实测编译
├── frontend/           # React 界面（vite dev 5173，/api 反代 8790）
└── scripts/            # 打包脚本（build-desktop.ps1）
```

## 本地预览（浏览器模式）

两个后台进程 + 前端 dev server：

```bash
# 1. IPC 桥（业务内核随 bridge 常驻，数据目录默认 ~/.traework2api-desktop-preview）
cargo run -p tw-bridge

# 2. 前端
cd frontend && npm install && npm run dev
```

浏览器打开 `http://localhost:5173` 即可完整操作（IPC 走 bridge HTTP，事件走 SSE）。

## 测试

```bash
cargo test -p tw-core            # 内核全量
cd frontend && npx tsc --noEmit  # 前端类型零错误
cd frontend && npm run build     # 生产构建
```

## 桌面打包（Windows）

```powershell
.\scripts\build-desktop.ps1
```

产物含安装器；网关端口/数据目录/TLS 等配置在 `crates/tw-core/src/config.rs`（config.json 落数据目录，前端「外观/常驻设置」页读写）。

## IPC 契约

单一注册表 `crates/tw-core/src/ipc.rs`（Tauri command 与 bridge 端点共用），命令表见 `.monkeycode/specs/2026-11-02-tauri-desktop-version/design.md` §IPC 契约。改动必须同步 `frontend/src/ipc.ts` 的调用形状。

## 关键行为对齐（与 Android 版一致）

- 网关：`/healthz`、`/v1/models`、`/v1/chat/completions`；API key 由本机安装 ID + 机器指纹派生（不落盘明文），换号纪律「未出 `choices` 帧才可安全重试、同平台最多 2 个号」。
- 签到/积分：Trae `checkin_credits` + 额度、Qoder campaigns 领取 + 额度，每平台 60s 节流，见底阈值下限 5。
- 续期：Trae `expires_at` 提前 3600 秒、Qoder jobToken 提前 3_600_000 毫秒。
- 模型：Trae 目录拉取 + Qoder 官方目录三级降级 + 内置 21 条兜底；`auto` 为并集路由（Trae 优先）。
