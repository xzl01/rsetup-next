//! 传输加密核心（第 1 层）行为测试：protocol_spec.md §4.4、§5。
//!
//! 覆盖：X25519 协商与公钥拒绝、HKDF-SHA256 派生一致性与方向区分、
//! 双向记录 roundtrip（空/1B/最大/多条交错）、篡改（tag/密文/长度/AAD-seq）、
//! 重放、乱序、截断/多余字节、长度 15/65537、序号末值耗尽、fatal 粘滞，
//! 以及记录线格式与按 §5.2 公式独立实现的"规范参考实现"逐字节互通。
//!
//! 序号耗尽与参考实现使用 `#[doc(hidden)]` 测试专用入口
//! `record_layer_at_seq` / `key_material_for_test`，不经生产 API。

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use rsetup_protocol::crypto::{
    CryptoError, EphemeralKeyPair, KeyMaterial, MAX_PLAINTEXT_LEN, RecordLayer, RecordLayerPair,
    SessionKeys, record_layer_at_seq,
};

const CLIENT_RANDOM: [u8; 32] = [0x11; 32];
const SERVER_RANDOM: [u8; 32] = [0x22; 32];

// ---------- 公共辅助 ----------

struct Established {
    client: RecordLayerPair,
    server: RecordLayerPair,
    material: KeyMaterial,
}

/// 完整走一遍 §4.4/§5.1：X25519 → 共享密钥 → HKDF 派生 → 双向记录层。
fn established() -> Established {
    let client_eph = EphemeralKeyPair::generate().expect("client eph keypair");
    let server_eph = EphemeralKeyPair::generate().expect("server eph keypair");
    let pub_c = client_eph.public_key();
    let pub_s = server_eph.public_key();
    let shared_c = client_eph.agree(&pub_s).expect("client agree");
    let shared_s = server_eph.agree(&pub_c).expect("server agree");
    assert_eq!(shared_c, shared_s, "两端共享密钥必须一致");
    let keys = SessionKeys::derive(&shared_c, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    Established {
        client: keys.client_record_layer(),
        server: keys.server_record_layer(),
        material: keys.key_material_for_test(),
    }
}

/// 以 c2s key/iv 构造序号从 0 起的独立接收层（互不干扰的负面用例）。
fn fresh_c2s_rx(material: &KeyMaterial) -> RecordLayer {
    record_layer_at_seq(&material.c2s_key, &material.c2s_iv, 0)
}

fn with_body_len(record: &[u8], new_body_len: u32) -> Vec<u8> {
    let mut out = record.to_vec();
    out[..4].copy_from_slice(&new_body_len.to_be_bytes());
    out
}

// ---------- §5.2 规范参考实现（独立于被测模块、直接按文档公式拼装） ----------

/// Nonce[0:4] = IV[0:4]；Nonce[4:12] = IV[4:12] XOR BE64(seq)。
fn ref_nonce(iv: &[u8; 12], seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..4].copy_from_slice(&iv[..4]);
    let mut tail = [0u8; 8];
    tail.copy_from_slice(&iv[4..]);
    let xored = u64::from_be_bytes(tail) ^ seq;
    n[4..].copy_from_slice(&xored.to_be_bytes());
    n
}

/// AAD = BE32(Record Length) || BE64(SeqID)。
fn ref_aad(body_len: u32, seq: u64) -> [u8; 12] {
    let mut a = [0u8; 12];
    a[..4].copy_from_slice(&body_len.to_be_bytes());
    a[4..].copy_from_slice(&seq.to_be_bytes());
    a
}

/// 按 §5.2 拼装：4B BE(密文+Tag 长度) || 密文 || Tag。
fn ref_seal(key: &[u8; 32], iv: &[u8; 12], seq: u64, plaintext: &[u8]) -> Vec<u8> {
    let body_len = (plaintext.len() + 16) as u32;
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("ref key"));
    let mut out = plaintext.to_vec();
    let tag = key
        .seal_in_place_separate_tag(
            Nonce::assume_unique_for_key(ref_nonce(iv, seq)),
            Aad::from(ref_aad(body_len, seq)),
            &mut out,
        )
        .expect("ref seal");
    let mut record = body_len.to_be_bytes().to_vec();
    record.extend_from_slice(&out);
    record.extend_from_slice(tag.as_ref());
    record
}

fn ref_open(key: &[u8; 32], iv: &[u8; 12], seq: u64, record: &[u8]) -> Option<Vec<u8>> {
    let mut len_be = [0u8; 4];
    len_be.copy_from_slice(&record[..4]);
    let body_len = u32::from_be_bytes(len_be);
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("ref key"));
    let mut buf = record[4..].to_vec();
    key.open_in_place(
        Nonce::assume_unique_for_key(ref_nonce(iv, seq)),
        Aad::from(ref_aad(body_len, seq)),
        &mut buf,
    )
    .ok()
    .map(|pt| pt.to_vec())
}

