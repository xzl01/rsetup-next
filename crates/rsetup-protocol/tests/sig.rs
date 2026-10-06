//! §4.3 签名输入布局行为测试：T08/T09、S12、D 边界。

use rsetup_protocol::sig::{
    MAX_DESCRIPTOR_WIRE_BYTES, SigInputError, client_signature_input, server_signature_input,
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

#[test]
fn client_sig_input_exact_bytes() {
    // T08：输出恰 96+6=102B，与逐段拼接逐字节相等；
    // 关键位置 hex 钉死（client_random@0、server_random@32、eph@64、descriptor@96）。
    let cr = ramp(0x00);
    let sr = ramp(0x20);
    let eph = ramp(0x40);
    let got = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    assert_eq!(got.len(), 102);
    let mut want = Vec::new();
    want.extend_from_slice(&cr);
    want.extend_from_slice(&sr);
    want.extend_from_slice(&eph);
    want.extend_from_slice(&DESC);
    assert_eq!(got, want);
    assert_eq!(&hex::encode(&got[..10]), "00010203040506070809");
    assert_eq!(&hex::encode(&got[32..42]), "20212223242526272829");
    assert_eq!(&hex::encode(&got[96..102]), "0a0441424344");
}

#[test]
fn server_sig_input_exact_bytes() {
    // T09：恰 161+6=167B；status 0/1 仅末 1B 不同；
    // 段序按 §4.3.2（server_random 开头，与客户端布局故意不同）。
    let sr = ramp(0x00);
    let cr = ramp(0x20);
    let ed = ramp(0x40);
    let eph = ramp(0x60);
    let server_eph = ramp(0x80);

    let approved = server_signature_input(&sr, &cr, &ed, &eph, &DESC, &server_eph, 0).unwrap();
    let rejected = server_signature_input(&sr, &cr, &ed, &eph, &DESC, &server_eph, 1).unwrap();
    assert_eq!(approved.len(), 167);
    assert_eq!(rejected.len(), 167);
    assert_eq!(&approved[..166], &rejected[..166]);
    assert_eq!(approved[166], 0);
    assert_eq!(rejected[166], 1);

    // 位置钉死：sr@0、cr@32、descriptor@128、server_eph@134、status 为末 1B。
    assert_eq!(&hex::encode(&approved[..10]), "00010203040506070809");
    assert_eq!(&hex::encode(&approved[32..42]), "20212223242526272829");
    assert_eq!(&hex::encode(&approved[128..134]), "0a0441424344");
    assert_eq!(&hex::encode(&approved[134..138]), "80818283");

    // C1：全量钉死所有段，含 [64..96) client_ed25519_pub 与 [96..128)
    // client_x25519_eph_pub 的先后——独立按 §4.3.2 顺序（sr/cr/ed/eph/desc/
    // server_eph/status）构造全量 want 后逐字节比较，段序互换必失败。
    let mut want = Vec::new();
    want.extend_from_slice(&sr);
    want.extend_from_slice(&cr);
    want.extend_from_slice(&ed);
    want.extend_from_slice(&eph);
    want.extend_from_slice(&DESC);
    want.extend_from_slice(&server_eph);
    want.push(0);
    assert_eq!(approved, want);
    // status=1（REJECTED）：仅末 1B 由 0 变 1，前 166B 全部不变。
    let mut want_rej = want.clone();
    want_rej[166] = 1;
    assert_eq!(rejected, want_rej);

    // 两条布局在同输入下不同（前 64B 顺序互换），防“共用一个顺序”的实现 bug。
    let client_in = client_signature_input(&cr, &sr, &eph, &DESC).unwrap();
    assert_ne!(&client_in[..96], &approved[..96]);
}

#[test]
fn server_sig_input_rejects_bad_status() {
    // S12：`status` 是裸 1 字节，不是 varint；0x02/0xFF 不产生签名输入。
    let z = [0u8; 32];
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, 2),
        Err(SigInputError::BadStatus(2)),
    );
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, 0xFF),
        Err(SigInputError::BadStatus(0xFF)),
    );
}

#[test]
fn server_sig_input_bad_status_beats_overlong_descriptor() {
    // I1：坏 status 与超长 descriptor 同时违例时，检查顺序固定为 status 先行，
    // 必须报 BadStatus（原 S12 测试 descriptor 为空，无法区分检查优先级）。
    let z = [0u8; 32];
    let overlong = vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1];
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, &overlong, &z, 2),
        Err(SigInputError::BadStatus(2)),
    );
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, &overlong, &z, 0xFF),
        Err(SigInputError::BadStatus(0xFF)),
    );
}

#[test]
fn descriptor_length_bounds() {
    // D=0 字节层合法；D=16247 上界通过；D=16248 拒绝（16KiB 单帧派生上限）。
    let z = [0u8; 32];
    assert_eq!(client_signature_input(&z, &z, &z, b"").unwrap().len(), 96);
    let max_desc = vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES];
    assert_eq!(
        client_signature_input(&z, &z, &z, &max_desc).unwrap().len(),
        96 + MAX_DESCRIPTOR_WIRE_BYTES,
    );
    assert_eq!(
        client_signature_input(&z, &z, &z, &vec![0u8; MAX_DESCRIPTOR_WIRE_BYTES + 1]),
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
            0,
        ),
        Err(SigInputError::DescriptorTooLarge {
            len: MAX_DESCRIPTOR_WIRE_BYTES + 1,
            max: MAX_DESCRIPTOR_WIRE_BYTES,
        }),
    );
    // 服务端同界通过侧：D=0 → 161B；D=16247 → 161+16247B。
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, b"", &z, 0)
            .unwrap()
            .len(),
        161
    );
    assert_eq!(
        server_signature_input(&z, &z, &z, &z, &max_desc, &z, 1)
            .unwrap()
            .len(),
        161 + MAX_DESCRIPTOR_WIRE_BYTES,
    );
}
