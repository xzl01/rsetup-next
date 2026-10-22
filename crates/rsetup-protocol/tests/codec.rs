//! 业务 Protobuf codec 的契约测试（03 §2）：固定向量、9 变体 round-trip、
//! canonical 确定性、以及严格解码的 fail-closed 负例（断言固定 `CodecError` 变体）。

use rsetup_protocol::codec::BusinessCodec;
use rsetup_protocol::wire::{
    ApprovedCodecWitness, BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind,
    Capabilities, ClockSample, Codec, CodecError, DeviceStatus, MAX_ACTION_BYTES,
    MAX_ENVELOPE_BYTES, MAX_METRICS, Metric, NoCodec, Query, RebootExecute, RebootPrepare,
    RebootTicket, SUPPORTED_ACTIONS, TaskQuery, TaskRecord, approved_codec_witness,
    decode_business, encode_business,
};

const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

fn witness() -> ApprovedCodecWitness {
    approved_codec_witness()
}

fn enc(message: &BusinessPayload) -> Vec<u8> {
    encode_business(&BusinessCodec, Some(&witness()), message).expect("合法 DTO 必须可编码")
}

fn dec(expected: BusinessPayloadKind, bytes: &[u8]) -> Result<BusinessPayload, CodecError> {
    decode_business(&BusinessCodec, Some(&witness()), expected, bytes)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 等长改写编码结果：用来在**保持 wire 合法**的前提下制造「DTO 不合法」的输入
/// （编码入口本身禁止产出 DTO 非法字节，故非法输入只能这样构造）。
fn patch(hay: &mut [u8], from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len());
    assert!(
        hay.windows(from.len()).any(|w| w == from),
        "未找到 {from:?}"
    );
    let start = hay.windows(from.len()).position(|w| w == from).unwrap();
    hay[start..start + from.len()].copy_from_slice(to);
}

fn expect_err(kind: BusinessPayloadKind, bytes: &[u8], want: CodecError) {
    let got = dec(kind, bytes);
    assert_eq!(got, Err(want), "输入 {:02x?} 实际得到 {got:?}", bytes);
}

// ---- fixtures ----

fn query() -> Query {
    Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    }
}

fn capabilities(actions: &[&str]) -> Capabilities {
    Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_version: "1.4.2".to_string(),
        supported_actions: actions.iter().map(|a| a.to_string()).collect(),
        durable_task_journal: true,
        max_status_bytes: 65_536,
        journal_epoch: EPOCH.to_string(),
    }
}

fn metric(key: &str, value: Option<f64>, unit: &str, quality: &str) -> Metric {
    Metric {
        key: key.to_string(),
        value,
        unit: unit.to_string(),
        quality: quality.to_string(),
    }
}

fn device_status() -> DeviceStatus {
    DeviceStatus {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_epoch: AGENT_EPOCH.to_string(),
        sample_seq: u64::MAX,
        sampled_unix_ms: -1_700_000_000_000,
        sampled_monotonic_ns: 1 << 63,
        clock_epoch: CLOCK_EPOCH.to_string(),
        metrics: vec![
            metric("cpu.usage_pct", Some(42.5), "percent", "ok"),
            metric(
                "memory.used_bytes",
                Some(9_007_199_254_740_991.0),
                "bytes",
                "ok",
            ),
            metric("temperature.cpu_celsius", None, "celsius", "unavailable"),
        ],
        clock_quality: "observed".to_string(),
    }
}

/// 生成恰好 `count` 条、key 互不重复的**合法** `DeviceStatus`（用于数量边界）。
fn status_with_metrics(count: usize) -> DeviceStatus {
    DeviceStatus {
        metrics: (0..count)
            .map(|i| metric(&format!("metric.{i:04}"), Some(1.0), "percent", "ok"))
            .collect(),
        ..device_status()
    }
}

/// 手写的**完整合法** Metric 条目（`DeviceStatus` field 8，长度前缀按实际字节数）：
/// key + optional double + unit + quality 全部齐备。用它追加条目，可证明「第 129 条」
/// 被拒的原因只可能是数量上限，而不是截断或非法字节。
fn metric_entry(key: &str) -> Vec<u8> {
    let mut inner = Vec::new();
    inner.push(0x0a); // Metric field 1 key (string)
    inner.push(key.len() as u8);
    inner.extend_from_slice(key.as_bytes());
    inner.push(0x11); // Metric field 2 value (optional double, wire 1)
    inner.extend_from_slice(&0.0f64.to_le_bytes());
    inner.extend_from_slice(&[0x1a, 0x07]); // Metric field 3 unit
    inner.extend_from_slice(b"percent");
    inner.extend_from_slice(&[0x22, 0x02]); // Metric field 4 quality
    inner.extend_from_slice(b"ok");
    let mut out = vec![0x42]; // DeviceStatus field 8 metrics（嵌套 message）
    out.push(u8::try_from(inner.len()).expect("单条 Metric 编码必须小于 256 字节"));
    out.extend_from_slice(&inner);
    out
}

fn clock_sample() -> ClockSample {
    ClockSample {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        clock_epoch: CLOCK_EPOCH.to_string(),
        receive_unix_ms: i64::MIN,
        send_unix_ms: -1,
        receive_monotonic_ns: u64::MAX,
        send_monotonic_ns: 7,
        quality: "clock_unstable".to_string(),
    }
}

fn ticket() -> RebootTicket {
    let mut token = [0u8; 32];
    for (i, b) in token.iter_mut().enumerate() {
        *b = i as u8 + 1;
    }
    RebootTicket {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        boot_id: BOOT.to_string(),
        execution_token: token,
        valid_for_ms: 30_000,
    }
}

