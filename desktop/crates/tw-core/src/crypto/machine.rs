//! Qoder 机器身份（对应 Kotlin `QoderMachine` + `QoderAuthStore.machineId`）。
//! 权威依据：qoder-cosy.md §2、auth-stores.md §4.5/§7.3。持久化复用 crate::storage（键 `q_machine_id`）。

use crate::storage::DataStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// reverseMask UUID 工具在本模块一并导出（交付要求 qoder-cosy.md §1.7）。
pub use crate::crypto::cosy::{format_lower_uuid, new_request_id, reverse_mask_uuid, runtime_ascii_key};

/// §2.1：machineId 持久化原料的存储键（沿用 Android SharedPreferences 的 key 名，审计友好）。
pub const STORAGE_KEY: &str = "q_machine_id";

/// §2.2：machine/campaigns 链路的 COSY 版本。⚠️ 与聊天链路 cosy::COSY_VERSION=1.1.74 不同，各用各的。
pub const COSY_VERSION: &str = "1.1.69";
/// §2.2：campaigns 实测决定性因素（5 则 campaigns 恒空）；聊天头仍固定 cosy::SCENE_CLIENT_TYPE="5"。
pub const CLIENT_TYPE: &str = "10";
/// §2.2：Cosy-MachineOS 参考值（`goarch_goos` 语义，桌面端典型值）。
pub const MACHINE_OS: &str = "aarch64_darwin";

/// §2.3：SHA-256 → 小写 hex（每字节 `%02x`，无前导省略）。
pub fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    crate::crypto::cosy::hex_lower(&h.finalize())
}

/// §2.1：持久 UUID（Android `QoderAuthStore.machineId` 等价语义）——
/// 读回时已存在且 `length>=16` 则复用（设备级永久稳定，漂移=新设备触发风控）；
/// 否则生成标准 v4 UUID（小写 36 位含连字符）并**明文**落盘（非机密）。
pub fn machine_uuid_in(store: &DataStore) -> String {
    if let Some(saved) = store.load_text(STORAGE_KEY) {
        if saved.chars().count() >= 16 {
            return saved;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = store.save_text(STORAGE_KEY, &id);
    id
}

static MACHINE_UUID_CACHE: OnceLock<String> = OnceLock::new();

/// 全局 store 上的 machineId 原料（首次生成后进程内缓存，等价 @Synchronized）。
pub fn machine_uuid() -> String {
    MACHINE_UUID_CACHE.get_or_init(|| machine_uuid_in(&crate::storage::store())).clone()
}

/// §2.1：QoderMachine.machineId = sha256Hex(持久 UUID 的 UTF-8 字节) → 64 位小写 hex。
pub fn machine_id_sha256_in(store: &DataStore) -> String {
    sha256_hex(&machine_uuid_in(store))
}

/// 全局版：⚠️ 关键错位（§2.1）——此 sha256 形态只服务于 campaigns/登录身份链；
/// 聊天头的 `Cosy-MachineId` 用的是**原始 UUID 字符串**（machine_uuid()），不要混用。
pub fn machine_id_sha256() -> String {
    sha256_hex(&machine_uuid())
}

/// §2.4：抓包回填的“真机指纹”（AuthStore 侧持久化结构，短键 JSON 由存储层负责）。
/// 默认值对齐 Kotlin QFingerprint：type="5"、os=aarch64_darwin、hostname=localhost、clientType=10、version=1.1.69。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct QFingerprint {
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub machine_os: String,
    pub hostname: String,
    pub client_type: String,
    pub version: String,
}

impl Default for QFingerprint {
    fn default() -> Self {
        Self {
            machine_id: String::new(),
            machine_token: String::new(),
            machine_type: "5".into(),
            machine_os: MACHINE_OS.into(),
            hostname: "localhost".into(),
            client_type: CLIENT_TYPE.into(),
            version: COSY_VERSION.into(),
        }
    }
}

impl QFingerprint {
    /// §2.4：空 `machine_id`/`machine_token` 视为无效（调用方应回退本机派生）。
    pub fn is_valid(&self) -> bool {
        !self.machine_id.is_empty() && !self.machine_token.is_empty()
    }
}

/// §2.2：请求身份（campaigns/登录链路）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub machine_id: String,
    pub machine_token: String,
    pub machine_type: String,
    pub machine_os: String,
    pub hostname: String,
    pub client_type: String,
    pub version: String,
    /// true = 抓包回填的真货；false = 本机派生（等价官方“无 umid 桌面端”退化形态）。
    pub from_capture: bool,
}

/// 全局 store 版 identity（§2.2）：captured 非空原样映射（from_capture=true），
/// 否则 token=machineId=sha256(持久 UUID)、type/hostname 留空（官方无 umid 形态，非自造）。
pub fn identity(captured: Option<&QFingerprint>) -> Identity {
    identity_with(captured, &crate::storage::store())
}

