//! Qoder COSY 加密原语（对应 Kotlin `QoderCosyCrypto`，权威依据 qoder-cosy.md §1、§5）。
//! 纯计算，无网络无 IO；任何一处字节级不一致（少个 `\n`、密钥长度、padding）上游只回 401/400。

use aes::cipher::{generic_array::GenericArray, BlockDecryptMut, BlockEncryptMut, consts::U16, KeyIvInit};
use base64::{engine::general_purpose, Engine as _};
use md5::{Digest, Md5};
use rand::{rngs::OsRng, RngCore};
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};

// ------------------------------------------------------------------
// §1.1 路径与协议常量（⚠️ URL 带 /algo、签名路径不带，两者不可混用）
// ------------------------------------------------------------------

/// 实际请求路径（带 `/algo` 前缀，仅拼 URL 用）。
pub const NATIVE_INFER_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";
/// 签名覆盖的路径（不带 `/algo`），§1.1 ⚠️。
pub const COSY_SIGNED_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";
/// 固定查询串。
pub const NATIVE_INFER_QUERY: &str = "?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
/// 模型目录请求路径（带 `/algo`）。
pub const NATIVE_MODEL_LIST_PATH: &str = "/algo/api/v2/model/list";
/// 模型目录签名路径（不带 `/algo`）。
pub const MODEL_LIST_SIGNED_PATH: &str = "/api/v2/model/list";
/// COSY 协议版本（聊天链路；machine/campaigns 用的是 1.1.69，见 machine.rs §2.2）。
pub const COSY_VERSION: &str = "1.1.74";
/// 聊天链路固定场景值（§1.1 / §3.2 请求头）。
pub const SCENE_CLIENT_TYPE: &str = "5";
pub const SCENE_BUSINESS_PRODUCT: &str = "cli";
pub const SCENE_BUSINESS_TYPE: &str = "agent";
pub const SCENE_SCENE: &str = "assistant";

// ------------------------------------------------------------------
// §1.2 自定义 base64（BODY_ALPHABET + pad '$'）
// ------------------------------------------------------------------

/// Qoder 专用 base64 字母表（64 字符，索引 0..63，顺序不能动，§1.2）。
pub const BODY_ALPHABET: &[u8; 64] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
/// 自定义 base64 填充字符（⚠️ 不是标准 base64 的 `=`，§1.2）。
pub const BODY_PAD: u8 = b'$';

/// 自定义 base64 编码（§1.2）：位分组与标准 base64 同构，仅换表 + pad `$`；输出长度恒为 4 的倍数。
pub fn custom_b64_encode(raw: &[u8]) -> Vec<u8> {
    let enc_len = (raw.len() + 2) / 3 * 4;
    let mut out = vec![0u8; enc_len];
    let mut i = 0;
    let mut o = 0;
    while i + 3 <= raw.len() {
        let (b0, b1, b2) = (raw[i] as u32, raw[i + 1] as u32, raw[i + 2] as u32);
        out[o] = BODY_ALPHABET[((b0 >> 2) & 0x3F) as usize];
        out[o + 1] = BODY_ALPHABET[(((b0 << 4) | (b1 >> 4)) & 0x3F) as usize];
        out[o + 2] = BODY_ALPHABET[(((b1 << 2) | (b2 >> 6)) & 0x3F) as usize];
        out[o + 3] = BODY_ALPHABET[(b2 & 0x3F) as usize];
        i += 3;
        o += 4;
    }
    match raw.len() - i {
        1 => {
            let b0 = raw[i] as u32;
            out[o] = BODY_ALPHABET[((b0 >> 2) & 0x3F) as usize];
            out[o + 1] = BODY_ALPHABET[((b0 << 4) & 0x3F) as usize];
            out[o + 2] = BODY_PAD;
            out[o + 3] = BODY_PAD;
        }
        2 => {
            let (b0, b1) = (raw[i] as u32, raw[i + 1] as u32);
            out[o] = BODY_ALPHABET[((b0 >> 2) & 0x3F) as usize];
            out[o + 1] = BODY_ALPHABET[(((b0 << 4) | (b1 >> 4)) & 0x3F) as usize];
            out[o + 2] = BODY_ALPHABET[((b1 << 2) & 0x3F) as usize];
            out[o + 3] = BODY_PAD;
        }
        _ => {}
    }
    out
}

