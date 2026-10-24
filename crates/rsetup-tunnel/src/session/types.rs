//! 会话状态公共类型；不持有网络或设备私钥。
use bytes::Bytes;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamEpoch(pub u64);
impl StreamEpoch {
    pub fn checked_next(self) -> Result<Self, RequestError> {
        self.0.checked_add(1).map(Self).ok_or(RequestError::Closed)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Reply {
    pub status_code: i32,
    pub error_message: String,
    pub payload: Bytes,
}
impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("status_code", &self.status_code)
            .field("error_message", &"<redacted>")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    #[error("session request budget exhausted")]
    Busy,
    #[error("session request deadline elapsed; outcome is unknown")]
    Timeout,
    #[error("session is closed")]
    Closed,
    #[error("response or request context violates the protocol")]
    ProtocolMismatch,
    #[error("process trace identifier space exhausted")]
    TraceExhausted,
    #[error("stream generation is closed")]
    StreamClosed,
}
