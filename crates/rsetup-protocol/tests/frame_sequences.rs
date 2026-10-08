use rsetup_protocol::frame::FrameType;
use rsetup_protocol::handshake::{
    HandshakeError, ServerHandshakePhase, ServerHandshakeSM, TypedPongPayloadContext,
};

#[test]
fn strict_handshake_order_and_phase_progression() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.phase(), ServerHandshakePhase::Initial);

    // 1. Initial 阶段不能发 ServerChallenge
    assert_eq!(
        sm.on_send_server_challenge(),
        Err(HandshakeError::UnexpectedFrame(FrameType::ServerChallenge))
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);

    // 2. 正常接收 ClientHello -> AwaitingChallengeSend
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::AwaitingChallengeSend);

    // 3. 在 AwaitingChallengeSend 阶段不能重复接 ClientHello，也不能直接接 ClientAuthRequest
    let mut bad_sm = sm.clone();
    assert_eq!(
        bad_sm.on_frame(FrameType::ClientHello),
        Err(HandshakeError::UnexpectedFrame(FrameType::ClientHello))
    );
    assert_eq!(bad_sm.phase(), ServerHandshakePhase::Terminated);

    let mut bad_sm2 = sm.clone();
    assert_eq!(
        bad_sm2.on_frame(FrameType::ClientAuthRequest),
        Err(HandshakeError::UnexpectedFrame(
            FrameType::ClientAuthRequest
        ))
    );
    assert_eq!(bad_sm2.phase(), ServerHandshakePhase::Terminated);

    // 4. 发送 ServerChallenge -> AwaitingAuthRequest
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::AwaitingAuthRequest);

    // 5. 不能重复发送 ServerChallenge
    let mut bad_sm3 = sm.clone();
    assert_eq!(
        bad_sm3.on_send_server_challenge(),
        Err(HandshakeError::UnexpectedFrame(FrameType::ServerChallenge))
    );
    assert_eq!(bad_sm3.phase(), ServerHandshakePhase::Terminated);

    // 6. 接收 ClientAuthRequest -> EvaluatingAuth
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::EvaluatingAuth);

    // 7. 不能重复接收 ClientAuthRequest
    let mut bad_sm4 = sm.clone();
    assert_eq!(
        bad_sm4.on_frame(FrameType::ClientAuthRequest),
        Err(HandshakeError::UnexpectedFrame(
            FrameType::ClientAuthRequest
        ))
    );
    assert_eq!(bad_sm4.phase(), ServerHandshakePhase::Terminated);
}

#[test]
fn in_flight_probe_blocks_final_approval_until_settled() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));

    // 进入 Pending，首个 probe 在途
    let status_nonce = [0x11; 16];
    let probe_token1 = [0x22; 16];
    assert_eq!(sm.enter_pending(status_nonce, probe_token1), Ok(()));
    assert!(sm.is_probe_in_flight());

    // 在途 probe 未 pong 时，直接尝试 final approval 必须 fail-closed 报错并转 Terminated
    let mut probe_in_flight_sm = sm.clone();
    assert_eq!(
        probe_in_flight_sm.on_send_auth_response(true),
        Err(HandshakeError::ProbeInFlightBeforeFinalApproval)
    );
    assert_eq!(probe_in_flight_sm.phase(), ServerHandshakePhase::Terminated);

    // 但若是 REJECTED (approved = false)，根据 protocol_spec §4.1 / §4.5 吊销/拒绝优先，立即终止
    let mut reject_sm = sm.clone();
    assert_eq!(reject_sm.on_send_auth_response(false), Ok(()));
    assert_eq!(reject_sm.phase(), ServerHandshakePhase::Terminated);

    // 结清 probe 后再 final approval 可以正常进入 Completed
    let pong_ctx = TypedPongPayloadContext {
        status_nonce,
        pending_token: probe_token1,
        probe_seq: 1,
    };
    assert_eq!(sm.on_pong(&pong_ctx), Ok(()));
    assert!(!sm.is_probe_in_flight());
    assert_eq!(sm.on_send_auth_response(true), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::Completed);
}

