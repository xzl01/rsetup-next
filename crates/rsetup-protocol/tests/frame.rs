//! 帧边界行为测试：T01–T07、S8（protocol_spec.md §4.1）。

use rsetup_protocol::frame::{
    FRAME_HEADER_LEN, FRAME_MAGIC, FRAME_VERSION, FrameError, FrameType, MAX_FRAME_BYTES,
    MAX_PAYLOAD_LEN, encode_frame, frame_is_complete, parse_frame_header,
};

fn header_bytes(magic: [u8; 2], ver: u8, ftype: u8, len: u32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&magic);
    b.push(ver);
    b.push(ftype);
    b.extend_from_slice(&len.to_be_bytes());
    b
}

#[test]
fn frame_encode_header_exact_layout() {
    // T01：输出前 8B 恰为 53 45 02 01 00 00 00 04；总长 = 8 + payload。
    let payload = [0xAA, 0xBB, 0xCC, 0xDD];
    let frame = encode_frame(FrameType::ClientHello, &payload).unwrap();
    assert_eq!(
        &frame[..8],
        [0x53, 0x45, 0x02, 0x01, 0x00, 0x00, 0x00, 0x04]
    );
    assert_eq!(frame.len(), 12);
    assert_eq!(&frame[8..], &payload);
}

#[test]
fn frame_roundtrip_all_six_types() {
    // T02：6 种 FrameType 编码 → 解析，type/len 全还原。
    let types = [
        FrameType::ClientHello,
        FrameType::ServerChallenge,
        FrameType::ClientAuthRequest,
        FrameType::ServerAuthResponse,
        FrameType::ServerPending,
        FrameType::PendingPong,
    ];
    for (i, t) in types.iter().enumerate() {
        let payload = vec![i as u8; 3];
        let frame = encode_frame(*t, &payload).unwrap();
        let h = parse_frame_header(&frame).unwrap();
        assert_eq!(h.frame_type, *t);
        assert_eq!(h.payload_len, 3);
    }
}

#[test]
fn frame_reject_bad_magic_variants() {
    // S1：3 个 magic 变体 → BadMagic；字节序颠倒 0x45 0x53 不是兼容变体。
    for magic in [[0x00, 0x00], [0x53, 0x46], [0x45, 0x53]] {
        assert_eq!(
            parse_frame_header(&header_bytes(magic, FRAME_VERSION, 0x01, 4)),
            Err(FrameError::BadMagic),
        );
    }
    assert_eq!(FRAME_MAGIC, [0x53, 0x45]);
}

#[test]
fn frame_reject_ver_01_no_downgrade() {
    // S2：Ver=0x01 直接拒绝，不存在“按 v1 降级解析”分支。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, 0x01, 0x01, 4)),
        Err(FrameError::BadVersion),
    );
}

#[test]
fn frame_reject_unknown_ver_and_type() {
    // S3/S4：Ver=0x03 → BadVersion；FrameType 0x00/0x07/0xFF → BadFrameType(v)，不透传。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, 0x03, 0x01, 4)),
        Err(FrameError::BadVersion),
    );
    for t in [0x00u8, 0x07, 0xFF] {
        assert_eq!(
            parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, t, 4)),
            Err(FrameError::BadFrameType(t)),
        );
    }
}

#[test]
fn frame_boundary_16kib() {
    // S5/S6/S7：16385 → PayloadTooLarge（分配前拒绝）；16384 边界通过；
    // frame_is_complete 差 1 字节 / 恰好完整。
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 16_385)),
        Err(FrameError::PayloadTooLarge {
            len: 16_385,
            max: MAX_PAYLOAD_LEN as u32
        }),
    );
    let h = parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 16_384)).unwrap();
    assert_eq!(h.payload_len, 16_384);
    assert_eq!(frame_is_complete(MAX_FRAME_BYTES - 1, 16_384), Ok(false));
    assert_eq!(frame_is_complete(MAX_FRAME_BYTES, 16_384), Ok(true));
    let big = vec![0u8; MAX_PAYLOAD_LEN + 1];
    assert_eq!(
        encode_frame(FrameType::ClientHello, &big),
        Err(FrameError::PayloadTooLarge {
            len: (MAX_PAYLOAD_LEN + 1) as u32,
            max: MAX_PAYLOAD_LEN as u32,
        }),
    );
    let full = vec![0u8; MAX_PAYLOAD_LEN];
    assert_eq!(
        encode_frame(FrameType::ServerPending, &full).unwrap().len(),
        MAX_FRAME_BYTES
    );
}

#[test]
fn frame_incomplete_prefixes() {
    // S7：不完整 TCP 片段是可续收的 Incomplete，不是畸形包；need = 8 - have。
    let buf = header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, 4);
    for have in 0..FRAME_HEADER_LEN {
        assert_eq!(
            parse_frame_header(&buf[..have]),
            Err(FrameError::Incomplete {
                have,
                need: FRAME_HEADER_LEN - have
            }),
        );
    }
}

#[test]
fn frame_reject_huge_declared_len_no_overflow() {
    // S8：声明长度 0xFFFFFFF0 → PayloadTooLarge；`8 + len` 加法不得溢出
    // （先校验上限再相加）。
    let huge = u32::MAX - 15; // 0xFFFFFFF0（= 4294967280），避免 1.85 下字面量溢出 u32
    assert_eq!(
        parse_frame_header(&header_bytes(FRAME_MAGIC, FRAME_VERSION, 0x01, huge)),
        Err(FrameError::PayloadTooLarge {
            len: huge,
            max: MAX_PAYLOAD_LEN as u32
        }),
    );
    assert_eq!(
        frame_is_complete(0, huge),
        Err(FrameError::PayloadTooLarge {
            len: huge,
            max: MAX_PAYLOAD_LEN as u32
        }),
    );
}
