//! §4.3 签名输入字节布局与状态语义（纯函数，不透明字节）。
//!
//! 两条布局均以带版本标识的显式域分离前缀开头，之后按 §4.3 顺序原样连续拼接；
//! 除明确标出的 `BE32` 外不添加隐含前缀或填充。`descriptor_raw` 是
//! `CLIENT_AUTH_REQUEST` Payload 内嵌套 `DeviceDescriptor` 的**内容原字节**
//! （不透明 `&[u8]`，不含外层 key/长度 varint/帧头），本模块不解析、不重序列化。

use thiserror::Error;

/// 派生上限：ClientAuthRequest 固定开销 134B + 嵌套 tag 1B + 2B varint
/// ⇒ `134 + 1 + 2 + D ≤ 16384` ⇒ `D ≤ 16247`。本切片只作上界防御，
/// 不定义 descriptor 字段语义。
pub const MAX_DESCRIPTOR_WIRE_BYTES: usize = 16_247;

/// 域分离前缀长度：26B ASCII 标签 `|| 0x00 || 0x03`。
pub const SIG_DOMAIN_PREFIX_LEN: usize = 28;

/// §4.3 第 1 条客户端域分离前缀（逐字）：
/// `ASCII("rsetup-next/client-auth/v3") || 0x00 || 0x03`。
pub const CLIENT_SIG_DOMAIN_PREFIX: &[u8; SIG_DOMAIN_PREFIX_LEN] =
    b"rsetup-next/client-auth/v3\0\x03";

/// §4.3 第 2 条服务端域分离前缀（逐字）：
/// `ASCII("rsetup-next/server-auth/v3") || 0x00 || 0x03`。
pub const SERVER_SIG_DOMAIN_PREFIX: &[u8; SIG_DOMAIN_PREFIX_LEN] =
    b"rsetup-next/server-auth/v3\0\x03";

/// 客户端签名输入除 `descriptor_raw` 外的固定长度：
/// `28(前缀) + 32 + 32 + 32 + 32 + 4(BE32 descriptor_raw_len) = 160`。
pub const CLIENT_SIG_INPUT_BASE_LEN: usize = 160;

/// 服务端签名输入除 `descriptor_raw` 外的固定长度：
/// `160(同客户端前导) + 32(server_x25519_eph_pub) + 1(status) + 4(BE32 reason_code) = 197`。
pub const SERVER_SIG_INPUT_BASE_LEN: usize = 197;

/// §4.2/§4.3 `ServerAuthResponse.status`：0 = APPROVED。
pub const STATUS_APPROVED: u8 = 0;
/// §4.2/§4.3 `ServerAuthResponse.status`：1 = REJECTED。
pub const STATUS_REJECTED: u8 = 1;

/// §4.2 `reason_code` 枚举：`0 = UNSPECIFIED`。
pub const REASON_UNSPECIFIED: u32 = 0;
/// §4.2 `reason_code` 枚举：`1 = SIGNATURE_INVALID`。v3 中绝不作为发出值
/// （验签失败直接关闭 TCP，见 §4.5），因此不在 REJECTED 允许集合内。
pub const REASON_SIGNATURE_INVALID: u32 = 1;
/// §4.2 `reason_code` 枚举：`2 = REVOKED`。
pub const REASON_REVOKED: u32 = 2;
/// §4.2 `reason_code` 枚举：`3 = APPROVAL_DENIED`。
pub const REASON_APPROVAL_DENIED: u32 = 3;
/// §4.2 `reason_code` 枚举：`4 = HANDSHAKE_TIMEOUT`（仅握手步超时）。
pub const REASON_HANDSHAKE_TIMEOUT: u32 = 4;
/// §4.2 `reason_code` 枚举：`5 = SERVER_ERROR`。
pub const REASON_SERVER_ERROR: u32 = 5;

/// APPROVED(status=0) 唯一合法 `reason_code`：必须为 0。
const APPROVED_REASON_CODES: [u32; 1] = [REASON_UNSPECIFIED];

/// REJECTED(status=1) 允许集合：`{0, 2, 3, 4, 5}`；`1 = SIGNATURE_INVALID`
/// 虽在 §4.2 定义，但 v3 绝不发出。
const REJECTED_REASON_CODES: [u32; 5] = [
    REASON_UNSPECIFIED,
    REASON_REVOKED,
    REASON_APPROVAL_DENIED,
    REASON_HANDSHAKE_TIMEOUT,
    REASON_SERVER_ERROR,
];

/// 签名输入构造错误。变体集合固定且去敏：不回显公钥字节、descriptor 内容或
/// 任何身份标识，只给出触发约束的类别与不含敏感信息的数值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SigInputError {
    #[error("descriptor wire bytes too large: {len} > {max}")]
    DescriptorTooLarge { len: usize, max: usize },
    #[error("bad status byte: {0:#04x} (only 0x00/0x01 allowed)")]
    BadStatus(u8),
    #[error("bad reason code for status: {0}")]
    BadReasonCode(u32),
    #[error("server ephemeral public key violates the status-dependent zero constraint")]
    BadServerEphemeral,
}

