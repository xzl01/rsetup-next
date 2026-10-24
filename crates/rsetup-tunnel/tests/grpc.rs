use rsetup_tunnel::envelope::{EnvelopeError, MAX_ENVELOPE_SIZE, PacketType, TunnelPacket};
use rsetup_tunnel::grpc::{
    GRPC_HEADER_SIZE, GrpcDecoder, GrpcError, InputCursor, MAX_GRPC_MESSAGE_SIZE,
    MAX_PACKETS_PER_PUSH, encode_message,
};
use std::collections::BTreeMap;

fn sample_packet(trace_id: u64) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: PacketType::Request,
        action: "device.status".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1, 2, 3],
        metadata: BTreeMap::new(),
    }
}

/// 手工拼一个 gRPC 帧：flag=0 + BE32 长度 + 任意 body。
fn frame(len: u32, body: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; GRPC_HEADER_SIZE + body.len()];
    buf[1..5].copy_from_slice(&len.to_be_bytes());
    buf[GRPC_HEADER_SIZE..].copy_from_slice(body);
    buf
}

/// 编码后恰好 512 KiB 的合法信封（边界用），连同其信元本体。
fn envelope_of_exact_size(target: usize) -> (Vec<u8>, TunnelPacket) {
    let mut packet = sample_packet(1);
    packet.action = "device.bulk".to_string();
    packet.payload = vec![0u8; target - 32];
    let mut enc = packet.encode().unwrap();
    while enc.len() < target {
        let delta = target - enc.len();
        packet.payload.extend(std::iter::repeat_n(0u8, delta));
        enc = packet.encode().unwrap();
    }
    while enc.len() > target {
        let over = enc.len() - target;
        let new_len = packet.payload.len().checked_sub(over).unwrap();
        packet.payload.truncate(new_len);
        enc = packet.encode().unwrap();
    }
    assert_eq!(enc.len(), target);
    (enc, packet)
}

// ---------------------------------------------------------------------------
// 编码入口
// ---------------------------------------------------------------------------

#[test]
fn test_grpc_roundtrip_basic() {
    let packet = sample_packet(42);
    let encoded = encode_message(&packet).expect("encode message should succeed");

    // 5B 头：第 0 字节压缩标志 0，1..5 为 BE32 长度
    assert_eq!(encoded[0], 0, "compression flag must be 0");
    let payload_len = u32::from_be_bytes(encoded[1..5].try_into().unwrap()) as usize;
    assert_eq!(payload_len, encoded.len() - GRPC_HEADER_SIZE);

    let mut decoder = GrpcDecoder::new();
    let out = decoder.push(&encoded).expect("push should succeed");
    assert_eq!(out.packets().len(), 1);
    assert_eq!(&out.packets()[0], &packet);
    assert_eq!(out.consumed(), encoded.len());
    assert!(!out.budget_exhausted());

    decoder.finish().expect("finish should succeed");
}

#[test]
fn test_encode_message_rejects_illegal_semantics() {
    // 公共编码入口自身必须做语义校验
    let mut p = sample_packet(1);
    p.trace_id = 0;
    assert!(matches!(
        encode_message(&p),
        Err(GrpcError::EnvelopeEncode(EnvelopeError::Validation(_)))
    ));

    let mut p = sample_packet(1);
    p.packet_type = PacketType::System; // 业务 action + System
    assert!(encode_message(&p).is_err());

    // ping 请求携带错误字段同样拒绝
    let mut ping = sample_packet(1);
    ping.action = "tunnel.ping".to_string();
    ping.payload = vec![7u8; 16];
    ping.status_code = 3;
    assert!(matches!(
        encode_message(&ping),
        Err(GrpcError::EnvelopeEncode(EnvelopeError::Validation(_)))
    ));

    // 超大信封在复制前失败
    let mut big = sample_packet(1);
    big.payload = vec![0u8; 8 * 1024 * 1024];
    assert!(matches!(
        encode_message(&big),
        Err(GrpcError::EnvelopeEncode(EnvelopeError::MessageTooLarge(_)))
    ));
}

// ---------------------------------------------------------------------------
// 头部与长度前缀
// ---------------------------------------------------------------------------

#[test]
fn test_grpc_rejects_compression_flag() {
    let packet = sample_packet(1);
    let mut encoded = encode_message(&packet).unwrap();
    encoded[0] = 1;

    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&encoded);
    assert!(matches!(err, Err(GrpcError::CompressionNotSupported)));
    assert!(decoder.is_poisoned());

    let next_err = decoder.push(&[0; 5]);
    assert!(matches!(next_err, Err(GrpcError::DecoderPoisoned)));
}