#[test]
fn frame_only_pong_is_fail_closed_and_requires_typed_atomic_pong() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));

    let status_nonce = [0x55; 16];
    let probe_token1 = [0x66; 16];
    assert_eq!(sm.enter_pending(status_nonce, probe_token1), Ok(()));

    // frame-only on_frame(PendingPong) 必须 fail-closed，绝不能结清 probe
    assert_eq!(
        sm.on_frame(FrameType::PendingPong),
        Err(HandshakeError::PayloadVerificationUnimplemented)
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);
}

#[test]
fn typed_atomic_pong_checks_all_three_fields_and_strictly_increments_seq() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));

    let status_nonce = [0xaa; 16];
    let probe_token1 = [0xbb; 16];
    assert_eq!(sm.enter_pending(status_nonce, probe_token1), Ok(()));

    // 1. nonce 不匹配 -> PongNonceMismatch & Terminated
    let mut bad_nonce_sm = sm.clone();
    let bad_nonce = TypedPongPayloadContext {
        status_nonce: [0x00; 16],
        pending_token: probe_token1,
        probe_seq: 1,
    };
    assert_eq!(
        bad_nonce_sm.on_pong(&bad_nonce),
        Err(HandshakeError::PongNonceMismatch)
    );
    assert_eq!(bad_nonce_sm.phase(), ServerHandshakePhase::Terminated);

    // 2. token 不匹配 -> PongTokenMismatch & Terminated
    let mut bad_token_sm = sm.clone();
    let bad_token = TypedPongPayloadContext {
        status_nonce,
        pending_token: [0x00; 16],
        probe_seq: 1,
    };
    assert_eq!(
        bad_token_sm.on_pong(&bad_token),
        Err(HandshakeError::PongTokenMismatch)
    );
    assert_eq!(bad_token_sm.phase(), ServerHandshakePhase::Terminated);

    // 3. seq 不匹配 -> ProbeSeqOutOfOrder & Terminated
    let mut bad_seq_sm = sm.clone();
    let bad_seq = TypedPongPayloadContext {
        status_nonce,
        pending_token: probe_token1,
        probe_seq: 2,
    };
    assert_eq!(
        bad_seq_sm.on_pong(&bad_seq),
        Err(HandshakeError::ProbeSeqOutOfOrder {
            expected: 1,
            actual: 2
        })
    );
    assert_eq!(bad_seq_sm.phase(), ServerHandshakePhase::Terminated);

    // 4. 正确匹配结清第一轮
    let valid_pong = TypedPongPayloadContext {
        status_nonce,
        pending_token: probe_token1,
        probe_seq: 1,
    };
    assert_eq!(sm.on_pong(&valid_pong), Ok(()));
    assert!(!sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 2);

    // 5. 发送第二轮 probe：禁止跳号（必须严格等于 next_probe_seq = 2）
    let mut bad_jump_sm = sm.clone();
    assert_eq!(
        bad_jump_sm.on_send_server_pending_probe(3, [0xcc; 16]),
        Err(HandshakeError::ProbeSeqOutOfOrder {
            expected: 2,
            actual: 3
        })
    );
    assert_eq!(bad_jump_sm.phase(), ServerHandshakePhase::Terminated);

    // 6. 正常发送第二轮 probe
    let probe_token2 = [0xcc; 16];
    assert_eq!(sm.on_send_server_pending_probe(2, probe_token2), Ok(()));
    assert!(sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 2);

    // 第二轮正确 pong 结清
    let valid_pong2 = TypedPongPayloadContext {
        status_nonce,
        pending_token: probe_token2,
        probe_seq: 2,
    };
    assert_eq!(sm.on_pong(&valid_pong2), Ok(()));
    assert!(!sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 3);
}

