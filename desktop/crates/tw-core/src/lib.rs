//! tw-core：TraeWork2API 桌面版全部业务逻辑（不依赖 Tauri）。
//! Kotlin 侧模块映射见 `.monkeycode/specs/2026-11-02-tauri-desktop-version/design.md`。

pub mod appcore;
pub mod auth;
pub mod backup;
pub mod checkin;
pub mod config;
pub mod credits;
pub mod crypto;
pub mod desktop;
pub mod emitter;
pub mod fingerprint;
pub mod gateway;
pub mod ipc;
pub mod locks;
pub mod models;
pub mod storage;
pub mod types;
pub mod upstream;
pub mod usage;
