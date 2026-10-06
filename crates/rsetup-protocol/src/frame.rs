//! 握手帧边界（protocol_spec.md §4.1）。
//!
//! 本模块只做 8B 帧头编解码与有界长度：payload 为不透明原始字节，
//! 本模块不解析；帧解析不表示消息顺序已合法。

use thiserror::Error;

pub const FRAME_HEADER_LEN: usize = 8;
pub const FRAME_MAGIC: [u8; 2] = [0x53, 0x45];
pub const FRAME_VERSION: u8 = 0x02;
pub const MAX_PAYLOAD_LEN: usize = 16_384;
pub const MAX_FRAME_BYTES: usize = FRAME_HEADER_LEN + MAX_PAYLOAD_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    ClientHello = 0x01,
    ServerChallenge = 0x02,
    ClientAuthRequest = 0x03,
    ServerAuthResponse = 0x04,
    ServerPending = 0x05,
    PendingPong = 0x06,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Option<FrameType> {
        Some(match v {
            0x01 => FrameType::ClientHello,
            0x02 => FrameType::ServerChallenge,
            0x03 => FrameType::ClientAuthRequest,
            0x04 => FrameType::ServerAuthResponse,
            0x05 => FrameType::ServerPending,
            0x06 => FrameType::PendingPong,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrameError {
    /// 流式状态：尚未收满 8B 头；不是协议错误，接收方可继续收字节。
    #[error("incomplete frame header: have {have} bytes, need {need}")]
    Incomplete { have: usize, need: usize },
    #[error("bad magic")]
    BadMagic,
    #[error("bad version")]
    BadVersion,
    #[error("bad frame type: {0:#04x}")]
    BadFrameType(u8),
    #[error("payload too large: {len} > {max}")]
    PayloadTooLarge { len: u32, max: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub payload_len: u32,
}

pub fn encode_frame(frame_type: FrameType, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    // 先校验上限再分配。
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload.len() as u32,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&FRAME_MAGIC);
    out.push(FRAME_VERSION);
    out.push(frame_type as u8);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn parse_frame_header(buf: &[u8]) -> Result<FrameHeader, FrameError> {
    if buf.len() < FRAME_HEADER_LEN {
        return Err(FrameError::Incomplete {
            have: buf.len(),
            need: FRAME_HEADER_LEN - buf.len(),
        });
    }
    // 检查顺序固定：magic → version → frame type → payload length。
    if buf[0..2] != FRAME_MAGIC {
        return Err(FrameError::BadMagic);
    }
    if buf[2] != FRAME_VERSION {
        return Err(FrameError::BadVersion);
    }
    let frame_type = FrameType::from_u8(buf[3]).ok_or(FrameError::BadFrameType(buf[3]))?;
    let payload_len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if payload_len as usize > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload_len,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    Ok(FrameHeader {
        frame_type,
        payload_len,
    })
}

pub fn frame_is_complete(buf_len: usize, payload_len: u32) -> Result<bool, FrameError> {
    // 先校验上限再相加，防大声明长度的 `8 + len` 溢出。
    if payload_len as usize > MAX_PAYLOAD_LEN {
        return Err(FrameError::PayloadTooLarge {
            len: payload_len,
            max: MAX_PAYLOAD_LEN as u32,
        });
    }
    Ok(buf_len >= FRAME_HEADER_LEN + payload_len as usize)
}
