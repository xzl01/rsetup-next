//! 真实 ring Ed25519 行为测试：T12–T16、S9–S11、S16–S18（protocol_spec.md §4.3/§4.2）。

use ring::signature::{Ed25519KeyPair, KeyPair};
use rsetup_protocol::ed25519::{VerifyError, sign_ed25519, verify_ed25519};
use rsetup_protocol::sig::{CLIENT_SIG_INPUT_BASE_LEN, client_signature_input};

/// 测试 fixture：固定种子（非秘密），模式字节 01 02 03 ... 20。
fn test_seed() -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(1);
    }
    a
}

fn keypair_from(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).expect("fixed 32B seed must be accepted")
}

fn ramp(base: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    a
}

/// 固定 fixture：(client_random, server_random, client_x25519_eph_pub)，确定性模式字节。
fn fixture() -> ([u8; 32], [u8; 32], [u8; 32]) {
    (ramp(0x20), ramp(0x40), ramp(0x60))
}

/// §4.3 第 1 条输入必须携带签名公钥本身：取真实密钥对的 32B Ed25519 公钥。
fn ed_pub(key: &Ed25519KeyPair) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(key.public_key().as_ref());
    a
}

/// SDD §2.4 参考 raw wire bytes：DeviceDescriptor field1 = "ABCD"。
const DESC: [u8; 6] = [0x0A, 0x04, b'A', b'B', b'C', b'D'];

#[test]
fn ring_verify_happy_path() {
    // T12：完整客户端签名输入上真实签名 + 真实验签（固定种子，确定性）。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &msg);
    assert_eq!(
        verify_ed25519(&msg, &sig, key.public_key().as_ref()),
        Ok(())
    );
}

#[test]
fn ring_verify_tamper_variants() {
    // S16：消息翻 1 bit / 签名改 1 字节 / 公钥改 1 字节 → InvalidSignature（3 个独立变体）。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &msg);
    let pk = key.public_key().as_ref().to_vec();

    let mut msg_t = msg.clone();
    msg_t[0] ^= 0x01;
    assert_eq!(
        verify_ed25519(&msg_t, &sig, &pk),
        Err(VerifyError::InvalidSignature)
    );

    // 单独钉死 descriptor 原始 wire 区任意 1B 改动必使签名失效。
    let mut descriptor_t = msg.clone();
    // 客户端布局中 descriptor 起始偏移即固定前导长度 160。
    descriptor_t[CLIENT_SIG_INPUT_BASE_LEN] ^= 0x01;
    assert_eq!(
        verify_ed25519(&descriptor_t, &sig, &pk),
        Err(VerifyError::InvalidSignature)
    );

    let mut sig_t = sig;
    sig_t[10] ^= 0xFF;
    assert_eq!(
        verify_ed25519(&msg, &sig_t, &pk),
        Err(VerifyError::InvalidSignature)
    );

    let mut pk_t = pk.clone();
    pk_t[3] ^= 0x80;
    assert_eq!(
        verify_ed25519(&msg, &sig, &pk_t),
        Err(VerifyError::InvalidSignature)
    );
}

