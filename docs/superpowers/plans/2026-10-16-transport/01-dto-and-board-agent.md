### Task 1: 共享类型化业务 DTO 与无 codec 的 fail-closed 边界（WIRE-01 结构层）

**Files:**
- Modify: `crates/rsetup-protocol/src/wire.rs`（在既有信封定义之后追加；不删改既有 `TunnelPacket` / `WireError` / `validate_packet`）
- Modify: `crates/rsetup-protocol/src/lib.rs`（追加重导出）
- Test: `crates/rsetup-protocol/tests/wire_business_dto.rs`

**Interfaces:**
```rust
// ---- 约束常量（03 §2 字段约束）----
pub const BUSINESS_SCHEMA_VERSION: u32 = 1;
pub const MAX_METRICS: usize = 128;
pub const MAX_METRIC_KEY_BYTES: usize = 64;
pub const MAX_METRIC_UNIT_BYTES: usize = 16;
pub const MAX_ACTOR_VERSION_BYTES: usize = 64;
pub const MAX_REASON_CODE_BYTES: usize = 64;
pub const MAX_SUB_TASK_ID_BYTES: usize = 64;
pub const MAX_EXECUTION_TOKEN_BYTES: usize = 64;
pub const MAX_TICKET_TTL_MS: u32 = 30_000; // 03 §5 待审默认，G0 审阅后才可定稿

// ---- 稳定词表（拒绝未知取值）----
pub const TASK_STATES: [&str; 5] = ["accepted", "attempted", "succeeded", "failed", "unknown"];
pub const TASK_REASON_CODES: [&str; 12] = [
    "PERMISSION_DENIED", "DEVICE_OFFLINE", "CAPABILITY_UNAVAILABLE", "DEVICE_BUSY",
    "QUEUE_EXPIRED", "TIME_UNCERTAIN", "EXECUTION_REJECTED", "OS_ERROR",
    "RESULT_TIMEOUT", "JOURNAL_LOST", "PROTOCOL_MISMATCH", "USER_CANCELLED",
];
pub const EVIDENCE_KINDS: [&str; 1] = ["boot_transition_observed"];
pub const METRIC_QUALITIES: [&str; 3] = ["ok", "unavailable", "error"];
pub const CLOCK_QUALITIES: [&str; 2] = ["observed", "clock_unstable"];
// 入站动作白名单仅 6 项；device.task.result 属于仅板端→中控的 TYPE_EVENT，绝不纳入入站支持列表
pub const SUPPORTED_ACTIONS: [&str; 6] = [
    "device.capabilities.get", "device.status.get", "device.clock.get", "device.task.get",
    "device.reboot.prepare", "device.reboot.execute",
];

// ---- DTO（字段与 03 §2 protobuf schema 一一对应；Rust 侧承载，不预设字节格式）----
// Metric 与 DeviceStatus 包含浮点数，按 Rust 规范绝不派生 Eq
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub key: String,
    pub value: Option<f64>, // proto3 optional double；校验 is_finite 与 bytes 整数范围 <= 2^53-1
    pub unit: String,
    pub quality: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query { pub schema_version: u32 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub schema_version: u32,
    pub boot_id: String,
    pub agent_version: String,
    pub supported_actions: Vec<String>,
    pub durable_task_journal: bool,
    pub max_status_bytes: u32,
    pub journal_epoch: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceStatus {
    pub schema_version: u32,
    pub boot_id: String,
    pub agent_epoch: String,
    pub sample_seq: u64,
    pub sampled_unix_ms: i64,
    pub sampled_monotonic_ns: u64,
    pub clock_epoch: String,
    pub metrics: Vec<Metric>,
    pub clock_quality: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockSample {
    pub schema_version: u32,
    pub boot_id: String,
    pub clock_epoch: String,
    pub receive_unix_ms: i64,
    pub send_unix_ms: i64,
    pub receive_monotonic_ns: u64,
    pub send_monotonic_ns: u64,
    pub quality: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootPrepare {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub expected_boot_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootTicket {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub boot_id: String,
    pub execution_token: [u8; 32],
    pub valid_for_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootExecute {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub expected_boot_id: String,
    pub execution_token: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskQuery {
    pub schema_version: u32,
    pub sub_task_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub journal_epoch: String,
    pub record_version: u64,
    pub state: String,
    pub pre_boot_id: String,
    pub observed_boot_id: Option<String>,
    pub reason_code: Option<String>,
    pub updated_unix_ms: Option<i64>,
    pub evidence_kind: Option<String>,
}
```

#### `evidence_kind` wire↔Rust 唯一兼容映射（账本决议②）
- wire 恒为 proto3 非 optional `string`（不改 wire 定义、不改账本）；Rust `Option<String>` 仅作内存表示。
- 写侧 `None` → 空串、`Some(v)` → `v`；读侧空串 → `None`、非空 → `Some(v)`；双向唯一无第三路径；producer 未提供 evidence 时缺省落空串。
- 词表校验在归一后进行（归一为 `None` 时视为未提供跳过词表校验；按既有缺省口径，非空时校验 `v ∈ EVIDENCE_KINDS`）。

```rust
// ---- 统一校验入口 ----
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DtoError {
    #[error("unsupported business schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("empty required field: {0}")]
    EmptyField(&'static str),
    #[error("field too long: {field} = {len} > {max}")]
    FieldTooLong { field: &'static str, len: usize, max: usize },
    #[error("invalid canonical lowercase UUIDv4: {0}")]
    InvalidUuid(&'static str),
    #[error("invalid literal for {field}: {value}")]
    InvalidLiteral { field: &'static str, value: String },
    #[error("value out of contract: {0}")]
    ValueOutOfContract(&'static str),
    #[error("duplicate metric key: {0}")]
    DuplicateMetricKey(String),
}

pub trait BusinessMessage {
    fn schema_version(&self) -> u32;
    fn validate(&self) -> Result<(), DtoError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessPayloadKind {
    Query, Capabilities, DeviceStatus, ClockSample, RebootPrepare, RebootTicket, RebootExecute,
    TaskQuery, TaskRecord,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BusinessPayload {
    Query(Query), Capabilities(Capabilities), DeviceStatus(DeviceStatus), ClockSample(ClockSample),
    RebootPrepare(RebootPrepare), RebootTicket(RebootTicket), RebootExecute(RebootExecute),
    TaskQuery(TaskQuery), TaskRecord(TaskRecord),
}

impl BusinessPayload {
    pub fn kind(&self) -> BusinessPayloadKind;
    pub fn validate(&self) -> Result<(), DtoError>;
}

pub fn validate_dto<T: BusinessMessage>(message: &T) -> Result<(), DtoError>;

// ---- codec 边界：sealed trait + 不可伪造见证，无获批 codec 时 fail-closed ----
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("business codec is not approved; decision record missing: {0}")]
    CodecUnapproved(&'static str),
    #[error("unexpected trailing bytes: {0}")]
    UnexpectedTrailingBytes(usize),
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::NoCodec {}
}

/// 不可伪造的审批见证类型：私有内部字段且无公开构造函数，
/// 杜绝第三方通过公共 `is_approved()` 虚方法绕过未批准 codec。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApprovedCodecWitness {
    _private: (),
}

/// 只负责 bytes <-> DTO；为 sealed trait 且要求提供不可伪造的见证实例。
pub trait Codec: sealed::Sealed {
    fn encode(&self, witness: &ApprovedCodecWitness, message: &BusinessPayload) -> Result<Vec<u8>, CodecError>;
    fn decode(&self, witness: &ApprovedCodecWitness, expected: BusinessPayloadKind, bytes: &[u8]) -> Result<BusinessPayload, CodecError>;
}

/// 默认实现：恒无有效见证。任何原始字节入口在它面前都必须失败关闭。
pub struct NoCodec;

impl Codec for NoCodec {
    fn encode(&self, _witness: &ApprovedCodecWitness, _message: &BusinessPayload) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::CodecUnapproved("no approved business codec"))
    }
    fn decode(&self, _witness: &ApprovedCodecWitness, _expected: BusinessPayloadKind, _bytes: &[u8]) -> Result<BusinessPayload, CodecError> {
        Err(CodecError::CodecUnapproved("no approved business codec"))
    }
}

/// 结构性 64KiB 建议上限（03 §2），不是 512KiB 整包证明。
pub fn validate_status_budget(bytes_hint: usize) -> Result<(), DtoError>;

pub fn encode_business(
    codec: &dyn Codec,
    witness: Option<&ApprovedCodecWitness>,
    message: &BusinessPayload,
) -> Result<Vec<u8>, CodecError>;

pub fn decode_business(
    codec: &dyn Codec,
    witness: Option<&ApprovedCodecWitness>,
    expected: BusinessPayloadKind,
    bytes: &[u8],
) -> Result<BusinessPayload, CodecError>;

/// 判定「规范的 36 字符小写十六进制 UUIDv4」；不引入任何新依赖。
pub fn is_canonical_uuid_v4(value: &str) -> bool;
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

先在 `crates/rsetup-protocol/src/wire.rs` 追加上述全部类型与函数签名，函数体一律为「错误放行」桩，保证 `cargo test` 能完整类型检查并可生成测试可执行文件：
```rust
pub fn validate_dto<T: BusinessMessage>(_message: &T) -> Result<(), DtoError> {
    Ok(()) // 桩：错误放行一切非法业务 DTO
}