/// 自定义 base64 解码（§1.2 补充说明，仅供调试/测试；聊天链路只有编码方向）。
/// 表外字符或残余长度非法（%4==1）返回 None；`$` 只允许作为末尾 0..2 个填充。
pub fn custom_b64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let val = |ch: u8| -> Option<u32> {
        if ch == BODY_PAD {
            return None;
        }
        BODY_ALPHABET.iter().position(|c| *c == ch).map(|p| p as u32)
    };
    let mut end = data.len();
    let mut pads = 0usize;
    while end > 0 && data[end - 1] == BODY_PAD {
        pads += 1;
        if pads > 2 {
            return None;
        }
        end -= 1;
    }
    let body = &data[..end];
    if body.contains(&BODY_PAD) || body.len() % 4 == 1 {
        return None;
    }
    let rem = body.len() % 4;
    let n_full = body.len() - rem;
    let expect_extra = match rem {
        2 => 1,
        3 => 2,
        _ => 0,
    };
    let mut out = Vec::with_capacity(n_full / 4 * 3 + expect_extra);
    for g in body[..n_full].chunks(4) {
        let (a, b, c, d) = (val(g[0])?, val(g[1])?, val(g[2])?, val(g[3])?);
        out.push(((a << 2) | (b >> 4)) as u8);
        out.push(((b << 4) | (c >> 2)) as u8);
        out.push(((c << 6) | d) as u8);
    }
    if rem >= 2 {
        let (a, b) = (val(body[n_full])?, val(body[n_full + 1])?);
        out.push(((a << 2) | (b >> 4)) as u8);
    }
    if rem == 3 {
        let (b, c) = (val(body[n_full + 1])?, val(body[n_full + 2])?);
        out.push(((b << 4) | (c >> 2)) as u8);
    }
    Some(out)
}

// ------------------------------------------------------------------
// §1.3 swapOuterThirds（三段交换，自反混淆）
// ------------------------------------------------------------------

/// 首尾各三分之一对调（§1.3）：`q = len/3`；性质**自反**（做两次=恒等），len<3 时原样返回。
pub fn swap_outer_thirds(src: &[u8]) -> Vec<u8> {
    let n = src.len();
    let q = n / 3;
    let mut out = Vec::with_capacity(n);
    out.extend_from_slice(&src[n - q..]);
    out.extend_from_slice(&src[q..n - q]);
    out.extend_from_slice(&src[..q]);
    out
}

// ------------------------------------------------------------------
// §1.4 encodeBody / §1.4 逆向 decodeBody
// ------------------------------------------------------------------

/// 请求体编码：先自定义 base64，再三段交换；返回**字节**（HTTP body 与 MD5 签名共用同一序列）。
pub fn encode_body(raw: &[u8]) -> Vec<u8> {
    swap_outer_thirds(&custom_b64_encode(raw))
}

/// encode_body 的逆（调试用；上游响应不走这条路，见 §1.2 解码说明）。
pub fn decode_body(encoded: &[u8]) -> Option<Vec<u8>> {
    custom_b64_decode(&swap_outer_thirds(encoded))
}

// ------------------------------------------------------------------
// §1.5 MD5 签名（Authorization: Bearer COSY.<payloadB64>.<md5hex>）
// ------------------------------------------------------------------

/// §1.5：`Bearer COSY.<payloadB64>.<md5hex>`，签名串 = 5 字段以**单个 LF** 顺序拼接
/// `payloadB64\nkey\nunixSeconds\nencodedBody\nsignedPath`（共 4 个 `\n`，⚠️ 末尾无换行）。
/// GET（模型目录）时 `encoded_body` 传**空串**（字段位置保留，出现连续两个 `\n`）。
pub fn authorization(payload_b64: &str, key: &str, unix_seconds: &str, encoded_body: &str, signed_path: &str) -> String {
    let signature = md5_hex(&format!("{payload_b64}\n{key}\n{unix_seconds}\n{encoded_body}\n{signed_path}"));
    format!("Bearer COSY.{payload_b64}.{signature}")
}

/// `authorization` 的聊天链路便捷形态（signedPath = COSY_SIGNED_PATH，§1.1）。
pub fn authorization_chat(payload_b64: &str, key: &str, unix_seconds: &str, encoded_body: &str) -> String {
    authorization(payload_b64, key, unix_seconds, encoded_body, COSY_SIGNED_PATH)
}