fn execute() -> RebootExecute {
    RebootExecute {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT.to_string(),
        execution_token: [0xab; 32],
    }
}

fn record() -> TaskRecord {
    TaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        journal_epoch: EPOCH.to_string(),
        record_version: u64::MAX,
        state: "succeeded".to_string(),
        pre_boot_id: BOOT.to_string(),
        observed_boot_id: Some(BOOT.to_string()),
        reason_code: None,
        updated_unix_ms: Some(-1),
        evidence_kind: Some("boot_transition_observed".to_string()),
    }
}

/// 覆盖 9 个变体的代表消息（多 Metric、value Some/None、可选字段 Some/None、
/// 负 int64、u64 高位、空 repeated）。
fn fixtures() -> Vec<BusinessPayload> {
    vec![
        BusinessPayload::Query(query()),
        BusinessPayload::Capabilities(capabilities(&[])),
        BusinessPayload::Capabilities(capabilities(&[
            "device.capabilities.get",
            "device.status.get",
            "device.clock.get",
            "device.task.get",
            "device.reboot.prepare",
            "device.reboot.execute",
        ])),
        BusinessPayload::DeviceStatus(device_status()),
        BusinessPayload::DeviceStatus(DeviceStatus {
            metrics: Vec::new(),
            ..device_status()
        }),
        BusinessPayload::ClockSample(clock_sample()),
        BusinessPayload::RebootPrepare(RebootPrepare {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: SUB_TASK.to_string(),
            expected_boot_id: BOOT.to_string(),
        }),
        BusinessPayload::RebootTicket(ticket()),
        BusinessPayload::RebootExecute(execute()),
        BusinessPayload::TaskQuery(TaskQuery {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: SUB_TASK.to_string(),
        }),
        BusinessPayload::TaskRecord(record()),
        BusinessPayload::TaskRecord(TaskRecord {
            observed_boot_id: None,
            reason_code: Some("OS_ERROR".to_string()),
            updated_unix_ms: None,
            evidence_kind: None,
            ..record()
        }),
    ]
}

// ====================================================================
// 固定字节向量
// ====================================================================

#[test]
fn query_schema_version_one_is_two_bytes_0801() {
    let bytes = enc(&BusinessPayload::Query(query()));
    assert_eq!(bytes, vec![0x08, 0x01]);
    assert_eq!(hex(&bytes), "0801");
    assert_eq!(
        dec(BusinessPayloadKind::Query, &bytes).unwrap(),
        BusinessPayload::Query(query())
    );
}

/// 人工计算的完整向量：`TaskQuery { schema_version: 1, sub_task_id: SUB_TASK }`
///
/// ```text
/// 08 01                     field 1 schema_version (uint32, wire 0) = 1
/// 12 24                     field 2 sub_task_id (string, wire 2)，len = 36 = 0x24
/// 39 63 38 35 38 39 30 31   "9c858901"
/// 2d 38 61 35 37 2d 34 37   "-8a57-47"
/// 39 31 2d 38 31 66 65 2d   "91-81fe-"
/// 34 63 34 35 35 62 30 39   "4c455b09"
/// 39 62 63 39               "9bc9"
/// ```
#[test]
fn task_query_matches_hand_computed_full_vector() {
    const EXPECTED_HEX: &str = "08011224\
        3963383538393031\
        2d386135372d3437\
        39312d383166652d\
        3463343535623039\
        39626339";
    let message = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    });
    let bytes = enc(&message);
    assert_eq!(hex(&bytes), EXPECTED_HEX);
    assert_eq!(
        dec(BusinessPayloadKind::TaskQuery, &bytes).unwrap(),
        message
    );
}

/// 负 int64 与 u64 高位必须编成 10 字节二补码 varint（`ff * 9, 01` 即 -1 与 u64::MAX）。
#[test]
fn int64_and_uint64_use_ten_byte_two_complement_varints() {
    let tail = TaskRecord {
        evidence_kind: None,
        ..record()
    };
    let bytes = enc(&BusinessPayload::TaskRecord(tail.clone()));

    // field 9 (updated_unix_ms, int64) tag = 0x48，值 -1 → ff*9 01，且它是最后一个字段
    let mut suffix = vec![0x48u8];
    suffix.extend_from_slice(&[0xff; 9]);
    suffix.push(0x01);
    assert!(
        bytes.ends_with(&suffix),
        "updated_unix_ms=-1 必须以 10 字节二补码 varint 结尾，实际 {:02x?}",
        bytes
    );

    // field 4 (record_version, uint64) tag = 0x20，值 u64::MAX → ff*9 01
    let mut version = vec![0x20u8];
    version.extend_from_slice(&[0xff; 9]);
    version.push(0x01);
    assert!(
        bytes.windows(version.len()).any(|w| w == version),
        "record_version=u64::MAX 应为 10 字节 varint，实际 {:02x?}",
        bytes
    );

    assert_eq!(
        dec(BusinessPayloadKind::TaskRecord, &bytes).unwrap(),
        BusinessPayload::TaskRecord(tail)
    );

    // ClockSample：i64::MIN 与 u64::MAX 全部 round-trip
    let clock = BusinessPayload::ClockSample(clock_sample());
    let bytes = enc(&clock);
    assert_eq!(
        dec(BusinessPayloadKind::ClockSample, &bytes).unwrap(),
        clock
    );
}

// ====================================================================
// round-trip 与确定性
// ====================================================================

