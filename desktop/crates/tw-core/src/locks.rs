//! 双锁模型：`Gateway.locked`（Trae，可重入——批量签到全程持锁且内部嵌套续期）与
//! `Gateway.lockedQoder`（Qoder 专用）。两把锁必须分离，防止一条链路的慢请求卡死另一条。

use parking_lot::{Mutex, ReentrantMutex};
use std::sync::OnceLock;

static TRAE: OnceLock<ReentrantMutex<()>> = OnceLock::new();
static QODER: OnceLock<Mutex<()>> = OnceLock::new();

fn trae() -> &'static ReentrantMutex<()> {
    TRAE.get_or_init(Default::default)
}

fn qoder() -> &'static Mutex<()> {
    QODER.get_or_init(Default::default)
}

/// Trae 串行段：取凭证 + 续期 + 换号 + 批量签到。Reentrant：外层批量已持锁时内层直接重入。
pub fn locked<T>(f: impl FnOnce() -> T) -> T {
    let g = trae().lock();
    let out = f();
    drop(g);
    out
}

pub fn locked_qoder<T>(f: impl FnOnce() -> T) -> T {
    let g = qoder().lock();
    let out = f();
    drop(g);
    out
}
