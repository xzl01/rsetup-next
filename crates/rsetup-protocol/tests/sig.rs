//! §4.3 签名输入布局行为测试：T08/T09、S12、约束矩阵、D 边界。
//!
//! 偏移按 §4.3 v3 布局（两条布局同前导）：
//! - 客户端：`prefix[0,28) cr[28,60) sr[60,92) ed[92,124) eph[124,156) BE32(len)[156,160) desc[160,..)`
//! - 服务端：同前导至 `desc[160,160+D)`，再 `server_eph[160+D,192+D) status[192+D] BE32(reason)[193+D,197+D)`

use rsetup_protocol::frame::FRAME_VERSION;
use rsetup_protocol::sig::{
    CLIENT_SIG_DOMAIN_PREFIX, CLIENT_SIG_INPUT_BASE_LEN, MAX_DESCRIPTOR_WIRE_BYTES, REASON_REVOKED,
    REASON_SERVER_ERROR, REASON_SIGNATURE_INVALID, SERVER_SIG_DOMAIN_PREFIX,
    SERVER_SIG_INPUT_BASE_LEN, SIG_DOMAIN_PREFIX_LEN, STATUS_APPROVED, STATUS_REJECTED,
    SigInputError, client_signature_input, server_signature_input,
};

/// 确定性模式字节：从 `base` 起 32 字节，逐字节 +1。
fn ramp(base: u8) -> [u8; 32] {
    let mut a = [0u8; 32];
    for (i, b) in a.iter_mut().enumerate() {
        *b = base.wrapping_add(i as u8);
    }
    a
}

/// SDD §2.4 参考 raw wire bytes：DeviceDescriptor field1 = "ABCD"（0A 04 41 42 43 44）。
const DESC: [u8; 6] = [0x0A, 0x04, b'A', b'B', b'C', b'D'];
/// descriptor 在两条布局中的共同起始偏移。
const DESC_OFF: usize = 160;

/// APPROVED 所需的合法服务端临时公钥（非全零）。
fn eph_nonzero() -> [u8; 32] {
    ramp(0x80)
}

/// REJECTED 所需的合法服务端临时公钥（32B 全零占位）。
fn eph_zero() -> [u8; 32] {
    [0u8; 32]
}

/// 独立按 §4.3 第 1 条拼接客户端签名输入，与实现无共享代码路径。
fn want_client(prefix: &[u8], cr: &[u8], sr: &[u8], ed: &[u8], eph: &[u8], desc: &[u8]) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(prefix);
    w.extend_from_slice(cr);
    w.extend_from_slice(sr);
    w.extend_from_slice(ed);
    w.extend_from_slice(eph);
    w.extend_from_slice(&(desc.len() as u32).to_be_bytes());
    w.extend_from_slice(desc);
    w
}

/// 独立按 §4.3 第 2 条拼接服务端签名输入。
#[allow(clippy::too_many_arguments)]
fn want_server(
    prefix: &[u8],
    sr: &[u8],
    cr: &[u8],
    ed: &[u8],
    eph: &[u8],
    desc: &[u8],
    server_eph: &[u8],
    status: u8,
    reason: u32,
) -> Vec<u8> {
    let mut w = want_client(prefix, sr, cr, ed, eph, desc);
    w.extend_from_slice(server_eph);
    w.push(status);
    w.extend_from_slice(&reason.to_be_bytes());
    w
}

