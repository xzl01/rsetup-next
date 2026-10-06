//! §4.3 签名输入 raw wire bytes（纯函数，不透明字节）。
//!
//! 两条布局均为直接拼接：不加长度前缀、不填充。`device_descriptor_wire_bytes`
//! 是帧内 DeviceDescriptor 的原始 wire bytes（不透明 `&[u8]`），本模块
//! 不解析、不重序列化。

use thiserror::Error;

/// 派生上限：ClientAuthRequest 固定开销 134B + 嵌套 tag 1B + 2B varint
/// ⇒ `134 + 1 + 2 + D ≤ 16384` ⇒ `D ≤ 16247`。本切片只作上界防御，
/// 不定义 descriptor 字段语义。
pub const MAX_DESCRIPTOR_WIRE_BYTES: usize = 16_247;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SigInputError {
    #[error("descriptor wire bytes too large: {len} > {max}")]
    DescriptorTooLarge { len: usize, max: usize },
    #[error("bad status byte: {0:#04x} (only 0x00/0x01 allowed)")]
    BadStatus(u8),
}

/// §4.3.1 客户端签名输入（板端 Ed25519 长期私钥签）：
/// `client_random(32) || server_random(32) || client_x25519_eph_pub(32) || descriptor_wire(D)`。
pub fn client_signature_input(
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
) -> Result<Vec<u8>, SigInputError> {
    check_descriptor(device_descriptor_wire_bytes)?;
    let mut out = Vec::with_capacity(96 + device_descriptor_wire_bytes.len());
    out.extend_from_slice(client_random);
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(device_descriptor_wire_bytes);
    Ok(out)
}

/// §4.3.2 服务端签名输入（中控 Ed25519 长期私钥签）：
/// `server_random(32) || client_random(32) || client_ed25519_pub(32) || client_x25519_eph_pub(32)`
/// `|| descriptor_wire(D) || server_x25519_eph_pub(32) || status(1B)`。
/// REJECTED 时 `server_x25519_eph_pub` 为全零 32B 占位；本函数按传入字节原样
/// 参与拼装，不做全零假设。
pub fn server_signature_input(
    server_random: &[u8; 32],
    client_random: &[u8; 32],
    client_ed25519_pub: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    device_descriptor_wire_bytes: &[u8],
    server_x25519_eph_pub: &[u8; 32],
    status: u8,
) -> Result<Vec<u8>, SigInputError> {
    // 检查顺序固定：先 status，后 descriptor 上限。
    if status > 1 {
        return Err(SigInputError::BadStatus(status));
    }
    check_descriptor(device_descriptor_wire_bytes)?;
    let mut out = Vec::with_capacity(161 + device_descriptor_wire_bytes.len());
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_random);
    out.extend_from_slice(client_ed25519_pub);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(device_descriptor_wire_bytes);
    out.extend_from_slice(server_x25519_eph_pub);
    out.push(status);
    Ok(out)
}

fn check_descriptor(d: &[u8]) -> Result<(), SigInputError> {
    if d.len() > MAX_DESCRIPTOR_WIRE_BYTES {
        return Err(SigInputError::DescriptorTooLarge {
            len: d.len(),
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        });
    }
    Ok(())
}