#[test]
fn ring_verify_length_guards() {
    // S18/S13：长度错误类型化且不进入 ring 调用；
    // 守卫顺序固定：先公钥后签名（两者皆错 → 报公钥错误）。
    let pk = [7u8; 32];
    let sig64 = [9u8; 64];
    let msg = [1u8; 5];
    assert_eq!(
        verify_ed25519(&msg, &sig64[..63], &pk),
        Err(VerifyError::SignatureLen { len: 63 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &[0u8; 65], &pk),
        Err(VerifyError::SignatureLen { len: 65 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64, &pk[..31]),
        Err(VerifyError::PublicKeyLen { len: 31 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64, &[0u8; 33]),
        Err(VerifyError::PublicKeyLen { len: 33 }),
    );
    assert_eq!(
        verify_ed25519(&msg, &sig64[..63], &pk[..31]),
        Err(VerifyError::PublicKeyLen { len: 31 }),
    );
}

#[test]
fn ring_verify_wrong_keypair_fails() {
    // S17：密钥对 A 的签名 + 密钥对 B 的公钥（各自合法、只是不匹配）→ InvalidSignature。
    let (cr, sr, eph) = fixture();
    let k1 = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &ed_pub(&k1), &eph, &DESC).unwrap();
    let mut seed2 = test_seed();
    seed2[0] = 0xFF;
    let k2 = keypair_from(&seed2);
    let sig = sign_ed25519(&k1, &msg);
    assert_eq!(
        verify_ed25519(&msg, &sig, k2.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn sig_input_order_is_binding() {
    // S9/T10：对正确布局输入签名，用 (client_random, server_random) 互换后的输入
    // 验签 → 必败；两段装配输出必不同字节。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let correct = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &DESC).unwrap();
    let swapped = client_signature_input(&sr, &cr, &ed_pub(&key), &eph, &DESC).unwrap();
    assert_ne!(correct, swapped);
    let sig = sign_ed25519(&key, &correct);
    assert_eq!(
        verify_ed25519(&swapped, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn descriptor_raw_bytes_rule() {
    // S10/T11：用 descriptor A（canonical 字段序）签名；用 B（字段序不同、
    // 同逻辑值）拼装验签 → 必败。A/B 均为手写 raw wire bytes；
    // 本测试不得出现“解码再编码”代码路径。
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let a: [u8; 10] = [0x0A, 0x04, b'A', b'B', b'C', b'D', 0x12, 0x02, b'M', b'1'];
    let b: [u8; 10] = [0x12, 0x02, b'M', b'1', 0x0A, 0x04, b'A', b'B', b'C', b'D'];
    let msg_a = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &a).unwrap();
    let msg_b = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &b).unwrap();
    let sig = sign_ed25519(&key, &msg_a);
    assert_eq!(
        verify_ed25519(&msg_a, &sig, key.public_key().as_ref()),
        Ok(())
    );
    assert_eq!(
        verify_ed25519(&msg_b, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn cross_session_replay_fails() {
    // S11/T15：会话 1（server_random=R1）输入签名；会话 2（R2）输入验签 → 必败。
    let (cr, _sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let r1 = ramp(0xA0);
    let r2 = ramp(0xB0);
    let s1 = client_signature_input(&cr, &r1, &ed_pub(&key), &eph, &DESC).unwrap();
    let s2 = client_signature_input(&cr, &r2, &ed_pub(&key), &eph, &DESC).unwrap();
    let sig = sign_ed25519(&key, &s1);
    assert_eq!(verify_ed25519(&s1, &sig, key.public_key().as_ref()), Ok(()));
    assert_eq!(
        verify_ed25519(&s2, &sig, key.public_key().as_ref()),
        Err(VerifyError::InvalidSignature),
    );
}

#[test]
fn vector_pinning() {
    // T16：固定种子 + 固定消息（166B 客户端签名输入，§4.3 v3 布局）→ 签名 hex 必须等于钉死常量。
    // 该向量是任何第二实现（含未来 board agent）的互操作基准。
    // RED：常量为全零哨兵；首次运行必失败，并从 `PINNED_SIG=` 输出读出真实值。
    // GREEN：用 ring 输出的真实值替换哨兵、删除 eprintln、重跑 PASS，
    // 并把最终向量记入任务报告。
    // 128 字符单条字面量 = 64B 签名；来源：ring 0.17.14 对上述固定种子 + 166B
    // v3 签名输入的真实输出（v3 布局转录后重新钉死，旧 v2 布局向量作废）。
    const PINNED_SIG_HEX: &str = "7cccf40c34b377eb749b6b0ede4b2438d6c45487a0033c42572e992147281627867f778ac672167e3f26b122494ccef547cacafded4c75e055dc91f212f52e08";
    let (cr, sr, eph) = fixture();
    let key = keypair_from(&test_seed());
    let msg = client_signature_input(&cr, &sr, &ed_pub(&key), &eph, &DESC).unwrap();
    assert_eq!(msg.len(), 166);
    let sig = sign_ed25519(&key, &msg);
    let hex_sig = hex::encode(sig);
    assert_eq!(hex_sig, PINNED_SIG_HEX);
}
