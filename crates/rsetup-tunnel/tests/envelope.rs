use rsetup_tunnel::envelope::{
    EnvelopeError, MAX_ACTION_LEN, MAX_ENVELOPE_SIZE, MAX_METADATA_ENTRIES, MAX_METADATA_KEY_LEN,
    MAX_METADATA_VALUE_LEN, PacketType, StreamKind, TunnelPacket,
};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// 测试辅助：wire 一律由参数化构造器拼装，wire 负例统一叠加在**合法信元**之上，
// 保证错误来自被测点，而不是顺带的语义违规（也不因此弱化校验）。
// ---------------------------------------------------------------------------

fn sample_request() -> TunnelPacket {
    TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: "device.reboot".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1, 2, 3, 4],
        metadata: BTreeMap::from([("k".to_string(), "v".to_string())]),
    }
}

fn varint(mut val: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        if val < 0x80 {
            out.push(val as u8);
            return out;
        }
        out.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
}

fn tag(field: u32, wire_type: u8) -> Vec<u8> {
    varint(((field as u64) << 3) | (wire_type as u64))
}

#[allow(dead_code)]
fn int32_field(field: u32, val: i32) -> Vec<u8> {
    let mut out = tag(field, 0);
    out.extend(varint(val as i64 as u64));
    out
}

fn uint64_field(field: u32, val: u64) -> Vec<u8> {
    let mut out = tag(field, 0);
    out.extend(varint(val));
    out
}

/// 以给定 varint 字节作为字段值（构造越界 int32 等负例）。
fn raw_varint_field(field: u32, raw: &[u8]) -> Vec<u8> {
    let mut out = tag(field, 0);
    out.extend_from_slice(raw);
    out
}

fn bytes_field(field: u32, val: &[u8]) -> Vec<u8> {
    let mut out = tag(field, 2);
    out.extend(varint(val.len() as u64));
    out.extend_from_slice(val);
    out
}

/// 长度前缀撒谎用的裸长度。
fn len_prefix(len: usize) -> Vec<u8> {
    varint(len as u64)
}

/// 合法信元 wire：字段 1/2/3。
fn wire_head(trace_id: u64, packet_type: u32, action: &str) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend(uint64_field(1, trace_id));
    raw.extend(uint64_field(2, u64::from(packet_type)));
    raw.extend(bytes_field(3, action.as_bytes()));
    raw
}

/// 语义合法的 Request 前缀。
fn legal_request_wire() -> Vec<u8> {
    wire_head(1, 1, "device.status")
}

/// Response 头：语义合法（status=0 且无 error_message 即成功响应）。
fn response_head_wire() -> Vec<u8> {
    wire_head(1, 2, "device.query")
}

/// 语义合法的 Response 前缀（已携带 status_code / error_message）。
fn legal_response_wire() -> Vec<u8> {
    let mut raw = wire_head(1, 2, "device.query");
    raw.extend(raw_varint_field(4, &[0x05]));
    raw.extend(bytes_field(5, b"boom"));
    raw
}

fn map_entry(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend(bytes_field(1, key));
    entry.extend(bytes_field(2, value));
    entry
}

const B62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 固定长度、互不相同的 base62 key，便于把 fixture 凑到精确字节数。
fn b62_key(v: u32, len: usize) -> Vec<u8> {
    let mut out = vec![b'0'; len];
    let mut i = len;
    let mut v = v;
    while i > 0 {
        i -= 1;
        out[i] = B62[(v % 62) as usize];
        v /= 62;
    }
    out
}

/// `entries` 项 metadata 的字段 7 序列（不含信元头部）。
///
/// 前 `entries-12` 项使用 3 字节 key（线上 10 字节/项），最后 12 项使用 2 字节 key（9 字节/项）。
fn metadata_entry_fields(entries: usize) -> Vec<u8> {
    assert!(entries >= 12);
    let mut raw = Vec::new();
    let long = (entries - 12) as u32;
    for i in 0..entries as u32 {
        let key = if i < long {
            b62_key(i, 3)
        } else {
            b62_key(i - long, 2)
        };
        raw.extend(bytes_field(7, &map_entry(&key, b"v")));
    }
    raw
}

/// 恰好 `expect_len` 字节的完整信元 wire。20,000 项 → 19 + 19988*10 + 12*9 = 200,007 字节。
fn metadata_heavy_wire(entries: usize, expect_len: usize) -> Vec<u8> {
    let mut raw = legal_request_wire();
    raw.extend(metadata_entry_fields(entries));
    assert_eq!(raw.len(), expect_len, "fixture 尺寸应为 {expect_len}B");
    raw
}