// ---------- §4.4 X25519 ----------

#[test]
fn x25519_shared_secret_matches_and_nonzero() {
    // 两端 X25519 共享密钥一致且非全零。
    let a = EphemeralKeyPair::generate().expect("keypair a");
    let b = EphemeralKeyPair::generate().expect("keypair b");
    let pub_a = a.public_key();
    let shared_ab = a.agree(&b.public_key()).expect("agree a->b");
    let shared_ba = b.agree(&pub_a).expect("agree b->a");
    assert_eq!(shared_ab, shared_ba);
    assert_ne!(shared_ab, [0u8; 32], "共享密钥不得全零（§4.4）");
}

#[test]
fn x25519_rejects_zero_and_low_order_public_keys() {
    // 全零公钥。
    let zero = [0u8; 32];
    let err = EphemeralKeyPair::generate()
        .expect("kp")
        .agree(&zero)
        .expect_err("zero pubkey must be rejected");
    assert_eq!(err, CryptoError::KeyAgreement);

    // 低阶（小序）点与非法编码（RFC 7748/§4.4 拒绝面，实测 ring 对以下输入
    // 全部返回错误，即 X25519 输出全零或编码非规范）：
    //   u=1：2 阶像点（2·P = (0,0)）；
    //   e0eb7a…49b800：8 阶点；
    //   ecff…7f / edff…7f：u ≡ 0 (mod p) 的 p、p+1 非规范表示；
    //   01…80：最高位（符号位）置位的非法编码。
    let bad_pubs = [
        "0100000000000000000000000000000000000000000000000000000000000000",
        "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
        "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        "0100000000000000000000000000000000000000000000000000000000000080",
    ];
    for hex_pub in bad_pubs {
        let low = hex::decode(hex_pub).expect("hex fixture");
        let err = EphemeralKeyPair::generate()
            .expect("kp")
            .agree_from_slice(&low)
            .expect_err("low-order / invalid-encoding pubkey must be rejected");
        assert_eq!(err, CryptoError::KeyAgreement, "pub={hex_pub}");
    }
    // 注：u=32 等"小整数"输入落在扭曲曲线上（X25519 输出非全零），
    // RFC 7748 语义下不属于必须拒绝的低阶点，不在本拒绝面内。

    // 错误长度：经切片入口逐一拒绝。
    for bad_len in [0usize, 1, 31, 33, 64] {
        let peer = vec![0xA5u8; bad_len];
        let err = EphemeralKeyPair::generate()
            .expect("kp")
            .agree_from_slice(&peer)
            .expect_err("wrong length must be rejected");
        assert_eq!(err, CryptoError::KeyAgreement, "len={bad_len}");
    }
}

// ---------- §5.1 HKDF ----------

