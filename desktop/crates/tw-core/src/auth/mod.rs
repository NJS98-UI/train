//! auth 层：TraeAuthApi/TraeAuthStore/QoderAuthApi/QoderAuthStore 对应体，依据 auth-stores.md。
//! 本 wave 交付存储层（`store`/`model`）；API（网络登录/续期/campaigns）由后续 wave 在 api 模块补齐。

pub mod api;
pub mod model;
pub mod store;