#[test]
fn domain_prefixes_are_verbatim() {
    // 域分离前缀逐字钉死：ASCII 标签 || 0x00 || 0x03，各 28B。
    assert_eq!(SIG_DOMAIN_PREFIX_LEN, 28);
    assert_eq!(
        hex::encode(CLIENT_SIG_DOMAIN_PREFIX),
        "7273657475702d6e6578742f636c69656e742d617574682f76330003"
    );
    assert_eq!(
        hex::encode(SERVER_SIG_DOMAIN_PREFIX),
        "7273657475702d6e6578742f7365727665722d617574682f76330003"
    );
    let mut want = Vec::new();
    want.extend_from_slice(b"rsetup-next/client-auth/v3");
    want.extend_from_slice(&[0x00, 0x03]);
    assert_eq!(CLIENT_SIG_DOMAIN_PREFIX.as_slice(), want.as_slice());
    let mut want = Vec::new();
    want.extend_from_slice(b"rsetup-next/server-auth/v3");
    want.extend_from_slice(&[0x00, 0x03]);
    assert_eq!(SERVER_SIG_DOMAIN_PREFIX.as_slice(), want.as_slice());
    // 版本尾字节与帧版本同源；域标签不同 ⇒ 两条输入绝不共用前缀。
    assert_eq!(CLIENT_SIG_DOMAIN_PREFIX[26..28], [0x00, FRAME_VERSION]);
    assert_eq!(SERVER_SIG_DOMAIN_PREFIX[26..28], [0x00, FRAME_VERSION]);
    assert_ne!(CLIENT_SIG_DOMAIN_PREFIX, SERVER_SIG_DOMAIN_PREFIX);
    assert_eq!(CLIENT_SIG_INPUT_BASE_LEN, 160);
    assert_eq!(SERVER_SIG_INPUT_BASE_LEN, 197);
}

#[test]
fn client_sig_input_exact_bytes() {
    // T08：输出恰 160+6=166B，与逐段拼接逐字节相等；
    // 段偏移钉死（prefix@0、cr@28、sr@60、ed@92、eph@124、BE32(len)@156、desc@160）。
    let cr = ramp(0x00);
    let sr = ramp(0x20);
    let ed = ramp(0x40);
    let eph = ramp(0x60);
    let got = client_signature_input(&cr, &sr, &ed, &eph, &DESC).unwrap();
    assert_eq!(got.len(), 166);
    assert_eq!(
        got,
        want_client(CLIENT_SIG_DOMAIN_PREFIX, &cr, &sr, &ed, &eph, &DESC)
    );
    // 逐偏移 hex 钉死。
    assert_eq!(&hex::encode(&got[..28]), CLIENT_HEX);
    assert_eq!(&hex::encode(&got[28..38]), "00010203040506070809");
    assert_eq!(&hex::encode(&got[60..70]), "20212223242526272829");
    assert_eq!(&hex::encode(&got[92..102]), "40414243444546474849");
    assert_eq!(&hex::encode(&got[124..134]), "60616263646566676869");
    assert_eq!(&hex::encode(&got[156..160]), "00000006");
    assert_eq!(&hex::encode(&got[160..166]), "0a0441424344");
    // 段序互换必失败（client/server random 交换后输入不同）。
    assert_ne!(
        client_signature_input(&sr, &cr, &ed, &eph, &DESC).unwrap(),
        got
    );
    // client_ed25519_pub 必须进入输入：换 ed 必然改变字节。
    assert_ne!(
        client_signature_input(&cr, &sr, &ramp(0x41), &eph, &DESC).unwrap(),
        got
    );
}

const CLIENT_HEX: &str = "7273657475702d6e6578742f636c69656e742d617574682f76330003";
const SERVER_HEX: &str = "7273657475702d6e6578742f7365727665722d617574682f76330003";