/// `authorization` 的模型目录形态（GET 无 body → encoded_body 为空串，§1.5/§3.2）。
pub fn authorization_model_list(payload_b64: &str, key: &str, unix_seconds: &str) -> String {
    authorization(payload_b64, key, unix_seconds, "", MODEL_LIST_SIGNED_PATH)
}

/// §1.5 authorizationPayload：固定字段序 JSON（version/requestId/info/cosyVersion/ideVersion）
/// → **标准** base64。`info` 已是 base64（运行时字段 encrypt_user_info），⚠️ 不可二次编码。
/// 用手工拼串保证字节序稳定（serde_json Map 不保序）。
pub fn authorization_payload(request_id: &str, info: &str, cosy_version: &str) -> String {
    let json = format!(
        "{{\"version\":\"v1\",\"requestId\":{},\"info\":{},\"cosyVersion\":{},\"ideVersion\":\"\"}}",
        json_quoted(request_id),
        json_quoted(info),
        json_quoted(cosy_version),
    );
    b64_std(json.as_bytes())
}

/// 聊天链路默认 COSY_VERSION 的 authorization_payload。
pub fn authorization_payload_default_version(request_id: &str, info: &str) -> String {
    authorization_payload(request_id, info, COSY_VERSION)
}

// ------------------------------------------------------------------
// §1.6 运行时字段（登录时生成一次并缓存，勿每请求重建）
// ------------------------------------------------------------------

/// 运行时字段输出：`encrypt_user_info`（AES 密文标准 b64）与 `key`（RSA 密文标准 b64，128B→172 字符）。
#[derive(Clone, Debug)]
pub struct RuntimeFields {
    pub encrypt_user_info: String,
    pub key: String,
}

/// §1.6：uuid(16 随机字节→reverseMask) → keyStr=前 8 字节 hex（16 ASCII，同时作 AES key 与 IV）
/// → AES-CBC-PKCS7(明文 JSON) → RSA-PKCS1v15(keyStr) → 各走**标准** base64。
pub fn generate_runtime_fields(
    uid: &str,
    job_token: &str,
    organization_id: &str,
    organization_tags: &[String],
    data_policy_agreed: bool,
) -> RuntimeFields {
    let plaintext = runtime_field_input_json(uid, job_token, organization_id, organization_tags, data_policy_agreed);
    let uuid = new_reverse_mask_uuid();
    let key_bytes = runtime_ascii_key(&uuid).into_bytes(); // US-ASCII 16 字节
    let ciphertext = aes_cbc_encrypt_pkcs7(plaintext.as_bytes(), &key_bytes, &key_bytes);
    let encrypted_key = rsa_encrypt_pkcs1v15(&key_bytes);
    RuntimeFields { encrypt_user_info: b64_std(&ciphertext), key: b64_std(&encrypted_key) }
}

/// §1.6：明文 JSON 固定字段序 uid → security_oauth_token → organization_id →
/// organization_tags → data_policy_agreed。⚠️ security_oauth_token 是 omitempty：
/// jobToken 为空串时**整个键缺席**（写空串上游会判 token 无效）；tags 空也输出 `[]`。
pub fn runtime_field_input_json(
    uid: &str,
    job_token: &str,
    organization_id: &str,
    organization_tags: &[String],
    data_policy_agreed: bool,
) -> String {
    let mut s = String::from("{\"uid\":");
    s.push_str(&json_quoted(uid));
    if !job_token.is_empty() {
        s.push_str(&format!(",\"security_oauth_token\":{}", json_quoted(job_token)));
    }
    s.push_str(&format!(",\"organization_id\":{}", json_quoted(organization_id)));
    let tags = organization_tags.iter().map(|t| json_quoted(t)).collect::<Vec<_>>().join(",");
    s.push_str(&format!(",\"organization_tags\":[{tags}]"));
    s.push_str(&format!(",\"data_policy_agreed\":{data_policy_agreed}}}"));
    s
}

// ------------------------------------------------------------------
// §1.7 UUID / 密钥派生
// ------------------------------------------------------------------

/// §1.7：16 字节整体反序，再置版本位 nibble=4（byte6 高 4 位）与变体位（byte8 高 2 位 = 10）。
pub fn reverse_mask_uuid(raw: &[u8; 16]) -> [u8; 16] {
    let mut v = *raw;
    v.reverse();
    v[6] = (v[6] & 0x0F) | 0x40;
    v[8] = (v[8] & 0x3F) | 0x80;
    v
}

