use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_ENVELOPE_BYTES: usize = 524_288; // 512 KiB 完整序列化信封上限
pub const MAX_ACTION_BYTES: usize = 64;
pub const MAX_METADATA_ENTRIES: usize = 16;
pub const MAX_METADATA_KEY_BYTES: usize = 64;
pub const MAX_METADATA_VAL_BYTES: usize = 256;
pub const PING_NONCE_LEN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    Unknown = 0,
    Request = 1,
    Response = 2,
    Event = 3,
    System = 4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelPacket {
    pub trace_id: u64,
    pub kind: PacketType,
    pub action: String,
    pub status_code: i32,
    pub error_message: String,
    pub payload: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum WireError {
    #[error("unsupported business schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("action cannot be empty")]
    ActionEmpty,
    #[error("action too long: {len} > {max}")]
    ActionTooLong { len: usize, max: usize },
    #[error("action contains reserved tunnel prefix or invalid characters")]
    InvalidAction,
    #[error("packet type unknown or invalid")]
    InvalidPacketType,
    #[error("metadata count too large: {len} > {max}")]
    MetadataTooMany { len: usize, max: usize },
    #[error("metadata key too long: {len} > {max}")]
    MetadataKeyTooLong { len: usize, max: usize },
    #[error("metadata value too long: {len} > {max}")]
    MetadataValTooLong { len: usize, max: usize },
    #[error("envelope wire size too large: {len} > {max}")]
    EnvelopeTooLarge { len: usize, max: usize },
    #[error("invalid ping nonce length: {0} (want 16)")]
    InvalidPingNonceLength(usize),
    #[error("ping request must have empty metadata")]
    PingRequestMetadataNotEmpty,
    #[error("ping response must have status_code=0: {0}")]
    InvalidPingStatusCode(i32),
    #[error("ping response must have empty error_message and metadata")]
    InvalidPingResponseFields,
    #[error("trace_id cannot be zero")]
    InvalidTraceId,
    #[error("non-response packet cannot carry status_code ({status_code}) or error_message")]
    UnexpectedResponseFields { status_code: i32 },
}

pub fn validate_business_version(v: u32) -> Result<(), WireError> {
    if v == 1 {
        Ok(())
    } else {
        Err(WireError::UnsupportedVersion(v))
    }
}

/// 结构合法性；最终 512KiB 整包校验必须在获审 codec 的实际 wire 边界进行
pub fn validate_packet(packet: &TunnelPacket) -> Result<(), WireError> {
    if packet.trace_id == 0 {
        return Err(WireError::InvalidTraceId);
    }
    if packet.kind == PacketType::Unknown {
        return Err(WireError::InvalidPacketType);
    }
    if packet.kind != PacketType::Response
        && (packet.status_code != 0 || !packet.error_message.is_empty())
    {
        return Err(WireError::UnexpectedResponseFields {
            status_code: packet.status_code,
        });
    }
    if packet.action.is_empty() {
        return Err(WireError::ActionEmpty);
    }
    if packet.action.len() > MAX_ACTION_BYTES {
        return Err(WireError::ActionTooLong {
            len: packet.action.len(),
            max: MAX_ACTION_BYTES,
        });
    }
    // 校验 action 格式与保留前缀
    if packet.action.starts_with("tunnel.") {
        match packet.action.as_str() {
            "tunnel.kick" | "tunnel.revoke" => {
                if packet.kind != PacketType::System {
                    return Err(WireError::InvalidAction);
                }
            }
            "tunnel.ping" => {
                if packet.kind != PacketType::Request && packet.kind != PacketType::Response {
                    return Err(WireError::InvalidAction);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    } else {
        // 普通业务 action 必须是由点分隔的段组成，每段非空且仅含小写字母或数字，不得为 tunnel. 前缀
        if packet.action.starts_with('.')
            || packet.action.ends_with('.')
            || packet.action.split('.').any(|seg| {
                seg.is_empty()
                    || seg
                        .chars()
                        .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            })
        {
            return Err(WireError::InvalidAction);
        }
    }

    // 仅做不会误拒合法包的确定越界检测；非空 action 确保编码信封
    // 在 payload 之外至少占用 1 字节。接收端必须另在 codec 边界严格
    // 按真实整包 wire 长度 <= MAX_ENVELOPE_BYTES 审核。
    if packet.payload.len() >= MAX_ENVELOPE_BYTES {
        return Err(WireError::EnvelopeTooLarge {
            len: packet.payload.len(),
            max: MAX_ENVELOPE_BYTES,
        });
    }

    if packet.metadata.len() > MAX_METADATA_ENTRIES {
        return Err(WireError::MetadataTooMany {
            len: packet.metadata.len(),
            max: MAX_METADATA_ENTRIES,
        });
    }
    for (k, v) in &packet.metadata {
        if k.len() > MAX_METADATA_KEY_BYTES {
            return Err(WireError::MetadataKeyTooLong {
                len: k.len(),
                max: MAX_METADATA_KEY_BYTES,
            });
        }
        if v.len() > MAX_METADATA_VAL_BYTES {
            return Err(WireError::MetadataValTooLong {
                len: v.len(),
                max: MAX_METADATA_VAL_BYTES,
            });
        }
    }
    if packet.action == "tunnel.ping" {
        if packet.payload.len() != PING_NONCE_LEN {
            return Err(WireError::InvalidPingNonceLength(packet.payload.len()));
        }
        match packet.kind {
            PacketType::Request => {
                if !packet.metadata.is_empty() {
                    return Err(WireError::PingRequestMetadataNotEmpty);
                }
            }
            PacketType::Response => {
                if packet.status_code != 0 {
                    return Err(WireError::InvalidPingStatusCode(packet.status_code));
                }
                if !packet.error_message.is_empty() || !packet.metadata.is_empty() {
                    return Err(WireError::InvalidPingResponseFields);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    }
    Ok(())
}