pub fn is_canonical_uuid_v4(_value: &str) -> bool { true } // 桩

pub fn decode_business(
    _codec: &dyn Codec,
    _witness: Option<&ApprovedCodecWitness>,
    _expected: BusinessPayloadKind,
    _bytes: &[u8],
) -> Result<BusinessPayload, CodecError> {
    Err(CodecError::CodecUnapproved("stub")) // 占位，Step 3 换成见证守卫
}

pub fn encode_business(
    _codec: &dyn Codec,
    _witness: Option<&ApprovedCodecWitness>,
    _message: &BusinessPayload,
) -> Result<Vec<u8>, CodecError> {
    Err(CodecError::CodecUnapproved("stub"))
}

pub fn validate_status_budget(_bytes_hint: usize) -> Result<(), DtoError> { Ok(()) }
```
各 DTO 的 `BusinessMessage::validate` 与 `BusinessPayload::{kind, validate}` 同样先写成 `Ok(())` 桩。

在 `crates/rsetup-protocol/src/lib.rs` 追加重导出（保持既有模块声明不变）：
```rust
pub use wire::{
    BUSINESS_SCHEMA_VERSION, ApprovedCodecWitness, BusinessMessage, BusinessPayload, BusinessPayloadKind, Capabilities,
    ClockSample, Codec, CodecError, DeviceStatus, DtoError, MAX_METRICS, MAX_METRIC_KEY_BYTES,
    MAX_METRIC_UNIT_BYTES, MAX_TICKET_TTL_MS, Metric, NoCodec, Query, RebootExecute,
    RebootPrepare, RebootTicket, TaskQuery, TaskRecord, decode_business, encode_business,
    is_canonical_uuid_v4, validate_dto,
};
```

在 `crates/rsetup-protocol/tests/wire_business_dto.rs` 写失败测试（覆盖 schema、UUID、字段长度、词表、冲突与 fail-closed）：
```rust
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, BusinessPayloadKind, Capabilities, ClockSample,
    CodecError, DeviceStatus, DtoError, Metric, NoCodec, RebootTicket, TaskQuery,
    TaskRecord, decode_business, encode_business, is_canonical_uuid_v4, validate_dto,
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
    assert!(!is_canonical_uuid_v4("3F2504E0-4F89-41D3-9A0C-0305E82C3301")); // 大写
    assert!(!is_canonical_uuid_v4("3f2504e04f8941d39a0c0305e82c3301"));       // 无连字符
    assert!(!is_canonical_uuid_v4("3f2504e0-4f89-11d3-9a0c-0305e82c3301"));    // 非 v4
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
        Err(DtoError::InvalidLiteral { field: "state", value: "finished".to_string() })
    );

    let mut record = canonical_record();
    record.reason_code = Some("SOMETHING_ELSE".to_string());
    assert_eq!(
        record.validate(),
        Err(DtoError::InvalidLiteral { field: "reason_code", value: "SOMETHING_ELSE".to_string() })
    );

    let mut record = canonical_record();
    record.journal_epoch = "not-a-uuid".to_string();
    assert_eq!(record.validate(), Err(DtoError::InvalidUuid("journal_epoch")));

    // evidence 是完成证据的必要组成：没有 observed_boot_id 就不允许自称 boot_transition_observed
    let mut record = canonical_record();
    record.state = "succeeded".to_string();
    record.evidence_kind = Some("boot_transition_observed".to_string());
    assert_eq!(record.validate(), Err(DtoError::EmptyField("observed_boot_id")));

    let mut record = canonical_record();
    record.schema_version = 0;
    assert_eq!(record.validate(), Err(DtoError::UnsupportedVersion(0)));

    let mut record = canonical_record();
    record.record_version = 0;
    assert_eq!(record.validate(), Err(DtoError::ValueOutOfContract("record_version")));

    // evidence_kind wire 兼容映射契约（None/空串双向一致、归一化后词表校验）
    let write_map = |opt: Option<&str>| opt.unwrap_or("").to_string();
    let read_map = |s: &str| if s.is_empty() { None } else { Some(s.to_string()) };
    assert_eq!(write_map(None), "");
    assert_eq!(read_map(""), None);
    assert_eq!(write_map(Some("boot_transition_observed")), "boot_transition_observed");
    assert_eq!(read_map(&write_map(Some("boot_transition_observed"))), Some("boot_transition_observed".to_string()));
    let mut record = canonical_record();
    record.evidence_kind = read_map("UNKNOWN_EVIDENCE");
    assert_eq!(record.validate(), Err(DtoError::InvalidLiteral { field: "evidence_kind", value: "UNKNOWN_EVIDENCE".to_string() }));
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
    assert_eq!(nan_status.validate(), Err(DtoError::ValueOutOfContract("metric.value_not_finite")));

    let mut inf_status = status.clone();
    inf_status.metrics[0].value = Some(f64::INFINITY);
    assert_eq!(inf_status.validate(), Err(DtoError::ValueOutOfContract("metric.value_not_finite")));

    // bytes 单位 double 必须为精确非负整数且 <= 2^53 - 1
    let mut bytes_status = status.clone();
    bytes_status.metrics[0].unit = "bytes".to_string();
    bytes_status.metrics[0].value = Some(9_007_199_254_740_991.0); // 2^53 - 1 (ok)
    assert!(bytes_status.validate().is_ok());

    let mut overflow_bytes = status.clone();
    overflow_bytes.metrics[0].unit = "bytes".to_string();
    overflow_bytes.metrics[0].value = Some(9_007_199_254_740_992.0); // 2^53 (超限拒绝)
    assert_eq!(overflow_bytes.validate(), Err(DtoError::ValueOutOfContract("metric.bytes_exceeds_max_safe_integer")));

    // 非 ok 不得携带 value（避免错误显示为 0）
    status.metrics[0].quality = "unavailable".to_string();
    status.metrics[0].value = Some(37.0);
    assert_eq!(status.validate(), Err(DtoError::ValueOutOfContract("metric.value_with_non_ok_quality")));

    // 重复 key 拒绝
    let mut duplicated = status.clone();
    duplicated.metrics[0].quality = "ok".to_string();
    duplicated.metrics[0].value = Some(37.0);
    duplicated.metrics.push(duplicated.metrics[0].clone());
    assert_eq!(duplicated.validate(), Err(DtoError::DuplicateMetricKey("cpu.usage_pct".to_string())));

    // sample_seq 必须 > 0
    let mut zero_seq = status.clone();
    zero_seq.sample_seq = 0;
    assert_eq!(zero_seq.validate(), Err(DtoError::ValueOutOfContract("sample_seq")));

    // 未知时钟质量拒绝
    let mut bad_quality = status.clone();
    bad_quality.clock_quality = "ntp_locked".to_string();
    assert!(matches!(bad_quality.validate(), Err(DtoError::InvalidLiteral { field: "clock_quality", .. })));
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
    assert_eq!(zero_ttl.validate(), Err(DtoError::ValueOutOfContract("valid_for_ms")));

    let query = TaskQuery { schema_version: BUSINESS_SCHEMA_VERSION, sub_task_id: SUB_TASK.to_string() };
    assert!(query.validate().is_ok());

    let empty = TaskQuery { schema_version: BUSINESS_SCHEMA_VERSION, sub_task_id: "   ".to_string() };
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
    assert_eq!(broken.validate(), Err(DtoError::EmptyField("journal_epoch")));
}

