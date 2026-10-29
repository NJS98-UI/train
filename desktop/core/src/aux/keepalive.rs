// PORT-TODO(agent=aux): 移植 KeepAlive.kt 的「保活体检面板」检查项。
// 桌面版映射：服务运行中 / 端口可绑定 / 本机可达 / 凭证可用 / 模型表非空 等等；
// Android 专属项（前台服务/唤醒锁/电池优化/闹钟）改为等价的桌面检查或标注不适用。
// 输出结构固定：json([{name, ok, detail}...])。
use crate::state::AppState;
use serde_json::Value;

pub async fn check(_app: &AppState) -> Value { todo!("port KeepAlive.kt") }
pub fn self_check() {}
