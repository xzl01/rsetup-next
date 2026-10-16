use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, BusinessPayloadKind, Capabilities,
    ClockSample, CodecError, DeviceStatus, DtoError, Metric, NoCodec, RebootExecute, RebootTicket,
    TaskQuery, TaskRecord, canonical_reboot_payload_hash, decode_business, encode_business,
    is_canonical_uuid_v4, validate_dto,
};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

fn canonical_record() -> TaskRecord {
    TaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        journal_epoch: EPOCH.to_string(),
        record_version: 1,
        state: "accepted".to_string(),
        pre_boot_id: BOOT.to_string(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: Some(1_700_000_000_000),
        evidence_kind: None,
    }
}

#[test]
fn uuid_validator_accepts_only_canonical_lowercase_v4() {
    assert!(is_canonical_uuid_v4(EPOCH));
    assert!(!is_canonical_uuid_v4(
        "3F2504E0-4F89-41D3-9A0C-0305E82C3301"
    )); // 大写
    assert!(!is_canonical_uuid_v4("3f2504e04f8941d39a0c0305e82c3301")); // 无连字符
    assert!(!is_canonical_uuid_v4(
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301"
    )); // 非 v4
    assert!(!is_canonical_uuid_v4(""));
}

#[test]
fn task_record_rejects_unknown_literals_and_bad_versions() {
    let mut record = canonical_record();
    assert!(validate_dto(&record).is_ok());
    assert!(record.validate().is_ok());

    record.state = "finished".to_string();
    assert_eq!(
        record.validate(),
        Err(DtoError::InvalidLiteral {
            field: "state",
            value: "finished".to_string()
        })
    );

    let mut record = canonical_record();
    record.reason_code = Some("SOMETHING_ELSE".to_string());
    assert_eq!(
        record.validate(),
        Err(DtoError::InvalidLiteral {
            field: "reason_code",
            value: "SOMETHING_ELSE".to_string()
        })
    );

    let mut record = canonical_record();
    record.journal_epoch = "not-a-uuid".to_string();
    assert_eq!(
        record.validate(),
        Err(DtoError::InvalidUuid("journal_epoch"))
    );

    // evidence 是完成证据的必要组成：没有 observed_boot_id 就不允许自称 boot_transition_observed
    let mut record = canonical_record();
    record.state = "succeeded".to_string();
    record.evidence_kind = Some("boot_transition_observed".to_string());
    assert_eq!(
        record.validate(),
        Err(DtoError::EmptyField("observed_boot_id"))
    );

    let mut record = canonical_record();
    record.schema_version = 0;
    assert_eq!(record.validate(), Err(DtoError::UnsupportedVersion(0)));

    let mut record = canonical_record();
    record.record_version = 0;
    assert_eq!(
        record.validate(),
        Err(DtoError::ValueOutOfContract("record_version"))
    );

    // evidence_kind wire 兼容映射契约（None/空串双向一致、归一化后词表校验）
    let write_map = |opt: Option<&str>| opt.unwrap_or("").to_string();
    let read_map = |s: &str| {
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    };
    assert_eq!(write_map(None), "");
    assert_eq!(read_map(""), None);
    assert_eq!(
        write_map(Some("boot_transition_observed")),
        "boot_transition_observed"
    );
    assert_eq!(
        read_map(&write_map(Some("boot_transition_observed"))),
        Some("boot_transition_observed".to_string())
    );
    let mut record = canonical_record();
    record.evidence_kind = read_map("UNKNOWN_EVIDENCE");
    assert_eq!(
        record.validate(),
        Err(DtoError::InvalidLiteral {
            field: "evidence_kind",
            value: "UNKNOWN_EVIDENCE".to_string()
        })
    );
}

#[test]
fn device_status_enforces_metric_contract() {
    let mut status = DeviceStatus {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_epoch: AGENT_EPOCH.to_string(),
        sample_seq: 1,
        sampled_unix_ms: 1_700_000_000_000,
        sampled_monotonic_ns: 123_456,
        clock_epoch: CLOCK_EPOCH.to_string(),
        metrics: vec![Metric {
            key: "cpu.usage_pct".to_string(),
            value: Some(37.0),
            unit: "percent".to_string(),
            quality: "ok".to_string(),
        }],
        clock_quality: "observed".to_string(),
    };
    assert!(status.validate().is_ok());

    // is_finite 约束：NaN 与 Inf 拒绝
    let mut nan_status = status.clone();
    nan_status.metrics[0].value = Some(f64::NAN);
    assert_eq!(
        nan_status.validate(),
        Err(DtoError::ValueOutOfContract("metric.value_not_finite"))
    );

    let mut inf_status = status.clone();
    inf_status.metrics[0].value = Some(f64::INFINITY);
    assert_eq!(
        inf_status.validate(),
        Err(DtoError::ValueOutOfContract("metric.value_not_finite"))
    );

    // bytes 单位 double 必须为精确非负整数且 <= 2^53 - 1
    let mut bytes_status = status.clone();
    bytes_status.metrics[0].unit = "bytes".to_string();
    bytes_status.metrics[0].value = Some(9_007_199_254_740_991.0); // 2^53 - 1 (ok)
    assert!(bytes_status.validate().is_ok());

    let mut overflow_bytes = status.clone();
    overflow_bytes.metrics[0].unit = "bytes".to_string();
    overflow_bytes.metrics[0].value = Some(9_007_199_254_740_992.0); // 2^53 (超限拒绝)
    assert_eq!(
        overflow_bytes.validate(),
        Err(DtoError::ValueOutOfContract(
            "metric.bytes_exceeds_max_safe_integer"
        ))
    );

    // 非 ok 不得携带 value（避免错误显示为 0）
    status.metrics[0].quality = "unavailable".to_string();
    status.metrics[0].value = Some(37.0);
    assert_eq!(
        status.validate(),
        Err(DtoError::ValueOutOfContract(
            "metric.value_with_non_ok_quality"
        ))
    );

    // 重复 key 拒绝
    let mut duplicated = status.clone();
    duplicated.metrics[0].quality = "ok".to_string();
    duplicated.metrics[0].value = Some(37.0);
    duplicated.metrics.push(duplicated.metrics[0].clone());
    assert_eq!(
        duplicated.validate(),
        Err(DtoError::DuplicateMetricKey("cpu.usage_pct".to_string()))
    );

    // sample_seq 必须 > 0
    let mut zero_seq = status.clone();
    zero_seq.sample_seq = 0;
    assert_eq!(
        zero_seq.validate(),
        Err(DtoError::ValueOutOfContract("sample_seq"))
    );

    // 未知时钟质量拒绝
    let mut bad_quality = status.clone();
    bad_quality.clock_quality = "ntp_locked".to_string();
    assert!(matches!(
        bad_quality.validate(),
        Err(DtoError::InvalidLiteral {
            field: "clock_quality",
            ..
        })
    ));
}

