//! Envelope + gRPC 解析器确定性压力测试（固定 seed 的 splitmix64 PRNG，仅测试用，
//! 非 coverage-guided fuzz，也非双进程/容量验证）。
//!
//! 校验合同（按真实实现，不擅加要求）：
//! - `decode` 成功必须通过 `validate_fields`，且 canonical encode→decode 语义等价；
//! - 随机变异/噪声得到 `Err` 是合法结果，不视为 bug；
//! - `GrpcDecoder` 任何 `Err` 后 poisoned 粘滞；`finish` 在非空缓冲上返回
//!   `TruncatedMessage` 并进入 poisoned（与实现一致）；
//! - `buffered_len` 逻辑 len ≤ `GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE`（不声称 Vec capacity）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use rsetup_tunnel::envelope::{
    MAX_ACTION_LEN, MAX_ENVELOPE_SIZE, MAX_METADATA_ENTRIES, PacketType, StreamKind, TunnelPacket,
};
use rsetup_tunnel::grpc::{GRPC_HEADER_SIZE, GrpcDecoder, GrpcError, InputCursor, encode_message};

const SEED: u64 = 0x5EED_2026_1023_0001;

/// 确定性 PRNG（splitmix64）：常驻固定 seed，仅测试用，不用生产 CSPRNG。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        assert!(lo <= hi);
        lo + self.below((hi - lo + 1) as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
    fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }
    fn fill_bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }
}

// 全局输入统计：TOTAL 为全部用例输入字节数；MAX 为套件级最大单输入（并行下为全局观测值）。
static TOTAL_INPUT_BYTES: AtomicU64 = AtomicU64::new(0);
static MAX_INPUT_LEN: AtomicUsize = AtomicUsize::new(0);

fn note_input(len: usize) {
    TOTAL_INPUT_BYTES.fetch_add(len as u64, Ordering::Relaxed);
    MAX_INPUT_LEN.fetch_max(len, Ordering::Relaxed);
}

fn report(name: &str, cases: usize, bytes_before: u64, t0: Instant) {
    let bytes = TOTAL_INPUT_BYTES.load(Ordering::Relaxed) - bytes_before;
    println!(
        "[parser_stress] case={name} cases={cases} input_bytes={bytes} \
         max_single_input_global={} elapsed_ms={}",
        MAX_INPUT_LEN.load(Ordering::Relaxed),
        t0.elapsed().as_millis()
    );
}

// ---------- 合法/非法信元构造 ----------

fn gen_action(rng: &mut Rng) -> String {
    loop {
        let segs: Vec<String> = (0..rng.range(1, 3))
            .map(|_| {
                (0..rng.range(1, 8))
                    .map(|_| {
                        b"abcdefghijklmnopqrstuvwxyz0123456789"[rng.below(36) as usize] as char
                    })
                    .collect()
            })
            .collect();
        let a = segs.join(".");
        if !a.is_empty() && !a.starts_with("tunnel.") {
            return a;
        }
    }
}

fn gen_metadata(rng: &mut Rng) -> BTreeMap<String, String> {
    (0..rng.range(0, 3))
        .map(|i| {
            let k: String = (0..rng.range(1, 16))
                .map(|_| b"abcdefghijklmnopqrstuvwxyz"[rng.below(26) as usize] as char)
                .collect();
            (k, format!("v{i}{}", rng.range(0, 999)))
        })
        .collect()
}