#[test]
fn all_nine_variants_round_trip() {
    let mut seen = vec![];
    for message in fixtures() {
        let kind = message.kind();
        seen.push(kind);
        let bytes = enc(&message);
        let decoded =
            dec(kind, &bytes).unwrap_or_else(|error| panic!("{kind:?} 解码失败: {error:?}"));
        assert_eq!(decoded, message, "{kind:?} round-trip 结果不一致");
        assert_eq!(decoded.kind(), kind);
    }
    for kind in [
        BusinessPayloadKind::Query,
        BusinessPayloadKind::Capabilities,
        BusinessPayloadKind::DeviceStatus,
        BusinessPayloadKind::ClockSample,
        BusinessPayloadKind::RebootPrepare,
        BusinessPayloadKind::RebootTicket,
        BusinessPayloadKind::RebootExecute,
        BusinessPayloadKind::TaskQuery,
        BusinessPayloadKind::TaskRecord,
    ] {
        assert!(seen.contains(&kind), "缺少 {kind:?} 的 round-trip 覆盖");
    }
}

#[test]
fn encoding_is_deterministic_and_canonical() {
    for message in fixtures() {
        let kind = message.kind();
        // 同一 DTO 编码两次字节相等
        assert_eq!(enc(&message), enc(&message), "{kind:?} 编码不确定");
        // canonical：解码后再编码，字节完全相同
        let bytes = enc(&message);
        let decoded = dec(kind, &bytes).expect("canonical 字节必须可解码");
        assert_eq!(enc(&decoded), bytes, "{kind:?} 重新编码不等于原字节");
    }
}

#[test]
fn empty_repeated_round_trips_as_absent() {
    let message = BusinessPayload::Capabilities(capabilities(&[]));
    let bytes = enc(&message);
    // field 4 (supported_actions) tag = 0x22：空 repeated 不得出现
    assert!(!bytes.contains(&0x22), "{bytes:02x?}");
    let decoded = dec(BusinessPayloadKind::Capabilities, &bytes).unwrap();
    assert_eq!(decoded, message);
    assert_eq!(hex(&enc(&decoded)), hex(&bytes));
}

#[test]
fn optional_metric_value_absent_is_none_not_zero() {
    let mut status = device_status();
    status.metrics = vec![metric("cpu.usage_pct", None, "percent", "unavailable")];
    let bytes = enc(&BusinessPayload::DeviceStatus(status.clone()));
    // optional double 缺省 → 不写 Metric field 2（tag 0x11）
    assert!(!bytes.contains(&0x11), "{bytes:02x?}");
    let decoded = dec(BusinessPayloadKind::DeviceStatus, &bytes).unwrap();
    match &decoded {
        BusinessPayload::DeviceStatus(s) => assert_eq!(s.metrics[0].value, None),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(decoded, BusinessPayload::DeviceStatus(status));

    // optional double 出现时必须是 fixed64（wire 1，tag = 0x11）
    assert!(enc(&BusinessPayload::DeviceStatus(device_status())).contains(&0x11));
}

/// 账本决议②：`evidence_kind` 在 wire 上是非 optional string，`None` 与 `Some("")`
/// 都写空串（即按 proto3 缺省省略字段），解码统一归一为 `None`。
#[test]
fn evidence_kind_empty_string_normalizes_to_none() {
    let message = BusinessPayload::TaskRecord(TaskRecord {
        evidence_kind: Some(String::new()),
        ..record()
    });
    let bytes = enc(&message);
    let decoded = dec(BusinessPayloadKind::TaskRecord, &bytes).unwrap();
    match &decoded {
        BusinessPayload::TaskRecord(r) => assert_eq!(r.evidence_kind, None),
        other => panic!("unexpected {other:?}"),
    }
    // 归一后重新编码字节不变（canonical 仍成立）
    assert_eq!(enc(&decoded), bytes);
}

/// 人工计算的结构向量：`Capabilities` 覆盖 uint32 多字节 varint、bool、repeated string。
/// 字段号/ wire type/ 分组均为手写（长度只引用已知值，避免自我证实）。
#[test]
fn capabilities_matches_hand_computed_structural_vector() {
    assert_eq!(BOOT.len(), 36);
    assert_eq!(EPOCH.len(), 36);
    let caps = Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_version: "1.4.2".to_string(),
        supported_actions: vec!["device.status.get".to_string()],
        durable_task_journal: true,
        max_status_bytes: 65_536,
        journal_epoch: EPOCH.to_string(),
    };
    let mut expected = vec![0x08, 0x01]; // 1 schema_version (varint) = 1
    expected.extend_from_slice(&[0x12, 36]); // 2 boot_id (string, len 36)
    expected.extend_from_slice(BOOT.as_bytes());
    expected.extend_from_slice(&[0x1a, 0x05]); // 3 agent_version (string, len 5)
    expected.extend_from_slice(b"1.4.2");
    expected.extend_from_slice(&[0x22, 17]); // 4 supported_actions (repeated string, len 17)
    expected.extend_from_slice(b"device.status.get");
    expected.extend_from_slice(&[0x28, 0x01]); // 5 durable_task_journal (bool) = true
    expected.extend_from_slice(&[0x30, 0x80, 0x80, 0x04]); // 6 max_status_bytes = 65536
    expected.extend_from_slice(&[0x3a, 36]); // 7 journal_epoch (string, len 36)
    expected.extend_from_slice(EPOCH.as_bytes());

    let bytes = enc(&BusinessPayload::Capabilities(caps.clone()));
    assert_eq!(hex(&bytes), hex(&expected));
    assert_eq!(
        dec(BusinessPayloadKind::Capabilities, &bytes).unwrap(),
        BusinessPayload::Capabilities(caps)
    );
}

