use rsetup_protocol::crypto::MAX_PLAINTEXT_LEN;
use rsetup_protocol::crypto::SessionKeys;
use rsetup_protocol::frame::{self, FrameType};
use rsetup_protocol::handshake_adapter::{
    AdapterError, EstablishedTunnel, HandshakeMessage, MAX_RECORD_BUFFER, RecordReassembler,
    decode_handshake_frame, encode_handshake_frame,
};
use rsetup_protocol::handshake_msg::{
    ClientAuthRequest, ClientHello, DeviceDescriptor, PendingPong, ServerAuthResponse,
    ServerChallenge, ServerPending,
};

fn session_keys() -> SessionKeys {
    SessionKeys::derive(&[0x5a; 32], &[0x11; 32], &[0x22; 32]).expect("derive")
}

fn tunnels() -> (EstablishedTunnel, EstablishedTunnel) {
    let client = EstablishedTunnel::new(session_keys().into_client_record_layers());
    let server = EstablishedTunnel::new(session_keys().into_server_record_layers());
    (client, server)
}

fn client_hello() -> HandshakeMessage {
    HandshakeMessage::ClientHello(ClientHello::new([0x01; 32]))
}

fn server_bundle() -> Vec<HandshakeMessage> {
    let descriptor = DeviceDescriptor::new("SN-1", "MODEL", "1.0.0", "");
    let auth = ClientAuthRequest::new([0x11; 32], [0x22; 32], descriptor, [0x33; 64])
        .expect("client auth");
    let pending = ServerPending::new([0xa1; 16], [0xa2; 16], 1).expect("pending");
    let pong = PendingPong::new([0xa1; 16], [0xa2; 16], 1).expect("pong");
    let approved = ServerAuthResponse::approved([0x44; 32], [0x55; 64]).expect("approved");
    vec![
        HandshakeMessage::ServerChallenge(ServerChallenge::new([0x02; 32])),
        HandshakeMessage::ClientAuthRequest(auth),
        HandshakeMessage::ServerPending(pending),
        HandshakeMessage::PendingPong(pong),
        HandshakeMessage::ServerAuthResponse(approved),
    ]
}

#[test]
fn all_handshake_messages_roundtrip_through_frames() {
    let mut messages = vec![client_hello()];
    messages.extend(server_bundle());

    for message in messages {
        let frame_type = match &message {
            HandshakeMessage::ClientHello(_) => FrameType::ClientHello,
            HandshakeMessage::ServerChallenge(_) => FrameType::ServerChallenge,
            HandshakeMessage::ClientAuthRequest(_) => FrameType::ClientAuthRequest,
            HandshakeMessage::ServerPending(_) => FrameType::ServerPending,
            HandshakeMessage::PendingPong(_) => FrameType::PendingPong,
            HandshakeMessage::ServerAuthResponse(_) => FrameType::ServerAuthResponse,
        };
        let encoded = encode_handshake_frame(frame_type, &message).expect("encode");
        assert_eq!(&encoded[..2], &frame::FRAME_MAGIC);
        assert_eq!(encoded[2], frame::FRAME_VERSION);
        let (decoded_type, decoded) = decode_handshake_frame(&encoded).expect("decode");
        assert_eq!(decoded_type, frame_type);
        assert_eq!(decoded, message);
    }
}

#[test]
fn frame_message_type_mismatch_is_rejected() {
    let err = encode_handshake_frame(FrameType::ServerChallenge, &client_hello()).unwrap_err();
    assert_eq!(
        err,
        AdapterError::TypeMismatch {
            frame: FrameType::ServerChallenge,
            message: "ClientHello",
        }
    );
}