fn decode_err(raw: &[u8]) -> EnvelopeError {
    match TunnelPacket::decode(raw) {
        Ok(p) => panic!("decode 竟然接受: {p:?}"),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// 1. 往返与线上格式
// ---------------------------------------------------------------------------

#[test]
fn test_roundtrip_basic() {
    let packet = TunnelPacket {
        trace_id: 42,
        packet_type: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0xaa; 16],
        metadata: BTreeMap::new(), // ping 规定 metadata 为空
    };

    packet.validate(StreamKind::Control).unwrap();
    packet.validate(StreamKind::Data).unwrap();

    let encoded = packet.encode().unwrap();
    assert_eq!(encoded.len(), packet.serialized_len().unwrap());
    let decoded = TunnelPacket::decode(&encoded).unwrap();
    assert_eq!(decoded, packet);
}

#[test]
fn test_roundtrip_with_boundary_metadata_and_response() {
    let packet = TunnelPacket {
        trace_id: u64::MAX,
        packet_type: PacketType::Response,
        action: "device.query".to_string(),
        status_code: 42,
        error_message: "upstream timeout".to_string(),
        payload: vec![9; 300],
        metadata: BTreeMap::from([
            ("a".to_string(), "v".repeat(MAX_METADATA_VALUE_LEN)),
            ("k".repeat(MAX_METADATA_KEY_LEN), String::new()),
        ]),
    };

    packet.validate(StreamKind::Data).unwrap();
    let encoded = packet.encode().unwrap();
    assert_eq!(encoded.len(), packet.serialized_len().unwrap());
    let decoded = TunnelPacket::decode(&encoded).unwrap();
    assert_eq!(decoded, packet, "预算上限长度的 metadata 必须原样往返");
}

#[test]
fn test_negative_status_code_varint_10_bytes() {
    let packet = TunnelPacket {
        trace_id: 100,
        packet_type: PacketType::Response,
        action: "device.query".to_string(),
        status_code: -1,
        error_message: "failed".to_string(),
        payload: Vec::new(),
        metadata: BTreeMap::new(),
    };

    packet.validate(StreamKind::Control).unwrap();
    let encoded = packet.encode().unwrap();

    // 字段 4 status_code = -1 必须是 10 字节 varint: ff*9 01
    let tag_4 = [0x20u8];
    let expected_varint = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];

    let tag_pos = encoded
        .windows(tag_4.len())
        .position(|w| w == tag_4)
        .expect("tag 4 must exist");
    assert_eq!(
        &encoded[tag_pos + 1..tag_pos + 1 + 10],
        &expected_varint[..],
        "int32 负数必须是 10 字节 varint"
    );

    let decoded = TunnelPacket::decode(&encoded).unwrap();
    assert_eq!(decoded.status_code, -1);

    // i32::MIN 同样是 10 字节符号扩展
    let mut min_pkt = packet.clone();
    min_pkt.status_code = i32::MIN;
    let enc_min = min_pkt.encode().unwrap();
    let pos = enc_min
        .windows(1)
        .position(|w| w == tag_4)
        .expect("tag 4 must exist");
    let min_varint = varint(i32::MIN as i64 as u64);
    assert_eq!(
        min_varint.len(),
        10,
        "i32::MIN 的符号扩展 varint 必须 10 字节"
    );
    assert_eq!(
        &enc_min[pos + 1..pos + 1 + min_varint.len()],
        &min_varint[..],
        "i32::MIN 必须是 10 字节符号扩展 varint"
    );
    assert_eq!(
        TunnelPacket::decode(&enc_min).unwrap().status_code,
        i32::MIN
    );
}