/// 人工计算的结构向量：`DeviceStatus` 覆盖嵌套 message 的长度前缀、
/// `optional double` 的 fixed64 小端、负 int64、以及 proto3 缺省字段不写。
#[test]
fn device_status_matches_hand_computed_nested_vector() {
    assert_eq!(AGENT_EPOCH.len(), 36);
    assert_eq!(CLOCK_EPOCH.len(), 36);
    let status = DeviceStatus {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_epoch: AGENT_EPOCH.to_string(),
        sample_seq: 7,
        sampled_unix_ms: -1,
        sampled_monotonic_ns: 0, // proto3 缺省 → 不写 field 6
        clock_epoch: CLOCK_EPOCH.to_string(),
        metrics: vec![metric("cpu.usage_pct", Some(-2.5), "percent", "ok")],
        clock_quality: "observed".to_string(),
    };
    // 内层 Metric：key(2+13) + optional double value(1+8) + unit(2+7) + quality(2+2) = 37B
    let mut inner = vec![0x0a, 13];
    inner.extend_from_slice(b"cpu.usage_pct");
    inner.extend_from_slice(&[0x11]); // optional double → wire 1
    inner.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0xc0]); // -2.5 LE
    inner.extend_from_slice(&[0x1a, 0x07]);
    inner.extend_from_slice(b"percent");
    inner.extend_from_slice(&[0x22, 0x02]);
    inner.extend_from_slice(b"ok");
    assert_eq!(inner.len(), 37);

    let mut expected = vec![0x08, 0x01]; // 1 schema_version
    expected.extend_from_slice(&[0x12, 36]); // 2 boot_id
    expected.extend_from_slice(BOOT.as_bytes());
    expected.extend_from_slice(&[0x1a, 36]); // 3 agent_epoch
    expected.extend_from_slice(AGENT_EPOCH.as_bytes());
    expected.extend_from_slice(&[0x20, 0x07]); // 4 sample_seq = 7
    expected.extend_from_slice(&[0x28]); // 5 sampled_unix_ms = -1
    expected.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]);
    expected.extend_from_slice(&[0x3a, 36]); // 7 clock_epoch（6 缺省不写）
    expected.extend_from_slice(CLOCK_EPOCH.as_bytes());
    expected.push(0x42); // 8 metrics（嵌套 message）
    expected.push(inner.len() as u8);
    expected.extend_from_slice(&inner);
    expected.extend_from_slice(&[0x4a, 0x08]); // 9 clock_quality
    expected.extend_from_slice(b"observed");

    let bytes = enc(&BusinessPayload::DeviceStatus(status.clone()));
    assert_eq!(hex(&bytes), hex(&expected));
    assert_eq!(
        dec(BusinessPayloadKind::DeviceStatus, &bytes).unwrap(),
        BusinessPayload::DeviceStatus(status)
    );
}

/// presence 语义：`optional double` 的 `Some(0.0)` 必须写出来、解回 `Some(0.0)`，
/// 绝不能被 proto3 缺省规则吃掉。
#[test]
fn optional_double_zero_is_still_written() {
    let mut status = device_status();
    status.metrics = vec![metric("cpu.usage_pct", Some(0.0), "percent", "ok")];
    let bytes = enc(&BusinessPayload::DeviceStatus(status.clone()));
    assert!(
        bytes.contains(&0x11),
        "Some(0.0) 也必须写 fixed64：{bytes:02x?}"
    );
    let decoded = dec(BusinessPayloadKind::DeviceStatus, &bytes).unwrap();
    match &decoded {
        BusinessPayload::DeviceStatus(s) => assert_eq!(s.metrics[0].value, Some(0.0)),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(decoded, BusinessPayload::DeviceStatus(status));
}

// ====================================================================
// 严格解码负例
// ====================================================================

const QUERY_ONE: [u8; 2] = [0x08, 0x01];

#[test]
fn rejects_unknown_field_number() {
    // field 7 (wire 0) 不在 Query schema 内
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x01, 0x38, 0x01],
        CodecError::UnknownField(7),
    );
    // 大字段号同样拒绝，不做 unknown-field 保留
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x01, 0xf8, 0xff, 0xff, 0xff, 0x0f, 0x01],
        CodecError::UnknownField(0x1fff_ffff),
    );
}

#[test]
fn rejects_duplicate_singular_and_optional_field() {
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x01, 0x08, 0x01],
        CodecError::DuplicateField(1),
    );
    // TaskRecord field 7 = optional string observed_boot_id，出现两次即拒
    expect_err(
        BusinessPayloadKind::TaskRecord,
        &[0x08, 0x01, 0x3a, 0x00, 0x3a, 0x00],
        CodecError::DuplicateField(7),
    );
}

#[test]
fn allows_repeated_field_more_than_once() {
    let caps = capabilities(&["device.status.get", "device.task.get"]);
    let bytes = enc(&BusinessPayload::Capabilities(caps.clone()));
    assert_eq!(
        dec(BusinessPayloadKind::Capabilities, &bytes).unwrap(),
        BusinessPayload::Capabilities(caps)
    );
}

#[test]
fn rejects_truncated_varint_length_and_payload() {
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x81],
        CodecError::Malformed("truncated"),
    );
    expect_err(
        BusinessPayloadKind::TaskQuery,
        &[0x08, 0x01, 0x12],
        CodecError::Malformed("truncated"),
    );
    expect_err(
        BusinessPayloadKind::TaskQuery,
        &[0x08, 0x01, 0x12, 0x10, 0x41],
        CodecError::Malformed("length exceeds remaining bytes"),
    );
    expect_err(
        BusinessPayloadKind::ClockSample,
        &[],
        CodecError::MissingField("schema_version"),
    );
}