#[test]
fn frame_decoder_requires_exactly_one_complete_frame() {
    let encoded = encode_handshake_frame(FrameType::ClientHello, &client_hello()).expect("encode");

    let mut extra = encoded.clone();
    extra.push(0);
    assert_eq!(
        decode_handshake_frame(&extra).unwrap_err(),
        AdapterError::NotExactlyOneFrame
    );

    assert_eq!(
        decode_handshake_frame(&encoded[..encoded.len() - 1]).unwrap_err(),
        AdapterError::NotExactlyOneFrame
    );

    let mut wrong_magic = encoded.clone();
    wrong_magic[0] = 0;
    assert!(matches!(
        decode_handshake_frame(&wrong_magic).unwrap_err(),
        AdapterError::Frame(frame::FrameError::BadMagic)
    ));

    let mut wrong_version = encoded;
    wrong_version[2] = 0x02;
    assert!(matches!(
        decode_handshake_frame(&wrong_version).unwrap_err(),
        AdapterError::Frame(frame::FrameError::BadVersion)
    ));
}

#[test]
fn reassembler_returns_only_complete_records() {
    let (mut client, _server) = tunnels();
    let a = client.send(b"hello").expect("seal a");
    let b = client.send(b"").expect("seal empty");
    let mut feed = a.clone();
    feed.extend_from_slice(&b);

    let mut reassembler = RecordReassembler::new();
    let records = reassembler.push(&feed).expect("push both");
    assert_eq!(records, vec![a.clone(), b.clone()]);
    assert_eq!(reassembler.buffered_len(), 0);

    // 拆包：逐字节输入也不会交付半条记录。
    let mut split = RecordReassembler::new();
    let mut delivered = Vec::new();
    for byte in &feed {
        delivered.extend(split.push(std::slice::from_ref(byte)).expect("push byte"));
    }
    assert_eq!(delivered, vec![a, b]);
    assert_eq!(split.buffered_len(), 0);
}

#[test]
fn reassembler_keeps_partial_record_buffered() {
    let (mut client, _server) = tunnels();
    let record = client.send(b"partial").expect("seal");
    let mut reassembler = RecordReassembler::new();
    assert!(reassembler.push(&record[..2]).unwrap().is_empty());
    assert_eq!(reassembler.buffered_len(), 2);
    assert!(reassembler.push(&record[2..7]).unwrap().is_empty());
    assert_eq!(reassembler.buffered_len(), 7);
    let records = reassembler.push(&record[7..]).expect("complete");
    assert_eq!(records, vec![record]);
}

#[test]
fn reassembler_rejects_invalid_lengths_without_waiting_for_payload() {
    // 长度字段一旦凑满 4B 就必须立刻判定，不等 payload 到达（§5.2）。
    let mut reassembler = RecordReassembler::new();
    assert_eq!(
        reassembler.push(&[0, 0, 0, 15]).unwrap_err(),
        AdapterError::RecordTooSmall(15)
    );

    let mut reassembler = RecordReassembler::new();
    let too_big = (65_537u32).to_be_bytes();
    assert_eq!(
        reassembler.push(&too_big).unwrap_err(),
        AdapterError::RecordTooLarge(65_537)
    );

    // 全部超过缓冲预算的**全零**批量：前 4B 已构成非法长度 0，判定为
    // RecordTooSmall（不是"预算超限"这种含糊错误）；预算本身由
    // `reassembler_buffer_budget_is_structurally_bounded` 钉死。
    let mut reassembler = RecordReassembler::new();
    let oversized = vec![0u8; 65_541];
    assert_eq!(
        reassembler.push(&oversized).unwrap_err(),
        AdapterError::RecordTooSmall(0)
    );
    assert!(reassembler.is_fatal(), "非法长度必须把重组器置为致命");
}