#[test]
fn test_int32_legal_range_boundaries() {
    // 合法：i32::MAX（5 字节）
    let mut raw = response_head_wire();
    raw.extend(raw_varint_field(4, &[0xff, 0xff, 0xff, 0xff, 0x07]));
    let p = TunnelPacket::decode(&raw).expect("i32::MAX 必须接受");
    assert_eq!(p.status_code, i32::MAX);

    // 合法：显式 status = 0（无 error_message 的成功 Response）
    let mut raw0 = wire_head(1, 2, "device.query");
    raw0.extend(raw_varint_field(4, &[0x00]));
    let p0 = TunnelPacket::decode(&raw0).expect("显式 0 应可用");
    assert_eq!(p0.status_code, 0);

    // 合法：10 字节符号扩展的 -1 与 i32::MIN
    for (name, raw_v, want) in [
        (
            "-1",
            vec![0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            -1i32,
        ),
        ("i32::MIN", varint(i32::MIN as i64 as u64), i32::MIN),
        ("-4096", varint(-4096i64 as u64), -4096i32),
    ] {
        assert_eq!(raw_v.len(), 10, "{name} 的负数编码必须是 10 字节");
        let mut raw = response_head_wire();
        raw.extend(raw_varint_field(4, &raw_v));
        let p = TunnelPacket::decode(&raw).unwrap_or_else(|e| panic!("{name} 应合法: {e}"));
        assert_eq!(p.status_code, want, "{name}");
    }
}

#[test]
fn test_int32_rejects_truncated_and_out_of_range_values() {
    // 这些输入全部会被 `as i32` 静默截断或吞掉溢出
    let cases: Vec<(&str, u64)> = vec![
        ("2^32 (截断成 0)", 1u64 << 32),
        ("2^32+1 (截断成 1)", (1u64 << 32) + 1),
        ("u32::MAX (截断成 -1)", u64::from(u32::MAX)),
        ("2^31 (> i32::MAX)", 1u64 << 31),
        ("i64::MAX", i64::MAX as u64),
        ("0xFFFFFFFF00000000 (-2^32)", 0xFFFF_FFFF_0000_0000u64),
        ("0xFFFFFFFF7FFFFFFF (> i64::MAX)", 0xFFFF_FFFF_7FFF_FFFFu64),
    ];
    for (name, val) in cases {
        let mut raw = response_head_wire();
        raw.extend(raw_varint_field(4, &varint(val)));
        match TunnelPacket::decode(&raw) {
            Ok(p) => panic!("{name} 被解码为 status_code={}", p.status_code),
            Err(EnvelopeError::Int32OutOfRange) => {}
            Err(other) => panic!("{name} 期望 Int32OutOfRange，实际 {other:?}"),
        }
    }

    // 5 字节 0xffffffff 的裸字节形态同样拒绝
    let mut raw = response_head_wire();
    raw.extend(raw_varint_field(4, &[0xff, 0xff, 0xff, 0xff, 0x0f]));
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::Int32OutOfRange)
    ));

    // encode 侧 i32 全域往返，不含截断
    for v in [1i32, -1, i32::MAX, i32::MIN, i32::MIN + 1, i32::MAX - 1] {
        let mut p = sample_request();
        p.packet_type = PacketType::Response;
        p.status_code = v;
        p.error_message = if v == 0 { String::new() } else { "e".into() };
        let enc = p.encode().unwrap();
        assert_eq!(TunnelPacket::decode(&enc).unwrap().status_code, v, "{v}");
    }
}

#[test]
fn test_envelope_size_limit_512kib() {
    // 恰好 524,288 字节允许；超过 1 字节即拒绝
    let base_packet = TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: "test.data".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: Vec::new(),
        metadata: BTreeMap::new(),
    };
    let base_len = base_packet.encode().unwrap().len();

    let target = MAX_ENVELOPE_SIZE;
    let mut payload_len = target - base_len - 4;
    let mut packet = base_packet.clone();
    packet.payload = vec![0u8; payload_len];
    let mut enc = packet.encode().unwrap();
    while enc.len() < target {
        payload_len += target - enc.len();
        packet.payload = vec![0u8; payload_len];
        enc = packet.encode().unwrap();
    }
    if enc.len() > target {
        payload_len -= enc.len() - target;
        packet.payload = vec![0u8; payload_len];
        enc = packet.encode().unwrap();
    }
    assert_eq!(enc.len(), target);
    assert!(packet.validate(StreamKind::Data).is_ok());

    let decoded = TunnelPacket::decode(&enc).unwrap();
    assert_eq!(decoded.payload.len(), payload_len);

    packet.payload.push(1);
    assert!(matches!(
        packet.encode(),
        Err(EnvelopeError::MessageTooLarge(_))
    ));

    let mut over_buf = enc.clone();
    over_buf.push(0);
    assert!(matches!(
        TunnelPacket::decode(&over_buf),
        Err(EnvelopeError::MessageTooLarge(_))
    ));
}

#[test]
fn test_encode_rejects_oversized_payload_without_copying_it() {
    // 语义层：8MiB payload 必须被拒绝（分配预算证据见 tests/grpc_memory_budget.rs）
    let mut packet = sample_request();
    packet.payload = vec![0u8; 8 * 1024 * 1024];
    assert!(matches!(
        packet.encode(),
        Err(EnvelopeError::MessageTooLarge(_))
    ));
    assert!(matches!(
        packet.serialized_len(),
        Err(EnvelopeError::MessageTooLarge(_))
    ));
    assert!(
        packet.validate(StreamKind::Data).is_ok(),
        "语义合法，仅尺寸超限"
    );
}

// ---------------------------------------------------------------------------
// 2. 公共入口自身必须做语义校验（不靠调用方记得 validate）
// ---------------------------------------------------------------------------