/// §1.7：8-4-4-4-12 小写 hex（连字符在下标 8/13/18/23）。
pub fn format_lower_uuid(v: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, b) in v.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            s.push('-');
        }
        s.push(HEX_DIGITS[(b >> 4) as usize] as char);
        s.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
    }
    s
}

/// §1.7：AES 密钥/IV 原料 = uuid 前 8 字节的十六进制小写（16 ASCII 字符）。
pub fn runtime_ascii_key(uuid: &[u8; 16]) -> String {
    let mut s = String::with_capacity(16);
    for b in &uuid[..8] {
        s.push(HEX_DIGITS[(b >> 4) as usize] as char);
        s.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
    }
    s
}

/// 16 随机字节 → reverseMask（§1.6/§1.7 的 uuid 生成形态）。
pub fn new_reverse_mask_uuid() -> [u8; 16] {
    let mut raw = [0u8; 16];
    OsRng.fill_bytes(&mut raw);
    reverse_mask_uuid(&raw)
}

/// §1.7：每次请求**新生成**的 requestId（⚠️ 与 body 里的 request_id 是两个不同 UUID，§3.4）。
pub fn new_request_id() -> String {
    format_lower_uuid(&new_reverse_mask_uuid())
}

/// n 个随机字节（OsRng）。
pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    OsRng.fill_bytes(&mut v);
    v
}

// ------------------------------------------------------------------
// §1.8 PKCS7 填充 与 AES-CBC
// ------------------------------------------------------------------

/// §1.8：PKCS#7 填充，padLen = block - len%block（整块时补满一整块），padLen 个值为 padLen 的字节。
pub fn pkcs7_pad(src: &[u8], block_size: usize) -> Vec<u8> {
    assert!((1..=255).contains(&block_size), "PKCS#7 block size 非法: {block_size}");
    let pad_len = block_size - src.len() % block_size;
    let mut out = Vec::with_capacity(src.len() + pad_len);
    out.extend_from_slice(src);
    out.resize(src.len() + pad_len, pad_len as u8);
    out
}

/// pkcs7_pad 的逆：校验并剥除填充；非法返回 None（解密侧数据错误，不能 panic）。
pub fn pkcs7_unpad(padded: &[u8], block_size: usize) -> Option<&[u8]> {
    if block_size == 0 || padded.is_empty() || padded.len() % block_size != 0 {
        return None;
    }
    let pad_len = *padded.last()? as usize;
    if pad_len == 0 || pad_len > block_size || pad_len > padded.len() {
        return None;
    }
    if padded[padded.len() - pad_len..].iter().all(|b| *b as usize == pad_len) {
        Some(&padded[..padded.len() - pad_len])
    } else {
        None
    }
}

/// §1.8：AES-CBC 加密（先 PKCS7 补到 16 的倍数）。key ∈ {16,24,32}（本链路实际只用 AES-128），
/// IV 固定 16 字节；长度非法按 Kotlin `require` 语义直接 panic（属于调用方编程错误）。
pub fn aes_cbc_encrypt_pkcs7(plain: &[u8], key: &[u8], iv: &[u8]) -> Vec<u8> {
    assert!(matches!(key.len(), 16 | 24 | 32), "AES key 长度非法: {}", key.len());
    assert_eq!(iv.len(), 16, "AES-CBC IV 长度非法: {}", iv.len());
    let mut data = pkcs7_pad(plain, 16);
    match key.len() {
        16 => cbc_encrypt::<cbc::Encryptor<aes::Aes128>>(&mut data, key, iv),
        24 => cbc_encrypt::<cbc::Encryptor<aes::Aes192>>(&mut data, key, iv),
        _ => cbc_encrypt::<cbc::Encryptor<aes::Aes256>>(&mut data, key, iv),
    }
    data
}