#[test]
fn server_sig_input_exact_bytes() {
    // T09：恰 197+6=203B；段序按 §4.3 第 2 条（server_random 开头，与客户端布局故意不同）。
    let sr = ramp(0x00);
    let cr = ramp(0x20);
    let ed = ramp(0x40);
    let eph = ramp(0x60);
    let server_eph = eph_nonzero();

    let approved =
        server_signature_input(&sr, &cr, &ed, &eph, &DESC, &server_eph, STATUS_APPROVED, 0)
            .unwrap();
    assert_eq!(approved.len(), 203);
    assert_eq!(
        approved,
        want_server(
            SERVER_SIG_DOMAIN_PREFIX,
            &sr,
            &cr,
            &ed,
            &eph,
            &DESC,
            &server_eph,
            STATUS_APPROVED,
            0,
        )
    );
    // 位置钉死：prefix@0、sr@28、cr@60、ed@92、eph@124、BE32(len)@156、
    // descriptor@160、server_eph@166、status@198、BE32(reason)@199..203。
    assert_eq!(&hex::encode(&approved[..28]), SERVER_HEX);
    assert_eq!(&hex::encode(&approved[28..38]), "00010203040506070809");
    assert_eq!(&hex::encode(&approved[60..70]), "20212223242526272829");
    assert_eq!(&hex::encode(&approved[92..102]), "40414243444546474849");
    assert_eq!(&hex::encode(&approved[124..134]), "60616263646566676869");
    assert_eq!(&hex::encode(&approved[156..160]), "00000006");
    assert_eq!(&hex::encode(&approved[160..166]), "0a0441424344");
    assert_eq!(&hex::encode(&approved[166..170]), "80818283");
    assert_eq!(approved[198], STATUS_APPROVED);
    assert_eq!(&hex::encode(&approved[199..203]), "00000000");

    // REJECTED（全零占位 + REVOKED）：前 160B 前导完全相同，尾部按约束替换。
    let rejected = server_signature_input(
        &sr,
        &cr,
        &ed,
        &eph,
        &DESC,
        &eph_zero(),
        STATUS_REJECTED,
        REASON_REVOKED,
    )
    .unwrap();
    assert_eq!(rejected.len(), 203);
    assert_eq!(
        rejected,
        want_server(
            SERVER_SIG_DOMAIN_PREFIX,
            &sr,
            &cr,
            &ed,
            &eph,
            &DESC,
            &eph_zero(),
            STATUS_REJECTED,
            REASON_REVOKED,
        )
    );
    assert_eq!(&approved[..DESC_OFF], &rejected[..DESC_OFF]);
    assert_eq!(&rejected[166..198], &[0u8; 32]);
    assert_eq!(rejected[198], STATUS_REJECTED);
    assert_eq!(&hex::encode(&rejected[199..203]), "00000002");

    // 两条布局在同输入下前缀就不同（域分离），防“共用一个前缀”的实现 bug。
    let client_in = client_signature_input(&cr, &sr, &ed, &eph, &DESC).unwrap();
    assert_ne!(&client_in[..28], &approved[..28]);
    assert_ne!(client_in, approved);
}

#[test]
fn descriptor_len_is_be32_not_varint() {
    // descriptor 长度是恰 4B 大端整数：D=258 → 00 00 01 02（varint 会是 82 02）；
    // D=0 → 00 00 00 00；descriptor 必须紧跟长度字段起始于偏移 160。
    let z = [0u8; 32];
    let empty = client_signature_input(&z, &z, &z, &z, b"").unwrap();
    assert_eq!(empty.len(), 160);
    assert_eq!(&empty[156..160], &[0x00, 0x00, 0x00, 0x00]);

    let desc = vec![0x2Au8; 258];
    let got = client_signature_input(&z, &z, &z, &z, &desc).unwrap();
    assert_eq!(got.len(), 160 + 258);
    assert_eq!(&got[156..160], &[0x00, 0x00, 0x01, 0x02]);
    assert_eq!(&got[160..], desc.as_slice());

    // 服务端同规则。
    let got_s =
        server_signature_input(&z, &z, &z, &z, &desc, &eph_zero(), STATUS_REJECTED, 0).unwrap();
    assert_eq!(got_s.len(), 197 + 258);
    assert_eq!(&got_s[156..160], &[0x00, 0x00, 0x01, 0x02]);
    assert_eq!(&got_s[160..418], desc.as_slice());
    assert_eq!(&got_s[418..450], &[0u8; 32]);
    assert_eq!(got_s[450], STATUS_REJECTED);
}

#[test]
fn length_prefix_binds_descriptor_boundary() {
    // 长度前缀把 descriptor 边界绑进签名：追加字节后输入不再是原输入的
    // 前缀（BE32 长度先变），截断/扩展攻击必然改变签名输入。
    let z = [0u8; 32];
    let a = client_signature_input(&z, &z, &z, &z, &DESC).unwrap();
    let mut longer_desc = DESC.to_vec();
    longer_desc.extend_from_slice(&[0x12, 0x02, b'M', b'1']);
    let b = client_signature_input(&z, &z, &z, &z, &longer_desc).unwrap();
    assert!(!b.starts_with(&a));
    assert_eq!(&b[156..160], &[0x00, 0x00, 0x00, 0x0A]);
}