/// 构造语义合法信元；payload 上限由调用方给（信封用例需 encode ≤ 4096B）。
fn gen_valid_packet(rng: &mut Rng, payload_max: usize) -> TunnelPacket {
    let status_pool = [200i32, 404, 500, -1, -418, i32::MAX, i32::MIN];
    let pl_len = rng.range(0, payload_max.min(300));
    let (packet_type, action, status_code, payload, metadata) = match rng.below(8) {
        0 | 1 => (
            if rng.chance(50) {
                PacketType::Request
            } else {
                PacketType::Event
            },
            gen_action(rng),
            0,
            rng.fill_bytes(pl_len),
            gen_metadata(rng),
        ),
        2 | 3 => (
            PacketType::Response,
            gen_action(rng),
            if rng.chance(70) {
                status_pool[rng.below(7) as usize]
            } else {
                0
            },
            rng.fill_bytes(pl_len),
            gen_metadata(rng),
        ),
        4 => (
            PacketType::Request,
            "tunnel.ping".into(),
            0,
            vec![7u8; 16],
            BTreeMap::new(),
        ),
        5 => (
            PacketType::Response,
            "tunnel.ping".into(),
            0,
            vec![9u8; 16],
            BTreeMap::new(),
        ),
        6 => (
            PacketType::System,
            "tunnel.kick".into(),
            0,
            Vec::new(),
            BTreeMap::new(),
        ),
        _ => (
            PacketType::System,
            "tunnel.revoke".into(),
            0,
            Vec::new(),
            BTreeMap::new(),
        ),
    };
    let mut p = TunnelPacket {
        trace_id: rng.next_u64().max(1),
        packet_type,
        action,
        status_code,
        error_message: String::new(),
        payload,
        metadata,
    };
    // Response 错误分支：status 非零时 error_message 可有可无；status 0 必须为空。
    if p.packet_type == PacketType::Response && p.status_code != 0 && rng.chance(60) {
        p.error_message = format!("err {}", rng.below(1000));
    }
    assert_eq!(
        p.validate_fields(),
        Ok(()),
        "构造的合法信元必须通过校验: {p:?}"
    );
    p
}

/// 构造语义非法信元：encode 必须拒绝。
fn gen_invalid_packet(rng: &mut Rng) -> TunnelPacket {
    let mut p = gen_valid_packet(rng, 32);
    match rng.below(10) {
        0 => p.trace_id = 0,
        1 => p.action = String::new(),
        2 => p.action = "Bad.Action".into(),
        3 => p.action = "a..b".into(),
        4 => p.action = "x".repeat(65),
        5 => {
            if p.packet_type == PacketType::Response {
                p.status_code = 0;
                p.error_message = "boom".into();
            } else {
                p.status_code = 500;
            }
        }
        6 => {
            p.status_code = 0;
            p.error_message = "boom".into();
        }
        7 => p.action = "tunnel.unknown".into(),
        8 => {
            p.action = "tunnel.kick".into();
            p.packet_type = PacketType::Request;
            p.status_code = 0;
        }
        _ => {
            p.packet_type = PacketType::System;
            p.action = "sys.other".into();
        }
    }
    p
}

/// decode 成功后的强检查：语义校验 + 规范化 encode→decode 幂等 + 预算边界。
fn check_decoded(p: &TunnelPacket) {
    p.validate_fields()
        .expect("decode 成功必须通过 validate_fields");
    assert!(p.action.len() <= MAX_ACTION_LEN && p.metadata.len() <= MAX_METADATA_ENTRIES);
    assert!(p.payload.len() <= MAX_ENVELOPE_SIZE);
    let re = p.encode().expect("decode 成功的信元必须可 encode");
    assert_eq!(
        &TunnelPacket::decode(&re).unwrap(),
        p,
        "encode→decode 必须语义等价"
    );
}

// ---------- A1: 信封结构化变异（7000 例，单输入 ≤4096+16B） ----------