#[test]
fn ticket_and_query_enforce_required_fields() {
    let ticket = RebootTicket {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        boot_id: BOOT.to_string(),
        execution_token: [7u8; 32],
        valid_for_ms: 30_000,
    };
    assert!(ticket.validate().is_ok());

    let mut zero_ttl = ticket.clone();
    zero_ttl.valid_for_ms = 0;
    assert_eq!(
        zero_ttl.validate(),
        Err(DtoError::ValueOutOfContract("valid_for_ms"))
    );

    let query = TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    };
    assert!(query.validate().is_ok());

    let empty = TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: "   ".to_string(),
    };
    assert_eq!(empty.validate(), Err(DtoError::EmptyField("sub_task_id")));
}

#[test]
fn capabilities_requires_durable_journal_epoch_uuid() {
    let caps = Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_version: "0.5.0".to_string(),
        supported_actions: vec!["device.reboot.prepare".to_string()],
        durable_task_journal: true,
        max_status_bytes: 65_536,
        journal_epoch: EPOCH.to_string(),
    };
    assert!(caps.validate().is_ok());

    let mut broken = caps.clone();
    broken.journal_epoch = String::new();
    assert_eq!(
        broken.validate(),
        Err(DtoError::EmptyField("journal_epoch"))
    );
}

#[test]
fn no_codec_means_raw_bytes_are_fail_closed() {
    let codec = NoCodec;
    let record = TaskRecord {
        state: "succeeded".to_string(),
        ..canonical_record()
    };
    let payload = BusinessPayload::TaskRecord(record);

    // 未提供批准见证（witness = None）：原始字节入口必须失败关闭
    assert_eq!(
        encode_business(&codec, None, &payload),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
    assert_eq!(
        decode_business(&codec, None, BusinessPayloadKind::TaskRecord, b"any-bytes"),
        Err(CodecError::CodecUnapproved("no approved business codec"))
    );
}

#[test]
fn clock_sample_quality_is_restricted_to_observed_or_unstable() {
    let sample = ClockSample {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        clock_epoch: CLOCK_EPOCH.to_string(),
        receive_unix_ms: 1,
        send_unix_ms: 2,
        receive_monotonic_ns: 10,
        send_monotonic_ns: 20,
        quality: "observed".to_string(),
    };
    assert!(sample.validate().is_ok());

    let mut unstable = sample.clone();
    unstable.quality = "clock_unstable".to_string();
    assert!(unstable.validate().is_ok());

    let mut invalid = sample;
    invalid.quality = "valid".to_string();
    assert!(matches!(
        invalid.validate(),
        Err(DtoError::InvalidLiteral {
            field: "quality",
            ..
        })
    ));
}

#[test]
fn canonical_reboot_payload_hash_is_semantic_not_ticket_bound() {
    // synthetic 输入：token 字节仅用于证明「票据一次性不属于载荷语义」
    let base = RebootExecute {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT.to_string(),
        execution_token: [7u8; 32],
    };
    // 正例：稳定重复哈希相同（纯函数、无 I/O、无时间源）
    let h1 = canonical_reboot_payload_hash(&base);
    let h2 = canonical_reboot_payload_hash(&base);
    assert_eq!(h1, h2, "稳定重复哈希必须相同");
    // 表：(用例名, 变更后请求, 与 h1 哈希相同?); token 变更是唯一「相同」用例
    let cases: [(&str, RebootExecute, bool); 4] = [
        (
            "相同语义 token 变更",
            {
                let mut r = base.clone();
                r.execution_token = [8u8; 32];
                r
            },
            true,
        ),
        (
            "schema_version 变更",
            {
                let mut r = base.clone();
                r.schema_version = 2;
                r
            },
            false,
        ),
        (
            "sub_task_id 变更",
            {
                let mut r = base.clone();
                r.sub_task_id = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d".to_string();
                r
            },
            false,
        ),
        (
            "expected_boot_id 变更",
            {
                let mut r = base.clone();
                r.expected_boot_id = "6f8b1c2d-0000-4000-8000-000000000002".to_string();
                r
            },
            false,
        ),
    ];
    for (name, request, same) in &cases {
        let h = canonical_reboot_payload_hash(request);
        if *same {
            assert_eq!(h1, h, "{name}: 同一语义重放不得因临时票据变化冲突");
        } else {
            assert_ne!(h1, h, "{name}: 语义字段变更必须哈希不同");
        }
    }
}
