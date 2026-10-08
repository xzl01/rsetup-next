use rsetup_protocol::frame::FrameType;
use rsetup_protocol::handshake::{
    HandshakeError, ServerHandshakePhase, ServerHandshakeSM, TypedPongPayloadContext,
};

#[test]
fn rejects_out_of_order_handshake_frames() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.phase(), ServerHandshakePhase::Initial);
    // 未收到 ClientHello 直接收到 ClientAuthRequest 必须被拒绝
    assert_eq!(
        sm.on_frame(FrameType::ClientAuthRequest),
        Err(HandshakeError::UnexpectedFrame(
            FrameType::ClientAuthRequest
        ))
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);
}

#[test]
fn server_outbound_frames_must_never_be_received_from_client() {
    // ServerChallenge / ServerAuthResponse / ServerPending 是服务端出站帧，绝不能从客户端入站
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
fn pending_precondition_requires_client_auth_request() {
    let mut sm = ServerHandshakeSM::new();
    // 还在 Initial 阶段，不得直接进入 Pending
    assert_eq!(
        sm.enter_pending(),
        Err(HandshakeError::UnexpectedFrame(FrameType::ServerPending))
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);

    // 收到 ClientHello
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::AwaitingAuthRequest);

    // 服务端发送 ServerChallenge，等待认证请求
    assert_eq!(sm.on_send_server_challenge(), Ok(()));

    // 收到 ClientAuthRequest
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));

    // 进入 Pending：首个 ServerPending 发送，probe_in_flight 激活，seq=1
    assert_eq!(sm.enter_pending(), Ok(()));
    assert_eq!(sm.phase(), ServerHandshakePhase::InPending);
    assert!(sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 1);
}

#[test]
fn single_in_flight_probe_and_monotonic_seq_rules() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm.enter_pending(), Ok(()));

    // 当前已有首个 probe 在途，尝试并发发送新 probe 必须被拒绝
    assert_eq!(
        sm.on_send_server_pending_probe(2),
        Err(HandshakeError::ProbeAlreadyInFlight)
    );
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);

    // 重新建状态机测试正常 probe 结清与步进
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm.enter_pending(), Ok(()));
    assert!(sm.is_probe_in_flight());

    // 收到匹配 Pong 帧（纯帧序列层结清）
    assert_eq!(sm.on_frame(FrameType::PendingPong), Ok(()));
    assert!(!sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 2);

    // 尝试乱序或者回绕 seq
    let mut bad_sm = sm.clone();
    assert_eq!(
        bad_sm.on_send_server_pending_probe(1),
        Err(HandshakeError::ProbeSeqOutOfOrder {
            expected: 2,
            actual: 1
        })
    );
    assert_eq!(bad_sm.phase(), ServerHandshakePhase::Terminated);

    // 发送第二轮 probe
    assert_eq!(sm.on_send_server_pending_probe(2), Ok(()));
    assert!(sm.is_probe_in_flight());
    assert_eq!(sm.next_probe_seq(), 2);
}

#[test]
fn probe_seq_wraparound_or_max_rejected() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.on_frame(FrameType::ClientHello), Ok(()));
    assert_eq!(sm.on_send_server_challenge(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Ok(()));
    assert_eq!(sm.enter_pending(), Ok(()));
    assert_eq!(sm.on_frame(FrameType::PendingPong), Ok(()));

    // u64::MAX 作为 seq 会溢出/回绕
    assert_eq!(
        sm.on_send_server_pending_probe(u64::MAX),
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
    // 完成握手
    assert_eq!(sm.on_send_auth_response(), Ok(()));
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
    let mismatched_seq = TypedPongPayloadContext {
        pending_token: [0x11; 16],
        status_nonce: [0x22; 16],
        probe_seq: 2,
    };

    assert_eq!(sm.verify_pong_payload(&expected, &matching), Ok(()));
    assert_eq!(
        sm.verify_pong_payload(&expected, &mismatched_token),
        Err(HandshakeError::UnexpectedFrame(FrameType::PendingPong))
    );
    assert_eq!(
        sm.verify_pong_payload(&expected, &mismatched_seq),
        Err(HandshakeError::ProbeSeqOutOfOrder {
            expected: 1,
            actual: 2
        })
    );
}