#[test]
fn envelope_structured_mutation() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    let mut rng = Rng::new(SEED);
    const CASES: usize = 7000;
    let mut ok_decodes = 0usize;
    for case in 0..CASES {
        if rng.chance(10) {
            let bad = gen_invalid_packet(&mut rng);
            assert!(
                bad.encode().is_err(),
                "非法信元 encode 必须 Err (case {case}): {bad:?}"
            );
            continue;
        }
        let base = gen_valid_packet(&mut rng, 300);
        let mut bytes = base.encode().expect("合法信元 encode 必须 Ok");
        let n = bytes.len();
        match rng.below(4) {
            0 => {
                let i = rng.range(0, n - 1);
                bytes[i] ^= (1 + rng.below(255)) as u8;
            }
            1 => bytes.truncate(rng.range(0, n)),
            2 => {
                let elen = rng.range(1, 16);
                let extra = rng.fill_bytes(elen);
                bytes.extend_from_slice(&extra);
            }
            _ => {
                if n >= 2 {
                    let (i, j) = (rng.range(0, n - 1), rng.range(0, n - 1));
                    bytes.swap(i, j);
                }
            }
        }
        assert!(bytes.len() <= 4096 + 16);
        note_input(bytes.len());
        if let Ok(p) = TunnelPacket::decode(&bytes) {
            ok_decodes += 1;
            check_decoded(&p);
        } // 变异得到 Err 属正常
    }
    report("envelope_structured_mutation", CASES, before, t0);
    println!("[parser_stress]   decode_ok={ok_decodes} / {CASES}");
}

// ---------- A2: 信封随机噪声（3000 例，单输入 ≤4096B，含空输入） ----------

#[test]
fn envelope_random_noise() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    let mut rng = Rng::new(SEED ^ 0xA2);
    const CASES: usize = 3000;
    let mut ok_decodes = 0usize;
    for _ in 0..CASES {
        let len = if rng.chance(5) {
            0
        } else if rng.chance(70) {
            rng.range(1, 48)
        } else {
            rng.range(1, 4096)
        };
        let mut input: Vec<u8> = Vec::new();
        if rng.chance(40) {
            // 结构相似噪声：合法 wire 的随机前缀 + 随机字节，总长恒为 len。
            let base = gen_valid_packet(&mut rng, 32).encode().unwrap();
            let take = rng.range(0, base.len().min(len));
            input.extend_from_slice(&base[..take]);
            input.extend(rng.fill_bytes(len - take));
        } else {
            input.extend(rng.fill_bytes(len));
        }
        assert!(input.len() <= 4096);
        note_input(input.len());
        if let Ok(p) = TunnelPacket::decode(&input) {
            ok_decodes += 1;
            check_decoded(&p);
        }
    }
    report("envelope_random_noise", CASES, before, t0);
    println!("[parser_stress]   decode_ok={ok_decodes} / {CASES}");
}

// ---------- B1: gRPC 合法信元随机分片/粘包 roundtrip（10000 例） ----------

#[test]
fn grpc_framing_roundtrip() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    let mut rng = Rng::new(SEED ^ 0xB1);
    const CASES: usize = 10000;
    for case in 0..CASES {
        let expected: Vec<TunnelPacket> = (0..rng.range(1, 3))
            .map(|_| gen_valid_packet(&mut rng, 512))
            .collect();
        let mut whole: Vec<u8> = Vec::new();
        for p in &expected {
            whole.extend(encode_message(p).expect("合法信元 encode_message 必须 Ok"));
        }
        let mut dec = GrpcDecoder::new();
        let (mut pos, mut idx) = (0usize, 0usize);
        let mut guard = whole.len() * 2 + 64; // 防 hang：每轮必须消费或结束
        while pos < whole.len() {
            let n = rng.range(1, 64).min(whole.len() - pos);
            let chunk = &whole[pos..pos + n];
            pos += n;
            note_input(n);
            let mut cur = InputCursor::new(chunk);
            loop {
                guard -= 1;
                assert!(guard > 0, "case {case}: 解码循环无进度，可能 hang");
                let before_consumed = cur.consumed();
                match dec.next_packet(&mut cur) {
                    Ok(Some(p)) => {
                        assert!(cur.consumed() > before_consumed, "产出包必须消费输入");
                        assert_eq!(p, expected[idx], "case {case}: 第 {idx} 个包必须严格等价");
                        idx += 1;
                    }
                    Ok(None) => {
                        assert_eq!(
                            cur.remaining(),
                            0,
                            "case {case}: 产 None 但输入未耗尽（停滞）"
                        );
                        break;
                    }
                    Err(e) => panic!("case {case}: 合法流不得出错: {e}"),
                }
            }
            assert!(dec.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE);
        }
        assert_eq!(idx, expected.len(), "case {case}: 包数不完整");
        dec.finish().expect("完整流的 finish 必须 Ok");
        assert!(!dec.is_poisoned() && dec.buffered_len() == 0);
    }
    report("grpc_framing_roundtrip", CASES, before, t0);
}

