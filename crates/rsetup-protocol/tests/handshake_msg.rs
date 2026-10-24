//! §4.2 握手消息严格 wire 编解码测试：7 类消息 roundtrip、`descriptor_raw` 原字节保留、
//! 固定字节向量（逐字节 hex 钉死）、以及 §4.2 严格 wire 校验契约的逐条负例。
//!
//! 断言口径：所有负例都断言**具体错误枚举变体**（含其携带的字段号/长度/取值），
//! 不写「错误串里不含某个字节」这类去敏断言——去敏由错误变体自身的定义保证。

use rsetup_protocol::handshake_msg::{
    ClientAuthRequest, ClientHello, DeviceDescriptor, HandshakeMsgError,
    MAX_DEVICE_DESCRIPTOR_BYTES, MAX_HANDSHAKE_MESSAGE_BYTES, PENDING_WAIT_HINT_MS, PendingPong,
    ServerAuthResponse, ServerChallenge, ServerPending,
};

// ====================================================================
// 测试夹具
// ====================================================================

/// 确定性模式字节：从 `base` 起逐字节 +1，共 `N` 字节。
fn ramp<const N: usize>(base: u8) -> [u8; N] {
    let mut a = [0u8; N];
    for (i, b) in a.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    a
}

fn hex(s: &str) -> Vec<u8> {
    hex::decode(s).expect("fixed vector must be valid hex")
}

/// 测试侧独立实现的最短 varint，用于手工拼装负例字节。
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if value < 0x80 {
            out.push(value as u8);
            return out;
        }
        out.push(((value as u8) & 0x7f) | 0x80);
        value >>= 7;
    }
}

/// 测试侧独立拼装的 `bytes`/`string`/嵌套字段（wire type 2）。
fn len_field(field: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(field << 3) as u8 | 2];
    out.extend_from_slice(&varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// 测试侧独立拼装的 varint 字段。
fn varint_field(field: u32, value: u64) -> Vec<u8> {
    let mut out = vec![(field << 3) as u8];
    out.extend_from_slice(&varint(value));
    out
}

fn append(mut base: Vec<u8>, extra: &[u8]) -> Vec<u8> {
    base.extend_from_slice(extra);
    base
}

/// 按 §4.2 外层字段升序拼出 `ClientAuthRequest` 载荷。
fn auth_request_bytes(pub_ed: &[u8], pub_eph: &[u8], descriptor_raw: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut bytes = len_field(1, pub_ed);
    bytes.extend_from_slice(&len_field(2, pub_eph));
    bytes.extend_from_slice(&len_field(3, descriptor_raw));
    bytes.extend_from_slice(&len_field(4, sig));
    bytes
}

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor::new("SN-0001", "MB-X1", "1.2.3", "")
}

fn request() -> ClientAuthRequest {
    ClientAuthRequest::new(
        ramp::<32>(0xa1),
        ramp::<32>(0xb1),
        descriptor(),
        ramp::<64>(0xc0),
    )
    .expect("sample request must satisfy §4.2")
}

fn client_hello_bytes() -> Vec<u8> {
    ClientHello::new(ramp::<32>(0x00)).encode().unwrap()
}

fn pong_bytes(seq: u64) -> Vec<u8> {
    PendingPong::new(ramp::<16>(0x11), ramp::<16>(0x22), seq)
        .unwrap()
        .encode()
        .unwrap()
}

fn pending_bytes(wait_hint_ms: Option<u64>, probe_seq: Option<u64>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(value) = wait_hint_ms {
        bytes.extend_from_slice(&varint_field(1, value));
    }
    bytes.extend_from_slice(&len_field(2, &ramp::<16>(0x11)));
    bytes.extend_from_slice(&len_field(3, &ramp::<16>(0x22)));
    if let Some(value) = probe_seq {
        bytes.extend_from_slice(&varint_field(4, value));
    }
    bytes
}

/// 构造 `ServerAuthResponse`；`status = 0`/`reason = 0` 按 canonical 缺省省略。
fn auth_response_bytes(status: u64, reason: u64, eph: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    if status != 0 {
        bytes.extend_from_slice(&varint_field(1, status));
    }
    if reason != 0 {
        bytes.extend_from_slice(&varint_field(2, reason));
    }
    bytes.extend_from_slice(&len_field(3, eph));
    bytes.extend_from_slice(&len_field(4, &ramp::<64>(0xc0)));
    bytes
}

/// 构造长度恰为 `len` 的 descriptor 原字节（单个超长 `device_sn`）。
fn descriptor_raw_of_len(len: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity(len);
    raw.push(0x0a);
    raw.extend_from_slice(&varint((len - 3) as u64));
    raw.extend_from_slice(&vec![b'a'; len - 3]);
    assert_eq!(raw.len(), len);
    raw
}

// ====================================================================
// 固定字节向量（逐字节钉死）
// ====================================================================

const HEX_CLIENT_HELLO: &str =
    "0a20000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const HEX_SERVER_CHALLENGE: &str =
    "0a20404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f";
/// 完整 `ClientAuthRequest`（159B）：descriptor 三字段非空、`extra_info` 空串缺省不写，
/// `descriptor_raw` = `0a07534e2d3030303112054d422d58311a05312e322e33`（23B）。
const HEX_CLIENT_AUTH_REQUEST: &str = concat!(
    "0a20a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0",
    "1220b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0",
    "1a170a07534e2d3030303112054d422d58311a05312e322e33",
    "2240c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
    "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
);
/// 同一请求但 descriptor 四字段全空：嵌套 descriptor 以 `1a00` 写出 presence（136B）。
const HEX_CLIENT_AUTH_REQUEST_EMPTY_DESCRIPTOR: &str = concat!(
    "0a20a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebfc0",
    "1220b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0",
    "1a00",
    "2240c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
    "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
);
/// REJECTED(reason_code=2 REVOKED)：`status=1`、`reason_code=2`、32B 全零占位 + 64B 签名。
const HEX_SERVER_AUTH_RESPONSE_REJECTED: &str = concat!(
    "08011002",
    "1a200000000000000000000000000000000000000000000000000000000000000000",
    "2240c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
    "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
);
/// APPROVED：`status`/`reason_code` 均为缺省 0，canonical 编码**不写**这两个字段（100B）。
const HEX_SERVER_AUTH_RESPONSE_APPROVED: &str = concat!(
    "1a20b1b2b3b4b5b6b7b8b9babbbcbdbebfc0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0",
    "2240c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf",
    "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff",
);
/// `wait_hint_ms = 5000` → `08 88 27`；`probe_seq = 1` → `20 01`。
const HEX_SERVER_PENDING: &str =
    "08882712101112131415161718191a1b1c1d1e1f201a1022232425262728292a2b2c2d2e2f30312001";