#[test]
fn rejects_non_canonical_varint_and_overflow() {
    // 0x81 0x00 解出 1，但不是最短编码
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x81, 0x00],
        CodecError::Malformed("non-canonical varint"),
    );
    // 11 组 continuation：超过 10 字节上限
    expect_err(
        BusinessPayloadKind::Query,
        &[
            0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
        ],
        CodecError::Malformed("varint too long"),
    );
    // 第 10 字节贡献 > 1 位：u64 溢出
    expect_err(
        BusinessPayloadKind::Query,
        &[
            0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02,
        ],
        CodecError::Malformed("varint overflows u64"),
    );
    // uint32 溢出：u64::MAX 的最短编码
    let mut bytes = vec![0x08];
    bytes.extend_from_slice(&[0xff; 9]);
    bytes.push(0x01);
    expect_err(
        BusinessPayloadKind::Query,
        &bytes,
        CodecError::Malformed("varint out of range for uint32"),
    );
    // 最短编码的正例：16384 中间字节载荷为 0 但合法（0x80 0x80 0x01）
    let mut bytes = vec![0x08];
    bytes.extend_from_slice(&[0x80, 0x80, 0x01]);
    assert_eq!(
        BusinessCodec.decode(&witness(), BusinessPayloadKind::Query, &bytes),
        Err(CodecError::UnsupportedSchemaVersion(16_384))
    );
}

/// 字段号 > `u32::MAX` 属**字节流越界**，不得谎报成 `UnknownField(0)`
/// （那等于伪造出「字段号 0」这个本应另作拒绝的取值）。
#[test]
fn rejects_field_number_beyond_u32_max() {
    // tag varint = (2^32 << 3) | 0 = 2^35：字段号正好越过 u32::MAX
    expect_err(
        BusinessPayloadKind::Query,
        &[0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        CodecError::Malformed("field number out of range"),
    );
    // 10 字节 tag：字段号 2^60（varint 本身合法、wire type 0）
    expect_err(
        BusinessPayloadKind::Query,
        &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        CodecError::Malformed("field number out of range"),
    );
    // schema 内字段号之后的越界 tag 同样拒绝（不是 UnknownField(0)）
    expect_err(
        BusinessPayloadKind::TaskQuery,
        &[0x08, 0x01, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        CodecError::Malformed("field number out of range"),
    );
    // 对照：字段号恰为 u32::MAX 仍在范围内 → 按 UnknownField(u32::MAX) 拒绝
    expect_err(
        BusinessPayloadKind::Query,
        &[0xf8, 0xff, 0xff, 0xff, 0x7f],
        CodecError::UnknownField(u32::MAX),
    );
}

#[test]
fn rejects_wrong_wire_type_and_reserved_wire_types() {
    // Query field 1 期望 wire 0，给了 wire 2
    expect_err(
        BusinessPayloadKind::Query,
        &[0x0a, 0x00],
        CodecError::Malformed("unexpected wire type"),
    );
    // Metric field 2 是 optional double（wire 1），给了 wire 0
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &[0x08, 0x01, 0x42, 0x02, 0x10, 0x01],
        CodecError::Malformed("unexpected wire type"),
    );
    // wire 5（fixed32）与 wire 3（group）属保留/未用，直接拒
    expect_err(
        BusinessPayloadKind::Query,
        &[0x0d, 0x01, 0x00, 0x00, 0x00],
        CodecError::Malformed("unsupported wire type"),
    );
    expect_err(
        BusinessPayloadKind::Query,
        &[0x0b, 0x01],
        CodecError::Malformed("unsupported wire type"),
    );
    // field number 0
    expect_err(
        BusinessPayloadKind::Query,
        &[0x00, 0x01],
        CodecError::Malformed("zero field number"),
    );
}

#[test]
fn rejects_invalid_utf8_but_bytes_field_is_verbatim() {
    expect_err(
        BusinessPayloadKind::TaskQuery,
        &[0x08, 0x01, 0x12, 0x02, 0xff, 0xfe],
        CodecError::InvalidUtf8,
    );
    // bytes 字段不做 UTF-8 校验：0xff×32 原样进入 execution_token
    let mut bytes = vec![0x08, 0x01, 0x12, 0x24];
    bytes.extend_from_slice(SUB_TASK.as_bytes());
    bytes.push(0x1a);
    bytes.push(0x24);
    bytes.extend_from_slice(BOOT.as_bytes());
    bytes.push(0x22);
    bytes.push(32);
    bytes.extend_from_slice(&[0xffu8; 32]);
    let decoded = dec(BusinessPayloadKind::RebootExecute, &bytes).unwrap();
    match &decoded {
        BusinessPayload::RebootExecute(e) => assert_eq!(e.execution_token, [0xff; 32]),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn rejects_trailing_bytes_at_top_level() {
    // 只有 tag、缺 payload
    let mut bytes = QUERY_ONE.to_vec();
    bytes.push(0x08);
    expect_err(
        BusinessPayloadKind::Query,
        &bytes,
        CodecError::Malformed("truncated"),
    );
    // 尾随一个 schema 外的完整字段
    let mut bytes = QUERY_ONE.to_vec();
    bytes.extend_from_slice(&[0x38, 0x02]);
    expect_err(
        BusinessPayloadKind::Query,
        &bytes,
        CodecError::UnknownField(7),
    );
    // 尾随一个 schema 内的重复字段
    let mut bytes = QUERY_ONE.to_vec();
    bytes.extend_from_slice(&[0x08, 0x01]);
    expect_err(
        BusinessPayloadKind::Query,
        &bytes,
        CodecError::DuplicateField(1),
    );
}

#[test]
fn rejects_oversized_message() {
    let oversized = vec![0x08u8; MAX_ENVELOPE_BYTES + 1];
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &oversized,
        CodecError::LengthLimit,
    );
    // 恰好等于上限时不因长度上限被拒（此处只会因 schema 被拒）
    let at_limit = vec![0x08u8; MAX_ENVELOPE_BYTES];
    assert_ne!(
        dec(BusinessPayloadKind::DeviceStatus, &at_limit),
        Err(CodecError::LengthLimit)
    );
}

#[test]
fn rejects_truncated_nested_metric() {
    // DeviceStatus field 8 声称 32 字节，实际只剩 3 字节
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &[0x08, 0x01, 0x42, 0x20, 0x0a, 0x01, 0x61],
        CodecError::Malformed("length exceeds remaining bytes"),
    );
    // 嵌套 Metric 内部截断（key 声称 5 字节，只剩 1 字节）
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &[0x08, 0x01, 0x42, 0x05, 0x0a, 0x05, 0x61],
        CodecError::Malformed("length exceeds remaining bytes"),
    );
    // 嵌套 Metric 里的未知字段同样拒绝
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &[0x08, 0x01, 0x42, 0x02, 0x38, 0x01],
        CodecError::UnknownField(7),
    );
}