#[test]
fn reassembler_fatal_is_sticky_and_never_delivers_again() {
    // 回归：初版实现出错后仍可继续被喂数据（无 fatal 粘滞），会把"整条 TCP
    // 已致命"降级成"这一批不要了"。
    let (mut client, _server) = tunnels();
    let good = client.send(b"good after error").expect("seal");

    let mut reassembler = RecordReassembler::new();
    assert_eq!(
        reassembler.push(&[0, 0, 0, 15]).unwrap_err(),
        AdapterError::RecordTooSmall(15)
    );
    assert!(reassembler.is_fatal());

    // 之后任何输入（包括完全合法的记录）都必须继续拒绝，且不交付。
    for bytes in [&good[..], &[0u8; 3], &[][..]] {
        assert_eq!(
            reassembler.push(bytes).unwrap_err(),
            AdapterError::ReassemblerFatal,
            "致命之后不得再接受输入"
        );
        assert_eq!(reassembler.buffered_len(), 0, "致命后不得再累积缓冲");
    }

    // 残留字节未清零时遇致命：残留必须清零（连接已作废，不留任何可续收状态）。
    // 注意语义边界：长度前缀一旦判定合法，后续字节都属于**这条记录的 payload**
    // （往记录体里塞"坏长度"不是帧错误），所以本例用"前 2B 前缀 + 2B 凑齐后
    // 声明长度 0"构造致命。
    let mut reassembler = RecordReassembler::new();
    assert!(reassembler.push(&good[..2]).unwrap().is_empty());
    assert_eq!(reassembler.buffered_len(), 2);
    // 再补 2B 凑满前缀 ⇒ 声明长度 0 ⇒ 立刻致命（不等 payload）。
    assert_eq!(
        reassembler.push(&[0, 0]).unwrap_err(),
        AdapterError::RecordTooSmall(0)
    );
    assert!(reassembler.is_fatal());
    assert_eq!(reassembler.buffered_len(), 0, "致命后必须清空残留");
    assert_eq!(
        reassembler.push(&good[4..]).unwrap_err(),
        AdapterError::ReassemblerFatal
    );

    // 合法前缀 + 半条 payload：字节属于这条记录，仍在等待，不是错误、也未致命。
    let mut reassembler = RecordReassembler::new();
    assert!(reassembler.push(&good[..5]).unwrap().is_empty());
    assert_eq!(reassembler.buffered_len(), 5);
    assert!(!reassembler.is_fatal());
    assert_eq!(reassembler.push(&good[5..]).unwrap(), vec![good]);
    assert_eq!(reassembler.buffered_len(), 0);
}

#[test]
fn reassembler_accepts_batch_larger_than_buffer_budget() {
    // 回归：初版按"已缓冲 + 本批"总量卡 65540，两三条完整记录一起到达就被
    // 误杀（BufferLimit）。上限只应约束**未完成的残留字节**。
    let (mut client, _server) = tunnels();
    let max_record = client
        .send(&vec![0x7eu8; MAX_PLAINTEXT_LEN])
        .expect("seal max");
    assert_eq!(max_record.len(), MAX_RECORD_BUFFER);

    let mut batch = max_record.clone();
    batch.extend_from_slice(&max_record);
    batch.extend_from_slice(&max_record);

    let mut reassembler = RecordReassembler::new();
    let records = reassembler
        .push(&batch)
        .expect("三条满记录批量到达不得被误杀");
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|r| r == &max_record));
    assert_eq!(reassembler.buffered_len(), 0);
    assert!(!reassembler.is_fatal());
}

#[test]
fn reassembler_buffer_budget_is_structurally_bounded() {
    // 上限不削弱：残留的未完成字节任何时刻都 ≤ 65540；逐字节喂入也必须如此，
    // 且第一条完整记录照常交付。
    let (mut client, _server) = tunnels();
    let record = client.send(&vec![0x42u8; MAX_PLAINTEXT_LEN]).expect("seal");
    let mut reassembler = RecordReassembler::new();
    let mut delivered = Vec::new();
    for byte in &record {
        delivered.extend(
            reassembler
                .push(std::slice::from_ref(byte))
                .expect("push byte"),
        );
        assert!(
            reassembler.buffered_len() <= MAX_RECORD_BUFFER,
            "残留 {} 字节超过 65540 预算",
            reassembler.buffered_len()
        );
    }
    assert_eq!(delivered, vec![record]);
    assert_eq!(reassembler.buffered_len(), 0);
}