const HEX_PENDING_PONG: &str =
    "0a101112131415161718191a1b1c1d1e1f20121022232425262728292a2b2c2d2e2f30311801";

#[test]
fn vector_client_hello() {
    assert_eq!(HEX_CLIENT_HELLO.len(), 34 * 2);
    let bytes = hex(HEX_CLIENT_HELLO);
    assert_eq!(bytes.len(), 34);
    let message = ClientHello::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(message.client_random, ramp::<32>(0x00));
    assert_eq!(message.encode().unwrap(), bytes);
    assert_eq!(ClientHello::new(ramp::<32>(0x00)).encode().unwrap(), bytes);
}

#[test]
fn vector_server_challenge() {
    assert_eq!(HEX_SERVER_CHALLENGE.len(), 34 * 2);
    let bytes = hex(HEX_SERVER_CHALLENGE);
    let message = ServerChallenge::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(message.server_random, ramp::<32>(0x40));
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_client_auth_request_full() {
    // 64B 签名 = 128 hex；整包 159B。
    assert_eq!(HEX_CLIENT_AUTH_REQUEST.len(), 159 * 2);
    let bytes = hex(HEX_CLIENT_AUTH_REQUEST);
    assert_eq!(bytes.len(), 159);
    let message = ClientAuthRequest::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(message, request());
    assert_eq!(message.client_ed25519_pub(), &ramp::<32>(0xa1));
    assert_eq!(message.client_x25519_eph_pub(), &ramp::<32>(0xb1));
    assert_eq!(message.descriptor(), &descriptor());
    assert_eq!(message.signature(), &ramp::<64>(0xc0));
    assert_eq!(hex::encode(message.signature()).len(), 64 * 2);
    assert_eq!(
        hex::encode(message.descriptor_raw()),
        "0a07534e2d3030303112054d422d58311a05312e322e33"
    );
    assert_eq!(message.descriptor_raw().len(), 23);
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_client_auth_request_empty_descriptor_keeps_presence() {
    assert_eq!(HEX_CLIENT_AUTH_REQUEST_EMPTY_DESCRIPTOR.len(), 136 * 2);
    let bytes = hex(HEX_CLIENT_AUTH_REQUEST_EMPTY_DESCRIPTOR);
    let message =
        ClientAuthRequest::decode(&bytes).expect("四个空串 descriptor 合法（§4.2 string 缺省）");
    assert_eq!(message.descriptor_raw(), b"");
    assert_eq!(message.descriptor(), &DeviceDescriptor::default());
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_server_auth_response_rejected() {
    // status(2B) + reason(2B) + 32B 全零占位(34B) + 64B 签名(66B) = 104B。
    assert_eq!(HEX_SERVER_AUTH_RESPONSE_REJECTED.len(), 104 * 2);
    let bytes = hex(HEX_SERVER_AUTH_RESPONSE_REJECTED);
    let message = ServerAuthResponse::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(
        message,
        ServerAuthResponse::rejected(2, ramp::<64>(0xc0)).unwrap()
    );
    assert_eq!(message.server_x25519_eph_pub, [0u8; 32]);
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_server_auth_response_approved_omits_default_scalars() {
    assert_eq!(HEX_SERVER_AUTH_RESPONSE_APPROVED.len(), 100 * 2);
    let bytes = hex(HEX_SERVER_AUTH_RESPONSE_APPROVED);
    let message = ServerAuthResponse::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(message.status, 0);
    assert_eq!(message.reason_code, 0);
    assert_eq!(message.server_x25519_eph_pub, ramp::<32>(0xb1));
    assert_eq!(
        message,
        ServerAuthResponse::approved(ramp::<32>(0xb1), ramp::<64>(0xc0)).unwrap()
    );
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_server_pending() {
    assert_eq!(HEX_SERVER_PENDING.len(), 41 * 2);
    let bytes = hex(HEX_SERVER_PENDING);
    let message = ServerPending::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(message.wait_hint_ms, PENDING_WAIT_HINT_MS);
    assert_eq!(
        message,
        ServerPending::new(ramp::<16>(0x11), ramp::<16>(0x22), 1).unwrap()
    );
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn vector_pending_pong() {
    assert_eq!(HEX_PENDING_PONG.len(), 38 * 2);
    let bytes = hex(HEX_PENDING_PONG);
    let message = PendingPong::decode(&bytes).expect("fixed vector must decode");
    assert_eq!(
        message,
        PendingPong::new(ramp::<16>(0x11), ramp::<16>(0x22), 1).unwrap()
    );
    assert_eq!(message.encode().unwrap(), bytes);
}

// ====================================================================
// roundtrip
// ====================================================================

#[test]
fn roundtrip_client_hello() {
    for base in [0x00u8, 0x7f, 0xff] {
        let message = ClientHello::new(ramp::<32>(base));
        let bytes = message.encode().unwrap();
        assert_eq!(bytes.len(), 34);
        let decoded = ClientHello::decode(&bytes).unwrap();
        assert_eq!(decoded, message);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
}

#[test]
fn roundtrip_server_challenge() {
    let message = ServerChallenge::new(ramp::<32>(0x90));
    let bytes = message.encode().unwrap();
    let decoded = ServerChallenge::decode(&bytes).unwrap();
    assert_eq!(decoded, message);
    assert_eq!(decoded.encode().unwrap(), bytes);
}

#[test]
fn roundtrip_client_auth_request() {
    let message = request();
    let bytes = message.encode().unwrap();
    let decoded = ClientAuthRequest::decode(&bytes).unwrap();
    assert_eq!(decoded, message);
    assert_eq!(decoded.descriptor_raw(), message.descriptor_raw());
    assert_eq!(decoded.encode().unwrap(), bytes);
}

#[test]
fn roundtrip_server_pending() {
    for seq in [1u64, 2, 127, 128, 4096, u64::MAX] {
        let message = ServerPending::new(ramp::<16>(0x31), ramp::<16>(0x42), seq).unwrap();
        let bytes = message.encode().unwrap();
        let decoded = ServerPending::decode(&bytes).unwrap();
        assert_eq!(decoded, message);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
}

#[test]
fn roundtrip_pending_pong() {
    for seq in [1u64, 255, u64::MAX] {
        let bytes = pong_bytes(seq);
        let decoded = PendingPong::decode(&bytes).unwrap();
        assert_eq!(decoded.probe_seq, seq);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
}

#[test]
fn roundtrip_server_auth_response_both_statuses() {
    let approved = ServerAuthResponse::approved(ramp::<32>(0xb1), ramp::<64>(0xc0)).unwrap();
    let bytes = approved.encode().unwrap();
    assert_eq!(ServerAuthResponse::decode(&bytes).unwrap(), approved);

    for reason in [0u32, 2, 3, 4, 5] {
        let rejected = ServerAuthResponse::rejected(reason, ramp::<64>(0xd0)).unwrap();
        assert_eq!(rejected.server_x25519_eph_pub, [0u8; 32]);
        let bytes = rejected.encode().unwrap();
        let decoded = ServerAuthResponse::decode(&bytes).unwrap();
        assert_eq!(decoded, rejected);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }
}

#[test]
fn roundtrip_device_descriptor_four_fields() {
    let message = DeviceDescriptor::new("SN-0001", "MB-X1", "1.2.3", "extra note");
    let bytes = message.encode();
    assert_eq!(bytes.len(), 9 + 7 + 7 + 12);
    assert_eq!(
        hex::encode(&bytes),
        format!(
            "0a07{}1205{}1a05{}220a{}",
            hex::encode("SN-0001"),
            hex::encode("MB-X1"),
            hex::encode("1.2.3"),
            hex::encode("extra note")
        )
    );
    let decoded = DeviceDescriptor::decode(&bytes).unwrap();
    assert_eq!(decoded, message);
    assert_eq!(decoded.encode(), bytes);
}

#[test]
fn roundtrip_device_descriptor_empty_strings() {
    // 4 个空串全部按 proto3 缺省不写。
    let message = DeviceDescriptor::default();
    assert_eq!(message.encode(), Vec::new());
    assert_eq!(DeviceDescriptor::decode(&[]).unwrap(), message);

    // 只有中间字段非空：字段号仍升序，缺省字段不占字节。
    let sparse = DeviceDescriptor::new("", "MB-X1", "", "x");
    let bytes = sparse.encode();
    assert_eq!(
        hex::encode(&bytes),
        format!("1205{}220178", hex::encode("MB-X1"))
    );
    assert_eq!(DeviceDescriptor::decode(&bytes).unwrap(), sparse);
}

#[test]
fn roundtrip_device_descriptor_multibyte_utf8() {
    let message = DeviceDescriptor::new("设备-01", "型号é", "固件😀", "备注");
    let bytes = message.encode();
    assert_eq!(DeviceDescriptor::decode(&bytes).unwrap(), message);
}

// ====================================================================
// descriptor_raw 原字节保留（§4.2 提取规则）
// ====================================================================

#[test]
fn descriptor_raw_is_canonical_for_constructed_request() {
    let message = request();
    // 客户端路径：raw 与 descriptor 的 canonical 编码同源（只编码一次并保留）。
    assert_eq!(
        message.descriptor_raw(),
        message.descriptor().encode().as_slice()
    );
}

#[test]
fn descriptor_raw_preserved_verbatim_for_non_canonical_wire() {
    // 非 canonical 嵌套字节：字段降序 + 显式写出两个空串缺省值。
    let raw = hex(concat!("2200", "1a05312e322e33", "12054d422d5831", "0a00"));
    let bytes = auth_request_bytes(
        &ramp::<32>(0xa1),
        &ramp::<32>(0xb1),
        &raw,
        &ramp::<64>(0xc0),
    );

    let message = ClientAuthRequest::decode(&bytes).expect("wire 字段顺序不受限制");
    // 原字节逐字保留，未被重编码。
    assert_eq!(message.descriptor_raw(), raw.as_slice());
    assert_eq!(
        message.descriptor(),
        &DeviceDescriptor::new("", "MB-X1", "1.2.3", "")
    );
    // 保留的 raw 与重编码结果确实不同：§4.2 禁止「重编码后验签」。
    assert_ne!(
        message.descriptor_raw(),
        message.descriptor().encode().as_slice()
    );
    // 外层编码原样写出保留的 raw，因此整包字节可复现。
    assert_eq!(message.encode().unwrap(), bytes);
}

#[test]
fn descriptor_raw_empty_nested_message_keeps_presence() {
    let bytes = hex(HEX_CLIENT_AUTH_REQUEST_EMPTY_DESCRIPTOR);
    let message = ClientAuthRequest::decode(&bytes).unwrap();
    assert!(message.descriptor_raw().is_empty());
    assert_eq!(message.descriptor_raw().len(), 0);
}

#[test]
fn with_descriptor_raw_accepts_equivalent_bytes() {
    // 从原始帧截取的 raw 字段乱序但解析结果一致：必须接受并原样保留。
    let raw = hex(concat!(
        "1a05312e322e33",
        "12054d422d5831",
        "0a07534e2d30303031"
    ));
    let message = request().with_descriptor_raw(&raw).unwrap();
    assert_eq!(message.descriptor_raw(), raw.as_slice());
    assert_eq!(message.descriptor(), &descriptor());
    // 编码写出保留的 raw，而不是 canonical 重编码。
    assert_eq!(
        message.encode().unwrap(),
        auth_request_bytes(
            &ramp::<32>(0xa1),
            &ramp::<32>(0xb1),
            &raw,
            &ramp::<64>(0xc0)
        )
    );
    // 往返后仍保留同一 raw。
    let again = ClientAuthRequest::decode(&message.encode().unwrap()).unwrap();
    assert_eq!(again.descriptor_raw(), raw.as_slice());
    assert_eq!(again, message);
}

#[test]
fn with_descriptor_raw_rejects_mismatch() {
    // 解析成功但语义不同（SN 不同）：必须拒绝，不接受「先替换再靠业务字段兜底」。
    let other = DeviceDescriptor::new("SN-9999", "MB-X1", "1.2.3", "").encode();
    assert_eq!(
        request().with_descriptor_raw(&other),
        Err(HandshakeMsgError::DescriptorRawMismatch)
    );
    // 多出字段：raw 自身的严格解析失败，报未知字段。
    assert_eq!(
        request().with_descriptor_raw(&append(descriptor().encode(), &[0x2a, 0x00])),
        Err(HandshakeMsgError::UnknownField(5))
    );
    // 非法 UTF-8。
    assert_eq!(
        request().with_descriptor_raw(&hex("0a02fffe")),
        Err(HandshakeMsgError::InvalidUtf8(1))
    );
    // 空 raw 解析成全空 descriptor，与当前 descriptor 不一致 → 拒绝。
    assert_eq!(
        request().with_descriptor_raw(b""),
        Err(HandshakeMsgError::DescriptorRawMismatch)
    );
}

#[test]
fn with_descriptor_raw_rejects_oversize() {
    let raw = vec![0u8; MAX_DEVICE_DESCRIPTOR_BYTES + 1];
    assert_eq!(
        request().with_descriptor_raw(&raw),
        Err(HandshakeMsgError::DescriptorTooLarge {
            len: MAX_DEVICE_DESCRIPTOR_BYTES + 1,
            max: MAX_DEVICE_DESCRIPTOR_BYTES,
        })
    );
}

#[test]
fn descriptor_limit_is_16247_bytes() {
    assert_eq!(MAX_DEVICE_DESCRIPTOR_BYTES, 16_247);
    assert_eq!(MAX_HANDSHAKE_MESSAGE_BYTES, 16_384);

    // 恰好 16 247B（1B tag + 2B 长度 varint + 16 244B 值）：合法，且整包恰 16 KiB。
    let at_limit = DeviceDescriptor::new("a".repeat(16_244), "", "", "");
    assert_eq!(at_limit.encode().len(), MAX_DEVICE_DESCRIPTOR_BYTES);
    let message = ClientAuthRequest::new(
        ramp::<32>(0xa1),
        ramp::<32>(0xb1),
        at_limit.clone(),
        ramp::<64>(0xc0),
    )
    .expect("descriptor 在派生上限上必须被接受");
    let bytes = message.encode().unwrap();
    assert_eq!(bytes.len(), MAX_HANDSHAKE_MESSAGE_BYTES);
    assert_eq!(
        ClientAuthRequest::decode(&bytes).unwrap().descriptor(),
        &at_limit
    );

    // 16 248B：构造器、descriptor 解析、raw 挂载三处都拒绝。
    let over = DeviceDescriptor::new("a".repeat(16_245), "", "", "");
    assert_eq!(over.encode().len(), MAX_DEVICE_DESCRIPTOR_BYTES + 1);
    let too_large = HandshakeMsgError::DescriptorTooLarge {
        len: MAX_DEVICE_DESCRIPTOR_BYTES + 1,
        max: MAX_DEVICE_DESCRIPTOR_BYTES,
    };
    assert_eq!(
        ClientAuthRequest::new(ramp::<32>(0xa1), ramp::<32>(0xb1), over, ramp::<64>(0xc0)),
        Err(too_large)
    );
    assert_eq!(
        DeviceDescriptor::decode(&descriptor_raw_of_len(MAX_DEVICE_DESCRIPTOR_BYTES + 1)),
        Err(too_large)
    );
    assert_eq!(
        request().with_descriptor_raw(&descriptor_raw_of_len(MAX_DEVICE_DESCRIPTOR_BYTES + 1)),
        Err(too_large)
    );
    // 上限内的 descriptor 原字节通过结构校验后按语义比较拒绝（内容不一致）。
    assert_eq!(
        request().with_descriptor_raw(&descriptor_raw_of_len(MAX_DEVICE_DESCRIPTOR_BYTES)),
        Err(HandshakeMsgError::DescriptorRawMismatch)
    );
}

// ====================================================================
// canonical 编码规则
// ====================================================================

#[test]
fn encoder_writes_fields_in_ascending_order() {
    let bytes = request().encode().unwrap();
    assert_eq!(bytes[0], 0x0a); // f1 bytes
    assert_eq!(bytes[34], 0x12); // f2 bytes
    assert_eq!(bytes[68], 0x1a); // f3 nested
    assert_eq!(bytes[69], 23);
    assert_eq!(bytes[70 + 23], 0x22); // f4 bytes
    assert_eq!(bytes[70 + 23 + 1], 64);

    let pending = ServerPending::new(ramp::<16>(0x11), ramp::<16>(0x22), 300).unwrap();
    let bytes = pending.encode().unwrap();
    assert_eq!(&bytes[..3], &[0x08, 0x88, 0x27]); // f1 = 5000
    assert_eq!(bytes[3], 0x12); // f2
    assert_eq!(bytes[21], 0x1a); // f3
    assert_eq!(bytes[39], 0x20); // f4
}

#[test]
fn encoder_omits_proto3_defaults() {
    // APPROVED 的 status/reason 均为 0：整包不出现 field 1/2 的 tag。
    let approved = ServerAuthResponse::approved(ramp::<32>(0x01), ramp::<64>(0x02)).unwrap();
    let bytes = approved.encode().unwrap();
    assert_eq!(&bytes[..2], &[0x1a, 0x20]);
    // 空串 descriptor 不写任何字段；wait_hint_ms 恒非 0 因此恒写出。
    assert_eq!(DeviceDescriptor::default().encode(), Vec::new());
    assert_eq!(
        ServerPending::new(ramp::<16>(0x11), ramp::<16>(0x22), 1)
            .unwrap()
            .encode()
            .unwrap()[0],
        0x08
    );
}

#[test]
fn encode_rejects_semantic_violations_before_writing() {
    let pending = ServerPending::new(ramp::<16>(0x11), ramp::<16>(0x22), 1).unwrap();
    assert_eq!(
        ServerPending {
            wait_hint_ms: 4_999,
            ..pending
        }
        .encode(),
        Err(HandshakeMsgError::BadWaitHintMs(4_999))
    );
    assert_eq!(
        ServerPending {
            probe_seq: 0,
            ..pending
        }
        .encode(),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    assert_eq!(
        PendingPong {
            probe_seq: 0,
            ..PendingPong::new(ramp::<16>(0x11), ramp::<16>(0x22), 1).unwrap()
        }
        .encode(),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    assert_eq!(
        ServerAuthResponse {
            status: 2,
            ..ServerAuthResponse::rejected(2, ramp::<64>(0xc0)).unwrap()
        }
        .encode(),
        Err(HandshakeMsgError::BadStatus(2))
    );
    // 构造器同样 fail-closed。
    assert_eq!(
        ServerAuthResponse::rejected(1, ramp::<64>(0xc0)),
        Err(HandshakeMsgError::BadReasonCode(1))
    );
    assert_eq!(
        ServerAuthResponse::approved([0u8; 32], ramp::<64>(0xc0)),
        Err(HandshakeMsgError::BadServerEphemeral)
    );
    assert_eq!(
        PendingPong::new(ramp::<16>(0x11), ramp::<16>(0x22), 0),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    assert_eq!(
        ServerPending::new(ramp::<16>(0x11), ramp::<16>(0x22), 0),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
}

// ====================================================================
// 负例：wire 结构严格性
// ====================================================================

#[test]
fn rejects_unknown_field_number() {
    // field 5（schema 外）wire type 2 / wire type 0 / 保留 wire type 3。
    assert_eq!(
        ClientHello::decode(&append(client_hello_bytes(), &[0x2a, 0x00])),
        Err(HandshakeMsgError::UnknownField(5))
    );
    assert_eq!(
        ClientHello::decode(&append(client_hello_bytes(), &[0x28, 0x01])),
        Err(HandshakeMsgError::UnknownField(5))
    );
    assert_eq!(
        ClientHello::decode(&append(client_hello_bytes(), &[0x2b])),
        Err(HandshakeMsgError::UnknownField(5))
    );
    // 嵌套 descriptor 的 schema 外字段号同样逐条审计。
    assert_eq!(
        DeviceDescriptor::decode(&hex("0a07534e2d303030312a00")),
        Err(HandshakeMsgError::UnknownField(5))
    );
    assert_eq!(
        ClientAuthRequest::decode(&auth_request_bytes(
            &ramp::<32>(0xa1),
            &ramp::<32>(0xb1),
            &append(descriptor().encode(), &[0x2a, 0x00]),
            &ramp::<64>(0xc0)
        )),
        Err(HandshakeMsgError::UnknownField(5))
    );
}

#[test]
fn rejects_duplicate_singular_field() {
    let hello = client_hello_bytes();
    assert_eq!(
        ClientHello::decode(&append(hello.clone(), &hello)),
        Err(HandshakeMsgError::DuplicateField(1))
    );

    let mut bytes = pong_bytes(7);
    bytes.extend_from_slice(&len_field(1, &ramp::<16>(0x33)));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::DuplicateField(1))
    );

    // 重复优先于 wire type：第二次出现即使 wire type 也错，仍报重复。
    let mut bytes = pong_bytes(7);
    bytes.extend_from_slice(&varint_field(1, 1));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::DuplicateField(1))
    );

    // 嵌套 descriptor 的重复字段。
    let mut raw = descriptor().encode();
    raw.extend_from_slice(&len_field(2, b"MB-X2"));
    assert_eq!(
        DeviceDescriptor::decode(&raw),
        Err(HandshakeMsgError::DuplicateField(2))
    );
    assert_eq!(
        request().with_descriptor_raw(&raw),
        Err(HandshakeMsgError::DuplicateField(2))
    );
}

#[test]
fn rejects_non_shortest_varint_key_length_and_integer() {
    // key varint 非最短：`8a 00` 是 0x0a 的两字节写法。
    let mut bytes = vec![0x8a, 0x00];
    bytes.extend_from_slice(&client_hello_bytes()[1..]);
    assert_eq!(
        ClientHello::decode(&bytes),
        Err(HandshakeMsgError::NonCanonicalVarint)
    );

    // 长度前缀 varint 非最短：`80 00` 是 0 的两字节写法。
    assert_eq!(
        ClientHello::decode(&[0x0a, 0x80, 0x00]),
        Err(HandshakeMsgError::NonCanonicalVarint)
    );

    // 整数值 varint 非最短：probe_seq = 1 写成 `20 81 00`。
    let mut bytes = pending_bytes(Some(5_000), None);
    bytes.extend_from_slice(&[0x20, 0x81, 0x00]);
    assert_eq!(
        ServerPending::decode(&bytes),
        Err(HandshakeMsgError::NonCanonicalVarint)
    );

    // 对照组：16 385 的规范写法是 `81 80 01`（中间组载荷为 0 但不是末组），必须接受。
    assert_eq!(varint(16_385), vec![0x81, 0x80, 0x01]);
    let mut bytes = pending_bytes(Some(5_000), None);
    bytes.extend_from_slice(&[0x20]);
    bytes.extend_from_slice(&varint(16_385));
    assert_eq!(ServerPending::decode(&bytes).unwrap().probe_seq, 16_385);
}

#[test]
fn rejects_truncated_input() {
    let bytes = client_hello_bytes();
    // 32B 负载少 1 字节：长度前缀已声明 32B，剩余不足 → 越界而非截断。
    assert_eq!(
        ClientHello::decode(&bytes[..33]),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 只剩 tag 与长度前缀：声明长度越过消息尾。
    assert_eq!(
        ClientHello::decode(&bytes[..2]),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 只有 tag：长度前缀缺失。
    assert_eq!(
        ClientHello::decode(&bytes[..1]),
        Err(HandshakeMsgError::Truncated)
    );
    // tag 的 varint 未闭合。
    assert_eq!(
        ClientHello::decode(&[0x8a]),
        Err(HandshakeMsgError::Truncated)
    );
    // 空载荷：必要字段缺失（不是截断）。
    assert_eq!(
        ClientHello::decode(&[]),
        Err(HandshakeMsgError::MissingField(1))
    );

    let pong = pong_bytes(9);
    assert_eq!(
        PendingPong::decode(&pong[..pong.len() - 1]),
        Err(HandshakeMsgError::Truncated)
    );
}

#[test]
fn rejects_bytes_after_message_end() {
    // 未消费的尾随字节按其 tag 逐条拒绝，绝不按「成功解析前缀」放行：
    // 指向 schema 外字段号 → `UnknownField`（未知字段先于其负载长度判定）。
    let hello = client_hello_bytes();
    assert_eq!(
        ClientHello::decode(&append(hello.clone(), &[0x2a, 0x00])),
        Err(HandshakeMsgError::UnknownField(5))
    );
    // 指向已知字段号 → `DuplicateField`。
    assert_eq!(
        ClientHello::decode(&append(hello.clone(), &hello)),
        Err(HandshakeMsgError::DuplicateField(1))
    );
    // 字段号 0 → `ZeroFieldNumber`。
    assert_eq!(
        ClientHello::decode(&append(hello.clone(), &[0x00])),
        Err(HandshakeMsgError::ZeroFieldNumber)
    );
    // 尾随字节指向尚未出现的已知字段且声明长度越过消息尾 → `LengthOutOfBounds`。
    let mut pending = varint_field(1, 5_000);
    pending.extend_from_slice(&varint_field(4, 1));
    assert_eq!(
        ServerPending::decode(&append(pending, &[0x12, 0x40])),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 嵌套 descriptor 内部的尾随字节同样拒绝。
    assert_eq!(
        DeviceDescriptor::decode(&append(descriptor().encode(), &[0x2a, 0x00])),
        Err(HandshakeMsgError::UnknownField(5))
    );
}

#[test]
fn rejects_invalid_utf8_in_descriptor_strings() {
    assert_eq!(
        DeviceDescriptor::decode(&hex("0a02fffe")),
        Err(HandshakeMsgError::InvalidUtf8(1))
    );
    assert_eq!(
        DeviceDescriptor::decode(&hex("12024180")),
        Err(HandshakeMsgError::InvalidUtf8(2))
    );
    // 截断的 3 字节序列（ED A0 80）同样非法。
    assert_eq!(
        DeviceDescriptor::decode(&hex("1a03eda080")),
        Err(HandshakeMsgError::InvalidUtf8(3))
    );
    // 经 ClientAuthRequest 路径也必须拒绝。
    assert_eq!(
        ClientAuthRequest::decode(&auth_request_bytes(
            &ramp::<32>(0xa1),
            &ramp::<32>(0xb1),
            &hex("2202c328"),
            &ramp::<64>(0xc0)
        )),
        Err(HandshakeMsgError::InvalidUtf8(4))
    );
}

#[test]
fn rejects_wrong_wire_type() {
    // field 1 期望 wire type 2，给出 0。
    assert_eq!(
        ClientHello::decode(&varint_field(1, 1)),
        Err(HandshakeMsgError::WrongWireType {
            field: 1,
            wire_type: 0
        })
    );
    // uint64 字段给出 wire type 2。
    let mut bytes = pending_bytes(Some(5_000), None);
    bytes.extend_from_slice(&[0x22, 0x01, 0x01]);
    assert_eq!(
        ServerPending::decode(&bytes),
        Err(HandshakeMsgError::WrongWireType {
            field: 4,
            wire_type: 2
        })
    );
    // 已出现过的已知字段再次出现，即使 wire type 也错，仍先报 `DuplicateField`。
    assert_eq!(
        PendingPong::decode(&append(pong_bytes(1), &[0x19])),
        Err(HandshakeMsgError::DuplicateField(3))
    );
    // 尚未出现的已知字段给出保留 wire type 1。
    let mut bytes = len_field(1, &ramp::<16>(0x11));
    bytes.extend_from_slice(&len_field(2, &ramp::<16>(0x22)));
    bytes.push(0x19);
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::WrongWireType {
            field: 3,
            wire_type: 1
        })
    );
    // 保留 wire type 5（fixed32）落在字段 1 上。
    assert_eq!(
        ClientHello::decode(&[0x0d]),
        Err(HandshakeMsgError::WrongWireType {
            field: 1,
            wire_type: 5
        })
    );
    // descriptor 的 string 字段给出 wire type 0。
    assert_eq!(
        DeviceDescriptor::decode(&hex("0801")),
        Err(HandshakeMsgError::WrongWireType {
            field: 1,
            wire_type: 0
        })
    );
}

#[test]
fn rejects_required_bytes_with_wrong_length() {
    assert_eq!(
        ClientHello::decode(&len_field(1, &[0u8; 31])),
        Err(HandshakeMsgError::BadFieldLength {
            field: 1,
            len: 31,
            expected: 32
        })
    );
    assert_eq!(
        ClientHello::decode(&len_field(1, &[0u8; 33])),
        Err(HandshakeMsgError::BadFieldLength {
            field: 1,
            len: 33,
            expected: 32
        })
    );
    // 空 bytes 不能冒充必要字段。
    assert_eq!(
        ServerChallenge::decode(&len_field(1, b"")),
        Err(HandshakeMsgError::BadFieldLength {
            field: 1,
            len: 0,
            expected: 32
        })
    );
    // 63B/65B 签名。
    assert_eq!(
        ClientAuthRequest::decode(&auth_request_bytes(
            &ramp::<32>(0xa1),
            &ramp::<32>(0xb1),
            &descriptor().encode(),
            &[0u8; 63]
        )),
        Err(HandshakeMsgError::BadFieldLength {
            field: 4,
            len: 63,
            expected: 64
        })
    );
    assert_eq!(
        ClientAuthRequest::decode(&auth_request_bytes(
            &ramp::<32>(0xa1),
            &ramp::<32>(0xb1),
            &descriptor().encode(),
            &[0u8; 65]
        )),
        Err(HandshakeMsgError::BadFieldLength {
            field: 4,
            len: 65,
            expected: 64
        })
    );
    // 31B 服务端临时公钥。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(0, 0, &[0u8; 31])),
        Err(HandshakeMsgError::BadFieldLength {
            field: 3,
            len: 31,
            expected: 32
        })
    );
    // 15B pending_token 与 17B status_nonce。
    let mut bytes = len_field(1, &[0u8; 15]);
    bytes.extend_from_slice(&len_field(2, &ramp::<16>(0x22)));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::BadFieldLength {
            field: 1,
            len: 15,
            expected: 16
        })
    );
    let mut bytes = len_field(1, &ramp::<16>(0x11));
    bytes.extend_from_slice(&len_field(2, &[0u8; 17]));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::BadFieldLength {
            field: 2,
            len: 17,
            expected: 16
        })
    );
}

#[test]
fn rejects_missing_required_fields() {
    assert_eq!(
        ClientHello::decode(&[]),
        Err(HandshakeMsgError::MissingField(1))
    );
    assert_eq!(
        ServerChallenge::decode(&[]),
        Err(HandshakeMsgError::MissingField(1))
    );

    // 逐个省略 ClientAuthRequest 的 4 个必要字段。
    let parts = [
        len_field(1, &ramp::<32>(0xa1)),
        len_field(2, &ramp::<32>(0xb1)),
        len_field(3, &descriptor().encode()),
        len_field(4, &ramp::<64>(0xc0)),
    ];
    for skip in 0..4 {
        let mut bytes = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i != skip {
                bytes.extend_from_slice(part);
            }
        }
        assert_eq!(
            ClientAuthRequest::decode(&bytes),
            Err(HandshakeMsgError::MissingField(skip as u32 + 1)),
            "省略字段 {} 必须报缺失",
            skip + 1
        );
    }

    // ServerPending 缺 token / nonce。
    let mut bytes = varint_field(1, 5_000);
    bytes.extend_from_slice(&varint_field(4, 1));
    assert_eq!(
        ServerPending::decode(&bytes),
        Err(HandshakeMsgError::MissingField(2))
    );
    let mut bytes = len_field(2, &ramp::<16>(0x11));
    bytes.extend_from_slice(&varint_field(1, 5_000));
    bytes.extend_from_slice(&varint_field(4, 1));
    assert_eq!(
        ServerPending::decode(&bytes),
        Err(HandshakeMsgError::MissingField(3))
    );

    // ServerAuthResponse 缺 eph / signature。
    let mut bytes = varint_field(1, 0);
    bytes.extend_from_slice(&len_field(4, &ramp::<64>(0xc0)));
    assert_eq!(
        ServerAuthResponse::decode(&bytes),
        Err(HandshakeMsgError::MissingField(3))
    );
    let mut bytes = varint_field(1, 1);
    bytes.extend_from_slice(&len_field(3, &[0u8; 32]));
    assert_eq!(
        ServerAuthResponse::decode(&bytes),
        Err(HandshakeMsgError::MissingField(4))
    );

    // PendingPong 缺 status_nonce（必要 bytes 缺失优先于 probe_seq 缺失报出）。
    let mut bytes = len_field(1, &ramp::<16>(0x11));
    bytes.extend_from_slice(&varint_field(3, 1));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::MissingField(2))
    );
}

#[test]
fn rejects_length_prefix_beyond_payload() {
    // 声明 64B 只给 32B。
    let mut bytes = vec![0x0a, 0x40];
    bytes.extend_from_slice(&ramp::<32>(0x00));
    assert_eq!(
        ClientHello::decode(&bytes),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 声明 2^21 长度：先判上限，不分配、不 panic。
    assert_eq!(
        ClientHello::decode(&[0x0a, 0x80, 0x80, 0x80, 0x10]),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 声明 2^64-1 长度。
    let mut bytes = vec![0x0a];
    bytes.extend_from_slice(&[0xff; 9]);
    bytes.push(0x01);
    assert_eq!(
        ClientHello::decode(&bytes),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
    // 嵌套 descriptor 声明长度越过消息尾。
    let mut bytes = len_field(1, &ramp::<32>(0xa1));
    bytes.extend_from_slice(&len_field(2, &ramp::<32>(0xb1)));
    bytes.extend_from_slice(&[0x1a, 0x7f]);
    bytes.extend_from_slice(&descriptor().encode());
    bytes.extend_from_slice(&len_field(4, &ramp::<64>(0xc0)));
    assert_eq!(
        ClientAuthRequest::decode(&bytes),
        Err(HandshakeMsgError::LengthOutOfBounds)
    );
}

#[test]
fn rejects_zero_field_number_and_uint32_overflow() {
    assert_eq!(
        ClientHello::decode(&[0x00]),
        Err(HandshakeMsgError::ZeroFieldNumber)
    );
    assert_eq!(
        ClientHello::decode(&[0x08]),
        Err(HandshakeMsgError::WrongWireType {
            field: 1,
            wire_type: 0
        })
    );
    // status = 2^32：varint 越出 uint32。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(1u64 << 32, 0, &ramp::<32>(0xb1))),
        Err(HandshakeMsgError::Uint32OutOfRange)
    );
    // uint32 边界的 4 294 967 295 可解析，随后按非法 status 拒绝。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(
            u64::from(u32::MAX),
            0,
            &ramp::<32>(0xb1)
        )),
        Err(HandshakeMsgError::BadStatus(u32::MAX))
    );
    // probe_seq 是 uint64：2^64-1 合法（tag + 10B varint）。
    assert_eq!(pong_bytes(u64::MAX).len(), 38 + 9);
}

// ====================================================================
// 负例：§4.3 与 §4.1 的状态语义
// ====================================================================

#[test]
fn rejects_illegal_status() {
    for status in [2u64, 3, 255] {
        assert_eq!(
            ServerAuthResponse::decode(&auth_response_bytes(status, 0, &ramp::<32>(0xb1))),
            Err(HandshakeMsgError::BadStatus(status as u32)),
            "status {status} 必须拒绝",
        );
    }
}

#[test]
fn rejects_illegal_reason_code() {
    // APPROVED(status=0) 只允许 reason=0。
    for reason in [1u64, 2, 3, 4, 5, 6] {
        assert_eq!(
            ServerAuthResponse::decode(&auth_response_bytes(0, reason, &ramp::<32>(0xb1))),
            Err(HandshakeMsgError::BadReasonCode(reason as u32)),
            "APPROVED 携带 reason {reason} 必须拒绝",
        );
    }
    // REJECTED(status=1)：1 = SIGNATURE_INVALID 在 v3 绝不发出；6/7/99 未定义。
    for reason in [1u64, 6, 7, 99] {
        assert_eq!(
            ServerAuthResponse::decode(&auth_response_bytes(1, reason, &[0u8; 32])),
            Err(HandshakeMsgError::BadReasonCode(reason as u32)),
            "REJECTED 携带 reason {reason} 必须拒绝",
        );
    }
    // 合法集合逐条通过。
    for reason in [0u32, 2, 3, 4, 5] {
        assert!(ServerAuthResponse::rejected(reason, ramp::<64>(0xc0)).is_ok());
    }
}

#[test]
fn rejects_ephemeral_key_zero_constraint() {
    // APPROVED 必须非全零。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(0, 0, &[0u8; 32])),
        Err(HandshakeMsgError::BadServerEphemeral)
    );
    // REJECTED 必须全零。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(1, 2, &ramp::<32>(0xb1))),
        Err(HandshakeMsgError::BadServerEphemeral)
    );
    // 末字节非零同样拒绝（全零判定逐字节）。
    let mut almost_zero = [0u8; 32];
    almost_zero[31] = 0x01;
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(1, 0, &almost_zero)),
        Err(HandshakeMsgError::BadServerEphemeral)
    );
    // reason 检查先于 eph 检查（固定顺序）。
    assert_eq!(
        ServerAuthResponse::decode(&auth_response_bytes(1, 1, &ramp::<32>(0xb1))),
        Err(HandshakeMsgError::BadReasonCode(1))
    );
}