#[test]
fn final_decision_distinguishes_completed_and_terminated() {
    // 场景 A: Direct approved 从 EvaluatingAuth 完成
    let mut sm1 = ServerHandshakeSM::new();
    assert_eq!(sm1.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm1.on_send_server_challenge(), Ok(()));
    assert_eq!(sm1.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm1.phase(), ServerHandshakePhase::EvaluatingAuth);
    assert_eq!(sm1.on_send_auth_response(true), Ok(()));
    assert_eq!(sm1.phase(), ServerHandshakePhase::Completed);

    // 场景 B: Direct rejected 从 EvaluatingAuth 拒绝
    let mut sm2 = ServerHandshakeSM::new();
    assert_eq!(sm2.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm2.on_send_server_challenge(), Ok(()));
    assert_eq!(sm2.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm2.on_send_auth_response(false), Ok(()));
    assert_eq!(sm2.phase(), ServerHandshakePhase::Terminated);

    // 场景 C: Pending approved 结清后完成
    let mut sm3 = ServerHandshakeSM::new();
    assert_eq!(sm3.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm3.on_send_server_challenge(), Ok(()));
    assert_eq!(sm3.on_frame(FrameType::ClientAuthRequest), Ok(()));
    let status_nonce = [0x12; 16];
    let probe_token = [0x34; 16];
    assert_eq!(sm3.enter_pending(status_nonce, probe_token), Ok(()));
    let pong = TypedPongPayloadContext {
        status_nonce,
        pending_token: probe_token,
        probe_seq: 1,
    };
    assert_eq!(sm3.on_pong(&pong), Ok(()));
    assert_eq!(sm3.on_send_auth_response(true), Ok(()));
    assert_eq!(sm3.phase(), ServerHandshakePhase::Completed);
}

#[test]
fn server_outbound_frames_must_never_be_received_from_client() {
    let outbound_types = [
        FrameType::ServerChallenge,
        FrameType::ServerAuthResponse,
        FrameType::ServerPending,
    ];

    for ft in outbound_types {
        let mut sm = ServerHandshakeSM::new();
        assert_eq!(
            sm.on_frame(ft),
            Err(HandshakeError::ServerOutboundFrame(ft))
        );
        assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);
    }
}

#[test]
fn probe_seq_wraparound_or_max_rejected() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    let status_nonce = [0x77; 16];
    let token = [0x88; 16];
    assert_eq!(sm.enter_pending(status_nonce, token), Ok(()));
    let pong = TypedPongPayloadContext {
        status_nonce,
        pending_token: token,
        probe_seq: 1,
    };
    assert_eq!(sm.on_pong(&pong), Ok(()));

    // u64::MAX 作为 seq 必须被拒绝
    assert_eq!(
        sm.on_send_server_pending_probe(u64::MAX, [0x99; 16]),
        Err(HandshakeError::ProbeSeqWrapAround(u64::MAX))
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);
}

#[test]
fn terminal_phases_cannot_receive_subsequent_handshake_frames() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm.on_send_auth_response(true), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::Completed);

    // 完成后收到任何握手帧都必须拒绝，且转移为 Terminated
    assert_eq!(
        sm.on_frame(FrameType::ClientHello),
        Err(HandshakeError::AlreadyFinished(
            ServerHandshakePhase::Completed
        ))
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);

    // 终止态再次收到握手帧同样拒绝
    assert_eq!(
        sm.on_frame(FrameType::ClientHello),
        Err(HandshakeError::AlreadyFinished(
            ServerHandshakePhase::Terminated
        ))
    );
}

#[test]
fn typed_pong_payload_context_validation() {
    let sm = ServerHandshakeSM::new();
    let expected = TypedPongPayloadContext {
        pending_token: [0x11; 16],
        status_nonce: [0x22; 16],
        probe_seq: 1,
    };
    let matching = TypedPongPayloadContext {
        pending_token: [0x11; 16],
        status_nonce: [0x22; 16],
        probe_seq: 1,
    };
    let mismatched_token = TypedPongPayloadContext {
        pending_token: [0x99; 16],
        status_nonce: [0x22; 16],
        probe_seq: 1,
    };
    let mismatched_nonce = TypedPongPayloadContext {
        pending_token: [0x11; 16],
        status_nonce: [0x33; 16],
        probe_seq: 1,
    };
    let mismatched_seq = TypedPongPayloadContext {
        pending_token: [0x11; 16],
        status_nonce: [0x22; 16],
        probe_seq: 2,
    };

    assert_eq!(sm.verify_pong_payload(&expected, &matching), Ok(()));
    assert_eq!(
        sm.verify_pong_payload(&expected, &mismatched_token),
        Err(HandshakeError::PongTokenMismatch)
    );
    assert_eq!(
        sm.verify_pong_payload(&expected, &mismatched_nonce),
        Err(HandshakeError::PongNonceMismatch)
    );
    assert_eq!(
        sm.verify_pong_payload(&expected, &mismatched_seq),
        Err(HandshakeError::ProbeSeqOutOfOrder {
            expected: 1,
            actual: 2
        })
    );
}