/// AES-CBC 解密并剥除 PKCS7 填充；密文未对齐/填充非法返回 Err。
pub fn aes_cbc_decrypt_pkcs7(ciphertext: &[u8], key: &[u8], iv: &[u8]) -> Result<Vec<u8>, String> {
    if !matches!(key.len(), 16 | 24 | 32) {
        return Err(format!("AES key 长度非法: {}", key.len()));
    }
    if iv.len() != 16 {
        return Err(format!("AES-CBC IV 长度非法: {}", iv.len()));
    }
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(format!("AES-CBC 密文长度必须为 16 的倍数且非空: {}", ciphertext.len()));
    }
    let mut data = ciphertext.to_vec();
    match key.len() {
        16 => cbc_decrypt::<cbc::Decryptor<aes::Aes128>>(&mut data, key, iv),
        24 => cbc_decrypt::<cbc::Decryptor<aes::Aes192>>(&mut data, key, iv),
        _ => cbc_decrypt::<cbc::Decryptor<aes::Aes256>>(&mut data, key, iv),
    }
    pkcs7_unpad(&data, 16).map(|p| p.to_vec()).ok_or_else(|| "PKCS7 填充非法".to_string())
}

fn cbc_encrypt<C>(data: &mut [u8], key: &[u8], iv: &[u8])
where
    C: KeyIvInit + BlockEncryptMut<BlockSize = U16>,
{
    let mut c = C::new_from_slices(key, iv).expect("AES key/IV 长度非法");
    for chunk in data.chunks_mut(16) {
        c.encrypt_block_mut(GenericArray::from_mut_slice(chunk));
    }
}

fn cbc_decrypt<C>(data: &mut [u8], key: &[u8], iv: &[u8])
where
    C: KeyIvInit + BlockDecryptMut<BlockSize = U16>,
{
    let mut c = C::new_from_slices(key, iv).expect("AES key/IV 长度非法");
    for chunk in data.chunks_mut(16) {
        c.decrypt_block_mut(GenericArray::from_mut_slice(chunk));
    }
}

// ------------------------------------------------------------------
// §5 RSA-1024 PKCS#1 v1.5（内置公钥；EM = 0x00‖0x02‖PS(非零随机)‖0x00‖M，输出恰 128 字节）
// ------------------------------------------------------------------

/// §5 内置 Qoder 客户端公钥 modulus（1024-bit，128 字节大端 hex，已剔除前导符号字节 00）；指数 65537。
/// ⚠️ 换掉它上游就解不开 `Cosy-Key`，聊天直接 401。
const RSA_MODULUS_HEX: &str = "c0f22307e5cd362e296bb04470f6de8fbf935ce24e8fcf511a0e2701329769c4\
a76e499bb938036a52af1eaf818cf79a2600620e3ce87e371d2ca6d85803606a\
1b3fa5e874643c9ed2db7e85673ef7227fca56e2e7c08f0927609bb896a9f24b\
e1782099a66016a5bfdc3f1ff756bfc9e88d7b5dc5be30bf45a0223a00ebcecf";

fn runtime_public_key() -> RsaPublicKey {
    let n = BigUint::from_bytes_be(&hex_to_bytes(RSA_MODULUS_HEX));
    let e = BigUint::from(65537u32);
    RsaPublicKey::new(n, e).expect("内置 RSA 公钥构造失败")
}

/// §5：RSA-1024 PKCS#1 v1.5 加密。按文档采用 rsa crate 的 `Pkcs1v15Encrypt`——其内部 EM 构造
/// `0x00‖0x02‖PS(逐字节非零随机)‖0x00‖M` 与大端左补零到 k=128 的输出，与 Kotlin/Go 手写实现逐字节同构。
/// 加密对象是 keyStr 的 16 个 ASCII 字节（⚠️ 不是 AES 密文，也不是 16 个原始 uuid 字节）。
pub fn rsa_encrypt_pkcs1v15(message: &[u8]) -> Vec<u8> {
    let pub_key = runtime_public_key();
    let mut rng = OsRng;
    pub_key
        .encrypt(&mut rng, Pkcs1v15Encrypt, message)
        .unwrap_or_else(|e| panic!("RSA PKCS1v15 加密失败（消息长度须 ≤ k-11）: {e}"))
}

// ------------------------------------------------------------------
// 小工具（§1.2/§1.5/§2.3 的 b64std / md5 / hex）
// ------------------------------------------------------------------

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// 标准 base64（含 `=` pad、无换行，等价 Android Base64.NO_WRAP），§1.5/§1.6。
pub fn b64_std(bytes: &[u8]) -> String {
    general_purpose::STANDARD.encode(bytes)
}

/// 标准 base64 解码（调试/测试用）。
pub fn b64_std_decode(s: &str) -> Option<Vec<u8>> {
    general_purpose::STANDARD.decode(s).ok()
}