/// 可注入 DataStore 的 identity（供测试与非全局上下文）。
pub fn identity_with(captured: Option<&QFingerprint>, store: &DataStore) -> Identity {
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
    let id = machine_id_sha256_in(store);
    Identity {
        machine_token: id.clone(),
        machine_type: String::new(),
        machine_os: MACHINE_OS.into(),
        hostname: String::new(),
        client_type: CLIENT_TYPE.into(),
        version: COSY_VERSION.into(),
        machine_id: id,
        from_capture: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> DataStore {
        DataStore::new(std::env::temp_dir().join(format!("tw-machine-test-{}-{}", std::process::id(), tag)))
    }

    // §2.1：首次生成 v4 UUID 并落盘；重复调用幂等（永久复用）；重开 store 仍复用（模拟重启）
    #[test]
    fn machine_uuid_persist_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("tw-machine-test-{}-persist", std::process::id()));
        let st = DataStore::new(&dir);
        let first = machine_uuid_in(&st);
        assert_eq!(first.len(), 36, "标准 v4 UUID 应为 36 字符");
        assert!(uuid::Uuid::parse_str(&first).is_ok());
        assert_eq!(first, first.to_lowercase());
        assert_eq!(st.load_text(STORAGE_KEY).as_deref(), Some(first.as_str()), "必须明文落盘");
        // 幂等：同 store、以及换新 DataStore 句柄读同一目录
        assert_eq!(machine_uuid_in(&st), first);
        assert_eq!(machine_uuid_in(&DataStore::new(&dir)), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // §2.1：短于 16 的残值视为无效 → 重生成；恰好 >=16 的字符串原样复用（对齐 Kotlin 的长度判据）
    #[test]
    fn machine_uuid_rejects_short_reuses_long() {
        let st = temp_store("lencheck");
        st.save_text(STORAGE_KEY, "abc").unwrap();
        let regenerated = machine_uuid_in(&st);
        assert_eq!(regenerated.len(), 36);
        assert_ne!(regenerated, "abc");
        st.save_text(STORAGE_KEY, "0123456789012345").unwrap(); // 恰 16 → 复用
        assert_eq!(machine_uuid_in(&st), "0123456789012345");
    }

    // §2.1/§2.3：machineId(sha256) 派生 + 已知摘要向量钉死实现
    #[test]
    fn sha256_chain_vectors() {
        // §2.3 selfCheck 向量
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex("").len(), 64);
        let st = temp_store("chain");
        let u = machine_uuid_in(&st);
        assert_eq!(machine_id_sha256_in(&st), sha256_hex(&u), "machineId = sha256(持久 UUID)");
        assert_eq!(machine_id_sha256_in(&st).len(), 64);
    }

    // §2.2：本机派生形态 = 官方无 umid 退化形态（token=id、type/hostname 空、clientType=10、version=1.1.69）
    #[test]
    fn identity_shape() {
        let st = temp_store("identity");
        let id = identity_with(None, &st);
        assert!(!id.from_capture);
        assert_eq!(id.machine_token, id.machine_id);
        assert!(id.machine_type.is_empty() && id.hostname.is_empty());
        assert_eq!(id.machine_os, "aarch64_darwin");
        assert_eq!(id.client_type, "10");
        assert_eq!(id.version, "1.1.69"); // ⚠️ 聊天链路用 1.1.74，这里是 campaigns 档
        let captured = QFingerprint {
            machine_id: "mid".into(),
            machine_token: "mtk".into(),
            ..Default::default()
        };
        assert!(captured.is_valid());
        assert!(!QFingerprint::default().is_valid()); // 空 id/tk 视为无效
        let got = identity_with(Some(&captured), &st);
        assert!(got.from_capture);
        assert_eq!(got.machine_id, "mid");
        assert_eq!(got.machine_token, "mtk");
        assert_eq!(got.machine_type, "5"); // 默认值原样透传
        assert_eq!(got.hostname, "localhost");
    }

    // 交付要求：reverseMask UUID 工具经 machine.rs 导出可用
    #[test]
    fn reexported_uuid_tools_work() {
        let mut raw = [0xFFu8; 16];
        raw[15] = 0x01;
        let v = reverse_mask_uuid(&raw);
        assert_eq!(v[0], 0x01);
        assert_eq!(v[6] & 0xF0, 0x40);
        assert_eq!(v[8] & 0xC0, 0x80);
        assert_eq!(format_lower_uuid(&v).len(), 36);
        assert_eq!(runtime_ascii_key(&v).len(), 16);
        assert_eq!(new_request_id().len(), 36);
    }
}
