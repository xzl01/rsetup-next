//! 传输加密核心（第 1 层）行为测试：protocol_spec.md §4.4、§5。
//!
//! 覆盖：X25519 协商与公钥拒绝、HKDF-SHA256 双方派生一致性与方向绑定、
//! 双向记录 roundtrip（空/1B/最大/多条交错）、篡改（tag/密文/长度/AAD-seq）、
//! 重放、乱序、截断/多余字节、长度 15/65537，以及 pair 共享 fatal 熔断位
//! 语义（单侧失败 ⇒ 对向 `seal`/`open` 必拒绝，整对不可用）。
//!
//! 本文件**只使用生产 API**（`SessionKeys::derive` + 按值消耗 `self` 的
//! `into_client_record_layers`/`into_server_record_layers` + `seal`/`open`）：
//! 一份 `SessionKeys` 是一次性消耗品，只能产出一个 `RecordLayerPair`，既不
//! 导出密钥四元组，也没有把序号重置回 0 或复制 key/IV 再启发送器的入口。
//! 需要内部状态的用例（HKDF 逐字节钉死、序号末值、线格式参考实现互通、
//! `mark_write_failed`）位于 `src/crypto.rs` 的 `#[cfg(test)] mod tests`。
//!
//! 需要"另一个全新方向"的用场，一律从同一共享密钥**重新 derive**（HKDF 是
//! 确定性的：重新派生得到相同 key 材料、seq 从 0 起、fatal 位独立），不依赖
//! 已删除的"同一份密钥重复产出 pair"能力。
//!
//! fatal 语义：pair 两方向共享同一个熔断位，任一侧失败 ⇒ 两个方向的
//! `seal`/`open` 一律 `Fatal`（由代码强制，不依赖调用方先检查
//! `RecordLayerPair::is_fatal()`）⇒ 调用方必须关闭整条 TCP。因此本文件不断言
//! "对向仍可用"；跨 pair 的"不受影响"对照只用于证明熔断位不是进程级全局。

use rsetup_protocol::crypto::{
    CryptoError, EphemeralKeyPair, MAX_PLAINTEXT_LEN, PreparedRecord, RecordLayerPair,
    RxRecordLayer, SessionKeys, TxRecordLayer,
};
// 序号可指定的构造器只在 `test-util` 下存在（默认关闭）：生产 feature 组合下
// 本文件不导入它，`cargo test -p rsetup-protocol` 编译通过本身就说明"生产构建
// 里没有这个入口"；依赖它的用例整体 cfg 掉。
#[cfg(feature = "test-util")]
use rsetup_protocol::crypto::{
    client_record_layers_at_seq_for_test, server_record_layers_at_seq_for_test,
};

const CLIENT_RANDOM: [u8; 32] = [0x11; 32];
const SERVER_RANDOM: [u8; 32] = [0x22; 32];

// ---------- 公共辅助（全部经生产 API 构造） ----------