#[test]
fn test_encode_rejects_illegal_semantics_at_public_entry() {
    let mut p = sample_request();
    p.trace_id = 0;
    assert!(matches!(
        p.encode(),
        Err(EnvelopeError::Validation(m)) if m.contains("trace_id")
    ));

    let mut p = sample_request();
    p.action = "Bad.Action".to_string();
    assert!(p.encode().is_err());

    let mut p = sample_request();
    p.action = String::new();
    assert!(p.encode().is_err());

    // 业务 action 不得使用 TYPE_SYSTEM（与流无关，公共校验即拒绝）
    let mut p = sample_request();
    p.packet_type = PacketType::System;
    assert!(p.encode().is_err());

    // 非 Response 携带错误字段
    let mut p = sample_request();
    p.error_message = "boom".to_string();
    assert!(p.encode().is_err());

    // 越界 action 长度在编码前拒绝
    let mut p = sample_request();
    p.action = "a".repeat(MAX_ACTION_LEN + 1);
    assert!(matches!(
        p.encode(),
        Err(EnvelopeError::ActionTooLarge(65, MAX_ACTION_LEN))
    ));
}

#[test]
fn test_decode_rejects_illegal_semantics_at_public_entry() {
    // 全部是**合法 wire**，只是语义非法
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("trace_id=0", wire_head(0, 1, "device.status")),
        (
            "业务 action 使用 TYPE_SYSTEM",
            wire_head(1, 4, "device.status"),
        ),
        (
            "tunnel.kick 未用 TYPE_SYSTEM",
            wire_head(1, 1, "tunnel.kick"),
        ),
        ("未知 tunnel. 前缀", wire_head(1, 1, "tunnel.other")),
        ("Request 携带 status_code", {
            let mut raw = legal_request_wire();
            raw.extend(raw_varint_field(4, &[0x01]));
            raw
        }),
        ("成功 Response 携带 error_message", {
            let mut raw = wire_head(1, 2, "device.query");
            raw.extend(bytes_field(5, b"boom"));
            raw
        }),
        ("ping 载荷不是 16B", {
            let mut raw = wire_head(1, 1, "tunnel.ping");
            raw.extend(bytes_field(6, b"0123456789"));
            raw
        }),
        ("ping 携带 metadata", {
            let mut raw = wire_head(1, 1, "tunnel.ping");
            raw.extend(bytes_field(6, &[3u8; 16]));
            raw.extend(bytes_field(7, &map_entry(b"k", b"v")));
            raw
        }),
        ("ping Response status != 0", {
            let mut raw = wire_head(1, 2, "tunnel.ping");
            raw.extend(bytes_field(6, &[3u8; 16]));
            raw.extend(raw_varint_field(4, &[0x01]));
            raw
        }),
    ];

    for (name, raw) in cases {
        match TunnelPacket::decode(&raw) {
            Ok(p) => panic!("{name}: decode 竟然接受，得 {p:?}"),
            Err(EnvelopeError::Validation(_)) => {}
            Err(other) => panic!("{name}: 期望 Validation，实际 {other:?}"),
        }
    }
}

#[test]
fn test_validate_fields_and_stream_split() {
    // 公共字段规则与流无关：两种流都必须拒绝同一批语义违规，且不因 stream 参数掩盖
    let mut p = sample_request();
    p.packet_type = PacketType::System;
    assert!(matches!(
        p.validate_fields(),
        Err(EnvelopeError::Validation(_))
    ));
    for stream in [StreamKind::Control, StreamKind::Data] {
        assert!(p.validate(stream).is_err(), "{stream:?} 必须拒绝");
    }

    // 字段合法、流归属非法：validate_fields 放行，validate(Data) 拒绝
    let kick = TunnelPacket {
        trace_id: 7,
        packet_type: PacketType::System,
        action: "tunnel.kick".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1],
        metadata: BTreeMap::new(),
    };
    kick.validate_fields().expect("kick 信元自身合法");
    kick.validate(StreamKind::Control).expect("控制流合法");
    assert!(matches!(
        kick.validate(StreamKind::Data),
        Err(EnvelopeError::Validation(m)) if m.contains("Control")
    ));

    // 公共编解码入口不依赖 stream：流判定属调用方上下文，不得掩盖公共规则
    let enc = kick.encode().expect("encode 不依赖 stream");
    assert_eq!(TunnelPacket::decode(&enc).unwrap(), kick);

    // 公共非法的信元，任何 stream 都拒绝，且公共入口也拒绝
    let mut bad = kick.clone();
    bad.action = "tunnel.kick".to_string();
    bad.status_code = 9; // System 属非 Response，禁止错误字段
    for stream in [StreamKind::Control, StreamKind::Data] {
        assert!(bad.validate(stream).is_err());
    }
    assert!(bad.encode().is_err());
}

// ---------------------------------------------------------------------------
// 3. metadata 预算（分配前拒绝）与错误去敏
// ---------------------------------------------------------------------------

