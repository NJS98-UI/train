//! models 层：Trae 模型表（ModelsRepo）+ Qoder 模型目录（QoderModelCatalog）+ auto 并集选路表。
//! 依据 `.monkeycode/analysis/models-usage-backup.md` §1/§2/§6、`trae-upstream.md` §2（域名与 SOLO 头）、
//! Kotlin 源码 `ModelsRepo.kt`/`QoderModelCatalog.kt`/`TraeUpstream.kt`/`MainActivity.kt`/`GatewayBackend.kt`。
//!
//! 两套模型体系互不通用（analysis §8.1）：
//! - Trae = 纯上游表，**空是合法态**（未刷新即空，空表拒覆盖，绝不清空已有清单）；
//! - Qoder = 内置 21 条权威快照 + 上游三级降级合并（永不为空，失败继续用内置/磁盘缓存）。
//!
//! 桌面版差异裁决（相对 Android）：
//! - `catalog.qoder.json` 存**合并后的目录 + fetchedAt**（design.md §Data Models）；Android 存上游原文
//!   `catalog_json`+`fetched_at`（analysis §2.3）。设计文档明确桌面形态，且 TTL 判据同样靠 fetchedAt，
//!   故按 design 落盘。上游原文仅留内存 `last_raw` 供 UI「上次拉取的原文」排查。
//! - 新增**目录缓存 TTL 3600s**（任务要求）：`needs_sync()` 在已有动态目录且未过期时跳过网络。
//! - `qoderCatalogAutoTried`/`autoModelsTried` 进程闸由调用方（UI/网关 wave）持有，本层只给判据。
//! - `QoderModelCache`（CLI catalog-v7 加密缓存导入器，analysis §3）不在本 wave：依赖 hkdf/aes-gcm，
//!   当前 Cargo.toml 无该依赖且禁止新增，留给后续 wave 在 crypto 层展开。
//!
//! 并发约束：本模块**不 import** `gateway`/`upstream`；「拉取失败降级到兜底」的判定全部内聚
//! （Trae 链 = `fetch_trae_models` 返回 Err 即保持现表；Qoder 链 = `resolve_sync_chain` 纯函数）。
//! HTTP 客户端直接用 reqwest 独立实现，只拉表不发聊天请求。

pub mod catalog;
pub mod repo;
pub mod routing;

pub use catalog::{CatalogSyncOutcome, QModel, QoderModelCatalog, Resolved, CATALOG_TTL_SECS};
pub use repo::{ModelEntry, ModelsRepo};
pub use routing::{auto_union, is_qoder_model, is_trae_model, models_for, resolved_backend, route, snapshot};