// ---------- B2: gRPC 噪声流 + 毒化粘滞合同（250 例） ----------

#[test]
fn grpc_noise_streams_and_poison() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    let mut rng = Rng::new(SEED ^ 0xB2);
    const CASES: usize = 250;
    for case in 0..CASES {
        let mut stream: Vec<u8> = Vec::new();
        if rng.chance(30) {
            stream.extend(encode_message(&gen_valid_packet(&mut rng, 64)).unwrap());
        }
        let len = if rng.chance(10) {
            0
        } else {
            rng.range(1, 2048)
        };
        stream.extend(rng.fill_bytes(len));
        note_input(stream.len());

        let mut dec = GrpcDecoder::new();
        let mut cur = InputCursor::new(&stream);
        let mut err_seen: Option<GrpcError> = None;
        let mut guard = stream.len() + 64;
        loop {
            guard -= 1;
            assert!(guard > 0, "case {case}: 噪声流解码循环无进度");
            let before_consumed = cur.consumed();
            match dec.next_packet(&mut cur) {
                Ok(Some(p)) => {
                    assert!(
                        cur.consumed() > before_consumed,
                        "case {case}: 产出包必须消费输入"
                    );
                    check_decoded(&p);
                }
                Ok(None) => {
                    assert_eq!(cur.remaining(), 0, "case {case}: 产 None 但输入未耗尽");
                    break;
                }
                Err(e) => {
                    err_seen = Some(e);
                    break;
                }
            }
        }
        assert!(
            dec.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE,
            "case {case}"
        );
        assert_eq!(
            dec.is_poisoned(),
            err_seen.is_some(),
            "case {case}: poisoned 必须与 Err 一致"
        );
        // 合同：poisoned → finish=DecoderPoisoned；半截缓冲 → TruncatedMessage+poison；否则 Ok。
        let buffered = dec.buffered_len(); // finish 会清空缓冲，必须先捕获
        let fin = dec.finish();
        if err_seen.is_some() {
            assert!(
                matches!(fin, Err(GrpcError::DecoderPoisoned)),
                "case {case}: 粘滞 finish"
            );
        } else if buffered > 0 {
            assert!(
                matches!(fin, Err(GrpcError::TruncatedMessage)),
                "case {case}: 截断 finish"
            );
            assert!(
                dec.is_poisoned(),
                "case {case}: TruncatedMessage 后必须 poisoned"
            );
        } else {
            assert!(fin.is_ok(), "case {case}: 干净流 finish 必须 Ok");
        }
        if dec.is_poisoned() {
            // 粘滞验证：再喂一条完全合法的消息仍必须 Err(DecoderPoisoned) 且不消费输入。
            let valid = encode_message(&gen_valid_packet(&mut rng, 32)).unwrap();
            let mut cur2 = InputCursor::new(&valid);
            let again = dec.next_packet(&mut cur2);
            assert!(
                matches!(again, Err(GrpcError::DecoderPoisoned)),
                "case {case}: 粘滞必须持续"
            );
            assert_eq!(cur2.consumed(), 0, "case {case}: 毒化后不得消费输入");
        }
    }
    report("grpc_noise_streams_and_poison", CASES, before, t0);
}

// ---------- 显式边界：gRPC 长度前缀 / 压缩 / EOF ----------

fn prefix(flag: u8, msg_len: u32) -> Vec<u8> {
    let mut v = vec![flag];
    v.extend_from_slice(&msg_len.to_be_bytes());
    v
}