#[test]
fn test_decode_rejects_20000_metadata_entries_before_construction() {
    // 200,007 字节 / 20,000 项：必须在第 17 项构造之前失败（分配预算见 allocator 回归）
    let raw = metadata_heavy_wire(20_000, 200_007);
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::MetadataTooManyEntries(MAX_METADATA_ENTRIES))
    ));

    // 17 项立即拒绝，16 项合法
    let mut raw17 = legal_request_wire();
    raw17.extend(metadata_entry_fields(17));
    assert!(matches!(
        TunnelPacket::decode(&raw17),
        Err(EnvelopeError::MetadataTooManyEntries(16))
    ));

    let mut raw16 = legal_request_wire();
    raw16.extend(metadata_entry_fields(16));
    let p = TunnelPacket::decode(&raw16).expect("16 项必须合法");
    assert_eq!(p.metadata.len(), MAX_METADATA_ENTRIES);
    assert!(p.encode().is_ok());
}

#[test]
fn test_decode_rejects_oversized_metadata_key_value_and_entry() {
    // key 超 64B：按声明长度预算拒绝，不分配字符串
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(7, &map_entry(&b"k".repeat(65), b"v")));
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::MetadataKeyTooLarge(65, MAX_METADATA_KEY_LEN))
    ));

    // value 超 256B
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(7, &map_entry(b"k", &b"v".repeat(257))));
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::MetadataValueTooLarge(
            257,
            MAX_METADATA_VALUE_LEN
        ))
    ));

    // entry 声明长度超单个 entry 预算
    let mut entry = map_entry(&b"k".repeat(64), &b"v".repeat(256));
    entry.extend(bytes_field(1, &b"extra".repeat(20)));
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(7, &entry));
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::FieldTooLarge(_, _))
    ));

    // 撒谎的 entry 长度（超出剩余输入）→ Truncated，而非先分配
    let mut lying = legal_request_wire();
    lying.extend(tag(7, 2));
    lying.extend(len_prefix(300));
    lying.extend(bytes_field(1, b"k"));
    assert!(matches!(
        TunnelPacket::decode(&lying),
        Err(EnvelopeError::Truncated)
    ));

    // 恰好处于预算上限：合法，且可再编码
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(
        7,
        &map_entry(&b"k".repeat(64), &b"v".repeat(256)),
    ));
    let p = TunnelPacket::decode(&raw).expect("预算上限长度必须合法");
    assert_eq!(p.metadata.len(), 1);
    assert!(p.encode().is_ok());
}

#[test]
fn test_action_and_string_length_budgets_at_decode() {
    // action 超 64B：分配前按预算拒绝
    let mut wire = Vec::new();
    wire.extend(uint64_field(1, 1));
    wire.extend(uint64_field(2, 1));
    wire.extend(bytes_field(3, &b"a".repeat(MAX_ACTION_LEN + 1)));
    assert!(matches!(
        TunnelPacket::decode(&wire),
        Err(EnvelopeError::ActionTooLarge(65, MAX_ACTION_LEN))
    ));

    // 64B action 合法
    let mut ok_wire = Vec::new();
    ok_wire.extend(uint64_field(1, 1));
    ok_wire.extend(uint64_field(2, 1));
    ok_wire.extend(bytes_field(3, &b"a".repeat(MAX_ACTION_LEN)));
    assert!(
        TunnelPacket::decode(&ok_wire).is_ok(),
        "64B action 必须合法"
    );

    // error_message 无独立规范上限，但受整包 512KiB 预算约束
    let mut big_err = response_head_wire();
    big_err.extend(tag(5, 2));
    big_err.extend(len_prefix(MAX_ENVELOPE_SIZE + 1));
    assert!(matches!(
        TunnelPacket::decode(&big_err),
        Err(EnvelopeError::FieldTooLarge(_, MAX_ENVELOPE_SIZE))
    ));
}

#[test]
fn test_proto3_metadata_map_entry_and_duplicate_keys() {
    // 两个相同 key 的 entry，decode 必须拒绝（错误只带出现序号）
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(7, &map_entry(b"dup", b"v1")));
    raw.extend(bytes_field(7, &map_entry(b"dup", b"v2")));

    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::DuplicateMetadataKey(1))
    ));
}

