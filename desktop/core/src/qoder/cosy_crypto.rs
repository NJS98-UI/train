// PORT-TODO(agent=qoder): 逐行移植 QoderCosyCrypto.kt 的全部常量与函数。
// 已预装依赖：md-5, sha1, sha2, hmac, aes, cbc, ecb, block-padding, rsa, num-bigint, base64, hex, rand。
// 要求：QoderUpstream / QoderAuthApi 用到的每个常量（COSY_VERSION、SCENE_*、NATIVE_*、
// MODEL_LIST_SIGNED_PATH 等）与函数（encode_body、authorization_payload、authorization、
// generate_runtime_fields + RuntimeFields 等）都必须 pub，语义逐字节一致。
pub fn self_check() {}