#[test]
fn rejects_too_many_metrics() {
    assert_eq!(MAX_METRICS, 128);
    // 128 条完整合法 Metric 必须整体解码成功：上限不是靠截断或字段顺序实现的
    let full = BusinessPayload::DeviceStatus(status_with_metrics(MAX_METRICS));
    let full_bytes = enc(&full);
    assert_eq!(
        dec(BusinessPayloadKind::DeviceStatus, &full_bytes).unwrap(),
        full
    );
    assert!(
        full_bytes.len() < MAX_ENVELOPE_BYTES / 8,
        "样本必须远小于 512 KiB，才能证明拒绝原因不是长度：{} B",
        full_bytes.len()
    );

    // 第 129 项是**完整且合法**的 Metric（不是截断字节）→ 独立的数量错误
    let mut bytes = full_bytes.clone();
    bytes.extend_from_slice(&metric_entry("metric.9999"));
    let got = dec(BusinessPayloadKind::DeviceStatus, &bytes);
    assert_eq!(got, Err(CodecError::TooManyMetrics));
    // 数量越界绝不得再借用只表示 512 KiB 整包上限的 `LengthLimit`
    assert_ne!(got, Err(CodecError::LengthLimit));
}

/// 证明上例的「第 129 条」确实是完整合法条目：把它换成第 128 条时，
/// 同一段字节必须能整体解码成功。
#[test]
fn hand_written_metric_entry_is_complete_and_valid() {
    let mut bytes = enc(&BusinessPayload::DeviceStatus(status_with_metrics(
        MAX_METRICS - 1,
    )));
    bytes.extend_from_slice(&metric_entry("metric.9999"));
    let decoded = dec(BusinessPayloadKind::DeviceStatus, &bytes).unwrap();
    match &decoded {
        BusinessPayload::DeviceStatus(status) => {
            assert_eq!(status.metrics.len(), MAX_METRICS);
            assert_eq!(status.metrics[MAX_METRICS - 1].key, "metric.9999");
            assert_eq!(status.metrics[MAX_METRICS - 1].value, Some(0.0));
        }
        other => panic!("unexpected {other:?}"),
    }
    // 再加一条同样完整的条目即越过数量上限
    bytes.extend_from_slice(&metric_entry("metric.8888"));
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &bytes,
        CodecError::TooManyMetrics,
    );
}