#[test]
fn rejects_bad_wait_hint_ms() {
    for hint in [0u64, 1, 4_999, 5_001, 10_000] {
        assert_eq!(
            ServerPending::decode(&pending_bytes(Some(hint), Some(1))),
            Err(HandshakeMsgError::BadWaitHintMs(hint as u32)),
            "wait_hint_ms {hint} 必须拒绝",
        );
    }
    // 缺失的 scalar 按 0 严格解释，不推导为「未提供」。
    assert_eq!(
        ServerPending::decode(&pending_bytes(None, Some(1))),
        Err(HandshakeMsgError::BadWaitHintMs(0))
    );
    assert_eq!(
        ServerPending::decode(&pending_bytes(Some(5_000), Some(1)))
            .unwrap()
            .wait_hint_ms,
        5_000
    );
}

#[test]
fn rejects_zero_probe_seq() {
    assert_eq!(
        ServerPending::decode(&pending_bytes(Some(5_000), Some(0))),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    // 缺失按 0 解释 → 同样拒绝。
    assert_eq!(
        ServerPending::decode(&pending_bytes(Some(5_000), None)),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    // wait_hint 检查先于 probe_seq。
    assert_eq!(
        ServerPending::decode(&pending_bytes(Some(0), Some(0))),
        Err(HandshakeMsgError::BadWaitHintMs(0))
    );
    // PendingPong 缺失或显式写 0 都拒绝。
    let mut bytes = len_field(1, &ramp::<16>(0x11));
    bytes.extend_from_slice(&len_field(2, &ramp::<16>(0x22)));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
    bytes.extend_from_slice(&varint_field(3, 0));
    assert_eq!(
        PendingPong::decode(&bytes),
        Err(HandshakeMsgError::ZeroProbeSeq)
    );
}

// ====================================================================
// 负例：整包长度上限
// ====================================================================

#[test]
fn rejects_oversized_message() {
    // 恰好 16 KiB（1B tag + 2B 长度 varint + 16 381B 值）：长度检查放行，
    // 由必要字段长度检查继续拒绝。
    let mut bytes = vec![0x0a];
    bytes.extend_from_slice(&varint(16_381));
    bytes.extend_from_slice(&vec![0u8; 16_381]);
    assert_eq!(bytes.len(), MAX_HANDSHAKE_MESSAGE_BYTES);
    assert_eq!(
        ClientHello::decode(&bytes),
        Err(HandshakeMsgError::BadFieldLength {
            field: 1,
            len: 16_381,
            expected: 32
        })
    );
    // 超 1 字节即整包拒绝，先于任何字段解析。
    let mut over = bytes;
    over.push(0x00);
    assert_eq!(
        ClientHello::decode(&over),
        Err(HandshakeMsgError::MessageTooLarge {
            len: MAX_HANDSHAKE_MESSAGE_BYTES + 1,
            max: MAX_HANDSHAKE_MESSAGE_BYTES,
        })
    );
    // descriptor 超 16 247B 时整包必然越过 16 KiB（外层固定开销 137B），
    // 因此在 ClientAuthRequest 上表现为整包超限；descriptor 自身的上限由
    // `DeviceDescriptor::decode`、构造器与 `with_descriptor_raw` 拒绝。
    let bytes = auth_request_bytes(
        &ramp::<32>(0xa1),
        &ramp::<32>(0xb1),
        &descriptor_raw_of_len(MAX_DEVICE_DESCRIPTOR_BYTES + 1),
        &ramp::<64>(0xc0),
    );
    assert_eq!(bytes.len(), MAX_HANDSHAKE_MESSAGE_BYTES + 1);
    assert_eq!(
        ClientAuthRequest::decode(&bytes),
        Err(HandshakeMsgError::MessageTooLarge {
            len: MAX_HANDSHAKE_MESSAGE_BYTES + 1,
            max: MAX_HANDSHAKE_MESSAGE_BYTES,
        })
    );
}

// ====================================================================
// 错误语义与方向无关
// ====================================================================

#[test]
fn same_violation_yields_same_variant_on_both_sides() {
    // 编码侧（构造非法结构体后 encode）与解码侧（同一字节）必须给同一变体。
    let bad = ServerAuthResponse {
        status: 1,
        reason_code: 1,
        server_x25519_eph_pub: ramp::<32>(0xb1),
        signature: ramp::<64>(0xc0),
    };
    let encode_err = bad.encode().unwrap_err();
    let decode_err =
        ServerAuthResponse::decode(&auth_response_bytes(1, 1, &ramp::<32>(0xb1))).unwrap_err();
    assert_eq!(encode_err, decode_err);
    assert_eq!(encode_err, HandshakeMsgError::BadReasonCode(1));
}

#[test]
fn crate_root_reexports_handshake_messages() {
    // lib.rs 的 re-export 与模块路径指向同一类型。
    let a: rsetup_protocol::ClientHello = ClientHello::new(ramp::<32>(0x01));
    let b: ClientHello = a;
    assert_eq!(b.client_random, ramp::<32>(0x01));
    assert_eq!(
        rsetup_protocol::MAX_HANDSHAKE_MESSAGE_BYTES,
        MAX_HANDSHAKE_MESSAGE_BYTES
    );
    assert_eq!(
        rsetup_protocol::MAX_DEVICE_DESCRIPTOR_BYTES,
        MAX_DEVICE_DESCRIPTOR_BYTES
    );
    assert_eq!(rsetup_protocol::PENDING_WAIT_HINT_MS, PENDING_WAIT_HINT_MS);
    assert_eq!(
        std::mem::size_of::<rsetup_protocol::ServerAuthResponse>(),
        std::mem::size_of::<ServerAuthResponse>()
    );
    let _ = std::mem::size_of::<rsetup_protocol::ServerChallenge>();
    let _ = std::mem::size_of::<rsetup_protocol::ClientAuthRequest>();
    let _ = std::mem::size_of::<rsetup_protocol::DeviceDescriptor>();
    let _ = std::mem::size_of::<rsetup_protocol::ServerPending>();
    let _ = std::mem::size_of::<rsetup_protocol::PendingPong>();
    assert_eq!(
        rsetup_protocol::HandshakeMsgError::Truncated,
        HandshakeMsgError::Truncated
    );
}
