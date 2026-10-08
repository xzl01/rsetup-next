//! 服务端握手帧序列状态机纯模型（protocol_spec.md §4.1, §4.5）。
//!
//! 关键规则：
//! 1. 严格消息序列：Initial(收 CLIENT_HELLO) -> 回送 SERVER_CHALLENGE -> AwaitingAuthRequest(收 CLIENT_AUTH_REQUEST)
//! 2. `SERVER_CHALLENGE` 与 `SERVER_AUTH_RESPONSE` 是服务端出站帧，绝不得作为客户端入站帧接收。
//! 3. 进入 PENDING 前序约束：必须在收到并验证合法 `CLIENT_AUTH_REQUEST` 后方可进入。
//! 4. PENDING 期间单连接仅允许一个在途 probe；`probe_seq` 从 1 开始严格单调递增，不得回绕。
//! 5. 完成/终止终态（Completed / Terminated）不可接任何后续握手帧。
//! 6. frame-only API 无法验证 Protobuf payload 中携带的 pong token/status_nonce/probe_seq；
//!    本模块明确只做纯帧序列状态机，任何 payload 级上下文核验必须标记为未实现或交由 typed payload context。

pub use crate::admission::ServerHandshakePhase;
use crate::frame::FrameType;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandshakeError {
    #[error("unexpected frame type: {0:?}")]
    UnexpectedFrame(FrameType),
    #[error("server outbound frame received from client: {0:?}")]
    ServerOutboundFrame(FrameType),
    #[error("probe sequence overflow or wrap around: {0}")]
    ProbeSeqWrapAround(u64),
    #[error("probe sequence out of order: expected {expected}, got {actual}")]
    ProbeSeqOutOfOrder { expected: u64, actual: u64 },
    #[error("concurrent probe in flight")]
    ProbeAlreadyInFlight,
    #[error("no probe in flight to settle")]
    NoProbeInFlight,
    #[error("handshake already finished in phase: {0:?}")]
    AlreadyFinished(ServerHandshakePhase),
    #[error("payload level verification not implemented in frame-only API")]
    PayloadVerificationUnimplemented,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedPongPayloadContext {
    pub pending_token: [u8; 16],
    pub status_nonce: [u8; 16],
    pub probe_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHandshakeSM {
    phase: ServerHandshakePhase,
    probe_in_flight: bool,
    next_probe_seq: u64,
}

impl Default for ServerHandshakeSM {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHandshakeSM {
    pub fn new() -> Self {
        Self {
            phase: ServerHandshakePhase::Initial,
            probe_in_flight: false,
            next_probe_seq: 1,
        }
    }

    pub fn phase(&self) -> ServerHandshakePhase {
        self.phase
    }

    pub fn is_probe_in_flight(&self) -> bool {
        self.probe_in_flight
    }

    pub fn next_probe_seq(&self) -> u64 {
        self.next_probe_seq
    }

    /// 服务端出站发送 ServerChallenge（仅在收到 ClientHello 之后由 Initial 迁移至 AwaitingAuthRequest）。
    /// 如果在非预期阶段调用，进入 Terminated 并报错。
    pub fn on_send_server_challenge(&mut self) -> Result<(), HandshakeError> {
        match self.phase {
            ServerHandshakePhase::Initial => {
                // 不能在未收到 ClientHello 时由服务端盲发 Challenge
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(FrameType::ServerChallenge))
            }
            ServerHandshakePhase::AwaitingAuthRequest => Ok(()),
            ServerHandshakePhase::Completed | ServerHandshakePhase::Terminated => {
                let err = HandshakeError::AlreadyFinished(self.phase);
                self.phase = ServerHandshakePhase::Terminated;
                Err(err)
            }
            _ => {
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(FrameType::ServerChallenge))
            }
        }
    }

    /// 标记进入 PENDING 阶段。
    /// 前序约束：只有在已经收到 ClientAuthRequest 之后（处于 AwaitingAuthRequest 阶段完成初始检查）才允许进入 InPending。
    /// 首个 ServerPending 即为第一轮 probe（probe_seq = 1），标记 probe_in_flight = true。
    pub fn enter_pending(&mut self) -> Result<(), HandshakeError> {
        match self.phase {
            ServerHandshakePhase::AwaitingAuthRequest => {
                self.phase = ServerHandshakePhase::InPending;
                self.probe_in_flight = true;
                self.next_probe_seq = 1;
                Ok(())
            }
            ServerHandshakePhase::Completed | ServerHandshakePhase::Terminated => {
                let err = HandshakeError::AlreadyFinished(self.phase);
                self.phase = ServerHandshakePhase::Terminated;
                Err(err)
            }
            _ => {
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(FrameType::ServerPending))
            }
        }
    }

    /// 服务端发送后续 SERVER_PENDING probe。
    /// 约束：
    /// - 必须处于 InPending
    /// - 单连接仅允许一个在途 probe（上一轮未结清不得发送新 probe）
    /// - probe_seq 从 1 开始单调递增，不得回绕（u64 溢出直接拒绝）
    pub fn on_send_server_pending_probe(&mut self, probe_seq: u64) -> Result<(), HandshakeError> {
        if self.phase == ServerHandshakePhase::Completed
            || self.phase == ServerHandshakePhase::Terminated
        {
            let err = HandshakeError::AlreadyFinished(self.phase);
            self.phase = ServerHandshakePhase::Terminated;
            return Err(err);
        }
        if self.phase != ServerHandshakePhase::InPending {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::UnexpectedFrame(FrameType::ServerPending));
        }
        if self.probe_in_flight {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeAlreadyInFlight);
        }
        if probe_seq < self.next_probe_seq {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeSeqOutOfOrder {
                expected: self.next_probe_seq,
                actual: probe_seq,
            });
        }
        if probe_seq == u64::MAX {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeSeqWrapAround(probe_seq));
        }

        self.probe_in_flight = true;
        self.next_probe_seq = probe_seq;
        Ok(())
    }

    /// 服务端发送 ServerAuthResponse 结束握手
    pub fn on_send_auth_response(&mut self) -> Result<(), HandshakeError> {
        match self.phase {
            ServerHandshakePhase::AwaitingAuthRequest | ServerHandshakePhase::InPending => {
                self.phase = ServerHandshakePhase::Completed;
                self.probe_in_flight = false;
                Ok(())
            }
            ServerHandshakePhase::Completed | ServerHandshakePhase::Terminated => {
                let err = HandshakeError::AlreadyFinished(self.phase);
                self.phase = ServerHandshakePhase::Terminated;
                Err(err)
            }
            _ => {
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(
                    FrameType::ServerAuthResponse,
                ))
            }
        }
    }

    /// 处理客户端入站帧：
    /// 1. 服务端出站帧（ServerChallenge / ServerAuthResponse）绝不得作为客户端入站帧接收。
    /// 2. 终止态 / 完成态不可接任何后续握手帧。
    /// 3. Initial 阶段只接受 ClientHello。
    /// 4. AwaitingAuthRequest 阶段只接受 ClientAuthRequest。
    /// 5. InPending 阶段只接受 PendingPong。注意：frame-only API 无法验证 Protobuf payload 中携带的 pong token/status_nonce/probe_seq，
    ///    因此仅在此更新帧序列状态（结清 probe_in_flight，并使 next_probe_seq 单调步进）。
    pub fn on_frame(&mut self, frame_type: FrameType) -> Result<(), HandshakeError> {
        // 完成/终止态不可接后续握手帧
        if self.phase == ServerHandshakePhase::Completed
            || self.phase == ServerHandshakePhase::Terminated
        {
            let err = HandshakeError::AlreadyFinished(self.phase);
            self.phase = ServerHandshakePhase::Terminated;
            return Err(err);
        }

        // 服务端出站帧绝不得从客户端入站
        if frame_type == FrameType::ServerChallenge
            || frame_type == FrameType::ServerAuthResponse
            || frame_type == FrameType::ServerPending
        {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ServerOutboundFrame(frame_type));
        }

        match (self.phase, frame_type) {
            (ServerHandshakePhase::Initial, FrameType::ClientHello) => {
                self.phase = ServerHandshakePhase::AwaitingAuthRequest;
                Ok(())
            }
            (ServerHandshakePhase::AwaitingAuthRequest, FrameType::ClientAuthRequest) => {
                // 收到认证请求，保持在 AwaitingAuthRequest 等待后续签名验证/准入决策（可转 InPending 或 Completed）
                Ok(())
            }
            (ServerHandshakePhase::InPending, FrameType::PendingPong) => {
                if !self.probe_in_flight {
                    self.phase = ServerHandshakePhase::Terminated;
                    return Err(HandshakeError::NoProbeInFlight);
                }
                // 收到 pong，结清在途 probe，准备下一轮
                self.probe_in_flight = false;
                if let Some(next) = self.next_probe_seq.checked_add(1) {
                    self.next_probe_seq = next;
                } else {
                    self.phase = ServerHandshakePhase::Terminated;
                    return Err(HandshakeError::ProbeSeqWrapAround(self.next_probe_seq));
                }
                Ok(())
            }
            _ => {
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(frame_type))
            }
        }
    }

    /// 当使用带 typed payload context 的验证时，对 PendingPong 载荷进行严格核验。
    /// 注意：由于目前是在协议纯序列切片，若没有 typed payload context 则应通过此显式方法表明区别。
    pub fn verify_pong_payload(
        &self,
        expected: &TypedPongPayloadContext,
        actual: &TypedPongPayloadContext,
    ) -> Result<(), HandshakeError> {
        if expected.status_nonce != actual.status_nonce
            || expected.pending_token != actual.pending_token
        {
            return Err(HandshakeError::UnexpectedFrame(FrameType::PendingPong));
        }
        if actual.probe_seq != expected.probe_seq {
            return Err(HandshakeError::ProbeSeqOutOfOrder {
                expected: expected.probe_seq,
                actual: actual.probe_seq,
            });
        }
        Ok(())
    }
}
