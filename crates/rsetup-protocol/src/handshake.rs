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
    #[error("in-flight probe must be settled before final approval")]
    ProbeInFlightBeforeFinalApproval,
    #[error("pong verification failed: status_nonce mismatch")]
    PongNonceMismatch,
    #[error("pong verification failed: pending_token mismatch")]
    PongTokenMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedPendingContext {
    pub status_nonce: [u8; 16],
    pub pending_token: [u8; 16],
    pub probe_seq: u64,
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
    pending_ctx: Option<TypedPendingContext>,
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
            pending_ctx: None,
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

    pub fn current_pending_context(&self) -> Option<&TypedPendingContext> {
        self.pending_ctx.as_ref()
    }

    /// 标记进入 PENDING 阶段。
    /// 前序约束：只有在已经收到 ClientAuthRequest 之后（处于 EvaluatingAuth 阶段完成初始检查）才允许进入 InPending。
    /// 首个 ServerPending 即为第一轮 probe（probe_seq = 1），标记 probe_in_flight = true。
    /// 必须提供初始 typed pending 上下文（status_nonce 与 pending_token）。
    pub fn enter_pending(
        &mut self,
        status_nonce: [u8; 16],
        pending_token: [u8; 16],
    ) -> Result<(), HandshakeError> {
        match self.phase {
            ServerHandshakePhase::EvaluatingAuth => {
                self.phase = ServerHandshakePhase::InPending;
                self.probe_in_flight = true;
                self.next_probe_seq = 1;
                self.pending_ctx = Some(TypedPendingContext {
                    status_nonce,
                    pending_token,
                    probe_seq: 1,
                });
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
    /// - probe_seq 从 1 开始单调递增，且必须等于 expected next_probe_seq，不得跳号或回绕
    /// - probe_seq == u64::MAX 溢出直接拒绝
    /// - 必须原子更新内部 pending_token 为新 probe token，同时保持 status_nonce 严格不变
    pub fn on_send_server_pending_probe(
        &mut self,
        probe_seq: u64,
        next_pending_token: [u8; 16],
    ) -> Result<(), HandshakeError> {
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
        if probe_seq == u64::MAX {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeSeqWrapAround(probe_seq));
        }
        if probe_seq != self.next_probe_seq {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeSeqOutOfOrder {
                expected: self.next_probe_seq,
                actual: probe_seq,
            });
        }

        let ctx = self.pending_ctx.as_mut().ok_or_else(|| {
            self.phase = ServerHandshakePhase::Terminated;
            HandshakeError::NoProbeInFlight
        })?;
        ctx.pending_token = next_pending_token;
        ctx.probe_seq = probe_seq;

        self.probe_in_flight = true;
        Ok(())
    }

    /// 收到并核验客户端回显的 typed PendingPong。
    /// 原子比较三字段：status_nonce 逐字相等、pending_token 逐字相等、probe_seq == expected。
    /// 匹配才结清 probe_in_flight 并将 next_probe_seq 严格 + 1。
    /// 失配则 fail-closed，进入 Terminated 并返回具体错误。
    pub fn on_pong(&mut self, actual: &TypedPongPayloadContext) -> Result<(), HandshakeError> {
        if self.phase == ServerHandshakePhase::Completed
            || self.phase == ServerHandshakePhase::Terminated
        {
            let err = HandshakeError::AlreadyFinished(self.phase);
            self.phase = ServerHandshakePhase::Terminated;
            return Err(err);
        }
        if self.phase != ServerHandshakePhase::InPending {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::UnexpectedFrame(FrameType::PendingPong));
        }
        if !self.probe_in_flight {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::NoProbeInFlight);
        }

        let ctx = match self.pending_ctx.as_ref() {
            Some(c) => c,
            None => {
                self.phase = ServerHandshakePhase::Terminated;
                return Err(HandshakeError::NoProbeInFlight);
            }
        };

        if actual.status_nonce != ctx.status_nonce {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::PongNonceMismatch);
        }
        if actual.pending_token != ctx.pending_token {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::PongTokenMismatch);
        }
        if actual.probe_seq != ctx.probe_seq {
            self.phase = ServerHandshakePhase::Terminated;
            return Err(HandshakeError::ProbeSeqOutOfOrder {
                expected: ctx.probe_seq,
                actual: actual.probe_seq,
            });
        }

        // 校验通过：结清在途 probe，准备下一轮
        self.probe_in_flight = false;
        if let Some(next) = self.next_probe_seq.checked_add(1) {
            self.next_probe_seq = next;
            Ok(())
        } else {
            self.phase = ServerHandshakePhase::Terminated;
            Err(HandshakeError::ProbeSeqWrapAround(self.next_probe_seq))
        }
    }

    /// 服务端出站发送 ServerChallenge（仅在 Initial 阶段收到 ClientHello 之后由 AwaitingChallengeSend 迁移至 AwaitingAuthRequest）。
    /// 如果在非预期阶段调用，进入 Terminated 并报错。
    pub fn on_send_server_challenge(&mut self) -> Result<(), HandshakeError> {
        match self.phase {
            ServerHandshakePhase::AwaitingChallengeSend => {
                self.phase = ServerHandshakePhase::AwaitingAuthRequest;
                Ok(())
            }
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

    /// 服务端做最终决策发送 ServerAuthResponse：
    /// - approved 为 true 时：必须在已经收到 ClientAuthRequest 后（EvaluatingAuth 或 InPending 且无在途 probe），迁移至 Completed。
    ///   若在 InPending 期间仍有在途 probe，禁止 final approved，必须 fail-closed 报错并转移至 Terminated。
    /// - approved 为 false 时（REJECTED 分支）：根据 protocol_spec §4.1 / §4.5，拒绝为终结状态，进入 Terminated。
    pub fn on_send_auth_response(&mut self, approved: bool) -> Result<(), HandshakeError> {
        if self.phase == ServerHandshakePhase::Completed
            || self.phase == ServerHandshakePhase::Terminated
        {
            let err = HandshakeError::AlreadyFinished(self.phase);
            self.phase = ServerHandshakePhase::Terminated;
            return Err(err);
        }

        match (self.phase, approved) {
            (ServerHandshakePhase::EvaluatingAuth, true) => {
                self.phase = ServerHandshakePhase::Completed;
                self.probe_in_flight = false;
                Ok(())
            }
            (ServerHandshakePhase::InPending, true) => {
                if self.probe_in_flight {
                    self.phase = ServerHandshakePhase::Terminated;
                    return Err(HandshakeError::ProbeInFlightBeforeFinalApproval);
                }
                self.phase = ServerHandshakePhase::Completed;
                Ok(())
            }
            (ServerHandshakePhase::EvaluatingAuth, false)
            | (ServerHandshakePhase::InPending, false) => {
                self.phase = ServerHandshakePhase::Terminated;
                self.probe_in_flight = false;
                Ok(())
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
    /// 1. 服务端出站帧（ServerChallenge / ServerAuthResponse / ServerPending）绝不得作为客户端入站帧接收。
    /// 2. 终止态 / 完成态不可接任何后续握手帧。
    /// 3. Initial 阶段只接受 ClientHello，迁移至 AwaitingChallengeSend。
    /// 4. AwaitingAuthRequest 阶段只接受 ClientAuthRequest，迁移至 EvaluatingAuth。
    /// 5. InPending 阶段收到 frame-only PendingPong 必须 fail-closed（返回 PayloadVerificationUnimplemented 并转移到 Terminated），
    ///    绝不能未验证 token/nonce/seq 即提前结清在途 probe。必须通过 `on_pong(&TypedPongPayloadContext)` 进行原子核验。
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
                self.phase = ServerHandshakePhase::AwaitingChallengeSend;
                Ok(())
            }
            (ServerHandshakePhase::AwaitingAuthRequest, FrameType::ClientAuthRequest) => {
                self.phase = ServerHandshakePhase::EvaluatingAuth;
                Ok(())
            }
            (ServerHandshakePhase::InPending, FrameType::PendingPong) => {
                // frame-only Pong 必须 fail-closed 不能更新状态
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::PayloadVerificationUnimplemented)
            }
            _ => {
                self.phase = ServerHandshakePhase::Terminated;
                Err(HandshakeError::UnexpectedFrame(frame_type))
            }
        }
    }

    /// 当使用外部 typed payload context 比较两组数据时，核验 PendingPong 载荷。
    /// 注意：若与状态机内部绑定的 context 校验，请直接使用原子方法 `sm.on_pong(actual)`。
    pub fn verify_pong_payload(
        &self,
        expected: &TypedPongPayloadContext,
        actual: &TypedPongPayloadContext,
    ) -> Result<(), HandshakeError> {
        if expected.status_nonce != actual.status_nonce {
            return Err(HandshakeError::PongNonceMismatch);
        }
        if expected.pending_token != actual.pending_token {
            return Err(HandshakeError::PongTokenMismatch);
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