#[test]
fn grpc_length_prefix_boundaries() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    // 512KiB+1 → EnvelopeTooLarge 且粘滞
    let pfx = prefix(0, (MAX_ENVELOPE_SIZE + 1) as u32);
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&pfx);
    assert!(
        matches!(d.next_packet(&mut c), Err(GrpcError::EnvelopeTooLarge(n)) if n == MAX_ENVELOPE_SIZE + 1)
    );
    assert!(d.is_poisoned());
    // 1MiB+1 → MessageTooLarge；1MiB 整 → EnvelopeTooLarge（两道上限顺序）
    let pfx = prefix(0, 1024 * 1024 + 1);
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&pfx);
    assert!(matches!(
        d.next_packet(&mut c),
        Err(GrpcError::MessageTooLarge(_))
    ));
    let pfx = prefix(0, 1024 * 1024);
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&pfx);
    assert!(matches!(
        d.next_packet(&mut c),
        Err(GrpcError::EnvelopeTooLarge(_))
    ));
    // flag=1 压缩 → CompressionNotSupported；粘滞后再喂仍 DecoderPoisoned
    let pfx = prefix(1, 16);
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&pfx);
    assert!(matches!(
        d.next_packet(&mut c),
        Err(GrpcError::CompressionNotSupported)
    ));
    assert!(d.is_poisoned());
    let pfx = prefix(0, 0);
    let mut c = InputCursor::new(&pfx);
    assert!(matches!(
        d.next_packet(&mut c),
        Err(GrpcError::DecoderPoisoned)
    ));
    note_input(25);
    // 512KiB 整：合法上限，先喂 5 字节头再喂 1024B，缓冲精确且无错误
    let pfx = prefix(0, MAX_ENVELOPE_SIZE as u32);
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&pfx);
    assert!(matches!(d.next_packet(&mut c), Ok(None)));
    let filler = vec![0u8; 1024];
    let mut c = InputCursor::new(&filler);
    assert!(matches!(d.next_packet(&mut c), Ok(None)));
    assert_eq!(d.buffered_len(), 5 + 1024);
    assert!(d.buffered_len() <= GRPC_HEADER_SIZE + MAX_ENVELOPE_SIZE && !d.is_poisoned());
    note_input(5 + 1024);
    // EOF 截断：完整消息少最后 3 字节 → Ok(None)，finish=TruncatedMessage 并毒化、清缓冲
    let msg = encode_message(&gen_valid_packet(&mut Rng::new(SEED ^ 0xE0F), 32)).unwrap();
    let mut d = GrpcDecoder::new();
    let mut c = InputCursor::new(&msg[..msg.len() - 3]);
    assert!(matches!(d.next_packet(&mut c), Ok(None)));
    assert!(matches!(d.finish(), Err(GrpcError::TruncatedMessage)));
    assert!(d.is_poisoned() && d.buffered_len() == 0);
    note_input(msg.len());
    report("grpc_length_prefix_boundaries", 8, before, t0);
}

// ---------- 显式边界：action 64/65、metadata 16/17、ping 语义、System/Data 路由 ----------

fn wire_varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

/// 手工拼一个 field-7 map entry 的 wire 字节（用于 wire 级第 17 项拒绝测试）。
fn wire_map_entry(k: &str, v: &str) -> Vec<u8> {
    let mut e = Vec::new();
    for (f, s) in [(1u32, k), (2, v)] {
        e.push(((f << 3) | 2) as u8);
        e.extend_from_slice(&wire_varint(s.len() as u64));
        e.extend_from_slice(s.as_bytes());
    }
    let mut out = vec![7u8 << 3 | 2];
    out.extend_from_slice(&wire_varint(e.len() as u64));
    out.extend_from_slice(&e);
    out
}