/// `supported_actions` 的数量/长度守卫：必须在分配 `String` 之前生效，
/// 且不靠截断、不靠词表校验的顺序。
#[test]
fn rejects_too_many_and_oversized_supported_actions() {
    assert_eq!(SUPPORTED_ACTIONS.len(), 6);
    // 正例：恰好 6 项（白名单全集）必须解码成功，证明守卫不是差一错误
    let six = capabilities(&SUPPORTED_ACTIONS);
    let six_bytes = enc(&BusinessPayload::Capabilities(six.clone()));
    assert_eq!(
        dec(BusinessPayloadKind::Capabilities, &six_bytes).unwrap(),
        BusinessPayload::Capabilities(six)
    );

    // 编码端对称：>6 项不得产出解码端必然拒绝的字节
    let mut seven: Vec<&str> = SUPPORTED_ACTIONS.to_vec();
    seven.push(SUPPORTED_ACTIONS[0]);
    assert_eq!(
        encode_business(
            &BusinessCodec,
            Some(&witness()),
            &BusinessPayload::Capabilities(capabilities(&seven)),
        ),
        Err(CodecError::InvalidDto("too_many_actions"))
    );

    // 解码端：在第 6 项之后手工追加**完整且取值合法**的第 7 项（重复项），
    // 字节完全合法也不截断，守卫必须在读第 7 项之前命中
    let mut bytes = six_bytes.clone();
    bytes.extend_from_slice(&[0x22, SUPPORTED_ACTIONS[0].len() as u8]);
    bytes.extend_from_slice(SUPPORTED_ACTIONS[0].as_bytes());
    expect_err(
        BusinessPayloadKind::Capabilities,
        &bytes,
        CodecError::InvalidDto("too_many_actions"),
    );

    // 放大防护：追加 4096 个完整合法条目，仍是同一个固定错误，
    // 守卫在第 7 项即返回，堆上最多只留下 6 个 `String`
    let mut bytes = six_bytes.clone();
    for _ in 0..4096 {
        bytes.extend_from_slice(&[0x22, SUPPORTED_ACTIONS[0].len() as u8]);
        bytes.extend_from_slice(SUPPORTED_ACTIONS[0].as_bytes());
    }
    expect_err(
        BusinessPayloadKind::Capabilities,
        &bytes,
        CodecError::InvalidDto("too_many_actions"),
    );

    // 单元素长度守卫：长度越过 MAX_ACTION_BYTES 的元素先于 UTF-8 校验与分配被拒
    let mut bytes = vec![0x08, 0x01, 0x22, (MAX_ACTION_BYTES + 1) as u8];
    bytes.extend_from_slice(&[b'a'; 65]);
    expect_err(
        BusinessPayloadKind::Capabilities,
        &bytes,
        CodecError::InvalidDto("action_too_long"),
    );
    // 恰好 MAX_ACTION_BYTES 长度不因长度守卫被拒（此处只会因词表/缺字段被拒）
    let mut bytes = vec![0x08, 0x01, 0x22, MAX_ACTION_BYTES as u8];
    bytes.extend_from_slice(&[b'a'; 64]);
    assert_ne!(
        dec(BusinessPayloadKind::Capabilities, &bytes),
        Err(CodecError::InvalidDto("action_too_long"))
    );
}

#[test]
fn rejects_missing_schema_version() {
    let mut bytes = vec![0x12, 0x24];
    bytes.extend_from_slice(SUB_TASK.as_bytes());
    expect_err(
        BusinessPayloadKind::TaskQuery,
        &bytes,
        CodecError::MissingField("schema_version"),
    );
    expect_err(
        BusinessPayloadKind::Query,
        &[],
        CodecError::MissingField("schema_version"),
    );
}

#[test]
fn rejects_execution_token_length_mismatch() {
    let mut bytes = vec![0x08, 0x01, 0x12, 0x24];
    bytes.extend_from_slice(SUB_TASK.as_bytes());
    bytes.extend_from_slice(&[0x1a, 0x24]);
    bytes.extend_from_slice(BOOT.as_bytes());
    bytes.push(0x22);
    bytes.push(31);
    bytes.extend_from_slice(&[0x01u8; 31]);
    expect_err(
        BusinessPayloadKind::RebootTicket,
        &bytes,
        CodecError::Malformed("execution_token length"),
    );
}

#[test]
fn rejects_invalid_bool_and_dto_violations() {
    expect_err(
        BusinessPayloadKind::Capabilities,
        &[0x08, 0x01, 0x28, 0x02],
        CodecError::Malformed("invalid bool"),
    );
    // schema_version=2：wire 合法，版本不受支持是**版本**错误（可协商），
    // 不得混入 `InvalidDto`，更不得伪装成「codec 未获批」
    expect_err(
        BusinessPayloadKind::Query,
        &[0x08, 0x02],
        CodecError::UnsupportedSchemaVersion(2),
    );
    // RebootPrepare 的 sub_task_id 非 UUID：DTO 拒
    expect_err(
        BusinessPayloadKind::RebootPrepare,
        &[0x08, 0x01, 0x12, 0x03, b'a', b'b', b'c'],
        CodecError::InvalidDto("invalid_uuid"),
    );
    // 重复 metric key：DTO 拒（先编两条不同 key，再等长改成同名）
    let mut status = device_status();
    status.metrics = vec![
        metric("cpu.usage_pct", Some(1.0), "percent", "ok"),
        metric("cpu.usage_pcg", Some(2.0), "percent", "ok"),
    ];
    let mut bytes = enc(&BusinessPayload::DeviceStatus(status));
    patch(&mut bytes, b"cpu.usage_pcg", b"cpu.usage_pct");
    expect_err(
        BusinessPayloadKind::DeviceStatus,
        &bytes,
        CodecError::InvalidDto("duplicate_metric_key"),
    );
    // optional string 出现但为空（presence 有、值为空）：DTO 拒
    expect_err(
        BusinessPayloadKind::TaskRecord,
        &[0x08, 0x01, 0x3a, 0x00],
        CodecError::InvalidDto("empty_field"),
    );
    // state 非词表：DTO 拒
    let mut bytes = enc(&BusinessPayload::TaskRecord(record()));
    patch(&mut bytes, b"succeeded", b"succeEded");
    expect_err(
        BusinessPayloadKind::TaskRecord,
        &bytes,
        CodecError::InvalidDto("invalid_literal"),
    );
}