#[test]
fn test_grpc_rejects_length_prefix_over_1mib() {
    let mut header = vec![0u8; 5];
    let bad_len = (MAX_GRPC_MESSAGE_SIZE + 1) as u32;
    header[1..5].copy_from_slice(&bad_len.to_be_bytes());

    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&header);
    assert!(matches!(err, Err(GrpcError::MessageTooLarge(len)) if len == 1048577));
    assert!(matches!(decoder.finish(), Err(GrpcError::DecoderPoisoned)));
}

#[test]
fn test_grpc_rejects_envelope_over_512kib() {
    // 长度前缀在 1MiB 内但超 512KiB 信封上限
    let mut header = vec![0u8; 5];
    let len = 600_000u32;
    header[1..5].copy_from_slice(&len.to_be_bytes());

    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&header);
    assert!(matches!(err, Err(GrpcError::EnvelopeTooLarge(l)) if l == 600_000));
    assert!(matches!(decoder.finish(), Err(GrpcError::DecoderPoisoned)));
}

#[test]
fn test_grpc_rejects_oversized_chunk_before_copying_it() {
    // 8MiB 的整块输入：必须在只看 5 字节头后即失败，不得先缓存 chunk
    let mut chunk = vec![0u8; 5 + 8 * 1024 * 1024];
    chunk[1..5].copy_from_slice(&(8u32 * 1024 * 1024).to_be_bytes());

    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&chunk);
    assert!(
        matches!(err, Err(GrpcError::MessageTooLarge(n)) if n == 8 * 1024 * 1024),
        "实际 {err:?}"
    );
    assert_eq!(decoder.buffered_len(), 0, "失败后不得留任何缓存");
    // 分配上限证据见 tests/grpc_memory_budget.rs
}

#[test]
fn test_grpc_accepts_exact_512kib_envelope() {
    let (body, packet) = envelope_of_exact_size(MAX_ENVELOPE_SIZE);
    let encoded = frame(MAX_ENVELOPE_SIZE as u32, &body);

    let mut decoder = GrpcDecoder::new();
    let out = decoder.push(&encoded).expect("恰好 512KiB 信封必须可解码");
    assert_eq!(out.packets().len(), 1);
    assert_eq!(out.packets()[0], packet);
    decoder.finish().unwrap();

    // 512KiB + 1 直接由长度前缀拒绝
    let oversized = frame(MAX_ENVELOPE_SIZE as u32 + 1, &body);
    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&oversized);
    assert!(matches!(err, Err(GrpcError::EnvelopeTooLarge(_))));
}

// ---------------------------------------------------------------------------
// 分片、粘包与批量预算
// ---------------------------------------------------------------------------

#[test]
fn test_grpc_fragmented_delivery() {
    let packet = sample_packet(7);
    let encoded = encode_message(&packet).unwrap();

    let mut decoder = GrpcDecoder::new();
    let mut received = Vec::new();
    let mut cursor = InputCursor::new(&encoded);
    while let Some(p) = decoder
        .next_packet(&mut cursor)
        .expect("fragment push should succeed")
    {
        received.push(p);
    }
    // 增量接口一次只取一条；未凑齐整帧时返回 None
    assert_eq!(received.len(), 1);
    assert_eq!(received[0], packet);
    assert_eq!(cursor.consumed(), encoded.len());
    decoder.finish().expect("finish should succeed");
}

#[test]
fn test_grpc_one_byte_at_a_time_via_push() {
    let packet = sample_packet(9);
    let encoded = encode_message(&packet).unwrap();

    let mut decoder = GrpcDecoder::new();
    let mut received = Vec::new();
    for b in &encoded {
        let byte = [*b];
        let out = decoder.push(&byte).expect("fragment push should succeed");
        received.extend(out.into_packets());
        assert!(decoder.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE);
    }
    assert_eq!(received.len(), 1);
    assert_eq!(received[0], packet);
    decoder.finish().expect("finish should succeed");
}