/// 小写 hex（每字节 `%02x`，无前导省略），§1.5 md5Hex / §2.3。
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX_DIGITS[(b >> 4) as usize] as char);
        s.push(HEX_DIGITS[(b & 0x0F) as usize] as char);
    }
    s
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let h: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(h.len() % 2 == 0, "hex 长度非法");
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).expect("hex 字符非法"))
        .collect()
}

/// UTF-8 字符串的 MD5 小写 hex（§1.5 签名摘要）。
pub fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    hex_lower(&h.finalize())
}

/// JSON 字符串字面量（含引号与转义），用于固定字段序手工拼串（§1.6/§1.5：唯一硬约束是
/// "参与 MD5 的字节串与发出的字节完全一致"，serde_json Map 不保序，故不经过 Map）。
fn json_quoted(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

// ------------------------------------------------------------------
// 测试：复刻 qoder-cosy.md §1.9 selfCheck 全部向量
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // §1.3：三段交换自反 + 钉死示例 abcdef→efcdab
    #[test]
    fn swap_outer_thirds_is_refl_and_pinned() {
        let probe = b"0123456789abcdef".to_vec();
        assert_eq!(swap_outer_thirds(&swap_outer_thirds(&probe)), probe);
        assert_eq!(swap_outer_thirds(b"abcdef"), b"efcdab".to_vec());
        // n<3 → 原样
        assert_eq!(swap_outer_thirds(b"ab"), b"ab".to_vec());
        assert_eq!(swap_outer_thirds(&[]), Vec::<u8>::new());
        // 多长度自反
        for n in 0..64 {
            let v: Vec<u8> = (0..n).map(|i| (i * 31 % 256) as u8).collect();
            assert_eq!(swap_outer_thirds(&swap_outer_thirds(&v)), v, "n={n} 不自反");
        }
    }

    // §1.2：字母表完整校验（64 字符、无重复、不含 pad 字符）
    #[test]
    fn body_alphabet_sane() {
        assert_eq!(BODY_ALPHABET.len(), 64);
        for (i, c) in BODY_ALPHABET.iter().enumerate() {
            assert_eq!(BODY_ALPHABET.iter().position(|x| x == c), Some(i), "重复字符 {c}");
        }
        assert!(!BODY_ALPHABET.contains(&BODY_PAD));
    }

    // §1.2/§1.4：自定义 base64 往返 + 钉死编码向量
    #[test]
    fn custom_b64_roundtrip_and_vectors() {
        // 向量（独立实现算出）：encode_body(0x66) = "$&$D"（2 个 '$'，位置在 0 和 2）
        let one = encode_body(&[0x66]);
        assert_eq!(one, b"$&$D".to_vec());
        assert_eq!(one.iter().filter(|b| **b == BODY_PAD).count(), 2);
        // 向量：encode_body("abc") = "MSK#"（4 字符、无 pad、全部在表内）
        let three = encode_body(b"abc");
        assert_eq!(three, b"MSK#".to_vec());
        assert!(!three.contains(&BODY_PAD));
        assert!(three.iter().all(|b| BODY_ALPHABET.contains(b)));
        // 往返：0..=22 每种长度 + 256 字节全表
        for n in 0..=22usize {
            let raw: Vec<u8> = (0..n).map(|i| ((i * 37 + 11) % 256) as u8).collect();
            let enc = custom_b64_encode(&raw);
            assert_eq!(enc.len() % 4, 0, "n={n} 输出长度须为 4 的倍数");
            assert_eq!(custom_b64_decode(&enc).as_deref(), Some(raw.as_slice()), "n={n} 往返失败");
        }
        let all: Vec<u8> = (0..=255u16).map(|i| i as u8).collect();
        assert_eq!(decode_body(&encode_body(&all)).as_deref(), Some(all.as_slice()));
        // 非法解码：%4==1 的残余、pad 后再有内容、表外字符都要拒绝
        assert_eq!(custom_b64_decode(b"A"), None);
        assert_eq!(custom_b64_decode(b"AB$$A"), None);
        assert_eq!(custom_b64_decode(b"AB=*"), None); // '=' 是标准表字符，不在自定义表内
        assert_eq!(custom_b64_decode(b"AB$$"), custom_b64_decode(b"AB")); // 尾部 pad 容忍
    }

    // §1.5：MD5 实现与签名拼接顺序（已知摘要钉死，等价于自算公式的外部校验）
    #[test]
    fn md5_and_signature_order() {
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        // 聊天向量（§1.5，python hashlib 独立算得）：5 字段 LF 顺序拼接、末尾无 \n
        assert_eq!(
            authorization("PAYLOAD", "KEY", "1700000000", "BODY", COSY_SIGNED_PATH),
            "Bearer COSY.PAYLOAD.1cb357ebbb77f182e8765b27eca93d24"
        );
        assert!(authorization_chat("PAYLOAD", "KEY", "1700000000", "BODY")
            .starts_with("Bearer COSY.PAYLOAD."));
        // 目录向量：GET 空 body → 连续两个 \n；且换签名路径必须改变签名（§1.9-4）
        assert_eq!(
            authorization_model_list("PAYLOAD", "KEY", "1700000000"),
            "Bearer COSY.PAYLOAD.e8c29e6ed6a442f5db540acf83580fb0"
        );
        assert_ne!(authorization_model_list("PAYLOAD", "KEY", "1700000000"), {
            authorization("PAYLOAD", "KEY", "1700000000", "", COSY_SIGNED_PATH)
        });
        // 末尾多一个 \n 摘要必须不同（§1.9-4 ⚠️ 最易写错处）
        assert_ne!(
            md5_hex("PAYLOAD\nKEY\n1700000000\nBODY\n/api/v2/service/pro/sse/agent_chat_generation\n"),
            md5_hex("PAYLOAD\nKEY\n1700000000\nBODY\n/api/v2/service/pro/sse/agent_chat_generation")
        );
    }

    // §1.5：authorizationPayload = 固定字段序 JSON → 标准 base64（= 可逐字节校验）
    #[test]
    fn authorization_payload_shape() {
        let b64 = authorization_payload("rid-1", "INF0", COSY_VERSION);
        let json = String::from_utf8(b64_std_decode(&b64).unwrap()).unwrap();
        assert_eq!(
            json,
            "{\"version\":\"v1\",\"requestId\":\"rid-1\",\"info\":\"INF0\",\"cosyVersion\":\"1.1.74\",\"ideVersion\":\"\"}"
        );
        assert_eq!(authorization_payload_default_version("rid-1", "INF0"), b64);
    }

    // §1.7：reverseMask UUID 掩码位 + 格式 + runtimeASCIIKey 等价关系
    #[test]
    fn uuid_masks_and_ascii_key() {
        let raw: [u8; 16] = std::array::from_fn(|i| (i + 1) as u8);
        let v = reverse_mask_uuid(&raw);
        assert_eq!(v[0], 0x10); // 整体反序（1..16 的末字节=0x10 挪到最前）
        assert_eq!(v[6] & 0xF0, 0x40); // 版本位 nibble=4
        assert_eq!(v[8] & 0xC0, 0x80); // 变体位 10
        let s = format_lower_uuid(&v);
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(s, s.to_lowercase());
        // §1.7：runtimeASCIIKey == 去横线后的前 16 hex
        let ascii = runtime_ascii_key(&v);
        assert_eq!(ascii.len(), 16);
        assert_eq!(ascii, s.replace('-', "")[..16]);
        // newRequestId 形态：version '4' 与变体位 nibble
        for _ in 0..8 {
            let id = new_request_id();
            assert_eq!(id.len(), 36);
            assert_eq!(&id[14..15], "4");
            assert!("89ab".contains(&id[19..20]));
        }
    }

    // §1.8/§1.9-6：PKCS7 向量
    #[test]
    fn pkcs7_vectors() {
        assert_eq!(pkcs7_pad(&[0u8; 16], 16).len(), 32); // 整块补满一整块
        assert_eq!(pkcs7_pad(&[0u8; 15], 16).len(), 16);
        let empty = pkcs7_pad(&[], 16);
        assert_eq!(empty.len(), 16);
        assert!(empty.iter().all(|b| *b == 16));
        assert_eq!(pkcs7_unpad(&empty, 16), Some(&[][..]));
        // 正向剥离：尾部 4 个 0x04 → 去掉 4 字节
        let mut ok = [0u8; 16];
        ok[12..].fill(4);
        assert_eq!(pkcs7_unpad(&ok, 16).map(|p| p.len()), Some(12));
        // 非法填充：长度不对齐 / 填充字节不一致 / padLen=0 / 空输入
        assert!(pkcs7_unpad(b"not padded", 16).is_none());
        let mut bad = [0u8; 16];
        bad[15] = 3;
        bad[14] = 0;
        assert!(pkcs7_unpad(&bad, 16).is_none());
        assert!(pkcs7_unpad(&[0u8; 16], 16).is_none()); // 最后一字节 0 非法
        assert!(pkcs7_unpad(&[], 16).is_none());
    }

    // §1.8：AES-CBC-PKCS7 —— 钉标准 CBC 向量（openssl/cryptography 双实现仲裁）+ 三密钥长度往返
    #[test]
    fn aes_cbc_roundtrip_and_nist() {
        // SP800-38A 风格 key/iv/plain（经 openssl 与 python cryptography 交叉验证）
        let key: Vec<u8> = hex_to_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: Vec<u8> = hex_to_bytes("000102030405060708090a0b0c0d0e0f");
        let pt = hex_to_bytes("6bc1bee22e409f96e93d7e117393172a");
        let ct = aes_cbc_encrypt_pkcs7(&pt, &key, &iv);
        assert_eq!(ct.len(), 32); // 16 字节明文 + 一整块 PKCS7 pad
        assert_eq!(hex_lower(&ct[..16]), "7649abac8119b246cee98e9b12e9197d"); // CBC 第一块
        assert_eq!(aes_cbc_decrypt_pkcs7(&ct, &key, &iv).unwrap(), pt);
        // 128/192/256 往返（含 UTF-8 多字节与非块对齐长度）
        for klen in [16usize, 24, 32] {
            let k = random_bytes(klen);
            let v = random_bytes(16);
            let plain = "你好，COSY！hello".as_bytes().to_vec();
            let enc = aes_cbc_encrypt_pkcs7(&plain, &k, &v);
            assert_eq!(enc.len() % 16, 0, "§1.9-7：AES 密文须 16 对齐");
            assert_eq!(aes_cbc_decrypt_pkcs7(&enc, &k, &v).unwrap(), plain, "klen={klen}");
            assert!(aes_cbc_decrypt_pkcs7(&enc[..15], &k, &v).is_err());
        }
        // 解密填充被篡改 → Err（不 panic）
        let mut bad = ct.clone();
        bad[16] ^= 0xFF;
        assert!(aes_cbc_decrypt_pkcs7(&bad, &key, &iv).is_err());
    }

    // §5：RSA 手工 EM（经 rsa crate 等价构造）—— 输出恰 128 字节、随机填充每次不同
    #[test]
    fn rsa_output_is_128_bytes_and_randomized() {
        let k1 = rsa_encrypt_pkcs1v15(b"0123456789abcdef");
        let k2 = rsa_encrypt_pkcs1v15(b"0123456789abcdef");
        assert_eq!(k1.len(), 128, "§1.9-7：RSA 密文必须是 128 字节（1024-bit）");
        assert_eq!(k2.len(), 128);
        assert_ne!(k1, k2, "PKCS#1 v1.5 随机填充须逐次不同");
    }

    // §1.6/§1.9-7：运行时字段整体形态 + omitempty 语义
    #[test]
    fn runtime_fields_and_omitempty() {
        let rf = generate_runtime_fields("u-1", "jt-1", "org-1", &[s("t1"), s("t2")], true);
        let enc = b64_std_decode(&rf.encrypt_user_info).unwrap();
        assert!(enc.len() % 16 == 0 && !enc.is_empty(), "AES 密文须为 16 对齐的非空块");
        assert_eq!(b64_std_decode(&rf.key).unwrap().len(), 128, "Cosy-Key 必须是 128 字节 RSA 密文");
        // omitempty：非空必含该键、空必缺席
        let with = runtime_field_input_json("u", "jt", "", &[], false);
        let without = runtime_field_input_json("u", "", "", &[], false);
        assert!(with.contains("\"security_oauth_token\":\"jt\""));
        assert!(!without.contains("security_oauth_token"));
        assert!(with.contains("\"organization_tags\":[]")); // 空 tags 也输出 []
        // 固定字段序（首尾键、键间顺序逐字符钉死）
        assert!(with.starts_with("{\"uid\":\"u\",\"security_oauth_token\":\"jt\",\"organization_id\":\"\""));
        assert!(with.ends_with(",\"organization_tags\":[],\"data_policy_agreed\":false}"));
    }

    fn s(v: &str) -> String {
        v.to_string()
    }
}