#[test]
fn reason_code_is_be32_not_varint() {
    // `status` 是裸 1 字节；`reason_code` 是语义 uint32 的恰 4B 大端整数。
    let z = [0u8; 32];
    for (reason, tail) in [
        (0u32, [0x00u8, 0x00, 0x00, 0x00]),
        (REASON_REVOKED, [0x00, 0x00, 0x00, 0x02]),
        (REASON_SERVER_ERROR, [0x00, 0x00, 0x00, 0x05]),
    ] {
        let got =
            server_signature_input(&z, &z, &z, &z, &DESC, &eph_zero(), STATUS_REJECTED, reason)
                .unwrap();
        assert_eq!(got.len(), 197 + DESC.len());
        let last = got.len();
        assert_eq!(&got[last - 4..], &tail, "reason {reason} 必须 BE32 编码");
        assert_eq!(got[last - 5], STATUS_REJECTED);
    }
}

#[test]
fn approved_requires_reason_zero() {
    // status=0 ⇒ reason_code 必须为 0，其余值（含 1=SIGNATURE_INVALID）拒绝。
    let eph = eph_nonzero();
    for reason in [1u32, 2, 3, 4, 5, 6, 0xFFFF_FFFF] {
        assert_eq!(
            server_signature_input(
                &[0u8; 32], &[0u8; 32], &[0u8; 32], &[0u8; 32], b"", &eph, 0, reason
            ),
            Err(SigInputError::BadReasonCode(reason)),
            "status=0 时 reason={reason} 必须拒绝"
        );
    }
    assert!(
        server_signature_input(
            &[0u8; 32], &[0u8; 32], &[0u8; 32], &[0u8; 32], b"", &eph, 0, 0
        )
        .is_ok()
    );
}

#[test]
fn rejected_allowed_and_denied_reason_codes() {
    // status=1 ⇒ reason ∈ {0,2,3,4,5}；1=SIGNATURE_INVALID 在 v3 绝不发出；
    // 6 及更大值同样拒绝。
    let z = [0u8; 32];
    for reason in [0u32, 2, 3, 4, 5] {
        assert!(
            server_signature_input(&z, &z, &z, &z, b"", &z, STATUS_REJECTED, reason).is_ok(),
            "status=1 时 reason={reason} 必须合法"
        );
    }
    assert_eq!(
        server_signature_input(
            &z,
            &z,
            &z,
            &z,
            b"",
            &z,
            STATUS_REJECTED,
            REASON_SIGNATURE_INVALID
        ),
        Err(SigInputError::BadReasonCode(REASON_SIGNATURE_INVALID)),
    );
    for reason in [6u32, 7, 0x8000_0000, u32::MAX] {
        assert_eq!(
            server_signature_input(&z, &z, &z, &z, b"", &z, STATUS_REJECTED, reason),
            Err(SigInputError::BadReasonCode(reason)),
        );
    }
}

#[test]
fn server_ephemeral_zero_constraint() {
    // status=0 ⇒ server_x25519_eph_pub 必非全零；status=1 ⇒ 必为 32B 全零占位。
    let z = [0u8; 32];
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, STATUS_APPROVED, 0),
        Err(SigInputError::BadServerEphemeral),
    );
    // 仅 1 字节非零即视为合法公钥（非全零）。
    for pos in [0usize, 15, 31] {
        let mut eph = [0u8; 32];
        eph[pos] = 0x01;
        assert!(
            server_signature_input(&z, &z, &z, &z, b"", &eph, STATUS_APPROVED, 0).is_ok(),
            "偏移 {pos} 非零的公钥在 APPROVED 下必须通过"
        );
        assert_eq!(
            server_signature_input(&z, &z, &z, &z, b"", &eph, STATUS_REJECTED, 0),
            Err(SigInputError::BadServerEphemeral),
            "REJECTED 必须使用全零占位"
        );
    }
    assert!(
        server_signature_input(&z, &z, &z, &z, b"", &z, STATUS_REJECTED, REASON_REVOKED).is_ok()
    );
}

#[test]
fn server_sig_input_rejects_bad_status() {
    // S12：`status` 是裸 1 字节，不是 varint；0x02/0xFF 不产生签名输入。
    let z = [0u8; 32];
    for bad in [2u8, 3, 0x7F, 0xFF] {
        assert_eq!(
            server_signature_input(&z, &z, &z, &z, b"", &z, bad, 0),
            Err(SigInputError::BadStatus(bad)),
        );
    }
}

