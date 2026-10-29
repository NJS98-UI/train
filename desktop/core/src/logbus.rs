use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

struct LogBus {
    buf: Mutex<VecDeque<String>>,
    emit: OnceLock<Box<dyn Fn(&str) + Send + Sync>>,
}

static LOG: OnceLock<LogBus> = OnceLock::new();
const CAP: usize = 600;

/// 注册 UI 回调（Tauri 壳在 setup 时注入：把日志转发为前端事件）
pub fn init(f: Box<dyn Fn(&str) + Send + Sync>) {
    let _ = LOG.set(LogBus { buf: Mutex::new(VecDeque::with_capacity(CAP)), emit: OnceLock::new() });
    if let Some(b) = LOG.get() {
        let _ = b.emit.set(f);
    }
}

fn stamp() -> String {
    let now = chrono::Local::now();
    now.format("%m-%d %H:%M:%S").to_string()
}

pub fn log(level: &str, tag: &str, msg: &str) {
    let line = if level == "I" {
        format!("[{}] {}/{}: {}", stamp(), tag, "", msg)
    } else {
        format!("[{}] {}/{}: {}", stamp(), tag, level, msg)
    };
    if let Some(b) = LOG.get() {
        if let Ok(mut q) = b.buf.lock() {
            if q.len() >= CAP {
                q.pop_front();
            }
            q.push_back(line.clone());
        }
        if let Some(f) = b.emit.get() {
            f(&line);
        }
    }
}

pub fn recent() -> Vec<String> {
    if let Some(b) = LOG.get() {
        if let Ok(q) = b.buf.lock() {
            return q.iter().cloned().collect();
        }
    }
    Vec::new()
}

#[macro_export]
macro_rules! log_i {
    ($tag:expr, $($arg:tt)*) => { $crate::logbus::log("I", $tag, &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_w {
    ($tag:expr, $($arg:tt)*) => { $crate::logbus::log("W", $tag, &format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_e {
    ($tag:expr, $($arg:tt)*) => { $crate::logbus::log("E", $tag, &format!($($arg)*)) };
}