#[test]
fn test_grpc_batch_messages() {
    let p1 = sample_packet(101);
    let p2 = sample_packet(102);
    let p3 = sample_packet(103);

    let mut batch = Vec::new();
    batch.extend(encode_message(&p1).unwrap());
    batch.extend(encode_message(&p2).unwrap());
    batch.extend(encode_message(&p3).unwrap());

    let mut decoder = GrpcDecoder::new();
    let pkts = decoder.push(&batch).expect("batch push should succeed");
    let pkts = pkts.into_packets();
    assert_eq!(pkts.len(), 3);
    assert_eq!(pkts[0], p1);
    assert_eq!(pkts[1], p2);
    assert_eq!(pkts[2], p3);

    decoder.finish().expect("finish should succeed");
}

#[test]
fn test_grpc_push_batch_budget_is_real_and_lossless() {
    // 10,000 个小包粘成一个 chunk：单次输出必须受限，且剩余输入必须可无损续喂
    let count = 10_000usize;
    let mut chunk = Vec::new();
    for i in 0..count as u64 {
        chunk.extend(encode_message(&sample_packet(i + 1)).unwrap());
    }

    let mut decoder = GrpcDecoder::new();
    let mut input = chunk.as_slice();
    let mut delivered = Vec::new();
    let mut calls = 0usize;

    while !input.is_empty() {
        let out = decoder.push(input).expect("合法粘包不得成为协议错误");
        assert!(
            out.packets().len() <= MAX_PACKETS_PER_PUSH,
            "单次输出 {} 个包，超出批量预算",
            out.packets().len()
        );
        assert_eq!(out.consumed(), input.len() - out.rest().len());
        if out.consumed() == 0 {
            panic!("解码器无进展");
        }
        delivered.extend(out.packets().iter().cloned());
        calls += 1;
        input = out.rest();
        assert!(!decoder.is_poisoned(), "合法粘包不得污染连接");
    }

    assert_eq!(delivered.len(), count);
    assert_eq!(delivered[0], sample_packet(1));
    assert_eq!(delivered[count - 1], sample_packet(count as u64));
    assert!(calls > count / MAX_PACKETS_PER_PUSH);
    decoder.finish().expect("finish should succeed");
}

#[test]
fn test_grpc_cursor_budget_returns_rest_without_poison() {
    let mut chunk = Vec::new();
    for i in 0..10u64 {
        chunk.extend(encode_message(&sample_packet(i + 1)).unwrap());
    }

    let mut decoder = GrpcDecoder::new();
    let mut cursor = InputCursor::new(&chunk);

    let first = decoder.next_packets(&mut cursor, 3).unwrap();
    assert_eq!(first.len(), 3);
    assert_eq!(cursor.consumed(), {
        let mut n = 0;
        for i in 0..3u64 {
            n += encode_message(&sample_packet(i + 1)).unwrap().len();
        }
        n
    });
    assert_eq!(cursor.remaining(), chunk.len() - cursor.consumed());
    assert!(!cursor.unconsumed().is_empty());

    // 预算上限夹紧：传更大值也不会突破 MAX_PACKETS_PER_PUSH
    let rest = decoder.next_packets(&mut cursor, usize::MAX).unwrap();
    assert_eq!(rest.len(), 7);
    assert_eq!(cursor.remaining(), 0);
    assert!(!decoder.is_poisoned());
}

#[test]
fn test_grpc_partial_buffer_stays_bounded_for_incomplete_message() {
    // 头声明 512KiB，仅到达 100KiB：解码器只缓存这 100KiB，绝不多拿
    let (body, _packet) = envelope_of_exact_size(MAX_ENVELOPE_SIZE);
    let encoded = frame(MAX_ENVELOPE_SIZE as u32, &body);
    let partial = &encoded[..GRPC_HEADER_SIZE + 100 * 1024];

    let mut decoder = GrpcDecoder::new();
    let out = decoder.push(partial).expect("未完成的帧不是错误");
    assert!(out.packets().is_empty());
    assert_eq!(out.consumed(), partial.len());
    assert!(out.rest().is_empty(), "整块输入都属当前帧，不应有剩余");
    assert_eq!(
        decoder.buffered_len(),
        partial.len(),
        "只允许缓存已到达的当前帧字节"
    );
    assert!(
        decoder.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE,
        "未完成缓冲 {} 超过 5 + 512KiB",
        decoder.buffered_len()
    );
    assert!(!decoder.is_poisoned());

    // 续喂剩余字节后交付完整包，缓冲立即归零
    let rest = &encoded[GRPC_HEADER_SIZE + 100 * 1024..];
    let out2 = decoder.push(rest).expect("续喂应继续有界");
    assert_eq!(out2.packets().len(), 1);
    assert_eq!(decoder.buffered_len(), 0);
    decoder.finish().unwrap();
}