#[test]
fn check_order_status_beats_reason_eph_descriptor() {
    // I1：多项违例同时存在时，检查顺序固定为
    // status → reason_code → server_x25519_eph_pub → descriptor 上限。
    let z = [0u8; 32];
    let overlong = vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1];
    let nonzero = eph_nonzero();
    // 1) 坏 status 优先于一切（含 reason=1、非零占位、超长 descriptor）。
    for bad in [2u8, 0xFF] {
        assert_eq!(
            server_signature_input(
                &z,
                &z,
                &z,
                &z,
                &overlong,
                &nonzero,
                bad,
                REASON_SIGNATURE_INVALID
            ),
            Err(SigInputError::BadStatus(bad)),
        );
    }
    // 2) 坏 reason 优先于 BadServerEphemeral 与超长 descriptor。
    assert_eq!(
        server_signature_input(
            &z,
            &z,
            &z,
            &z,
            &overlong,
            &nonzero,
            STATUS_APPROVED,
            REASON_REVOKED
        ),
        Err(SigInputError::BadReasonCode(REASON_REVOKED)),
    );
    // 3) 坏公钥占位优先于超长 descriptor。
    assert_eq!(
        server_signature_input(
            &z,
            &z,
            &z,
            &z,
            &overlong,
            &nonzero,
            STATUS_REJECTED,
            REASON_REVOKED
        ),
        Err(SigInputError::BadServerEphemeral),
    );
    // 4) 其余约束均合法时才报 descriptor 上限。
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, &overlong, &nonzero, STATUS_APPROVED, 0),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
    // 5) 客户端侧只有 descriptor 上限一类约束。
    assert_eq!(
        client_signature_input(&z, &z, &z, &z, &overlong),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
}

#[test]
fn descriptor_length_bounds() {
    // D=0 字节层合法；D=16247 上界通过；D=16248 拒绝（16KiB 单帧派生上限）。
    let z = [0u8; 32];
    let max_desc = vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES];
    assert_eq!(
        client_signature_input(&z, &z, &z, &z, b"").unwrap().len(),
        160
    );
    assert_eq!(
        client_signature_input(&z, &z, &z, &z, &max_desc)
            .unwrap()
            .len(),
        CLIENT_SIG_INPUT_BASE_LEN + MAX_DESCRIPTOR_WIRE_BYTES,
    );
    assert_eq!(
        client_signature_input(&z, &z, &z, &z, &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1]),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
    assert_eq!(
        server_signature_input(
            &z,
            &z,
            &z,
            &z,
            &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1],
            &z,
            STATUS_REJECTED,
            0,
        ),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
    // 服务端同界通过侧：D=0 → 197B；D=16247 → 197+16247B；BE32 长度字段同步。
    let eph = eph_nonzero();
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &eph, STATUS_APPROVED, 0)
            .unwrap()
            .len(),
        SERVER_SIG_INPUT_BASE_LEN,
    );
    let at_max =
        server_signature_input(&z, &z, &z, &z, &max_desc, &eph, STATUS_APPROVED, 0).unwrap();
    assert_eq!(
        at_max.len(),
        SERVER_SIG_INPUT_BASE_LEN + MAX_DESCRIPTOR_WIRE_BYTES,
    );
    assert_eq!(
        &at_max[156..160],
        &(MAX_DESCRIPTOR_WIRE_BYTES as u32).to_be_bytes()
    );
    assert_eq!(
        &at_max[DESC_OFF..DESC_OFF + MAX_DESCRIPTOR_WIRE_BYTES],
        &max_desc
    );
    // 边界处偏移仍成立：server_eph 紧随 descriptor 尾。
    assert_eq!(
        &at_max[DESC_OFF + MAX_DESCRIPTOR_WIRE_BYTES..192 + MAX_DESCRIPTOR_WIRE_BYTES],
        &eph[..]
    );
    assert_eq!(at_max[192 + MAX_DESCRIPTOR_WIRE_BYTES], STATUS_APPROVED);
}