#[test]
fn no_codec_means_raw_bytes_are_fail_closed() {
    let codec = NoCodec;
    let record = TaskRecord { state: "succeeded".to_string(), ..canonical_record() };
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
    assert!(matches!(invalid.validate(), Err(DtoError::InvalidLiteral { field: "quality", .. })));
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
cargo test --offline -p rsetup-protocol --test wire_business_dto uuid_validator_accepts_only_canonical_lowercase_v4 -- --exact
```
预期：**编译通过**，执行 1 个用例，因桩 `is_canonical_uuid_v4` 恒 `true` 而断言失败（大写/无连字符/非 v4 用例返回 `true`），退出码非零。
再确认第二个 RED 证据：
```bash
cargo test --offline -p rsetup-protocol --test wire_business_dto no_codec_means_raw_bytes_are_fail_closed -- --exact
```
预期：`encode_business`/`decode_business` 桩返回 `CodecUnapproved("stub")`，与期望的 `"no approved business codec"` 不等而断言失败。**若任何一步是编译错误或 0 用例，不得计入 RED。**

- [ ] **Step 3: 最小实现**

实现细节（严格按序）：
1. `is_canonical_uuid_v4`：长度必须为 36；`[8] [13] [18] [23]` 必须是 `-`；其余位置必须是 `[0-9a-f]`；`value.as_bytes()[14] == b'4'`；variant 位 `value.as_bytes()[19]` 必须是 `8`/`9`/`a`/`b`。不引入任何依赖。
2. 公共前置校验辅助：
```rust
fn require_version(v: u32) -> Result<(), DtoError> {
    if v == BUSINESS_SCHEMA_VERSION { Ok(()) } else { Err(DtoError::UnsupportedVersion(v)) }
}
fn require_non_empty(field: &'static str, value: &str) -> Result<(), DtoError> {
    if value.trim().is_empty() { Err(DtoError::EmptyField(field)) } else { Ok(()) }
}
fn require_max(field: &'static str, value: &str, max: usize) -> Result<(), DtoError> {
    let len = value.len();
    if len > max { Err(DtoError::FieldTooLong { field, len, max }) } else { Ok(()) }
}
fn require_uuid(field: &'static str, value: &str) -> Result<(), DtoError> {
    require_non_empty(field, value)?;
    if is_canonical_uuid_v4(value) { Ok(()) } else { Err(DtoError::InvalidUuid(field)) }
}
fn require_literal(field: &'static str, value: &str, allowed: &[&str]) -> Result<(), DtoError> {
    if allowed.contains(&value) { Ok(()) } else {
        Err(DtoError::InvalidLiteral { field, value: value.to_string() })
    }
}
```
3. 每个 DTO 的 `validate()` 依次校验：`require_version(self.schema_version)?` → 必填非空 → UUID 字段 `require_uuid` → 长度上限 → 词表 `require_literal` → 数值合同。
   - `Capabilities`：`boot_id`/`agent_version`/`journal_epoch` 非空，`journal_epoch` 是 UUIDv4；`supported_actions` 每项必须属于 `SUPPORTED_ACTIONS`（入站动作 6 项，不含 `device.task.result`）。
   - `DeviceStatus`：`sample_seq > 0`；`metrics.len() <= MAX_METRICS`；每个 metric 的 `key` 非空且长度 ≤ `MAX_METRIC_KEY_BYTES`、`unit` 长度 ≤ `MAX_METRIC_UNIT_BYTES`、`quality ∈ METRIC_QUALITIES`；`quality != "ok"` 时 `value` 必须为 `None`（否则 `ValueOutOfContract("metric.value_with_non_ok_quality")`）；`quality == "ok"` 时 `value` 必须为 `Some(v)`；若 `value` 为 `Some(v)`，必须校验 `v.is_finite()`（不为 NaN / ±∞，否则 `ValueOutOfContract("metric.value_not_finite")`）；若 `unit == "bytes"`，`v` 必须为非负精确整数且 `<= 9_007_199_254_740_991.0`（2^53 - 1），否则 `ValueOutOfContract("metric.bytes_exceeds_max_safe_integer")`；key 不得重复（`DuplicateMetricKey`）；`clock_quality ∈ CLOCK_QUALITIES`。
   - `ClockSample`：`quality ∈ CLOCK_QUALITIES`；`boot_id`/`clock_epoch` 为 UUIDv4。
   - `RebootPrepare`：`sub_task_id` UUIDv4、`expected_boot_id` 为 UUIDv4。
   - `RebootTicket`：`sub_task_id`/`boot_id` UUIDv4；`valid_for_ms > 0`（`ValueOutOfContract("valid_for_ms")`）；`execution_token != [0u8; 32]`。
   - `RebootExecute`：同 ticket 的 UUID 与 token 非零校验。
   - `TaskQuery`：`sub_task_id` 非空且 UUIDv4。
   - `TaskRecord`：`sub_task_id`、`journal_epoch` 为 UUIDv4；`pre_boot_id` 为 UUIDv4；`record_version > 0`；`state ∈ TASK_STATES`；`reason_code`（若有）∈ `TASK_REASON_CODES` 且长度 ≤ `MAX_REASON_CODE_BYTES`；`evidence_kind` 归一后（若有）∈ `EVIDENCE_KINDS`；`evidence_kind == Some("boot_transition_observed")` 时 `observed_boot_id` 必须为 `Some` 且是 UUIDv4，若为 `None` 返回 `EmptyField("observed_boot_id")`。
4. `validate_status_budget(bytes_hint)`：`bytes_hint > 65_536` 返回 `Err(DtoError::ValueOutOfContract("device_status_bytes"))`；注释明确它是 03 §2 建议上限，不构成 512 KiB 整包证明。
5. `encode_business` / `decode_business`：先做不可伪造见证前置判定，**在触碰 bytes 之前**：
```rust
pub fn encode_business(
    codec: &dyn Codec,
    witness: Option<&ApprovedCodecWitness>,
    message: &BusinessPayload,
) -> Result<Vec<u8>, CodecError> {
    message.validate().map_err(|_| CodecError::CodecUnapproved("invalid business payload"))?;
    let witness = witness.ok_or(CodecError::CodecUnapproved("no approved business codec"))?;
    codec.encode(witness, message)
}

pub fn decode_business(
    codec: &dyn Codec,
    witness: Option<&ApprovedCodecWitness>,
    expected: BusinessPayloadKind,
    bytes: &[u8],
) -> Result<BusinessPayload, CodecError> {
    let witness = witness.ok_or(CodecError::CodecUnapproved("no approved business codec"))?;
    let decoded = codec.decode(witness, expected, bytes)?;
    if decoded.kind() != expected {
        return Err(CodecError::UnexpectedTrailingBytes(0));
    }
    decoded.validate().map_err(|_| CodecError::CodecUnapproved("invalid business payload"))?;
    Ok(decoded)
}
```
   > 语义说明（必须写进代码注释）：`CodecUnapproved` 在此兼表「无获批 codec 见证」与「DTO 本身不合法」两种拒绝原因；两者的共同效果都是**原始字节不得被当作有效业务消息接受**，这正是 fail-closed 的要求。没有有效 `ApprovedCodecWitness` 时，任何外部自制 codec 均无法接入解码。

- [ ] **Step 4: 运行并确认通过（PASS）**

```bash
cargo test --offline -p rsetup-protocol --test wire_business_dto
```
预期：PASS，全部用例通过。同时回归既有切片：
```bash
cargo test --offline -p rsetup-protocol
```
预期：PASS（既有 `wire_contract` / `frame` / `frame_sequences` / `admission_state` / `sig` / `ed25519` 不受影响）。

- [ ] **Step 5: 提交**

```bash
cargo fmt -p rsetup-protocol
cargo clippy --offline -p rsetup-protocol -- -D warnings
git add crates/rsetup-protocol/src/wire.rs crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/tests/wire_business_dto.rs
git commit -m "feat(protocol): 新增类型化业务 DTO 契约与失败关闭 codec 边界"
```

---

### Task 1B: `RebootExecute` 规范载荷哈希 producer（CanonicalPayloadHash，纯 TDD）

**一次性切片边界：** 本任务只交付 `canonical_reboot_payload_hash` 的完整生产定义与其 1 个表驱动纯测试；不接线 Task 4 的 `payload_matches` 比较 / journal 只读访问器 / `execute` 调用方（仍缺，见文末台账），不声称完整 dedup 链闭合、不声称全链可编译、不声称 producer 已实测。位置依据：producer 放 Transport 唯一 DTO producer `crates/rsetup-protocol/src/wire.rs`（不建新模块）——`RebootExecute` 在此定义（Task 1）；`ring` 已是 `rsetup-protocol` 直接依赖（worktree 与主工作区真实 `crates/rsetup-protocol/Cargo.toml` 第 11 行均为 `ring.workspace = true`），复用 `ring::digest::Context`/`SHA256`，**不加新依赖、不改 manifest、不手写 SHA256**；board-agent 允许直接依赖（Tech Stack 第 19 行 / Task 2 manifest）无 `ring`/`rand`，板端不得直接借未允许依赖；协议 wire DTO 为计划 Create 且 codec 未批，本函数仅 typed 纯逻辑、不造 Protobuf / raw codec。前置：Task 1（`RebootExecute` DTO 与 `wire.rs`/`lib.rs` 重导出列表）。

**Files:**
- Modify: `crates/rsetup-protocol/src/wire.rs`（在 Task 1 DTO 定义之后追加一个纯函数；不触碰既有信封/DTO 代码）
- Modify: `crates/rsetup-protocol/src/lib.rs`（在既有 `pub use wire::{...}` 列表追加 `canonical_reboot_payload_hash`；不动其他重导出）
- Test: `crates/rsetup-protocol/tests/wire_business_dto.rs`（追加 1 个完整表驱动测试；`use` 列表补 `RebootExecute` 与 `canonical_reboot_payload_hash`）

**Interfaces:**
- Consumes: `RebootExecute`（Task 1，四字段：`schema_version: u32`、`sub_task_id: String`、`expected_boot_id: String`、`execution_token: [u8; 32]`，**无 nonce 字段**）；`ring`（`rsetup-protocol` 既有直接依赖，无新 manifest 项）。
- Produces: `pub fn canonical_reboot_payload_hash(request: &RebootExecute) -> [u8; 32]` —— Task 4 `execute` 第 0 步形状校验完成后由调用方取 `payload_hash` 传入 `execute(...)`（`execute` 签名不变：`payload_hash: &[u8; 32]` 参数已在签名中）。

**规范编码（固定字节序、无歧义；整体为 SHA-256 输入）：** 1B 标签长度（固定 31）+ UTF-8 固定版本化 domain/action 标签 `rsetup.device.reboot.execute.v1` → `schema_version` 固定 u32 大端 4B → `sub_task_id`（8B 固定 u64 大端长度 + 规范小写 UUID 字符串 UTF-8 字节）→ `expected_boot_id`（8B 固定 u64 大端长度 + 规范小写 UUID 字符串 UTF-8 字节）。

- **参与 = 全部固定语义字段**：`schema_version`/`sub_task_id`/`expected_boot_id`（DTO 仅 4 字段，`execution_token` 除外）。
- **`execution_token` 不参与**：票据一次性、不属于载荷语义；同一语义不得因临时票据变化被判 `TASK_CONFLICT`（03 §5 WIRE-03「同 ID 不同 payload」按语义字段判定）。
- **`nonce` 不参与**：`nonce` **不是** `RebootExecute` DTO 字段——它属于 wire 信封/传输层（信封级握手/保活 nonce）；producer 输入仅 `&RebootExecute`，结构上不可能参与；nonce 在 wire 信封层、本任务不造 nonce 字段。
- **`controller_key`（已认证 controller 公钥）归 journal key 单独隔离**：去重键为 `(controller_key, sub_task_id)`，经 `journal.get`/`insert_accepted` 参数传递、不进入本哈希；本 producer 不暗添任何 HTTP/wire 字段。
- **形状校验是 Task 4 execute 前置**（第 0 步：`schema_version == 1` 且两个 UUID 为规范 UUIDv4）；本函数是纯函数（无 I/O、无权限/boot 判断、不校验），且**不是 wire codec**（不定义字节帧/信封、不造 Protobuf）；规范编码定义于形状校验后的 DTO——调用方须先校验再哈希，dedup 比较复用同一哈希。
- **长度转换**：字符串长度前缀一律**固定 u64 大端**（绝不平台 `usize` 编码）；`u64::try_from(s.len())` 在所有受支持 Rust 目标（16/32/64-bit 的 usize ≤ 2^64−1）可证明无损，`expect` 只是对该不可达不变量的 fail-closed 守卫——函数返回 `[u8; 32]` 不带 `Result`，不为长度引假错误枚举。

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-protocol/src/wire.rs` 追加：
```rust
/// RebootExecute 规范载荷哈希（规范编码见上节；排除 execution token/nonce）。
pub fn canonical_reboot_payload_hash(_request: &RebootExecute) -> [u8; 32] {
    [0u8; 32] // 桩：恒返回零哈希；RED 目标断言是语义字段变更 assert_ne
}
```
在 `crates/rsetup-protocol/src/lib.rs` 既有 `pub use wire::{...}` 列表追加 `canonical_reboot_payload_hash`。

在 `crates/rsetup-protocol/tests/wire_business_dto.rs` 追加（**1 个完整表驱动测试**，全部 synthetic 输入、无真实凭据；复用 Task 1 既有 `BOOT`/`SUB_TASK` 合成常量，表内两个替代 UUID 亦为 synthetic 规范小写 UUIDv4；期望仅等/不等关系，token 字节不放进预期秘密）：
```rust
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
        ("相同语义 token 变更", { let mut r = base.clone(); r.execution_token = [8u8; 32]; r }, true),
        ("schema_version 变更", { let mut r = base.clone(); r.schema_version = 2; r }, false),
        ("sub_task_id 变更", { let mut r = base.clone(); r.sub_task_id = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d".to_string(); r }, false),
        ("expected_boot_id 变更", { let mut r = base.clone(); r.expected_boot_id = "6f8b1c2d-0000-4000-8000-000000000002".to_string(); r }, false),
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
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
cargo test --offline -p rsetup-protocol --test wire_business_dto canonical_reboot_payload_hash_is_semantic_not_ticket_bound -- --exact
```
预期：**编译通过**，执行 1 个用例，`schema_version 变更` 用例的 `assert_ne` 断言失败（左右均为桩的 `[0u8; 32]`）；其前「稳定重复哈希」与「相同语义 token 变更」通过；**若是编译错误或 0 用例，不得计入 RED**。按 worktree 私有 `CARGO_TARGET_DIR` 约定（Global Constraints 第 6 条）运行、不新引 `CARGO_HOME`，本任务不执行。

- [ ] **Step 3: 最小实现（GREEN，同名桩替换）**

将 `crates/rsetup-protocol/src/wire.rs` 中同名函数（同签名）桩体替换为真实实现：
```rust
pub fn canonical_reboot_payload_hash(request: &RebootExecute) -> [u8; 32] {
    // 版本化 domain/action 标签：固定 31B（rsetup + device.reboot.execute + v1）
    const LABEL: &[u8] = b"rsetup.device.reboot.execute.v1";
    let mut buf = Vec::with_capacity(128);
    buf.push(LABEL.len() as u8);
    buf.extend_from_slice(LABEL);
    buf.extend_from_slice(&request.schema_version.to_be_bytes());
    // 长度前缀固定 u64 BE：所有受支持目标 usize ≤ 2^64-1 可证明无损；try_from 为 checked 转换（fail-closed 守卫）、不引新错误枚举
    let sub_len = u64::try_from(request.sub_task_id.len())
        .expect("usize fits u64 on all supported Rust targets");
    buf.extend_from_slice(&sub_len.to_be_bytes());
    buf.extend_from_slice(request.sub_task_id.as_bytes());
    let boot_len = u64::try_from(request.expected_boot_id.len())
        .expect("usize fits u64 on all supported Rust targets");
    buf.extend_from_slice(&boot_len.to_be_bytes());
    buf.extend_from_slice(request.expected_boot_id.as_bytes());
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(&buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_slice());
    out
}
```

- [ ] **Step 4: 运行并确认通过（PASS）**

```bash
cargo test --offline -p rsetup-protocol --test wire_business_dto canonical_reboot_payload_hash_is_semantic_not_ticket_bound -- --exact
```
预期：PASS（1 用例、多断言）。

- [ ] **Step 5: 提交**

```bash
cargo fmt -p rsetup-protocol
cargo clippy --offline -p rsetup-protocol -- -D warnings
git add crates/rsetup-protocol/src/wire.rs crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/tests/wire_business_dto.rs
git commit -m "feat(protocol): 新增 RebootExecute 规范载荷哈希 producer（语义字段、排除票据与 nonce）"
```

---

### Task 2: `rsetup-board-agent` crate、严格类型化动作白名单与零变更隔离

**Files:**
- Create: `crates/rsetup-board-agent/Cargo.toml`
- Create: `crates/rsetup-board-agent/src/lib.rs`
- Create: `crates/rsetup-board-agent/src/actions.rs`
- Create: `crates/rsetup-board-agent/src/dispatch.rs`
- Create: `crates/rsetup-board-agent/src/read_only.rs`
- Modify: `Cargo.toml`（根 `members` 追加 `crates/rsetup-board-agent`）
- Modify: `Cargo.lock`（由 cargo 重算，必须与根 manifest 同提交）
- Test: `crates/rsetup-board-agent/tests/read_only.rs`

**Interfaces:**
```rust
// ---- actions.rs：封闭动作表 ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind { ReadOnly, JournalRead, RebootPrepare, RebootExecute }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    pub name: &'static str,
    pub kind: ActionKind,
    pub reserved_prefix: bool,
}

/// 封闭入站动作白名单：仅 6 项入站请求动作。
/// device.task.result 是仅板端→中控的 TYPE_EVENT，绝不纳入入站白名单。
pub const ACTION_ALLOWLIST: &[ActionSpec] = &[
    ActionSpec { name: "device.capabilities.get", kind: ActionKind::ReadOnly,      reserved_prefix: false },
    ActionSpec { name: "device.status.get",       kind: ActionKind::ReadOnly,      reserved_prefix: false },
    ActionSpec { name: "device.clock.get",        kind: ActionKind::ReadOnly,      reserved_prefix: false },
    ActionSpec { name: "device.task.get",         kind: ActionKind::JournalRead,   reserved_prefix: false },
    ActionSpec { name: "device.reboot.prepare",   kind: ActionKind::RebootPrepare, reserved_prefix: false },
    ActionSpec { name: "device.reboot.execute",   kind: ActionKind::RebootExecute, reserved_prefix: false },
];

/// 板端主动事件动作名称常量（仅供出站事件使用，不参与入站查表）
pub const TASK_RESULT_EVENT_ACTION: &str = "device.task.result";

pub fn lookup_action(action: &str) -> Option<&'static ActionSpec>;
/// 白名单之外的动作（含空串、超长、`tunnel.` 保留前缀、板端出站事件名及任意变更动作）一律 None。
pub fn is_reserved(action: &str) -> bool; // `action.starts_with("tunnel.")`
pub fn allowed_action_names() -> Vec<&'static str>;

// ---- read_only.rs ----
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerContext { pub authenticated_controller_key: [u8; 32] }

/// 只读来源必须返回**类型化 DTO**，从而让 BoardAgent 能执行结构校验；
/// 因此不需要 wire 字节，也不受未获批 codec 影响。
pub trait ReadOnlySource: Send + Sync {
    fn capabilities(&self) -> rsetup_protocol::wire::Capabilities;
    fn status(&self) -> rsetup_protocol::wire::DeviceStatus;
    fn clock(&self) -> rsetup_protocol::wire::ClockSample;
}
/// 变更隔离见证：测试用它证明白名单拒绝路径从未触达任何变更入口。
pub trait ReadOnlyMutationGuard {
    fn mutation_calls(&self) -> usize;
}

// ---- dispatch.rs ----
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BusinessStatus {
    #[error("ok")] Success = 0,
    #[error("unsupported version")] UnsupportedVersion = 1001,
    #[error("unsupported action")] UnsupportedAction = 1002,
    #[error("invalid argument")] InvalidArgument = 1003,
    #[error("capability unavailable")] CapabilityUnavailable = 1004,
    #[error("device busy")] DeviceBusy = 1005,
    #[error("task not found")] TaskNotFound = 1006,
    #[error("task conflict")] TaskConflict = 1007,
    #[error("resource exhausted")] ResourceExhausted = 1008,
    #[error("internal error")] InternalError = 1009,
    #[error("execution token invalid")] ExecutionTokenInvalid = 1010,
}

pub trait TaskJournalReader: Send + Sync {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &rsetup_protocol::wire::TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus>;
}

pub trait JournalEpochSource: Send + Sync {
    fn journal_epoch(&self) -> String;
}

/// 结构校验成功后的类型化响应；原始 wire 只有在获批 codec 就位后才可能产出。
#[derive(Clone, Debug, PartialEq)]
pub enum BusinessResponse {
    Capabilities(rsetup_protocol::wire::Capabilities),
    DeviceStatus(rsetup_protocol::wire::DeviceStatus),
    ClockSample(rsetup_protocol::wire::ClockSample),
    TaskRecord(rsetup_protocol::wire::TaskRecord),
}

pub struct BoardAgent<R, J, E> {
    read_only: R,
    journal: J,
    epoch: E,
}

impl<R: ReadOnlySource, J: TaskJournalReader, E: JournalEpochSource> BoardAgent<R, J, E> {
    pub fn new(read_only: R, journal: J, epoch: E) -> Self;
    pub fn source(&self) -> &R;
    pub fn journal_epoch(&self) -> String;

    /// 唯一类型化入口：`action` 必须由调用方显式给出，顺序恒为
    /// 「白名单查表 → DTO 结构校验 → 触达 source/journal」。三个只读查询的 DTO 无法自证
    /// action，因此绝不允许由 DTO 变体反推 action。
    pub fn handle_typed_action(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &rsetup_protocol::wire::BusinessPayload,
    ) -> Result<BusinessResponse, BusinessStatus>;

    /// 原始字节入口：先查白名单，再 fail-closed；无获批 codec 时永不解析、永不触达 I/O。
    pub fn handle_wire(
        &self,
        codec: &dyn rsetup_protocol::wire::Codec,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus>;
}
```

> **对上游 transport 计划的接口偏差（有意为之，须在提交信息中说明）：** 上游 transport Task 3 写的是
> `pub async fn handle(&self, peer: &PeerContext, action: &str, payload: &[u8]) -> Result<Vec<u8>, BusinessStatus>`。
> 本计划用 `handle_typed_action`（类型化、可校验 DTO）取代它，并让 `handle_wire` 承担原始字节路径且 **fail-closed**。
> 理由：`Vec<u8>` 返回类型在无获批 codec 时不可能真实产出，保留它只能靠假字节冒充互通；而 `action` 与
> `BusinessPayload` 分离能让白名单判定先于任何解码或 I/O。上游 Task 3 的两个测试因此按下述方式改写：
> `remote_mutations_are_strictly_rejected` 改用 `handle_wire(&NoCodec, ...)`（未知动作 → `UnsupportedAction`）；
> `task_get_validates_schema_and_version` 改用 `handle_typed_action` 并断言
> `schema_version = 2` → `UnsupportedVersion`、空白 `sub_task_id` → `InvalidArgument`、原始字节 → `UnsupportedVersion`。

- [ ] **Step 1: 建立可编译桩与编写失败测试**

1. 根 `Cargo.toml` 的 `members` 追加 `"crates/rsetup-board-agent"`。
2. 创建 `crates/rsetup-board-agent/Cargo.toml`：
```toml
[package]
name = "rsetup-board-agent"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
rsetup-protocol = { path = "../rsetup-protocol" }
rsetup-core = { path = "../rsetup-core" }
thiserror.workspace = true
tokio.workspace = true
uuid.workspace = true
serde.workspace = true
serde_json.workspace = true

[dev-dependencies]
tokio.workspace = true
uuid.workspace = true

[features]
default = []
test-seam = []
```
   > Feature 策略（workspace 普通回归统一口径）：`tests/journal_fault_injection.rs` 与 `tests/reboot_boundary.rs` 顶部无条件 `use rsetup_board_agent::test_seam::…`，而 `test_seam` 模块由既有 `#[cfg(any(test, feature = "test-seam"))]` 门控（该 cfg 写法不改），故这两个集成测试目标将以 `required-features = ["test-seam"]` 声明——feature 关闭时这两个目标不参与编译，集成计划 Step 4 的 `cargo clippy --workspace --all-targets -- -D warnings` 无 feature 可通过；功能验收仍必须显式带 `--features test-seam` 且确认执行用例数非零。本 Task 2 的 manifest 只声明 `[features]`（`default = []` 与 `test-seam = []`），不提前声明这两个 `[[test]]` target：`required-features` 不豁免显式 target 解析，target 文件缺失时 manifest 解析即失败，本 Task 首次 `cargo build` 会在行为 RED 之前直接报错。两个 `[[test]]` 声明分别延后到 Task 3 创建 `tests/journal_fault_injection.rs` 与 Task 4 创建 `tests/reboot_boundary.rs` 的同一准备步骤中加入，且先于各自段落的首个测试命令。
   > 依赖守则：这里没有加入 `tempfile`、`prost`、`tonic`、任何密码学或网络 crate。`tokio`/`uuid`/`thiserror`/`serde`/`serde_json` 均已在根工作区 `Cargo.lock` 中锁定。board-agent 若真实用 JSON 序列化，必须声明已锁定的 `serde` 与 `serde_json` 依赖，绝不手写伪解析。
3. 创建 `src/lib.rs`（初始仅模块声明与导出）：
```rust
//! 板端代理：只读白名单分发、任务 journal、票据与无参 OS 重启边界。
//! 本 crate 不包含任何密码学实现、不建立真实网络连接。

pub mod actions;
pub mod dispatch;
pub mod read_only;

pub use actions::{ACTION_ALLOWLIST, ActionKind, ActionSpec, allowed_action_names, is_reserved, lookup_action};
pub use dispatch::{BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader};
pub use read_only::{PeerContext, ReadOnlyMutationGuard, ReadOnlySource};
```
4. 创建 `actions.rs`，按上述接口写出**完整常量表**与桩函数（`lookup_action` 桩恒返回 `None`，`is_reserved` 桩恒返回 `false`），保证可编译。
5. 创建 `read_only.rs`（只有类型与 trait，无实现）。
6. 创建 `dispatch.rs`，其中 `BoardAgent` 方法为桩：
```rust
pub fn handle_typed_action(
    &self,
    _peer: &PeerContext,
    _action: &str,
    _payload: &rsetup_protocol::wire::BusinessPayload,
) -> Result<BusinessResponse, BusinessStatus> {
    Err(BusinessStatus::Success) // 桩：错误地把一切请求当作成功空结果
}
pub fn handle_wire(
    &self,
    _codec: &dyn rsetup_protocol::wire::Codec,
    _peer: &PeerContext,
    _action: &str,
    _payload: &[u8],
) -> Result<BusinessResponse, BusinessStatus> {
    Err(BusinessStatus::Success) // 桩：错误放行一切原始字节
}
```
7. 在 `crates/rsetup-board-agent/tests/read_only.rs` 写失败测试：
```rust
use rsetup_board_agent::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, PeerContext,
    ReadOnlyMutationGuard, ReadOnlySource, TaskJournalReader, allowed_action_names,
};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    TaskQuery, TaskRecord,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

#[derive(Default)]
struct FakeSource { mutations: AtomicUsize }

impl ReadOnlySource for FakeSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.reboot.prepare".to_string()],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: EPOCH.to_string(),
        }
    }
    fn status(&self) -> DeviceStatus {
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_epoch: AGENT_EPOCH.to_string(),
            sample_seq: 1,
            sampled_unix_ms: 1_700_000_000_000,
            sampled_monotonic_ns: 42,
            clock_epoch: CLOCK_EPOCH.to_string(),
            metrics: vec![],
            clock_quality: "observed".to_string(),
        }
    }
    fn clock(&self) -> ClockSample {
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            clock_epoch: CLOCK_EPOCH.to_string(),
            receive_unix_ms: 1,
            send_unix_ms: 2,
            receive_monotonic_ns: 10,
            send_monotonic_ns: 20,
            quality: "observed".to_string(),
        }
    }
}

impl ReadOnlyMutationGuard for FakeSource {
    fn mutation_calls(&self) -> usize { self.mutations.load(Ordering::SeqCst) }
}

#[derive(Default)]
struct FakeJournal { queries: AtomicUsize }

impl TaskJournalReader for FakeJournal {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<TaskRecord>, BusinessStatus> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if query.schema_version != BUSINESS_SCHEMA_VERSION {
            return Err(BusinessStatus::UnsupportedVersion);
        }
        if query.sub_task_id.trim().is_empty() {
            return Err(BusinessStatus::InvalidArgument);
        }
        if controller_key == &[1u8; 32] && query.sub_task_id == SUB_TASK {
            Ok(Some(TaskRecord {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                journal_epoch: EPOCH.to_string(),
                record_version: 1,
                state: "accepted".to_string(),
                pre_boot_id: BOOT.to_string(),
                observed_boot_id: None,
                reason_code: None,
                updated_unix_ms: None,
                evidence_kind: None,
            }))
        } else {
            Ok(None)
        }
    }
}

struct FixedEpoch;
impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String { EPOCH.to_string() }
}

fn agent() -> BoardAgent<FakeSource, FakeJournal, FixedEpoch> {
    BoardAgent::new(FakeSource::default(), FakeJournal::default(), FixedEpoch)
}

#[test]
fn allowlist_is_closed_and_reserved_prefix_is_never_allowlisted() {
    let names = allowed_action_names();
    assert_eq!(names.len(), 6);
    assert!(names.contains(&"device.task.get"));
    assert!(!names.contains(&"device.task.result"));
    assert!(!names.contains(&"device.fan.apply"));
    for name in names {
        assert!(!name.starts_with("tunnel."), "{name} must not use the reserved prefix");
    }
}

#[test]
fn unknown_and_mutation_actions_return_unsupported_action_with_zero_mutations() {
    let agent = agent();
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    for action in [
        "device.fan.apply",
        "device.power.off",
        "system.shell",
        "tunnel.ping",
        "tunnel.kick",
        "device.task.result",
        "",
    ] {
        let result = agent.handle_wire(&NoCodec, &peer, action, &[]);
        assert_eq!(result, Err(BusinessStatus::UnsupportedAction), "action={action}");
    }
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn known_actions_with_unapproved_raw_bytes_fail_closed() {
    let agent = agent();
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    // 无获批 codec：即使 action 合法，也不得把原始字节当作业务消息接受
    assert_eq!(
        agent.handle_wire(&NoCodec, &peer, "device.task.get", b"not_valid_query"),
        Err(BusinessStatus::UnsupportedVersion)
    );
    assert_eq!(
        agent.handle_wire(&NoCodec, &peer, "device.status.get", &[]),
        Err(BusinessStatus::UnsupportedVersion)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn typed_task_get_validates_schema_and_isolates_controller_namespace() {
    let agent = agent();
    let owner = PeerContext { authenticated_controller_key: [1u8; 32] };
    let stranger = PeerContext { authenticated_controller_key: [2u8; 32] };

    let good = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    });
    match agent.handle_typed_action(&owner, "device.task.get", &good) {
        Ok(BusinessResponse::TaskRecord(record)) => {
            assert_eq!(record.state, "accepted");
            assert_eq!(record.journal_epoch, EPOCH);
        }
        other => panic!("expected TaskRecord, got {other:?}"),
    }

    // 另一把已认证公钥不得看到同一 sub_task_id 的记录
    assert_eq!(
        agent.handle_typed_action(&stranger, "device.task.get", &good),
        Err(BusinessStatus::TaskNotFound)
    );

    // schema_version 未知必须拒绝
    let bad_version = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: 2,
        sub_task_id: SUB_TASK.to_string(),
    });
    assert_eq!(
        agent.handle_typed_action(&owner, "device.task.get", &bad_version),
        Err(BusinessStatus::UnsupportedVersion)
    );

    // 空 sub_task_id 必须拒绝
    let empty = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: "   ".to_string(),
    });
    assert_eq!(
        agent.handle_typed_action(&owner, "device.task.get", &empty),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn typed_read_only_actions_return_validated_typed_dtos() {
    let agent = agent();
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    let query = BusinessPayload::Query(rsetup_protocol::wire::Query { schema_version: 1 });

    match agent.handle_typed_action(&peer, "device.capabilities.get", &query) {
        Ok(BusinessResponse::Capabilities(caps)) => {
            assert!(caps.durable_task_journal);
            assert_eq!(caps.journal_epoch, EPOCH);
        }
        other => panic!("expected Capabilities, got {other:?}"),
    }
    assert!(matches!(
        agent.handle_typed_action(&peer, "device.status.get", &BusinessPayload::DeviceStatus(agent.source().status())),
        Ok(BusinessResponse::DeviceStatus(_))
    ));
    assert!(matches!(
        agent.handle_typed_action(&peer, "device.clock.get", &BusinessPayload::ClockSample(agent.source().clock())),
        Ok(BusinessResponse::ClockSample(_))
    ));
    // 规范实现统一接受 Query 载荷（不校验 DTO 与 action 的对应关系），
    // 因此 clock.get + Query 与 status.get + Query 一样应被接受并返回正确类型
    assert!(matches!(
        agent.handle_typed_action(&peer, "device.clock.get", &query),
        Ok(BusinessResponse::ClockSample(_))
    ));
    // 板端事件类型不得由远程请求触发（未在入站白名单中，返回 UnsupportedAction）
    assert_eq!(
        agent.handle_typed_action(&peer, "device.task.result", &BusinessPayload::TaskRecord(TaskRecord {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: SUB_TASK.to_string(),
            journal_epoch: EPOCH.to_string(),
            record_version: 1,
            state: "accepted".to_string(),
            pre_boot_id: BOOT.to_string(),
            observed_boot_id: None,
            reason_code: None,
            updated_unix_ms: None,
            evidence_kind: None,
        })),
        Err(BusinessStatus::UnsupportedAction)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn action_lookup_covers_every_kind_and_rejects_reserved_prefix() {
    use rsetup_board_agent::{ActionKind, is_reserved, lookup_action};
    assert_eq!(lookup_action("device.capabilities.get").unwrap().kind, ActionKind::ReadOnly);
    assert_eq!(lookup_action("device.task.get").unwrap().kind, ActionKind::JournalRead);
    assert_eq!(lookup_action("device.reboot.prepare").unwrap().kind, ActionKind::RebootPrepare);
    assert_eq!(lookup_action("device.reboot.execute").unwrap().kind, ActionKind::RebootExecute);
    assert!(lookup_action("device.task.result").is_none());
    assert!(lookup_action("device.fan.apply").is_none());
    assert!(is_reserved("tunnel.ping"));
    assert!(!is_reserved("device.task.get"));
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
cargo build --offline -p rsetup-board-agent    # 首次：允许 cargo 重算 Cargo.lock，生成 board-agent 成员条目
cargo test --offline -p rsetup-board-agent --test read_only unknown_and_mutation_actions_return_unsupported_action_with_zero_mutations -- --exact
```
预期：**编译通过**，执行 1 个用例，因 `handle_wire` 桩返回 `Ok`/`Err(Success)` 而非 `Err(UnsupportedAction)` 而断言失败，退出码非零。
```bash
cargo test --offline -p rsetup-board-agent --test read_only typed_task_get_validates_schema_and_isolates_controller_namespace -- --exact
```
预期：断言失败（拿到 `Err(Success)` 而非 `TaskRecord`）。**严禁把「模块未定义」的编译失败当作 RED。**
确认锁文件纪律：
```bash
git diff --stat Cargo.lock Cargo.toml
```
预期：两者都被修改（新成员解析已写入 lock）。

- [ ] **Step 3: 最小实现**

1. `actions.rs` 实现查表（严格 `==` 比较，不做大小写/前缀/模糊匹配）：
```rust
pub fn lookup_action(action: &str) -> Option<&'static ActionSpec> {
    ACTION_ALLOWLIST.iter().find(|spec| spec.name == action)
}

pub fn is_reserved(action: &str) -> bool { action.starts_with("tunnel.") }

pub fn allowed_action_names() -> Vec<&'static str> {
    ACTION_ALLOWLIST.iter().map(|spec| spec.name).collect()
}
```
2. `dispatch.rs` 实现 `handle_typed_action` 与 `handle_wire`，**顺序不可调换**：
```rust
impl<R: ReadOnlySource, J: TaskJournalReader, E: JournalEpochSource> BoardAgent<R, J, E> {
    pub fn new(read_only: R, journal: J, epoch: E) -> Self {
        Self { read_only, journal, epoch }
    }
    pub fn source(&self) -> &R { &self.read_only }
    pub fn journal_epoch(&self) -> String { self.epoch.journal_epoch() }

    /// 唯一类型化入口：action 显式给出，先查表、再校验 DTO、最后才触达 I/O。
    pub fn handle_typed_action(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &BusinessPayload,
    ) -> Result<BusinessResponse, BusinessStatus> {
        // 1) 先做动作判定：不在封闭表内的动作（含 device.task.result）在触碰任何 source/journal 之前被拒绝
        let spec = lookup_action(action).ok_or(BusinessStatus::UnsupportedAction)?;
        // 2) 结构校验：失败即拒绝，且未触达 I/O。错误按映射表分类（先匹配先赢）：
        //    仅 schema 版本/能力不匹配 → UnsupportedVersion；
        //    结构/字段/枚举/长度类校验失败（含空白 sub_task_id）→ InvalidArgument
        payload.validate().map_err(|err| match err {
            DtoError::UnsupportedVersion(_) => BusinessStatus::UnsupportedVersion,
            _ => BusinessStatus::InvalidArgument,
        })?;

        match (spec.kind, payload) {
            // 只读动作接受三类请求载荷：Query / DeviceStatus / ClockSample（均已经过上方 payload.validate()）；
            // 载荷 kind 只决定校验面，响应恒按 action 路由由 source 提供，绝不按 DTO 变体反推 action
            (ActionKind::ReadOnly, BusinessPayload::Query(_) | BusinessPayload::DeviceStatus(_) | BusinessPayload::ClockSample(_)) => match action {
                // TR-R8-2：出站响应返回前必须先执行结构校验，非法 producer DTO 绝不流出；
                // 校验失败固定映射 InternalError，本分支保持 OS 调用与持久写为 0。
                "device.capabilities.get" => {
                    let caps = self.read_only.capabilities();
                    caps.validate().map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::Capabilities(caps))
                }
                "device.status.get" => {
                    let status = self.read_only.status();
                    status.validate().map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::DeviceStatus(status))
                }
                "device.clock.get" => {
                    let clock = self.read_only.clock();
                    clock.validate().map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::ClockSample(clock))
                }
                _ => Err(BusinessStatus::InvalidArgument),
            },
            (ActionKind::ReadOnly, _) => Err(BusinessStatus::InvalidArgument),
            (ActionKind::JournalRead, BusinessPayload::TaskQuery(query)) => {
                if query.sub_task_id.trim().is_empty() {
                    return Err(BusinessStatus::InvalidArgument);
                }
                match self.journal.query(&peer.authenticated_controller_key, query)? {
                    Some(record) => {
                        // TR-R8-2：journal query 返回的 record 出站前同样执行结构校验，非法记录绝不流出。
                        record.validate().map_err(|_| BusinessStatus::InternalError)?;
                        Ok(BusinessResponse::TaskRecord(record))
                    }
                    None => Err(BusinessStatus::TaskNotFound),
                }
            }
            (ActionKind::JournalRead, _) => Err(BusinessStatus::InvalidArgument),
            // prepare/execute 由 Task 4 的 RebootExecutor 承担；本切片明确拒绝，绝不假装成功
            (ActionKind::RebootPrepare, _) | (ActionKind::RebootExecute, _) => {
                Err(BusinessStatus::CapabilityUnavailable)
            }
        }
    }

    /// 原始字节入口：先查白名单，再 fail-closed，最后才允许解码。
    pub fn handle_wire(
        &self,
        codec: &dyn Codec,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus> {
        // 1) 白名单先行：未授权动作不消耗 codec，也不触碰任何 I/O
        let spec = lookup_action(action).ok_or(BusinessStatus::UnsupportedAction)?;
        // 2) 按 action 精确给出期望载荷类型；板端事件不在入站白名单
        let expected = match (spec.kind, action) {
            (ActionKind::ReadOnly, "device.capabilities.get") => BusinessPayloadKind::Query,
            (ActionKind::ReadOnly, "device.status.get") => BusinessPayloadKind::Query,
            (ActionKind::ReadOnly, "device.clock.get") => BusinessPayloadKind::Query,
            (ActionKind::JournalRead, "device.task.get") => BusinessPayloadKind::TaskQuery,
            (ActionKind::RebootPrepare, "device.reboot.prepare") => BusinessPayloadKind::RebootPrepare,
            (ActionKind::RebootExecute, "device.reboot.execute") => BusinessPayloadKind::RebootExecute,
            _ => return Err(BusinessStatus::InvalidArgument),
        };
        // 3) 无获批 codec 见证：fail-closed，绝不解析、绝不自造编码器
        let decoded = decode_business(codec, None, expected, payload)
            .map_err(|_| BusinessStatus::UnsupportedVersion)?;
        self.handle_typed_action(peer, action, &decoded)
    }
}
```
   > `BusinessPayloadKind` 需要包含 `Query`（见 Task 1 的枚举定义），使三个只读动作的期望载荷类型可被精确表达。
   > 原始字节路径的期望载荷仍固定为 `Query`；类型化路径额外接受只读动作的 `DeviceStatus` / `ClockSample` 请求载荷（见上方 match 分支）。

   > **DTO 校验错误映射（判定顺序：先匹配先赢，GREEN 与测试断言一致）：**
   >
   > | DtoError 变体 | BusinessStatus |
   > | --- | --- |
   > | `UnsupportedVersion(_)`（仅 schema 版本或能力不匹配） | `UnsupportedVersion` |
   > | `EmptyField` / `FieldTooLong` / `InvalidUuid` / `InvalidLiteral` / `ValueOutOfContract` / `DuplicateMetricKey`（结构/字段/枚举/长度类失败，含空白 `sub_task_id`） | `InvalidArgument` |
   >
   > 无获批 codec 的原始字节仍 fail-closed → `UnsupportedVersion`，与 `known_actions_with_unapproved_raw_bytes_fail_closed` 断言一致。

- [ ] **Step 4: 运行并确认通过（PASS）**

```bash
cargo test --offline -p rsetup-board-agent --test read_only
cargo test --offline --locked -p rsetup-board-agent
```
预期：PASS；且 `git diff Cargo.toml Cargo.lock` 均包含 board-agent 成员。

- [ ] **Step 5: 提交**

```bash
cargo fmt -p rsetup-board-agent
cargo clippy --offline --locked -p rsetup-board-agent -- -D warnings
git add Cargo.toml Cargo.lock crates/rsetup-board-agent/Cargo.toml crates/rsetup-board-agent/src crates/rsetup-board-agent/tests/read_only.rs crates/rsetup-protocol/src/wire.rs
git commit -m "feat(board-agent): 新增含严格类型化动作白名单与零写隔离的 crate"
```

---