#[test]
fn test_grpc_partial_buffer_with_valid_large_envelope() {
    // 合法的 512KiB 信封按 7KiB 分片喂入：任何时刻缓冲有界，最终交付 1 个包
    let (body, packet) = envelope_of_exact_size(MAX_ENVELOPE_SIZE);
    let encoded = frame(MAX_ENVELOPE_SIZE as u32, &body);

    let mut decoder = GrpcDecoder::new();
    let mut delivered = Vec::new();
    for piece in encoded.chunks(7 * 1024) {
        let out = decoder.push(piece).expect("分片不得报错");
        assert!(decoder.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE);
        delivered.extend(out.into_packets());
    }
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0], packet);
    assert_eq!(decoder.buffered_len(), 0);
    decoder.finish().unwrap();
}

#[test]
fn test_grpc_empty_push_is_noop() {
    let mut decoder = GrpcDecoder::new();
    let out = decoder.push(&[]).expect("空输入不是错误");
    assert!(out.packets().is_empty());
    assert_eq!(out.consumed(), 0);
    decoder.finish().expect("空流可正常结束");
}

// ---------------------------------------------------------------------------
// 致命错误与粘滞
// ---------------------------------------------------------------------------

#[test]
fn test_grpc_finish_with_partial_data_fails() {
    let packet = sample_packet(8);
    let encoded = encode_message(&packet).unwrap();

    let mut decoder = GrpcDecoder::new();
    decoder.push(&encoded[..encoded.len() - 1]).unwrap();

    let err = decoder.finish();
    assert!(matches!(err, Err(GrpcError::TruncatedMessage)));
    assert!(matches!(decoder.finish(), Err(GrpcError::DecoderPoisoned)));
    assert!(matches!(decoder.push(&[]), Err(GrpcError::DecoderPoisoned)));
}

#[test]
fn test_grpc_rejects_malformed_envelope_content() {
    let mut buf = vec![0u8; 5 + 10];
    buf[1..5].copy_from_slice(&10u32.to_be_bytes());
    buf[5] = 0x80; // 截断的 varint

    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&buf);
    assert!(matches!(err, Err(GrpcError::EnvelopeDecode(_))));
    assert!(matches!(decoder.push(&[]), Err(GrpcError::DecoderPoisoned)));
}

#[test]
fn test_grpc_rejects_illegal_semantic_envelope() {
    // 合法 wire、非法语义：trace_id=0
    // field 1 trace_id = 0（非法语义），field 2 = Request，field 3 = action
    let mut body = vec![0x08, 0x00, 0x10, 0x01, 0x1a, 0x0d];
    body.extend_from_slice(b"device.status");

    let encoded = frame(body.len() as u32, &body);
    let mut decoder = GrpcDecoder::new();
    let err = decoder.push(&encoded);
    match err {
        Err(GrpcError::EnvelopeDecode(EnvelopeError::Validation(m))) => {
            assert!(m.contains("trace_id"), "实际 {m}");
        }
        other => panic!("期望 EnvelopeDecode(Validation(trace_id))，实际 {other:?}"),
    }
    assert!(decoder.is_poisoned());
    assert!(decoder.finish().is_err());
}

#[test]
fn test_grpc_error_details_never_leak_peer_content() {
    // 帧内信封带重复 metadata key：错误链不得回显对端原文
    let mut body = Vec::new();
    body.extend_from_slice(&[0x08, 0x01, 0x10, 0x01, 0x1a, 0x0d]);
    body.extend_from_slice(b"device.status");
    for v in [b"v1".as_slice(), b"v2".as_slice()] {
        let mut entry = Vec::new();
        entry.extend_from_slice(&[0x0a, 0x0e]); // key, 14B
        entry.extend_from_slice(b"EVIL\nKEYsecret");
        entry.extend_from_slice(&[0x12, v.len() as u8]);
        entry.extend_from_slice(v);
        body.push(0x3a);
        body.push(entry.len() as u8);
        body.extend_from_slice(&entry);
    }

    let mut decoder = GrpcDecoder::new();
    let err = decoder
        .push(&frame(body.len() as u32, &body))
        .expect_err("重复 key 必须拒绝");
    let msg = err.to_string();
    assert!(
        !msg.contains("EVIL") && !msg.contains("secret") && !msg.contains('\n'),
        "gRPC 错误回显对端原文: {msg:?}"
    );
}