#[test]
fn test_errors_never_echo_peer_content() {
    // 对端构造的合成 key 带控制字符与"秘密"字样：任何错误文案都不得回显
    let evil = "EVIL\nKEY\x01secret";
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(7, &map_entry(evil.as_bytes(), b"v1")));
    raw.extend(bytes_field(7, &map_entry(evil.as_bytes(), b"v2")));

    let err = decode_err(&raw);
    let msg = err.to_string();
    assert!(
        !msg.contains("EVIL") && !msg.contains("secret") && !msg.contains('\n'),
        "错误文案回显对端原文: {msg:?}"
    );
    assert!(
        matches!(err, EnvelopeError::DuplicateMetadataKey(_)),
        "实际 {err:?}"
    );
    assert!(
        !format!("{err:?}").contains("EVIL"),
        "Debug 形态也不得携带对端原文"
    );

    // 越界 value：错误只带数字
    let mut raw = legal_request_wire();
    raw.extend(bytes_field(
        7,
        &map_entry(b"SECRETKEY", &b"leak-me".repeat(64)),
    ));
    let err = decode_err(&raw);
    let msg = err.to_string();
    assert!(
        !msg.contains("SECRETKEY") && !msg.contains("leak-me"),
        "错误文案回显对端原文: {msg:?}"
    );

    // 非法 action：Validation 是 'static 文案，不得回显
    let mut wire = Vec::new();
    wire.extend(uint64_field(1, 1));
    wire.extend(uint64_field(2, 1));
    wire.extend(bytes_field(3, b"badaction\x01"));
    let err = decode_err(&wire);
    let msg = err.to_string();
    assert!(
        !msg.contains("badaction") && !msg.contains('\u{1}'),
        "错误文案回显对端 action: {msg:?}"
    );
    assert!(!format!("{err:?}").contains("badaction"));
}

// ---------------------------------------------------------------------------
// 4. 语义规则
// ---------------------------------------------------------------------------

#[test]
fn test_action_validation() {
    let mut packet = sample_request();

    // 下划线非法（spec：小写字母/数字/点分隔）
    packet.action = "device1.sub_mod.cmd".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = "device1.sub2.cmd3".to_string();
    assert!(packet.validate(StreamKind::Control).is_ok());

    packet.action = "Device.cmd".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = "dev-ice.cmd".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = String::new();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = ".foo.bar".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.action = "foo.bar.".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.action = "foo..bar".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = "a".repeat(65);
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.action = "a".repeat(64);
    assert!(packet.validate(StreamKind::Control).is_ok());
}

#[test]
fn test_tunnel_prefix_action_rules() {
    let mut packet = sample_request();

    packet.action = "tunnel.other".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.action = "tunnel.kick".to_string();
    packet.packet_type = PacketType::Request;
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.packet_type = PacketType::System;
    assert!(packet.validate(StreamKind::Control).is_ok());
    assert!(packet.validate(StreamKind::Data).is_err()); // Data 上拒绝 SYSTEM

    packet.action = "tunnel.revoke".to_string();
    packet.packet_type = PacketType::System;
    assert!(packet.validate(StreamKind::Control).is_ok());
    assert!(packet.validate(StreamKind::Data).is_err());

    packet.action = "other.action".to_string();
    packet.packet_type = PacketType::System;
    assert!(packet.validate(StreamKind::Control).is_err());
}

#[test]
fn test_tunnel_ping_rules() {
    let mut packet = TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![1u8; 16],
        metadata: BTreeMap::new(),
    };

    assert!(packet.validate(StreamKind::Control).is_ok());
    assert!(packet.validate(StreamKind::Data).is_ok());

    packet.payload = vec![1u8; 15];
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.payload = vec![1u8; 17];
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.payload = vec![1u8; 16];

    packet.metadata.insert("foo".to_string(), "bar".to_string());
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.metadata.clear();

    packet.packet_type = PacketType::System;
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.packet_type = PacketType::Event;
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.packet_type = PacketType::Response;
    assert!(packet.validate(StreamKind::Control).is_ok());

    packet.status_code = 1;
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.status_code = 0;

    packet.error_message = "err".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());
}

#[test]
fn test_ping_rules_do_not_bypass_generic_error_field_rules() {
    // 通用 status/error 规则必须先于 ping 专用规则；ping Request 也拒绝错误字段
    let mut ping = TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![7u8; 16],
        metadata: BTreeMap::new(),
    };
    ping.status_code = 5;
    let err = ping
        .validate(StreamKind::Control)
        .expect_err("ping 请求携带 status_code 必须拒绝");
    assert!(
        matches!(&err, EnvelopeError::Validation(m) if m.starts_with("non-response")),
        "ping 分支绕过了通用错误字段规则，实际 {err:?}"
    );
    assert!(
        ping.encode().is_err(),
        "ping 携带 status_code 时 encode 必须失败"
    );

    let mut ping2 = ping.clone();
    ping2.status_code = 0;
    ping2.error_message = "boom".to_string();
    let err = ping2
        .validate(StreamKind::Control)
        .expect_err("ping 请求携带 error_message 必须拒绝");
    assert!(
        matches!(&err, EnvelopeError::Validation(m) if m.starts_with("non-response")),
        "实际 {err:?}"
    );
    assert!(ping2.encode().is_err());
    assert!(
        TunnelPacket::decode(&{
            let mut raw = wire_head(1, 1, "tunnel.ping");
            raw.extend(bytes_field(6, &[7u8; 16]));
            raw.extend(raw_varint_field(4, &[0x05]));
            raw
        })
        .is_err()
    );

    // Event / System 携带错误字段同样走通用规则
    for pt in [PacketType::Event, PacketType::System] {
        let mut p = TunnelPacket {
            trace_id: 1,
            packet_type: pt,
            action: if pt == PacketType::System {
                "tunnel.kick".to_string()
            } else {
                "device.telemetry".to_string()
            },
            status_code: 3,
            error_message: String::new(),
            payload: vec![1],
            metadata: BTreeMap::new(),
        };
        let err = p
            .validate(StreamKind::Control)
            .expect_err("非 Response 携带 status 必须拒绝");
        assert!(
            matches!(&err, EnvelopeError::Validation(m) if m.starts_with("non-response")),
            "{pt:?} 实际 {err:?}"
        );
        p.status_code = 0;
        p.error_message = "boom".to_string();
        assert!(p.validate(StreamKind::Control).is_err(), "{pt:?}");
    }
}