#[test]
fn hkdf_derivation_deterministic_and_direction_distinct() {
    let shared = [0x5Au8; 32];
    let k1 = SessionKeys::derive(&shared, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive 1");
    let k2 = SessionKeys::derive(&shared, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive 2");
    let m1 = k1.key_material_for_test();
    let m2 = k2.key_material_for_test();
    assert_eq!(m1, m2, "同输入重复派生必须逐字节一致");

    // 方向区分：c2s 与 s2c 的 key/iv 互不相同。
    assert_ne!(m1.c2s_key, m1.s2c_key);
    assert_ne!(m1.c2s_iv, m1.s2c_iv);

    // Salt 绑定 random 顺序：交换 client/server random 必须得到不同材料。
    let swapped = SessionKeys::derive(&shared, &SERVER_RANDOM, &CLIENT_RANDOM).expect("derive s");
    assert_ne!(k1.key_material_for_test(), swapped.key_material_for_test());

    // 不同共享密钥 → 不同材料。
    let other =
        SessionKeys::derive(&[0x5Bu8; 32], &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive o");
    assert_ne!(k1.key_material_for_test(), other.key_material_for_test());
}

#[test]
fn layers_from_independent_derivations_interoperate() {
    // 同输入两次独立派生：一侧 client 层封箱，另一侧 server 层解封，
    // 验证 HKDF 双方派生出的 c2s/s2c key/iv 一致且方向绑定正确。
    let shared = [0x77u8; 32];
    let k1 = SessionKeys::derive(&shared, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive k1");
    let k2 = SessionKeys::derive(&shared, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive k2");
    let mut c1 = k1.client_record_layer();
    let mut s2 = k2.server_record_layer();

    let r = c1.tx.seal(b"cross-derive c2s").expect("seal");
    assert_eq!(s2.rx.open(&r).expect("cross open c2s"), b"cross-derive c2s");

    let r = s2.tx.seal(b"cross-derive s2c").expect("seal");
    assert_eq!(c1.rx.open(&r).expect("cross open s2c"), b"cross-derive s2c");

    // 方向不可混用：拿 c2s 记录去 client.rx（s2c 方向）解封必须认证失败。
    let r_c2s = c1.tx.seal(b"direction probe").expect("seal");
    assert_eq!(c1.rx.open(&r_c2s), Err(CryptoError::AuthFailed));
}

// ---------- §5.2 记录层 roundtrip ----------

#[test]
fn record_roundtrip_bidirectional_mixed_sizes() {
    let mut es = established();

    let sizes: [usize; 5] = [0, 1, 31, 4096, MAX_PLAINTEXT_LEN];
    // 多轮双向交错：每个方向内部严格保持发送顺序（§5.2 接收端按序）。
    for (round, &size) in sizes.iter().enumerate() {
        let out_c = (0..size)
            .map(|i| ((i * 7 + round * 13 + 1) % 251) as u8)
            .collect::<Vec<u8>>();
        let out_s = (0..size)
            .map(|i| ((i * 11 + round * 17 + 2) % 253) as u8)
            .collect::<Vec<u8>>();

        let rec_c = es.client.tx.seal(&out_c).expect("c seal");
        let rec_s = es.server.tx.seal(&out_s).expect("s seal");

        // 记录长度校验：4B 前缀 + 明文 + 16B Tag。
        assert_eq!(rec_c.len(), 4 + out_c.len() + 16);
        assert_eq!(rec_s.len(), 4 + out_s.len() + 16);

        assert_eq!(es.server.rx.open(&rec_c).expect("s open"), out_c);
        assert_eq!(es.client.rx.open(&rec_s).expect("c open"), out_s);
    }
    assert!(!es.client.tx.is_fatal() && !es.server.rx.is_fatal());
}

#[test]
fn empty_plaintext_record_is_16_byte_body() {
    // 零明文允许：记录体恰 16B（仅 Tag），线长 20B。
    let mut es = established();
    let rec = es.client.tx.seal(&[]).expect("seal empty");
    assert_eq!(rec.len(), 20);
    assert_eq!(&rec[..4], &16u32.to_be_bytes());
    assert_eq!(
        es.server.rx.open(&rec).expect("open empty"),
        Vec::<u8>::new()
    );
}

#[test]
fn record_length_prefix_is_big_endian_body_only() {
    // 长度前缀为大端且只计密文+Tag，不含 4B 前缀本身。
    let mut es = established();
    let plaintext = vec![0x42u8; 1000];
    let rec = es.client.tx.seal(&plaintext).expect("seal");
    assert_eq!(&rec[..4], &1016u32.to_be_bytes(), "BE32(1000+16)");
    assert_eq!(rec.len(), 1020);

    let big = vec![0x77u8; MAX_PLAINTEXT_LEN];
    let rec = es.client.tx.seal(&big).expect("seal max");
    assert_eq!(&rec[..4], &65_536u32.to_be_bytes(), "BE32(65536) 大端");
    assert_eq!(rec.len(), 65_540);
}

#[test]
fn wire_format_matches_spec_reference_implementation() {
    // 与按 §5.2 公式独立实现的参考封装逐字节互通：锁定 Nonce、AAD、
    // 大端长度前缀的确切线格式。
    let es = established();
    let m = &es.material;
    for seq in [0u64, 1, 2, 7, 1000] {
        let msg = format!("spec-fidelity-{seq}");
        let mine = {
            let mut layer = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, seq);
            layer.seal(msg.as_bytes()).expect("seal")
        };
        let theirs = ref_seal(&m.c2s_key, &m.c2s_iv, seq, msg.as_bytes());
        assert_eq!(mine, theirs, "seq={seq} 线格式必须与规范参考实现一致");

        // 反向：参考实现封箱 → 被测层解封（并验证 ref_open 本身可用）。
        let mut layer = record_layer_at_seq(&m.s2c_key, &m.s2c_iv, seq);
        let rec = ref_seal(&m.s2c_key, &m.s2c_iv, seq, msg.as_bytes());
        assert_eq!(layer.open(&rec).expect("open ref"), msg.as_bytes());
        assert_eq!(
            ref_open(&m.s2c_key, &m.s2c_iv, seq, rec.as_slice()).as_deref(),
            Some(msg.as_bytes())
        );
    }
}

// ---------- 篡改 / 重放 / 乱序 / 截断 ----------

#[test]
fn tampered_tag_rejected_and_layer_fatal() {
    let mut es = established();
    let mut rec = es.client.tx.seal(b"payload under tag").expect("seal");
    let last = rec.len() - 1;
    rec[last] ^= 0x80;
    let err = es.server.rx.open(&rec).expect_err("tampered tag must fail");
    assert_eq!(err, CryptoError::AuthFailed);
    assert!(es.server.rx.is_fatal(), "认证失败后必须进入 fatal");

    // fatal 粘滞：该方向后续 open/seal 一律拒绝。
    let fresh = es.client.tx.seal(b"next").expect("seal next");
    assert_eq!(es.server.rx.open(&fresh), Err(CryptoError::Fatal));
    assert_eq!(es.server.rx.seal(b"x"), Err(CryptoError::Fatal));

    // 另一方向（server.tx）不受影响。
    assert!(es.server.tx.seal(b"other direction ok").is_ok());
}

#[test]
fn tampered_ciphertext_rejected() {
    let mut es = established();
    let mut rx = fresh_c2s_rx(&es.material);
    let mut rec = es
        .client
        .tx
        .seal(b"payload under ciphertext")
        .expect("seal");
    // 篡改密文区第一个字节（tag 与前缀不动）。
    rec[4] ^= 0x01;
    assert_eq!(rx.open(&rec), Err(CryptoError::AuthFailed));
    assert!(rx.is_fatal());
}

#[test]
fn tampered_length_field_variants_rejected() {
    let mut es = established();
    let rec = es.client.tx.seal(&[0x33u8; 64]).expect("seal");
    // body_len = 80（64B 明文 + 16B Tag）。
    let variants: Vec<(Vec<u8>, CryptoError)> = vec![
        // 声明 +1：与真实帧长不符（截断/多余），BadFrame。
        (with_body_len(&rec, 81), CryptoError::BadFrame),
        // 声明 -1：同上。
        (with_body_len(&rec, 79), CryptoError::BadFrame),
        // 长度 <16（含 0）：RecordTooSmall。
        (with_body_len(&rec, 15), CryptoError::RecordTooSmall),
        (with_body_len(&rec, 0), CryptoError::RecordTooSmall),
        // 长度 >65536：RecordTooLarge。
        (with_body_len(&rec, 65_537), CryptoError::RecordTooLarge),
        (with_body_len(&rec, u32::MAX), CryptoError::RecordTooLarge),
    ];
    for (tampered, expected) in variants {
        let mut rx = fresh_c2s_rx(&es.material);
        assert_eq!(rx.open(&tampered), Err(expected), "长度字段变体");
        assert!(rx.is_fatal(), "长度违规同样致命");
    }
}

#[test]
fn out_of_order_record_rejected() {
    // 乱序即 AAD(seq)/nonce 不匹配：先投第 2 条必须失败并 fatal。
    let mut es = established();
    let mut rx = fresh_c2s_rx(&es.material);
    let _r0 = es.client.tx.seal(b"first").expect("seal r0");
    let r1 = es.client.tx.seal(b"second").expect("seal r1");
    assert_eq!(rx.open(&r1), Err(CryptoError::AuthFailed));
    assert!(rx.is_fatal());
}

#[test]
fn replay_of_consumed_record_rejected() {
    let mut es = established();
    let mut rx = fresh_c2s_rx(&es.material);
    let r0 = es.client.tx.seal(b"zero").expect("seal r0");
    let r1 = es.client.tx.seal(b"one").expect("seal r1");
    assert_eq!(rx.open(&r0).expect("open r0"), b"zero");
    assert_eq!(rx.open(&r1).expect("open r1"), b"one");
    // 重放 r0：期望序号已是 2，AAD/nonce 均不匹配。
    assert_eq!(rx.open(&r0), Err(CryptoError::AuthFailed));
    assert!(rx.is_fatal());
}

#[test]
fn truncated_or_extended_record_rejected() {
    let mut es = established();
    let rec = es.client.tx.seal(&[0x44u8; 48]).expect("seal");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("缺最后 1B", rec[..rec.len() - 1].to_vec()),
        ("缺整个 tag", rec[..rec.len() - 16].to_vec()),
        ("只有 4B 前缀", rec[..4].to_vec()),
        ("不足前缀 3B", rec[..3].to_vec()),
        ("空输入", Vec::new()),
    ];
    for (name, bad) in cases {
        let mut rx = fresh_c2s_rx(&es.material);
        assert_eq!(rx.open(&bad), Err(CryptoError::BadFrame), "{name}");
    }
    // 多余尾随字节同样不是"恰好一条完整记录"。
    let mut extended = rec;
    extended.push(0x00);
    let mut rx = fresh_c2s_rx(&es.material);
    assert_eq!(
        rx.open(&extended),
        Err(CryptoError::BadFrame),
        "多余尾随字节"
    );
}

#[test]
fn plaintext_too_large_rejected_and_fatal() {
    let mut es = established();
    let too_big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
    assert_eq!(
        es.client.tx.seal(&too_big),
        Err(CryptoError::RecordTooLarge)
    );
    assert!(es.client.tx.is_fatal(), "明文超限后该方向必须 fatal");
    // 同一方向的新实例可用；另一方向不受影响。
    let mut tx = record_layer_at_seq(&es.material.c2s_key, &es.material.c2s_iv, 0);
    assert!(
        tx.seal(&[0u8; MAX_PLAINTEXT_LEN]).is_ok(),
        "65520B 恰为上限"
    );
    let mut c_rx = record_layer_at_seq(&es.material.s2c_key, &es.material.s2c_iv, 0);
    let mut server = es.server;
    let r = server.tx.seal(b"still fine").expect("s2c still works");
    assert_eq!(c_rx.open(&r).expect("open"), b"still fine");
}

// ---------- §5.2 序号末值 ----------

#[test]
fn send_side_sequence_last_valid_then_exhausted() {
    // 序号 2^64-2 是最后一个可发送序号；再发返回 SequenceExhausted，随后 Fatal。
    let es = established();
    let m = &es.material;
    let mut tx = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, u64::MAX - 1);
    let record = tx
        .seal(b"tail record")
        .expect("seq 2^64-2 must be sendable");
    assert_eq!(&record[..4], &27u32.to_be_bytes(), "11B 明文 + 16B Tag");

    // 对端从同序号起可正常解封（AAD 里的 SeqID 一致）。
    let mut rx = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, u64::MAX - 1);
    assert_eq!(rx.open(&record).expect("open last"), b"tail record");

    assert_eq!(
        tx.seal(b"one too many"),
        Err(CryptoError::SequenceExhausted)
    );
    assert_eq!(tx.seal(b"and again"), Err(CryptoError::Fatal));
    assert!(tx.is_fatal());
}

#[test]
fn receive_side_sequence_exhausted_at_max() {
    let es = established();
    let m = &es.material;

    // 情形 1：RcvSeq 已是 2^64-1 → 任何记录立即拒绝（序号耗尽，连接级致命）。
    let mut rx = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, u64::MAX);
    let bogus = ref_seal(&m.c2s_key, &m.c2s_iv, u64::MAX, b"never acceptable");
    assert_eq!(rx.open(&bogus), Err(CryptoError::SequenceExhausted));
    assert_eq!(rx.open(&bogus), Err(CryptoError::Fatal));

    // 情形 2：成功解封 2^64-2 后 RcvSeq 推到 2^64-1，下一条即便合法也拒绝。
    // （发送端自身已不可能再封出记录，这里用参考实现构造"序号越界记录"。）
    let mut tx = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, u64::MAX - 1);
    let last = tx.seal(b"last").expect("seal last");
    let beyond = ref_seal(&m.c2s_key, &m.c2s_iv, u64::MAX, b"beyond");
    let mut rx2 = record_layer_at_seq(&m.c2s_key, &m.c2s_iv, u64::MAX - 1);
    assert_eq!(rx2.open(&last).expect("open seq 2^64-2"), b"last");
    assert_eq!(rx2.open(&beyond), Err(CryptoError::SequenceExhausted));
}

// ---------- fatal 粘滞（跨操作类型） ----------

#[test]
fn fatal_layer_rejects_everything_after_bad_frame() {
    let es = established();
    let mut rx = fresh_c2s_rx(&es.material);
    // 不足 4B 前缀 → BadFrame。
    assert_eq!(rx.open(b"abc"), Err(CryptoError::BadFrame));
    assert_eq!(rx.open(&[]), Err(CryptoError::Fatal));
    assert_eq!(rx.seal(b"also dead"), Err(CryptoError::Fatal));

    // 独立对照：同 key/iv 的新实例不受 fatal 实例影响。
    let mut fresh = fresh_c2s_rx(&es.material);
    let mut tx = record_layer_at_seq(&es.material.c2s_key, &es.material.c2s_iv, 0);
    let rec = tx.seal(b"alive").expect("seal alive");
    assert_eq!(
        fresh.open(&rec).expect("open on fresh layer"),
        b"alive".to_vec()
    );
}