/// §4.3 第 1 条客户端签名输入（板端 Ed25519 长期私钥签）：
/// `CLIENT_SIG_DOMAIN_PREFIX(28)`
/// `|| client_random(32) || server_random(32)`
/// `|| client_ed25519_pub(32) || client_x25519_eph_pub(32)`
/// `|| BE32(descriptor_raw_len) || descriptor_raw`。
///
/// 总长 `160 + D`；`descriptor_raw` 起始偏移固定为 160。
pub fn client_signature_input(
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_ed25519_pub: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    descriptor_raw: &[u8],
) -> Result<Vec<u8>, SigInputError> {
    check_descriptor(descriptor_raw)?;
    let mut out = Vec::with_capacity(CLIENT_SIG_INPUT_BASE_LEN + descriptor_raw.len());
    out.extend_from_slice(CLIENT_SIG_DOMAIN_PREFIX);
    out.extend_from_slice(client_random);
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_ed25519_pub);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(&descriptor_len_be32(descriptor_raw));
    out.extend_from_slice(descriptor_raw);
    Ok(out)
}

/// §4.3 第 2 条服务端签名输入（中控 Ed25519 长期私钥签，覆盖板端全部认证数据）：
/// `SERVER_SIG_DOMAIN_PREFIX(28)`
/// `|| server_random(32) || client_random(32)`
/// `|| client_ed25519_pub(32) || client_x25519_eph_pub(32)`
/// `|| BE32(descriptor_raw_len) || descriptor_raw`
/// `|| server_x25519_eph_pub(32) || status(1B) || BE32(reason_code)`。
///
/// 总长 `197 + D`；`descriptor_raw` 起始偏移 160，`server_x25519_eph_pub`
/// 起始偏移 `160 + D`，`status` 偏移 `192 + D`，末尾 4B 为 `BE32(reason_code)`
/// ——语义 uint32 的恰 4 字节大端整数，**不是** Protobuf wire varint。
///
/// 约束校验顺序固定（`status → reason_code → server_x25519_eph_pub →
/// descriptor 上限`），多项违例时报首个类别：
/// - `status` 只允许 0/1；
/// - `status = 0` ⇒ `reason_code = 0`；`status = 1` ⇒ `reason_code ∈ {0,2,3,4,5}`；
/// - `status = 0` ⇒ `server_x25519_eph_pub` 必非全零；`status = 1` ⇒ 必为全零 32B 占位。
// 8 参数签名由 protocol_spec.md §4.3 第 2 条的字段顺序逐字固定，不折叠为结构体。
#[allow(clippy::too_many_arguments)]
pub fn server_signature_input(
    server_random: &[u8; 32],
    client_random: &[u8; 32],
    client_ed25519_pub: &[u8; 32],
    client_x25519_eph_pub: &[u8; 32],
    descriptor_raw: &[u8],
    server_x25519_eph_pub: &[u8; 32],
    status: u8,
    reason_code: u32,
) -> Result<Vec<u8>, SigInputError> {
    check_status(status, reason_code, server_x25519_eph_pub)?;
    check_descriptor(descriptor_raw)?;
    let mut out = Vec::with_capacity(SERVER_SIG_INPUT_BASE_LEN + descriptor_raw.len());
    out.extend_from_slice(SERVER_SIG_DOMAIN_PREFIX);
    out.extend_from_slice(server_random);
    out.extend_from_slice(client_random);
    out.extend_from_slice(client_ed25519_pub);
    out.extend_from_slice(client_x25519_eph_pub);
    out.extend_from_slice(&descriptor_len_be32(descriptor_raw));
    out.extend_from_slice(descriptor_raw);
    out.extend_from_slice(server_x25519_eph_pub);
    out.push(status);
    out.extend_from_slice(&reason_code.to_be_bytes());
    Ok(out)
}

/// 检查顺序固定：`status` → `reason_code` → `server_x25519_eph_pub`。
fn check_status(status: u8, reason_code: u32, server_eph: &[u8; 32]) -> Result<(), SigInputError> {
    match status {
        STATUS_APPROVED => {
            if !APPROVED_REASON_CODES.contains(&reason_code) {
                return Err(SigInputError::BadReasonCode(reason_code));
            }
            if is_all_zero(server_eph) {
                return Err(SigInputError::BadServerEphemeral);
            }
            Ok(())
        }
        STATUS_REJECTED => {
            if !REJECTED_REASON_CODES.contains(&reason_code) {
                return Err(SigInputError::BadReasonCode(reason_code));
            }
            if !is_all_zero(server_eph) {
                return Err(SigInputError::BadServerEphemeral);
            }
            Ok(())
        }
        other => Err(SigInputError::BadStatus(other)),
    }
}

fn is_all_zero(v: &[u8; 32]) -> bool {
    v.iter().all(|b| *b == 0)
}

/// `BE32(descriptor_raw_len)`：长度已受 [`MAX_DESCRIPTOR_WIRE_BYTES`] 约束，
/// 转换无损且不得改用 varint。
fn descriptor_len_be32(descriptor_raw: &[u8]) -> [u8; 4] {
    debug_assert!(descriptor_raw.len() <= u32::MAX as usize);
    (descriptor_raw.len() as u32).to_be_bytes()
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