#[test]
fn test_response_error_state_is_legal() {
    let mut p = sample_request();
    p.packet_type = PacketType::Response;
    p.status_code = -5;
    p.error_message = "something went wrong".to_string();
    p.validate(StreamKind::Control).unwrap();
    let enc = p.encode().unwrap();
    assert_eq!(TunnelPacket::decode(&enc).unwrap(), p);

    // Response status != 0 也可以不带 error_message
    p.error_message = String::new();
    p.validate(StreamKind::Control).unwrap();
    let enc = p.encode().unwrap();
    assert_eq!(TunnelPacket::decode(&enc).unwrap(), p);

    // Response status == 0 且带 error_message 非法
    p.status_code = 0;
    p.error_message = "boom".to_string();
    assert!(p.validate(StreamKind::Control).is_err());
    assert!(p.encode().is_err());
}

#[test]
fn test_trace_id_rules() {
    let mut packet = sample_request();
    packet.trace_id = 0;
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.trace_id = 1;
    assert!(packet.validate(StreamKind::Control).is_ok());
}

#[test]
fn test_response_and_non_response_status_error_rules() {
    let mut packet = sample_request();

    packet.packet_type = PacketType::Request;
    packet.status_code = 1;
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.status_code = 0;
    packet.error_message = "err".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.error_message = String::new();

    packet.packet_type = PacketType::Event;
    packet.status_code = 1;
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.status_code = 0;
    packet.error_message = "err".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());
    packet.error_message = String::new();

    packet.packet_type = PacketType::Response;
    packet.status_code = 0;
    packet.error_message = "err".to_string();
    assert!(packet.validate(StreamKind::Control).is_err());

    packet.status_code = -5;
    packet.error_message = "something went wrong".to_string();
    assert!(packet.validate(StreamKind::Control).is_ok());
}

#[test]
fn test_metadata_limits() {
    let mut packet = sample_request();
    packet.metadata.clear();

    for i in 0..MAX_METADATA_ENTRIES {
        packet.metadata.insert(format!("k{i}"), format!("v{i}"));
    }
    assert!(packet.validate(StreamKind::Control).is_ok());
    assert!(packet.encode().is_ok(), "16 项必须可编码");

    packet.metadata.insert("k16".to_string(), "v16".to_string());
    assert!(matches!(
        packet.validate(StreamKind::Control),
        Err(EnvelopeError::MetadataTooManyEntries(16))
    ));
    assert!(matches!(
        packet.encode(),
        Err(EnvelopeError::MetadataTooManyEntries(16))
    ));
    packet.metadata.remove("k16");

    packet.metadata.clear();
    packet.metadata.insert("k".repeat(65), "v".to_string());
    assert!(matches!(
        packet.validate(StreamKind::Control),
        Err(EnvelopeError::MetadataKeyTooLarge(65, 64))
    ));
    assert!(packet.encode().is_err());
    packet.metadata.remove(&"k".repeat(65));

    packet.metadata.insert("k".repeat(64), "v".to_string());
    assert!(packet.validate(StreamKind::Control).is_ok());
    packet.metadata.remove(&"k".repeat(64));

    packet.metadata.insert("key".to_string(), "v".repeat(257));
    assert!(matches!(
        packet.validate(StreamKind::Control),
        Err(EnvelopeError::MetadataValueTooLarge(257, 256))
    ));
    assert!(packet.encode().is_err());
    packet.metadata.remove("key");

    packet.metadata.insert("key".to_string(), "v".repeat(256));
    assert!(packet.validate(StreamKind::Control).is_ok());
    assert!(packet.encode().is_ok());
}

// ---------------------------------------------------------------------------
// 5. Wire 严格性（负例一律叠加在合法信元之上）
// ---------------------------------------------------------------------------

