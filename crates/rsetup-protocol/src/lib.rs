//! rsetup-protocol：板端/中控设备协议字节契约（纯函数，无 IO，无全局状态）。
//!
//! 本切片只覆盖帧边界（protocol_spec.md §4.1）、签名输入 raw wire bytes（§4.3）
//! 与 ring Ed25519 签/验薄封装；不含 Protobuf codec、网络、设备 DB。
//! 其交付不构成任何握手步骤，更不构成握手完成。

pub mod ed25519;
pub mod frame;
pub mod sig;