#[test]
fn envelope_limit_and_routing_boundaries() {
    let t0 = Instant::now();
    let before = TOTAL_INPUT_BYTES.load(Ordering::Relaxed);
    let mut req = TunnelPacket {
        trace_id: 42,
        packet_type: PacketType::Request,
        action: String::new(),
        status_code: 0,
        error_message: String::new(),
        payload: Vec::new(),
        metadata: BTreeMap::new(),
    };
    // action 64 合法、65 拒绝
    req.action = format!("a.{}", "b".repeat(62));
    assert_eq!(req.action.len(), 64);
    assert_eq!(TunnelPacket::decode(&req.encode().unwrap()).unwrap(), req);
    req.action.push('c');
    assert!(req.encode().is_err(), "65B action 必须拒绝");
    // metadata 16 项合法（encode 层 17 项拒绝、wire 级第 17 项 decode 拒绝）
    req.action = "svc.op".into();
    req.metadata = (0..MAX_METADATA_ENTRIES)
        .map(|i| (format!("k{i}"), format!("v{i}")))
        .collect();
    assert_eq!(TunnelPacket::decode(&req.encode().unwrap()).unwrap(), req);
    req.metadata.insert("zzz17".into(), "v".into());
    assert!(req.encode().is_err(), "17 项 metadata 必须拒绝");
    req.metadata.remove("zzz17");
    req.metadata = (0..MAX_METADATA_ENTRIES)
        .map(|i| (format!("mk{i}"), "v".repeat(4)))
        .collect();
    let mut wire = req.encode().unwrap();
    wire.extend(wire_map_entry("mk17", "mv17"));
    assert!(
        TunnelPacket::decode(&wire).is_err(),
        "wire 级第 17 项 metadata 必须拒绝"
    );
    note_input(65 + 1024 + wire.len() + 128);

    // tunnel.ping 语义：Request/Response + payload==16 + metadata 空 + Response status 0
    let mk = |pt: PacketType, payload: Vec<u8>, meta: BTreeMap<String, String>, status: i32| {
        TunnelPacket {
            trace_id: 42,
            packet_type: pt,
            action: "tunnel.ping".into(),
            status_code: status,
            error_message: String::new(),
            payload,
            metadata: meta,
        }
    };
    let empty = BTreeMap::new();
    let mut meta1 = BTreeMap::new();
    meta1.insert("k".to_string(), "v".to_string());
    assert_eq!(
        mk(PacketType::Request, vec![1u8; 16], empty.clone(), 0).validate_fields(),
        Ok(())
    );
    assert!(
        mk(PacketType::Request, vec![1u8; 15], empty.clone(), 0)
            .validate_fields()
            .is_err()
    );
    assert!(
        mk(PacketType::Request, vec![1u8; 17], empty.clone(), 0)
            .validate_fields()
            .is_err()
    );
    assert!(
        mk(PacketType::Request, vec![1u8; 16], meta1, 0)
            .validate_fields()
            .is_err()
    );
    assert!(
        mk(PacketType::Event, vec![1u8; 16], empty.clone(), 0)
            .validate_fields()
            .is_err()
    );
    assert!(
        mk(PacketType::Response, vec![1u8; 16], empty.clone(), 1)
            .validate_fields()
            .is_err()
    );

    // System 仅控制流；数据流必须拒绝（独立明确测试点）
    let kick = TunnelPacket {
        packet_type: PacketType::System,
        action: "tunnel.kick".into(),
        ..mk(PacketType::System, Vec::new(), empty.clone(), 0)
    };
    assert_eq!(kick.validate(StreamKind::Control), Ok(()));
    assert!(
        kick.validate(StreamKind::Data).is_err(),
        "System 在 Data 流必须拒绝"
    );
    let echo = TunnelPacket {
        packet_type: PacketType::Request,
        action: "svc.echo".into(),
        ..kick.clone()
    };
    assert_eq!(echo.validate(StreamKind::Data), Ok(()));
    report("envelope_limit_and_routing_boundaries", 13, before, t0);
}