/// client 视角记录层对：tx = c2s、rx = s2c。`into_client_record_layers`
/// 消耗 `SessionKeys`，一份密钥只出一个 pair。
fn client_pair(shared: &[u8; 32]) -> RecordLayerPair {
    SessionKeys::derive(shared, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("derive")
        .into_client_record_layers()
}

/// server 视角记录层对：tx = s2c、rx = c2s。一次性语义同上。
fn server_pair(shared: &[u8; 32]) -> RecordLayerPair {
    SessionKeys::derive(shared, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("derive")
        .into_server_record_layers()
}

/// 一次完整握手的产物：两端已建立的记录层对 + 共享密钥。
///
/// 这里存的是**共享密钥**而不是 `SessionKeys`：后者一次性消耗，无法重复产出
/// pair。负面用例需要互不干扰的"全新方向"时，从 `shared` 重新 derive 一份
/// （确定性 ⇒ key 材料一致、seq 从 0 起、fatal 位独立）。
struct Established {
    shared: [u8; 32],
    client: RecordLayerPair,
    server: RecordLayerPair,
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
    Established::from_shared(&shared_c)
}

impl Established {
    fn from_shared(shared: &[u8; 32]) -> Self {
        let shared = *shared;
        Self {
            client: client_pair(&shared),
            server: server_pair(&shared),
            shared,
        }
    }

    /// 全新的 c2s 接收方向（等价于"另一个中控端刚从序号 0 开始收"）。
    ///
    /// 返回类型是**只读方向** [`RxRecordLayer`]：它结构上没有 `seal`，
    /// 接收方向封箱在编译期就不可能（见 `rx_layer_has_no_seal_primitive`）。
    fn fresh_c2s_rx(&self) -> RxRecordLayer {
        server_pair(&self.shared).rx
    }

    /// 全新的 c2s 发送方向（另一个板端从序号 0 开始发）：**只发方向**
    /// [`TxRecordLayer`]，结构上没有 `open`。
    fn fresh_c2s_tx(&self) -> TxRecordLayer {
        client_pair(&self.shared).tx
    }

    /// 全新的完整 client pair（与本会话密钥材料一致、状态独立的另一对）。
    fn fresh_client_pair(&self) -> RecordLayerPair {
        client_pair(&self.shared)
    }

    /// 一条对本会话 c2s 接收方向（seq 0）**完全合法**的记录：用于证明
    /// 对向的拒绝来自共享熔断位，而不是记录本身不可解。
    fn valid_c2s_record(&self) -> Vec<u8> {
        let mut tx = self.fresh_c2s_tx();
        tx.seal(b"valid c2s record at seq 0").expect("seal")
    }
}

fn with_body_len(record: &[u8], new_body_len: u32) -> Vec<u8> {
    let mut out = record.to_vec();
    out[..4].copy_from_slice(&new_body_len.to_be_bytes());
    out
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

// ---------- §5.1 HKDF（仅经可观察行为断言；逐字节钉死见单元测试） ----------

#[test]
fn hkdf_derivation_is_deterministic_across_independent_instances() {
    // 同输入两次完整派生：两侧记录层必须跨实例互通 ⇒ 四组 key/iv 逐字节一致。
    let shared = [0x5Au8; 32];
    let a = Established::from_shared(&shared);
    let b = Established::from_shared(&shared);

    let mut a_client = a.client;
    let mut b_server = b.server;
    let rec = a_client.tx.seal(b"determinism").expect("seal a");
    assert_eq!(b_server.rx.open(&rec).expect("open b"), b"determinism");

    let rec = b_server.tx.seal(b"determinism s2c").expect("seal b");
    assert_eq!(a_client.rx.open(&rec).expect("open a"), b"determinism s2c");
}

#[test]
fn hkdf_salt_order_and_direction_are_bound() {
    let shared = [0x77u8; 32];

    // Salt 顺序绑定：交换 client/server random 派生的密钥必须完全不互通
    // （四元组不同），且失败是认证级的。
    let mut swapped_client = SessionKeys::derive(&shared, &SERVER_RANDOM, &CLIENT_RANDOM)
        .expect("derive swapped")
        .into_client_record_layers();
    let mut normal_server = server_pair(&shared);
    let rec = swapped_client.tx.seal(b"salt order matters").expect("seal");
    assert_eq!(
        normal_server.rx.open(&rec),
        Err(CryptoError::AuthFailed),
        "交换 random 顺序后的记录必须无法被本会话解封"
    );
    // 方向不互换（server 侧同理）：拿 s2c 记录去 server.rx（c2s 方向）解封。
    let mut other_server = server_pair(&shared);
    let s2c = other_server.tx.seal(b"direction probe").expect("seal s2c");
    assert_eq!(
        other_server.rx.open(&s2c),
        Err(CryptoError::AuthFailed),
        "c2s/s2c 方向不可互换"
    );
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
    // 全程无失败：两侧 pair 都不得进入 fatal。
    assert!(!es.client.is_fatal());
    assert!(!es.server.is_fatal());
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

// ---------- 篡改 / 重放 / 乱序 / 截断（失败 ⇒ 整对共享熔断位拉闸） ----------

#[test]
fn tampered_tag_rejected_and_whole_pair_fatal() {
    let mut es = established();
    let mut rec = es.client.tx.seal(b"payload under tag").expect("seal");
    let last = rec.len() - 1;
    rec[last] ^= 0x80;
    let err = es.server.rx.open(&rec).expect_err("tampered tag must fail");
    assert_eq!(err, CryptoError::AuthFailed);

    // 单侧 rx 认证失败 ⇒ 共享熔断位拉闸：未直接失败的 tx 也立即 fatal，
    // 且对向 `seal` 必拒绝——不存在"一侧失败、另一侧仍可用"的状态。
    assert!(es.server.rx.is_fatal(), "认证失败后 rx 必须 fatal");
    assert!(es.server.tx.is_fatal(), "共享熔断位：rx 失败 ⇒ tx 必 fatal");
    assert!(es.server.is_fatal(), "任一方向失败 ⇒ pair fatal");
    // fatal 粘滞：失败方向此后一律拒绝。
    assert_eq!(es.server.rx.open(&rec), Err(CryptoError::Fatal));
    // 单侧 rx.open 失败后，tx.seal 必拒绝（对向同样已被拉闸）。
    assert_eq!(es.server.tx.seal(b"one more"), Err(CryptoError::Fatal));
    // 本端 pair 已作废 ⇒ 关闭整条 TCP、丢弃会话密钥，重新握手才能继续。
}

#[test]
fn tampered_ciphertext_rejected() {
    let mut es = established();
    let mut rx = es.fresh_c2s_rx();
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
        let mut rx = es.fresh_c2s_rx();
        assert_eq!(rx.open(&tampered), Err(expected), "长度字段变体");
        assert!(rx.is_fatal(), "长度违规同样致命");
    }
}

#[test]
fn out_of_order_record_rejected() {
    // 乱序即 AAD(seq)/nonce 不匹配：先投第 2 条必须失败并 fatal。
    let mut es = established();
    let mut rx = es.fresh_c2s_rx();
    let _r0 = es.client.tx.seal(b"first").expect("seal r0");
    let r1 = es.client.tx.seal(b"second").expect("seal r1");
    assert_eq!(rx.open(&r1), Err(CryptoError::AuthFailed));
    assert!(rx.is_fatal());
}

#[test]
fn replay_of_consumed_record_rejected() {
    let mut es = established();
    let mut rx = es.fresh_c2s_rx();
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
        let mut rx = es.fresh_c2s_rx();
        assert_eq!(rx.open(&bad), Err(CryptoError::BadFrame), "{name}");
        assert!(rx.is_fatal(), "{name} 后必须 fatal");
    }
    // 多余尾随字节同样不是"恰好一条完整记录"。
    let mut extended = rec;
    extended.push(0x00);
    let mut rx = es.fresh_c2s_rx();
    assert_eq!(
        rx.open(&extended),
        Err(CryptoError::BadFrame),
        "多余尾随字节"
    );
    assert!(rx.is_fatal());
}

#[test]
fn plaintext_too_large_rejected_and_pair_fatal() {
    let mut es = established();
    let too_big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
    assert_eq!(
        es.client.tx.seal(&too_big),
        Err(CryptoError::RecordTooLarge)
    );
    assert!(es.client.tx.is_fatal(), "明文超限后该方向必须 fatal");
    // 单侧 tx.seal 失败 ⇒ 共享熔断位拉闸：未直接失败的 rx 同样 fatal。
    assert!(es.client.rx.is_fatal(), "共享熔断位：tx 失败 ⇒ rx 必 fatal");
    assert!(es.client.is_fatal(), "任一方向失败 ⇒ pair fatal");

    // 单侧 tx 失败后 rx.open 必拒绝——喂给它的记录**完全合法**（本会话
    // c2s、seq 0）：先对照证明该记录在未拉闸的新 pair 上可正常解封，
    // 拒绝只可能来自熔断位，而不是记录本身不可解。
    let good = es.valid_c2s_record();
    let mut fresh_rx = es.fresh_c2s_rx();
    assert_eq!(
        fresh_rx
            .open(&good)
            .expect("合法记录在未拉闸的新 pair 上可解"),
        b"valid c2s record at seq 0"
    );
    let mut killed_rx_pair = es.fresh_client_pair();
    let too_big_again = vec![0u8; MAX_PLAINTEXT_LEN + 1];
    assert_eq!(
        killed_rx_pair.tx.seal(&too_big_again),
        Err(CryptoError::RecordTooLarge)
    );
    assert_eq!(killed_rx_pair.rx.open(&good), Err(CryptoError::Fatal));
}

// ---------- fatal 粘滞（跨操作类型）与共享熔断位 ----------

#[test]
fn fatal_layer_rejects_everything_after_bad_frame() {
    let es = established();
    let mut rx = es.fresh_c2s_rx();
    // 不足 4B 前缀 → BadFrame。
    assert_eq!(rx.open(b"abc"), Err(CryptoError::BadFrame));
    assert_eq!(rx.open(&[]), Err(CryptoError::Fatal));
    // 接收方向**结构上没有** `seal`（`RxRecordLayer` 不提供封箱原语）：
    // "rx 还能不能 seal" 已不是运行时问题而是编译期问题，见
    // `direction_primitives_are_not_interchangeable` 与 `src/crypto.rs`
    // 中 `TxRecordLayer::seal` / `RxRecordLayer::open` 的 `compile_fail` 示例。

    // 独立对照：**独立 derive 的另一对 pair** 不受该 pair 的熔断位影响
    // （熔断位是 pair 级共享状态，不是进程级全局；同一 pair 内两个方向
    // 则必须一起停用，见 tampered_tag / plaintext_too_large 用例）。
    let mut fresh_pair = es.fresh_client_pair();
    let rec = fresh_pair.tx.seal(b"alive").expect("seal alive");
    let mut peer_rx = es.fresh_c2s_rx();
    assert_eq!(
        peer_rx.open(&rec).expect("open on fresh pair"),
        b"alive".to_vec()
    );
    assert!(!fresh_pair.is_fatal());
}

#[test]
fn pair_is_fatal_is_false_only_while_both_directions_are_clean() {
    let es = established();
    let mut pair = es.fresh_client_pair();
    assert!(!pair.is_fatal());

    // 一次正常往返后仍不 fatal。
    let rec = pair.tx.seal(b"clean").expect("seal");
    let mut peer = server_pair(&es.shared);
    assert_eq!(peer.rx.open(&rec).expect("open"), b"clean");
    assert!(!pair.is_fatal() && !peer.is_fatal());

    // rx 方向被非法长度打挂 ⇒ 共享熔断位：tx 从未失败，但读值与实际操作
    // 都必须是"已死"。
    assert_eq!(
        pair.rx.open(&8u32.to_be_bytes()),
        Err(CryptoError::RecordTooSmall)
    );
    assert!(
        pair.tx.is_fatal(),
        "任一方向失败 ⇒ 两个方向共享 fatal 熔断位"
    );
    assert!(pair.is_fatal(), "任一方向失败 ⇒ pair fatal");
    assert_eq!(pair.tx.seal(b"after kill"), Err(CryptoError::Fatal));
}

#[test]
fn pair_directions_share_single_fatal_bit_both_ways() {
    let es = established();
    let good = es.valid_c2s_record();

    // 方向一：tx.seal 失败（序号无关的长度违规）⇒ 同 pair 的 rx.open 必拒绝，
    // 哪怕喂给它的记录本身完全合法。
    let mut pair = es.fresh_client_pair();
    let too_big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
    assert_eq!(pair.tx.seal(&too_big), Err(CryptoError::RecordTooLarge));
    assert!(pair.rx.is_fatal(), "tx 失败 ⇒ rx 共享 fatal");
    assert_eq!(pair.rx.open(&good), Err(CryptoError::Fatal));

    // 方向二：rx.open 失败（帧不完整）⇒ 同 pair 的 tx.seal 必拒绝。
    let mut pair = es.fresh_client_pair();
    assert_eq!(pair.rx.open(&[0u8; 3]), Err(CryptoError::BadFrame));
    assert!(pair.tx.is_fatal(), "rx 失败 ⇒ tx 共享 fatal");
    assert_eq!(pair.tx.seal(b"too late"), Err(CryptoError::Fatal));

    // 独立 pair 不受影响：熔断位随 pair 独立（每份 SessionKeys 各自一个）。
    let mut clean = es.fresh_client_pair();
    assert!(!clean.is_fatal());
    assert!(clean.tx.seal(b"still alive").is_ok());
}

// =====================================================================
// Task 1：方向类型（Tx 仅封箱 / Rx 仅解封）+ 两阶段发送（prepare → commit）
// =====================================================================

/// 用"同名 fallback trait"探测**内在方法是否长出**：Rust 的方法解析规则是
/// 内在方法优先于 trait 方法。若 `RxRecordLayer` 哪天又长出内在 `seal`，
/// 调用点会解析到内在方法，下面的标志位保持 false，断言立刻失败；若两个方向
/// 又合并成同一个"可 seal 又可 open"的类型，字段类型与 `type_name` 断言同样
/// 失败。这就是"方向不能误用"的回归约束（编译期能力的运行时哨兵）。
mod direction_probe {
    use std::sync::atomic::{AtomicBool, Ordering};

    use rsetup_protocol::crypto::{RxRecordLayer, TxRecordLayer};

    static RX_FELL_BACK_SEAL: AtomicBool = AtomicBool::new(false);
    static RX_FELL_BACK_PREPARE: AtomicBool = AtomicBool::new(false);
    static TX_FELL_BACK_OPEN: AtomicBool = AtomicBool::new(false);

    // fallback 实现的返回类型由本模块自定，调用点一律写成 `let _ = ...`，
    // 因此"内在方法存在"与"回落到 trait 方法"两种情况都能编译。
    pub trait RxSealFallback {
        fn seal(&mut self, _plaintext: &[u8]);
    }
    pub trait RxPrepareFallback {
        fn prepare(&mut self, _plaintext: &[u8]);
    }
    pub trait TxOpenFallback {
        fn open(&mut self, _record: &[u8]);
    }
    impl RxSealFallback for RxRecordLayer {
        fn seal(&mut self, _plaintext: &[u8]) {
            RX_FELL_BACK_SEAL.store(true, Ordering::Relaxed);
        }
    }
    impl RxPrepareFallback for RxRecordLayer {
        fn prepare(&mut self, _plaintext: &[u8]) {
            RX_FELL_BACK_PREPARE.store(true, Ordering::Relaxed);
        }
    }
    impl TxOpenFallback for TxRecordLayer {
        fn open(&mut self, _record: &[u8]) {
            TX_FELL_BACK_OPEN.store(true, Ordering::Relaxed);
        }
    }
    pub fn rx_seal_fell_back() -> bool {
        RX_FELL_BACK_SEAL.swap(false, Ordering::Relaxed)
    }
    pub fn rx_prepare_fell_back() -> bool {
        RX_FELL_BACK_PREPARE.swap(false, Ordering::Relaxed)
    }
    pub fn tx_open_fell_back() -> bool {
        TX_FELL_BACK_OPEN.swap(false, Ordering::Relaxed)
    }
}

#[test]
fn direction_primitives_are_not_interchangeable() {
    // 只有 fallback trait 在作用域内时，`rx.seal(..)` 这类调用才可编译；
    // 若内在方法存在则优先解析到内在方法（哨兵位保持 false）。
    use direction_probe::{RxPrepareFallback, RxSealFallback, TxOpenFallback};

    let es = established();

    // 正向 API 面钉死：seal 只挂在发送方向，open 只挂在接收方向。
    let _: fn(&mut TxRecordLayer, &[u8]) -> Result<Vec<u8>, CryptoError> = TxRecordLayer::seal;
    let _: fn(&mut TxRecordLayer, &[u8]) -> Result<PreparedRecord, CryptoError> =
        TxRecordLayer::prepare;
    let _: fn(&mut TxRecordLayer, PreparedRecord) -> Result<u64, CryptoError> =
        TxRecordLayer::commit;
    let _: fn(&mut RxRecordLayer, &[u8]) -> Result<Vec<u8>, CryptoError> = RxRecordLayer::open;
    // 两个方向的字段类型各自独立，不是同一个"两用"类型的别名。
    fn pin_fields(pair: &RecordLayerPair) -> (&TxRecordLayer, &RxRecordLayer) {
        (&pair.tx, &pair.rx)
    }
    let mut pair = es.fresh_client_pair();
    let (tx, rx) = pin_fields(&pair);
    assert_ne!(
        std::any::type_name::<TxRecordLayer>(),
        std::any::type_name::<RxRecordLayer>(),
        "两个方向又变成同一个类型 = 两用原语回归"
    );
    // 两个方向的序号计数器互相独立（TxSeq 推进不得带动 RcvSeq）。
    assert_eq!((tx.tx_seq(), rx.rx_seq()), (0, 0));
    pin_fields(&pair).0.tx_seq();
    assert_eq!(pair.tx.seal(b"advance tx").map(|_| pair.tx.tx_seq()), Ok(1));
    assert_eq!(pair.rx.rx_seq(), 0, "发送方向推进不得带动接收序号");

    // 反向不存在（哨兵见 direction_probe 模块 + src/crypto.rs 的 compile_fail 文档）：
    // 接收方向 seal/prepare、发送方向 open 都必须走 fallback trait。
    let mut rx_pair = es.fresh_client_pair();
    rx_pair.rx.seal(b"rx must not seal");
    assert!(
        direction_probe::rx_seal_fell_back(),
        "RxRecordLayer 又长出了内在 seal：接收方向可封箱"
    );
    rx_pair.rx.prepare(b"rx must not prepare");
    assert!(
        direction_probe::rx_prepare_fell_back(),
        "RxRecordLayer 又长出了内在 prepare：接收方向可消耗发送序号"
    );
    pair.tx.open(&[0, 0, 0, 16]);
    assert!(
        direction_probe::tx_open_fell_back(),
        "TxRecordLayer 又长出了内在 open：发送方向可被喂对端字节"
    );
}

#[test]
fn commit_is_the_only_point_where_tx_seq_advances() {
    let es = established();
    let mut tx = es.fresh_c2s_tx();
    assert_eq!(tx.tx_seq(), 0, "新记录层发送序号从 0 起");

    let first = tx.prepare(b"first").expect("prepare seq0");
    assert_eq!(first.seq(), 0);
    assert_eq!(first.wire().len(), 4 + 5 + 16);
    // prepare **不**推进序号：nonce/AAD 只是被预留，还没被写出确认。
    assert_eq!(tx.tx_seq(), 0, "prepare 之后、commit 之前不得推进 TxSeq");

    // 未 commit 之前，第二条预封箱必须被拒绝（同一序号的 nonce 只允许存在一份）。
    assert_eq!(
        tx.prepare(b"one too many").map(|_dup| ()),
        Err(CryptoError::PendingOutstanding),
        "允许同时存在两条同序号 pending = nonce 重用入口"
    );

    assert_eq!(tx.commit(first).expect("commit seq0"), 0);
    assert_eq!(tx.tx_seq(), 1, "整条记录写出确认后才推进");

    let second = tx.prepare(b"second").expect("prepare seq1");
    assert_eq!(second.seq(), 1);
    assert_eq!(tx.commit(second).expect("commit seq1"), 1);
    assert_eq!(tx.tx_seq(), 2);
    assert!(!tx.is_fatal(), "正常两阶段提交不得拉闸");

    // 预封箱输出的字节必须与原子 seal **逐字节相同**（同 key/iv/seq ⇒ 同
    // 密文/tag），且长度前缀与 AAD 仍按 §5.2 绑定该序号。
    let mut other = es.fresh_c2s_tx();
    let prepared = other.prepare(b"same").expect("prepare");
    let prepared_bytes = prepared.wire().to_vec();
    assert_eq!(other.commit(prepared).expect("commit"), 0);
    let mut atomic = es.fresh_c2s_tx();
    assert_eq!(
        prepared_bytes,
        atomic.seal(b"same").expect("atomic seal"),
        "prepare/commit 与 seal 的输出必须一致（同一实现，不是两套线格式）"
    );
    let mut rx = es.fresh_c2s_rx();
    assert_eq!(rx.open(&prepared_bytes).expect("open"), b"same");
}

#[test]
fn pending_dropped_or_aborted_without_commit_kills_the_pair() {
    let es = established();

    // ① 忘记 commit（pending 被 drop）⇒ 整对作废：绝不允许"这条没写就再发一条"。
    let mut pair = es.fresh_client_pair();
    let pending = pair.tx.prepare(b"written unknown").expect("prepare");
    assert!(!pair.is_fatal(), "pending 在途期间仍可正常 commit");
    drop(pending);
    assert!(pair.is_fatal(), "pending 未 commit 被丢弃必须拉闸整对");
    assert_eq!(pair.tx.seal(b"retry"), Err(CryptoError::Fatal));
    assert_eq!(
        pair.rx.open(&es.valid_c2s_record()),
        Err(CryptoError::Fatal)
    );

    // ② 显式 abort（写出结果不确定）⇒ 同样整对作废，且返回 true 供调用方判定。
    let mut pair = es.fresh_client_pair();
    let pending = pair.tx.prepare(b"half written").expect("prepare");
    let wire_len = pending.wire().len();
    assert!(wire_len > 4);
    drop(pending);
    assert!(pair.tx.abort_pending());

    // ③ 没有 pending 时 abort 返回 false（不谎报"作废了一条"）。
    let mut clean = es.fresh_client_pair();
    assert!(!clean.tx.abort_pending());
    assert!(!clean.is_fatal(), "无 pending 的 abort 不应拉闸");
}

#[test]
fn commit_rejects_foreign_or_stale_pending_record() {
    let es = established();

    // 别的 pair 的 pending：即使密钥材料完全相同（确定性 HKDF）也不得 commit
    // —— 否则等于把 A 连接的记录塞进 B 连接的序号空间。
    let mut a = es.fresh_client_pair();
    let mut b = es.fresh_client_pair();
    let foreign = b.tx.prepare(b"foreign").expect("prepare on b");
    assert_eq!(a.tx.tx_seq(), 0);
    assert!(
        a.tx.commit(foreign).is_err(),
        "跨 pair 的 pending 必须被拒绝"
    );
    assert!(
        a.is_fatal() || b.is_fatal(),
        "跨 pair commit 之后至少一侧必须拉闸"
    );

    // 同 pair 但序号已不匹配（stale）：commit 只能消费当前 pending 的那一条。
    let mut pair = es.fresh_client_pair();
    let p0 = pair.tx.prepare(b"p0").expect("prepare p0");
    pair.tx.commit(p0).expect("commit p0");
    let p1 = pair.tx.prepare(b"p1").expect("prepare p1");
    assert_eq!(p1.seq(), 1);
    assert_eq!(pair.tx.commit(p1).expect("commit p1"), 1);
    // 现在再拿"序号 1"的构造体去 commit 已无对应 pending ⇒ 拒绝 + 拉闸。
    let stale = pair.tx.prepare(b"p2").expect("prepare p2");
    assert_eq!(pair.tx.commit(stale).expect("commit p2"), 2);
    let orphan = {
        let mut other = es.fresh_client_pair();
        other.tx.prepare(b"orphan").expect("prepare orphan")
    };
    assert!(
        pair.tx.commit(orphan).is_err(),
        "已 commit 之后再消费同一序号的 pending 必须失败"
    );
    assert!(pair.is_fatal(), "commit 不匹配必须拉闸");
}

#[cfg(feature = "test-util")]
#[test]
fn test_seq_constructor_still_consumes_session_keys() {
    // 序号末值只能由**消耗 SessionKeys** 的入口构造：函数指针签名钉死，
    // 有人把它改成 `&SessionKeys`（可反复造同密钥同序号的发送器）即编译失败。
    let _client: fn(SessionKeys, u64, u64) -> RecordLayerPair =
        client_record_layers_at_seq_for_test;
    let _server: fn(SessionKeys, u64, u64) -> RecordLayerPair =
        server_record_layers_at_seq_for_test;

    // 发送方向序号末值：2^64-2 仍可发一条，其后即 SequenceExhausted ⇒ Fatal。
    let keys = SessionKeys::derive(&[0x5a; 32], &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let mut pair = client_record_layers_at_seq_for_test(keys, u64::MAX - 1, 0);
    let pending = pair.tx.prepare(b"tail").expect("prepare last valid seq");
    assert_eq!(pending.seq(), u64::MAX - 1);
    assert_eq!(
        pair.tx.commit(pending).expect("commit last valid"),
        u64::MAX - 1
    );
    assert_eq!(
        pair.tx.prepare(b"one too many").map(|_p| ()),
        Err(CryptoError::SequenceExhausted)
    );
    assert_eq!(pair.tx.seal(b"and again"), Err(CryptoError::Fatal));

    // 接收方向期望序号 2^64-1 ⇒ 任何来帧一律 SequenceExhausted。
    let keys = SessionKeys::derive(&[0x5a; 32], &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let mut pair = server_record_layers_at_seq_for_test(keys, 0, u64::MAX);
    assert_eq!(pair.rx.rx_seq(), u64::MAX);
    let valid = es_valid_record();
    assert_eq!(
        pair.rx.open(valid.as_slice()),
        Err(CryptoError::SequenceExhausted),
        "记录本身合法，仅期望序号已耗尽 ⇒ 仍须终止"
    );
}

/// 一条对 c2s 方向 seq 0 合法的记录（用于 seq 末值负例的"记录本身没问题"对照）。
#[cfg(feature = "test-util")]
fn es_valid_record() -> Vec<u8> {
    let mut pair = SessionKeys::derive(&[0x5a; 32], &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("derive")
        .into_client_record_layers();
    pair.tx.seal(b"valid at seq 0").expect("seal")
}
