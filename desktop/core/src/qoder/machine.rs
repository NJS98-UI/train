// PORT-TODO(agent=qoder): 逐行移植 QoderMachine.kt。
use crate::state::AppState;

pub fn sha256_hex(_s: &str) -> String { todo!("port QoderMachine.kt") }
/// 持久且稳定的机器标识（COSY 头 Cosy-MachineId；对齐 Kotlin machineId）
pub fn machine_id(_app: &AppState) -> String { todo!("port QoderMachine.kt") }
pub fn self_check() {}
// PORT-TODO: 其余 pub 项