#[test]
fn test_rejects_non_canonical_varints_and_truncation() {
    // 截断的 varint：payload 长度只写了一半
    let mut truncated = legal_request_wire();
    truncated.extend(tag(6, 2));
    truncated.push(0x80);
    assert!(matches!(
        TunnelPacket::decode(&truncated),
        Err(EnvelopeError::Truncated)
    ));

    // 非最短 varint：status_code = 0 写成 0x80 0x00
    let mut non_canonical = legal_request_wire();
    non_canonical.extend(tag(4, 0));
    non_canonical.extend_from_slice(&[0x80, 0x00]);
    assert!(matches!(
        TunnelPacket::decode(&non_canonical),
        Err(EnvelopeError::NonCanonicalVarint)
    ));

    // varint 超过 10 字节
    let mut too_long = legal_request_wire();
    too_long.extend(tag(4, 0));
    too_long.extend_from_slice(&[0x80; 10]);
    too_long.push(0x01);
    assert!(matches!(
        TunnelPacket::decode(&too_long),
        Err(EnvelopeError::VarintTooLong)
    ));

    // varint 溢出 u64
    let mut overflow = legal_request_wire();
    overflow.extend(tag(4, 0));
    overflow.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
    assert!(matches!(
        TunnelPacket::decode(&overflow),
        Err(EnvelopeError::VarintOverflow)
    ));

    // 重复 singular 字段
    let mut dup_field = legal_request_wire();
    dup_field.extend(bytes_field(3, b"device.status"));
    assert!(matches!(
        TunnelPacket::decode(&dup_field),
        Err(EnvelopeError::DuplicateField(3))
    ));

    // map entry 内部重复 key 字段
    let mut entry = Vec::new();
    entry.extend(bytes_field(1, b"k1"));
    entry.extend(bytes_field(1, b"k2"));
    let mut dup_entry = legal_request_wire();
    dup_entry.extend(bytes_field(7, &entry));
    assert!(matches!(
        TunnelPacket::decode(&dup_entry),
        Err(EnvelopeError::DuplicateField(1))
    ));

    // 声明长度超过剩余输入
    let mut lying_len = legal_request_wire();
    lying_len.extend(tag(6, 2));
    lying_len.extend(len_prefix(1_000));
    assert!(matches!(
        TunnelPacket::decode(&lying_len),
        Err(EnvelopeError::Truncated)
    ));
}

#[test]
fn test_rejects_unknown_fields_and_bad_wire() {
    // 未定义字段号 8
    let mut raw = legal_request_wire();
    raw.extend(uint64_field(8, 123));
    assert!(matches!(
        TunnelPacket::decode(&raw),
        Err(EnvelopeError::UnknownField(8))
    ));

    // 非法 wire type
    let mut bad_wire = legal_request_wire();
    bad_wire.extend(tag(4, 3));
    assert!(matches!(
        TunnelPacket::decode(&bad_wire),
        Err(EnvelopeError::InvalidWireType(3))
    ));

    // 非法 UTF-8（error_message）
    let mut bad_utf8 = legal_response_wire();
    bad_utf8.extend(bytes_field(5, &[0xff, 0xfe]));
    // 字段 5 重复会先撞 DuplicateField，因此改为独立信元
    let mut bad_utf8_solo = Vec::new();
    bad_utf8_solo.extend(uint64_field(1, 1));
    bad_utf8_solo.extend(uint64_field(2, 1));
    bad_utf8_solo.extend(bytes_field(3, b"device.status"));
    bad_utf8_solo.extend(bytes_field(5, &[0xff, 0xfe]));
    assert!(matches!(
        TunnelPacket::decode(&bad_utf8_solo),
        Err(EnvelopeError::InvalidUtf8)
    ));

    // 字段 5 重复
    assert!(matches!(
        TunnelPacket::decode(&bad_utf8),
        Err(EnvelopeError::DuplicateField(5))
    ));

    // 非法 packet_type
    let mut bad_type = Vec::new();
    bad_type.extend(uint64_field(1, 1));
    bad_type.extend(bytes_field(3, b"device.status"));
    bad_type.extend(uint64_field(2, 99));
    assert!(matches!(
        TunnelPacket::decode(&bad_type),
        Err(EnvelopeError::InvalidPacketType(99))
    ));

    // 缺省 packet_type
    let mut no_type = Vec::new();
    no_type.extend(uint64_field(1, 1));
    no_type.extend(bytes_field(3, b"device.status"));
    assert!(matches!(
        TunnelPacket::decode(&no_type),
        Err(EnvelopeError::InvalidPacketType(0))
    ));

    // map entry 内部未定义字段
    let mut entry = Vec::new();
    entry.extend(bytes_field(1, b"k"));
    entry.extend(bytes_field(3, b"v"));
    let mut entry_unknown = legal_request_wire();
    entry_unknown.extend(bytes_field(7, &entry));
    assert!(matches!(
        TunnelPacket::decode(&entry_unknown),
        Err(EnvelopeError::UnknownField(3))
    ));
}

#[test]
fn test_int32_field_helper() {
    let raw = int32_field(4, -42);
    assert!(!raw.is_empty());
}