#[test]
fn reassembler_never_panics_on_arbitrary_bytes() {
    // 回归：初版用 `expect` 解析长度前缀（panic 路径）。这里把所有 4 字节
    // 前缀的边界值与任意垃圾字节都过一遍，只允许返回错误，不允许 panic。
    let mut reassembler = RecordReassembler::new();
    for len in [0u32, 1, 15, 16, 17, 65_535, 65_536, 65_537, u32::MAX] {
        let prefix = len.to_be_bytes();
        let mut r = RecordReassembler::new();
        match r.push(&prefix) {
            Ok(records) => {
                // 16..=65536 只喂了前缀：必须等待后续字节，不得交付半条。
                assert!(records.is_empty(), "len={len} 前缀不足以成帧却交付了记录");
                assert!(r.buffered_len() <= MAX_RECORD_BUFFER);
            }
            Err(err) => {
                assert!(
                    matches!(
                        err,
                        AdapterError::RecordTooSmall(_) | AdapterError::RecordTooLarge(_)
                    ),
                    "len={len} 意外错误 {err:?}"
                );
                assert!(r.is_fatal());
            }
        }
        // 垃圾字节（含不足前缀）也绝不能 panic。
        for junk in [&[][..], &[0x00][..], &[0xff, 0xfe][..], &[0u8; 70000][..]] {
            let _ = reassembler.push(junk);
        }
        reassembler = RecordReassembler::new();
    }
}

#[test]
fn reassembler_interleaves_partial_records_across_pushes() {
    // 边界：一条记录的前缀被拆成 1+3 两次到达，且同一批里还带下一条的头部。
    let (mut client, _server) = tunnels();
    let a = client.send(b"aaaa").expect("seal a");
    let b = client.send(b"bbbb").expect("seal b");

    let mut reassembler = RecordReassembler::new();
    assert!(reassembler.push(&a[..1]).unwrap().is_empty());
    let mut batch = a[1..].to_vec();
    batch.extend_from_slice(&b[..3]); // b 的前缀 + 1 字节
    let records = reassembler.push(&batch).expect("push batch");
    assert_eq!(records, vec![a]);
    assert_eq!(reassembler.buffered_len(), 3);
    let records = reassembler.push(&b[3..]).expect("complete b");
    assert_eq!(records, vec![b]);
    assert_eq!(reassembler.buffered_len(), 0);
}

#[test]
fn established_tunnel_roundtrips_both_directions() {
    let (mut client, mut server) = tunnels();
    let client_record = client.send(b"client->server").expect("client seal");
    assert_eq!(
        server.recv(&client_record).expect("server open"),
        b"client->server"
    );

    let server_record = server.send(b"server->client").expect("server seal");
    assert_eq!(
        client.recv(&server_record).expect("client open"),
        b"server->client"
    );
}

#[test]
fn tampered_record_makes_whole_server_tunnel_fatal() {
    let (mut client, mut server) = tunnels();
    let mut record = client.send(b"secret").expect("seal");
    let last = record.len() - 1;
    record[last] ^= 0xff;

    assert!(server.recv(&record).is_err());
    assert!(server.is_fatal());
    // 同一 pair 的发送方向也必须立即拒绝，即使明文完全合法。
    assert_eq!(
        server.send(b"after-failure").unwrap_err(),
        AdapterError::TunnelFatal
    );
}

#[test]
fn abort_makes_both_directions_fatal() {
    let (mut client, _server) = tunnels();
    client.abort();
    assert!(client.is_fatal());
    assert_eq!(client.send(b"x").unwrap_err(), AdapterError::TunnelFatal);

    let (mut other_client, _server) = tunnels();
    let record = other_client.send(b"y").expect("seal");
    client.abort();
    assert_eq!(client.recv(&record).unwrap_err(), AdapterError::TunnelFatal);
}
