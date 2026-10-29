// 逐行移植 QoderMachine.kt —— Qoder 的「桌面端机器身份」（按官方 CLI 真实逻辑复刻）。
//
// 关键结论（Kotlin 注释里的实测对照）：
//  - campaigns 能否下发取决于 Cosy-ClientType=10（ClientType 5 恒为空），服务端并不校验
//    machine token；
//  - 官方客户端拿不到 umid 时就退化成 MachineToken = MachineId = sha256(seed)，
//    MachineType / MachineCode 留空 —— 本模块照抄这一形态。
//
// 机器身份的 seed 复用 QoderAuthStore::machine_id 已持久化的 UUID，同一台设备永远同一个身份。
use crate::qoder::auth_store;
use crate::state::AppState;
use crate::types::QFingerprint;
use sha2::{Digest, Sha256};

/// Cosy-Version / Cosy-ClientType —— **campaigns 能不能下发就看这两个**（实测版本不影响）。
pub const COSY_VERSION: &str = "1.1.69";

/// 必须 10：实测 5 时 campaigns 恒为空。
pub const CLIENT_TYPE: &str = "10";

/// Cosy-MachineOS：CLI 里 `${arch}_${platform}`，桌面端典型值。
pub const MACHINE_OS: &str = "aarch64_darwin";

pub fn sha256_hex(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    let mut sb = String::with_capacity(d.len() * 2);
    for b in d {
        sb.push_str(&format!("{:02x}", b));
    }
    sb
}

/// 持久且稳定的机器标识（COSY 头 Cosy-MachineId；对齐 Kotlin machineId）。
///
/// seed 复用 auth_store 里已落盘的 UUID：不会因重装/重启漂移（漂移等于换设备，会被风控当成新机器）。
pub fn machine_id(app: &AppState) -> String {
    sha256_hex(&auth_store::machine_id(app))
}

/// 组装本次请求要用的身份：优先真实指纹（抓包回填），否则用本机派生。
pub fn identity(app: &AppState, captured: Option<&QFingerprint>) -> Identity {
    if let Some(fp) = captured {
        return Identity {
            machine_id: fp.machine_id.clone(),
            machine_token: fp.machine_token.clone(),
            machine_type: fp.machine_type.clone(),
            machine_os: fp.machine_os.clone(),
            hostname: fp.hostname.clone(),
            client_type: fp.client_type.clone(),
            version: fp.version.clone(),
            from_capture: true,
        };
    }
    let id = machine_id(app);
    Identity {
        machine_id: id.clone(),
        // 官方在无 umid 时就是 token = machineId（见类注释），不是随便编一个
        machine_token: id,
        machine_type: String::new(),
        machine_os: MACHINE_OS.to_string(),
        hostname: String::new(),
        client_type: CLIENT_TYPE.to_string(),
        version: COSY_VERSION.to_string(),
        from_capture: false,
    }
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub machine_os: String,
    pub hostname: String,
    pub client_type: String,
    pub version: String,
    /// true = 用户抓包回填的真货；false = 本机派生（等价官方无 umid 客户端）。
    pub from_capture: bool,
}

pub fn self_check() {
    // 已知向量：sha256("abc") 是标准值，写死钉住编码（大小写/前导零都容易写错）
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "自检失败：sha256 实现不对"
    );
    assert_eq!(sha256_hex("").len(), 64, "自检失败：摘要长度应为 64");
    assert!(
        !uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000000")
            .expect("自检失败：UUID 解析异常")
            .to_string()
            .is_empty(),
        "自检失败：UUID 解析异常"
    );
}