/// 去敏：任何错误文案都不得带出 payload 内容或字段取值。
#[test]
fn error_messages_never_echo_payload_content() {
    let marker = "TOPSECRET";
    let mut status = device_status();
    status.metrics = vec![
        metric(format!("{marker}-1").as_str(), Some(1.0), "percent", "ok"),
        metric(format!("{marker}-2").as_str(), Some(2.0), "percent", "ok"),
    ];
    let mut bytes = enc(&BusinessPayload::DeviceStatus(status));
    patch(&mut bytes, b"TOPSECRET-2", b"TOPSECRET-1");
    let err = dec(BusinessPayloadKind::DeviceStatus, &bytes).unwrap_err();
    assert_eq!(err, CodecError::InvalidDto("duplicate_metric_key"));
    assert!(!err.to_string().contains(marker), "Display 泄露: {err}");
    assert!(!format!("{err:?}").contains(marker), "Debug 泄露: {err:?}");

    // 非词表取值同样不回显
    let mut bytes = enc(&BusinessPayload::TaskRecord(record()));
    patch(&mut bytes, b"succeeded", b"succeEded");
    let err = dec(BusinessPayloadKind::TaskRecord, &bytes).unwrap_err();
    assert_eq!(err, CodecError::InvalidDto("invalid_literal"));
    assert!(!err.to_string().contains(marker), "Display 泄露: {err}");
    assert!(!format!("{err:?}").contains(marker), "Debug 泄露: {err:?}");

    // 非法 UTF-8 不回显字节
    let err = dec(
        BusinessPayloadKind::TaskQuery,
        &[0x08, 0x01, 0x12, 0x01, 0xff],
    )
    .unwrap_err();
    assert_eq!(err, CodecError::InvalidUtf8);
    assert!(!err.to_string().contains("ff"), "Display 泄露: {err}");
}

// ====================================================================
// codec 审批边界（fail-closed 语义不变）
// ====================================================================

#[test]
fn witness_less_paths_stay_fail_closed() {
    let message = BusinessPayload::Query(query());
    let bytes = enc(&message);

    assert_eq!(
        encode_business(&BusinessCodec, None, &message),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
    assert_eq!(
        decode_business(&BusinessCodec, None, BusinessPayloadKind::Query, &bytes),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
    assert_eq!(
        encode_business(&NoCodec, Some(&witness()), &message),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
    assert_eq!(
        decode_business(
            &NoCodec,
            Some(&witness()),
            BusinessPayloadKind::Query,
            &bytes
        ),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
    // 原始字节不得被当作业务消息接受（连触碰 bytes 都不发生）
    assert_eq!(
        decode_business(
            &BusinessCodec,
            None,
            BusinessPayloadKind::Query,
            b"\xff\xff\xff"
        ),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
}

#[test]
fn approved_witness_round_trips_every_variant() {
    for message in fixtures() {
        let kind = message.kind();
        let bytes = encode_business(&BusinessCodec, Some(&witness()), &message).unwrap();
        let decoded = decode_business(&BusinessCodec, Some(&witness()), kind, &bytes).unwrap();
        assert_eq!(decoded, message);
    }
}

#[test]
fn invalid_dto_cannot_be_encoded() {
    let mut bad = capabilities(&[]);
    bad.agent_version = String::new();
    // DTO 违规是这条消息自身的契约缺陷 → InvalidDto，不再借用门禁语义
    assert_eq!(
        encode_business(
            &BusinessCodec,
            Some(&witness()),
            &BusinessPayload::Capabilities(bad)
        ),
        Err(CodecError::InvalidDto("empty_field"))
    );
    // 直接调用 trait 方法同样被 DTO 契约拦住，且版本问题单独成类
    assert_eq!(
        BusinessCodec.encode(
            &witness(),
            &BusinessPayload::Query(Query { schema_version: 2 })
        ),
        Err(CodecError::UnsupportedSchemaVersion(2))
    );
    // `CodecUnapproved` 只剩「没有见证」这一种语义
    assert_eq!(
        encode_business(&BusinessCodec, None, &BusinessPayload::Query(query())),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
}

#[test]
fn codec_is_kind_specific() {
    // 同一段 canonical Query 字节不能按 TaskQuery 解释（sub_task_id 缺省 → DTO 拒）
    let bytes = enc(&BusinessPayload::Query(query()));
    assert_eq!(
        dec(BusinessPayloadKind::TaskQuery, &bytes),
        Err(CodecError::InvalidDto("empty_field"))
    );
    assert_eq!(
        dec(BusinessPayloadKind::DeviceStatus, &bytes),
        Err(CodecError::InvalidDto("empty_field"))
    );
    assert_eq!(
        dec(BusinessPayloadKind::TaskRecord, &bytes),
        Err(CodecError::InvalidDto("empty_field"))
    );
}

/// 跨变体混用字节必须被拒。`BusinessCodec` 按调用方声明的 `expected` 逐字段
/// 严格解析，所以「把 A 变体的字节按 B 变体解释」总在**结构解码阶段**就撞上
/// schema 外的字段号，绝不会被静默改写成另一种形状的载荷。
#[test]
fn rejects_cross_variant_bytes() {
    // 用 Capabilities 的 canonical 字节按 Query 解码：field 2 不在 Query schema 内
    let caps = enc(&BusinessPayload::Capabilities(capabilities(&[
        "device.status.get",
    ])));
    expect_err(
        BusinessPayloadKind::Query,
        &caps,
        CodecError::UnknownField(2),
    );
    // 反向：Query 的字节按 RebootTicket 解码，只能得到 DTO 缺字段的拒绝
    let bytes = enc(&BusinessPayload::Query(query()));
    expect_err(
        BusinessPayloadKind::RebootTicket,
        &bytes,
        CodecError::InvalidDto("empty_field"),
    );
    // 「解码结果 kind 与 expected 不符」这一条守卫由 codec 之外的
    // `decode_business` 兜底，返回 `Malformed("payload kind mismatch")`：
    // 它只能由「返回错误 kind 的 codec」触发，而 `Codec` 是 sealed trait，
    // 集成测试无法构造，故断言见 `wire::codec_error_classification_tests`
    // （payload_kind_mismatch_is_malformed）。
}
