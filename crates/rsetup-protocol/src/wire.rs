use std::collections::BTreeMap;
use thiserror::Error;

pub const MAX_ENVELOPE_BYTES: usize = 524_288; // 512 KiB 完整序列化信封上限
pub const MAX_ACTION_BYTES: usize = 64;
pub const MAX_METADATA_ENTRIES: usize = 16;
pub const MAX_METADATA_KEY_BYTES: usize = 64;
pub const MAX_METADATA_VAL_BYTES: usize = 256;
pub const PING_NONCE_LEN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    Unknown = 0,
    Request = 1,
    Response = 2,
    Event = 3,
    System = 4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelPacket {
    pub trace_id: u64,
    pub kind: PacketType,
    pub action: String,
    pub status_code: i32,
    pub error_message: String,
    pub payload: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum WireError {
    #[error("unsupported business schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("action cannot be empty")]
    ActionEmpty,
    #[error("action too long: {len} > {max}")]
    ActionTooLong { len: usize, max: usize },
    #[error("action contains reserved tunnel prefix or invalid characters")]
    InvalidAction,
    #[error("packet type unknown or invalid")]
    InvalidPacketType,
    #[error("metadata count too large: {len} > {max}")]
    MetadataTooMany { len: usize, max: usize },
    #[error("metadata key too long: {len} > {max}")]
    MetadataKeyTooLong { len: usize, max: usize },
    #[error("metadata value too long: {len} > {max}")]
    MetadataValTooLong { len: usize, max: usize },
    #[error("envelope wire size too large: {len} > {max}")]
    EnvelopeTooLarge { len: usize, max: usize },
    #[error("invalid ping nonce length: {0} (want 16)")]
    InvalidPingNonceLength(usize),
    #[error("ping request must have empty metadata")]
    PingRequestMetadataNotEmpty,
    #[error("ping response must have status_code=0: {0}")]
    InvalidPingStatusCode(i32),
    #[error("ping response must have empty error_message and metadata")]
    InvalidPingResponseFields,
    #[error("trace_id cannot be zero")]
    InvalidTraceId,
    #[error("non-response packet cannot carry status_code ({status_code}) or error_message")]
    UnexpectedResponseFields { status_code: i32 },
}

pub fn validate_business_version(v: u32) -> Result<(), WireError> {
    if v == 1 {
        Ok(())
    } else {
        Err(WireError::UnsupportedVersion(v))
    }
}

/// 结构合法性；最终 512KiB 整包校验必须在获审 codec 的实际 wire 边界进行
pub fn validate_packet(packet: &TunnelPacket) -> Result<(), WireError> {
    if packet.trace_id == 0 {
        return Err(WireError::InvalidTraceId);
    }
    if packet.kind == PacketType::Unknown {
        return Err(WireError::InvalidPacketType);
    }
    if packet.kind != PacketType::Response
        && (packet.status_code != 0 || !packet.error_message.is_empty())
    {
        return Err(WireError::UnexpectedResponseFields {
            status_code: packet.status_code,
        });
    }
    if packet.action.is_empty() {
        return Err(WireError::ActionEmpty);
    }
    if packet.action.len() > MAX_ACTION_BYTES {
        return Err(WireError::ActionTooLong {
            len: packet.action.len(),
            max: MAX_ACTION_BYTES,
        });
    }
    // 校验 action 格式与保留前缀
    if packet.action.starts_with("tunnel.") {
        match packet.action.as_str() {
            "tunnel.kick" | "tunnel.revoke" => {
                if packet.kind != PacketType::System {
                    return Err(WireError::InvalidAction);
                }
            }
            "tunnel.ping" => {
                if packet.kind != PacketType::Request && packet.kind != PacketType::Response {
                    return Err(WireError::InvalidAction);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    } else {
        // 普通业务 action 必须是由点分隔的段组成，每段非空且仅含小写字母或数字，不得为 tunnel. 前缀
        if packet.action.starts_with('.')
            || packet.action.ends_with('.')
            || packet.action.split('.').any(|seg| {
                seg.is_empty()
                    || seg
                        .chars()
                        .any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            })
        {
            return Err(WireError::InvalidAction);
        }
    }

    // 仅做不会误拒合法包的确定越界检测；非空 action 确保编码信封
    // 在 payload 之外至少占用 1 字节。接收端必须另在 codec 边界严格
    // 按真实整包 wire 长度 <= MAX_ENVELOPE_BYTES 审核。
    if packet.payload.len() >= MAX_ENVELOPE_BYTES {
        return Err(WireError::EnvelopeTooLarge {
            len: packet.payload.len(),
            max: MAX_ENVELOPE_BYTES,
        });
    }

    if packet.metadata.len() > MAX_METADATA_ENTRIES {
        return Err(WireError::MetadataTooMany {
            len: packet.metadata.len(),
            max: MAX_METADATA_ENTRIES,
        });
    }
    for (k, v) in &packet.metadata {
        if k.len() > MAX_METADATA_KEY_BYTES {
            return Err(WireError::MetadataKeyTooLong {
                len: k.len(),
                max: MAX_METADATA_KEY_BYTES,
            });
        }
        if v.len() > MAX_METADATA_VAL_BYTES {
            return Err(WireError::MetadataValTooLong {
                len: v.len(),
                max: MAX_METADATA_VAL_BYTES,
            });
        }
    }
    if packet.action == "tunnel.ping" {
        if packet.payload.len() != PING_NONCE_LEN {
            return Err(WireError::InvalidPingNonceLength(packet.payload.len()));
        }
        match packet.kind {
            PacketType::Request => {
                if !packet.metadata.is_empty() {
                    return Err(WireError::PingRequestMetadataNotEmpty);
                }
            }
            PacketType::Response => {
                if packet.status_code != 0 {
                    return Err(WireError::InvalidPingStatusCode(packet.status_code));
                }
                if !packet.error_message.is_empty() || !packet.metadata.is_empty() {
                    return Err(WireError::InvalidPingResponseFields);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    }
    Ok(())
}

// ====================================================================
// 业务 DTO 契约（03 §2）：Rust 侧类型化业务载荷与无 codec 的 fail-closed 边界。
// 本段只定义结构与校验，不预设任何字节格式（codec 未获批）。
// ====================================================================

// ---- 约束常量（03 §2 字段约束）----
pub const BUSINESS_SCHEMA_VERSION: u32 = 1;
pub const MAX_METRICS: usize = 128;
pub const MAX_METRIC_KEY_BYTES: usize = 64;
pub const MAX_METRIC_UNIT_BYTES: usize = 16;
pub const MAX_ACTOR_VERSION_BYTES: usize = 64;
pub const MAX_REASON_CODE_BYTES: usize = 64;
// MAX_SUB_TASK_ID_BYTES / MAX_EXECUTION_TOKEN_BYTES 为 wire 侧合同常量：Rust 侧
// sub_task_id 一律经 UUIDv4 校验（恒 36 字节）、execution_token 为定长 [u8; 32]，
// 结构上已不可能越界，故不重复设长度检查。
pub const MAX_SUB_TASK_ID_BYTES: usize = 64;
pub const MAX_EXECUTION_TOKEN_BYTES: usize = 64;
pub const MAX_TICKET_TTL_MS: u32 = 30_000; // 03 §5 待审默认，G0 审阅后才可定稿

// ---- 稳定词表（拒绝未知取值）----
pub const TASK_STATES: [&str; 5] = ["accepted", "attempted", "succeeded", "failed", "unknown"];
pub const TASK_REASON_CODES: [&str; 12] = [
    "PERMISSION_DENIED",
    "DEVICE_OFFLINE",
    "CAPABILITY_UNAVAILABLE",
    "DEVICE_BUSY",
    "QUEUE_EXPIRED",
    "TIME_UNCERTAIN",
    "EXECUTION_REJECTED",
    "OS_ERROR",
    "RESULT_TIMEOUT",
    "JOURNAL_LOST",
    "PROTOCOL_MISMATCH",
    "USER_CANCELLED",
];
pub const EVIDENCE_KINDS: [&str; 1] = ["boot_transition_observed"];
pub const METRIC_QUALITIES: [&str; 3] = ["ok", "unavailable", "error"];
pub const CLOCK_QUALITIES: [&str; 2] = ["observed", "clock_unstable"];
// 入站动作白名单仅 6 项；device.task.result 属于仅板端→中控的 TYPE_EVENT，绝不纳入入站支持列表
pub const SUPPORTED_ACTIONS: [&str; 6] = [
    "device.capabilities.get",
    "device.status.get",
    "device.clock.get",
    "device.task.get",
    "device.reboot.prepare",
    "device.reboot.execute",
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
pub struct Query {
    pub schema_version: u32,
}

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

// ---- 统一校验入口 ----
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DtoError {
    #[error("unsupported business schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("empty required field: {0}")]
    EmptyField(&'static str),
    #[error("field too long: {field} = {len} > {max}")]
    FieldTooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
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

// ---- 公共前置校验辅助（fail-closed：未知取值、超长、非有限浮点一律拒绝）----
fn require_version(v: u32) -> Result<(), DtoError> {
    if v == BUSINESS_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(DtoError::UnsupportedVersion(v))
    }
}
fn require_non_empty(field: &'static str, value: &str) -> Result<(), DtoError> {
    if value.trim().is_empty() {
        Err(DtoError::EmptyField(field))
    } else {
        Ok(())
    }
}
fn require_max(field: &'static str, value: &str, max: usize) -> Result<(), DtoError> {
    let len = value.len();
    if len > max {
        Err(DtoError::FieldTooLong { field, len, max })
    } else {
        Ok(())
    }
}
fn require_uuid(field: &'static str, value: &str) -> Result<(), DtoError> {
    require_non_empty(field, value)?;
    if is_canonical_uuid_v4(value) {
        Ok(())
    } else {
        Err(DtoError::InvalidUuid(field))
    }
}
fn require_literal(field: &'static str, value: &str, allowed: &[&str]) -> Result<(), DtoError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(DtoError::InvalidLiteral {
            field,
            value: value.to_string(),
        })
    }
}

/// `Metric` 合同：key/unit 长度、quality 词表、非 ok 不得携带 value、
/// value 必须有限、`bytes` 单位必须为 <= 2^53-1 的非负精确整数。
fn validate_metric(metric: &Metric) -> Result<(), DtoError> {
    require_non_empty("metric.key", &metric.key)?;
    require_max("metric.key", &metric.key, MAX_METRIC_KEY_BYTES)?;
    require_max("metric.unit", &metric.unit, MAX_METRIC_UNIT_BYTES)?;
    require_literal("metric.quality", &metric.quality, &METRIC_QUALITIES)?;
    match metric.quality.as_str() {
        // 非 ok 读数不得携带 value，避免把不可用/错误显示成 0
        "unavailable" | "error" => {
            if metric.value.is_some() {
                return Err(DtoError::ValueOutOfContract(
                    "metric.value_with_non_ok_quality",
                ));
            }
        }
        _ => {
            if metric.value.is_none() {
                return Err(DtoError::ValueOutOfContract(
                    "metric.ok_quality_without_value",
                ));
            }
        }
    }
    if let Some(v) = metric.value {
        if !v.is_finite() {
            return Err(DtoError::ValueOutOfContract("metric.value_not_finite"));
        }
        if metric.unit == "bytes" {
            // 2^53 - 1：超过即无法用 double 精确表示整数
            if v < 0.0 || v.fract() != 0.0 || v > 9_007_199_254_740_991.0 {
                return Err(DtoError::ValueOutOfContract(
                    "metric.bytes_exceeds_max_safe_integer",
                ));
            }
        }
    }
    Ok(())
}

impl BusinessMessage for Query {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)
    }
}

impl BusinessMessage for Capabilities {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("boot_id", &self.boot_id)?;
        require_non_empty("agent_version", &self.agent_version)?;
        require_uuid("journal_epoch", &self.journal_epoch)?;
        require_max(
            "agent_version",
            &self.agent_version,
            MAX_ACTOR_VERSION_BYTES,
        )?;
        // 入站动作白名单仅 SUPPORTED_ACTIONS 6 项；device.task.result 不在其中
        for action in &self.supported_actions {
            require_literal("supported_actions", action, &SUPPORTED_ACTIONS)?;
        }
        Ok(())
    }
}

impl BusinessMessage for DeviceStatus {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("boot_id", &self.boot_id)?;
        require_uuid("agent_epoch", &self.agent_epoch)?;
        require_uuid("clock_epoch", &self.clock_epoch)?;
        if self.sample_seq == 0 {
            return Err(DtoError::ValueOutOfContract("sample_seq"));
        }
        require_literal("clock_quality", &self.clock_quality, &CLOCK_QUALITIES)?;
        if self.metrics.len() > MAX_METRICS {
            return Err(DtoError::ValueOutOfContract("metrics_count"));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.metrics.len());
        for metric in &self.metrics {
            validate_metric(metric)?;
            let key = metric.key.as_str();
            if seen.contains(&key) {
                return Err(DtoError::DuplicateMetricKey(key.to_string()));
            }
            seen.push(key);
        }
        Ok(())
    }
}

impl BusinessMessage for ClockSample {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("boot_id", &self.boot_id)?;
        require_uuid("clock_epoch", &self.clock_epoch)?;
        require_literal("quality", &self.quality, &CLOCK_QUALITIES)?;
        Ok(())
    }
}

impl BusinessMessage for RebootPrepare {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("sub_task_id", &self.sub_task_id)?;
        require_uuid("expected_boot_id", &self.expected_boot_id)?;
        Ok(())
    }
}

/// 票据必须携带非零执行令牌；全零 token 视为缺省伪造，直接拒绝。
fn require_execution_token(token: &[u8; 32]) -> Result<(), DtoError> {
    if token == &[0u8; 32] {
        return Err(DtoError::ValueOutOfContract("execution_token"));
    }
    Ok(())
}

impl BusinessMessage for RebootTicket {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("sub_task_id", &self.sub_task_id)?;
        require_uuid("boot_id", &self.boot_id)?;
        // 上限 MAX_TICKET_TTL_MS 属 03 §5 待审默认，G0 审阅后才可定稿，此处只拒绝对外无效的非正 TTL
        if self.valid_for_ms == 0 {
            return Err(DtoError::ValueOutOfContract("valid_for_ms"));
        }
        require_execution_token(&self.execution_token)?;
        Ok(())
    }
}

impl BusinessMessage for RebootExecute {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("sub_task_id", &self.sub_task_id)?;
        require_uuid("expected_boot_id", &self.expected_boot_id)?;
        require_execution_token(&self.execution_token)?;
        Ok(())
    }
}

/// RebootExecute 规范载荷哈希（规范编码见计划；排除 execution token/nonce）。
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
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

impl BusinessMessage for TaskQuery {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("sub_task_id", &self.sub_task_id)?;
        Ok(())
    }
}

impl BusinessMessage for TaskRecord {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }
    fn validate(&self) -> Result<(), DtoError> {
        require_version(self.schema_version)?;
        require_uuid("sub_task_id", &self.sub_task_id)?;
        require_uuid("journal_epoch", &self.journal_epoch)?;
        require_uuid("pre_boot_id", &self.pre_boot_id)?;
        if self.record_version == 0 {
            return Err(DtoError::ValueOutOfContract("record_version"));
        }
        require_literal("state", &self.state, &TASK_STATES)?;
        if let Some(code) = &self.reason_code {
            require_max("reason_code", code, MAX_REASON_CODE_BYTES)?;
            require_literal("reason_code", code, &TASK_REASON_CODES)?;
        }
        if let Some(boot) = self.observed_boot_id.as_deref() {
            require_uuid("observed_boot_id", boot)?;
        }
        // evidence_kind 的 wire↔Rust 唯一兼容映射（账本决议②）：wire 恒为 proto3 非 optional
        // string，写侧 None → 空串、Some(v) → v；读侧空串 → None、非空 → Some(v)。
        // 词表校验在归一后进行：归一为 None（含空串）视为未提供，跳过词表校验。
        let evidence = self.evidence_kind.as_deref().filter(|v| !v.is_empty());
        if let Some(kind) = evidence {
            require_literal("evidence_kind", kind, &EVIDENCE_KINDS)?;
            // evidence 是完成证据的必要组成：没有 observed_boot_id 不得自称观察到 boot 迁移
            if kind == "boot_transition_observed" && self.observed_boot_id.is_none() {
                return Err(DtoError::EmptyField("observed_boot_id"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessPayloadKind {
    Query,
    Capabilities,
    DeviceStatus,
    ClockSample,
    RebootPrepare,
    RebootTicket,
    RebootExecute,
    TaskQuery,
    TaskRecord,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BusinessPayload {
    Query(Query),
    Capabilities(Capabilities),
    DeviceStatus(DeviceStatus),
    ClockSample(ClockSample),
    RebootPrepare(RebootPrepare),
    RebootTicket(RebootTicket),
    RebootExecute(RebootExecute),
    TaskQuery(TaskQuery),
    TaskRecord(TaskRecord),
}

impl BusinessPayload {
    pub fn kind(&self) -> BusinessPayloadKind {
        match self {
            BusinessPayload::Query(_) => BusinessPayloadKind::Query,
            BusinessPayload::Capabilities(_) => BusinessPayloadKind::Capabilities,
            BusinessPayload::DeviceStatus(_) => BusinessPayloadKind::DeviceStatus,
            BusinessPayload::ClockSample(_) => BusinessPayloadKind::ClockSample,
            BusinessPayload::RebootPrepare(_) => BusinessPayloadKind::RebootPrepare,
            BusinessPayload::RebootTicket(_) => BusinessPayloadKind::RebootTicket,
            BusinessPayload::RebootExecute(_) => BusinessPayloadKind::RebootExecute,
            BusinessPayload::TaskQuery(_) => BusinessPayloadKind::TaskQuery,
            BusinessPayload::TaskRecord(_) => BusinessPayloadKind::TaskRecord,
        }
    }

    pub fn validate(&self) -> Result<(), DtoError> {
        match self {
            BusinessPayload::Query(m) => m.validate(),
            BusinessPayload::Capabilities(m) => m.validate(),
            BusinessPayload::DeviceStatus(m) => m.validate(),
            BusinessPayload::ClockSample(m) => m.validate(),
            BusinessPayload::RebootPrepare(m) => m.validate(),
            BusinessPayload::RebootTicket(m) => m.validate(),
            BusinessPayload::RebootExecute(m) => m.validate(),
            BusinessPayload::TaskQuery(m) => m.validate(),
            BusinessPayload::TaskRecord(m) => m.validate(),
        }
    }
}

pub fn validate_dto<T: BusinessMessage>(message: &T) -> Result<(), DtoError> {
    message.validate()
}

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
    fn encode(
        &self,
        witness: &ApprovedCodecWitness,
        message: &BusinessPayload,
    ) -> Result<Vec<u8>, CodecError>;
    fn decode(
        &self,
        witness: &ApprovedCodecWitness,
        expected: BusinessPayloadKind,
        bytes: &[u8],
    ) -> Result<BusinessPayload, CodecError>;
}

/// 默认实现：恒无有效见证。任何原始字节入口在它面前都必须失败关闭。
pub struct NoCodec;

impl Codec for NoCodec {
    fn encode(
        &self,
        _witness: &ApprovedCodecWitness,
        _message: &BusinessPayload,
    ) -> Result<Vec<u8>, CodecError> {
        Err(CodecError::CodecUnapproved("no approved business codec"))
    }
    fn decode(
        &self,
        _witness: &ApprovedCodecWitness,
        _expected: BusinessPayloadKind,
        _bytes: &[u8],
    ) -> Result<BusinessPayload, CodecError> {
        Err(CodecError::CodecUnapproved("no approved business codec"))
    }
}

/// 结构性 64KiB 建议上限（03 §2），不是 512KiB 整包证明。
///
/// 该判定只基于调用方给出的字节提示，属建议性预算；真实 512 KiB 整包
/// 上限必须在获批 codec 的实际 wire 边界另行审核（见 `validate_packet` 注释）。
pub fn validate_status_budget(bytes_hint: usize) -> Result<(), DtoError> {
    if bytes_hint > 65_536 {
        return Err(DtoError::ValueOutOfContract("device_status_bytes"));
    }
    Ok(())
}

/// 编码前置：DTO 必须自校验通过，且必须携带不可伪造的获批 codec 见证。
///
/// 语义说明：`CodecUnapproved` 在此兼表「无获批 codec 见证」与「DTO 本身不合法」
/// 两种拒绝原因；两者的共同效果都是**原始字节不得被当作有效业务消息接受**，
/// 这正是 fail-closed 的要求。没有有效 `ApprovedCodecWitness` 时，任何外部自制
/// codec 均无法接入编解码。
pub fn encode_business(
    codec: &dyn Codec,
    witness: Option<&ApprovedCodecWitness>,
    message: &BusinessPayload,
) -> Result<Vec<u8>, CodecError> {
    message
        .validate()
        .map_err(|_| CodecError::CodecUnapproved("invalid business payload"))?;
    let witness = witness.ok_or(CodecError::CodecUnapproved("no approved business codec"))?;
    codec.encode(witness, message)
}

/// 解码前置：先做见证守卫，**在触碰 bytes 之前**失败关闭；解码结果还必须
/// 与 `expected` 形状一致并再次自校验通过。
///
/// 语义说明：`CodecUnapproved` 在此兼表「无获批 codec 见证」与「DTO 本身不合法」
/// 两种拒绝原因；两者的共同效果都是**原始字节不得被当作有效业务消息接受**，
/// 这正是 fail-closed 的要求。没有有效 `ApprovedCodecWitness` 时，任何外部自制
/// codec 均无法接入解码。
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
    decoded
        .validate()
        .map_err(|_| CodecError::CodecUnapproved("invalid business payload"))?;
    Ok(decoded)
}

/// 判定「规范的 36 字符小写十六进制 UUIDv4」；不引入任何新依赖。
pub fn is_canonical_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if b != b'-' {
                    return false;
                }
            }
            _ => {
                if !matches!(b, b'0'..=b'9' | b'a'..=b'f') {
                    return false;
                }
            }
        }
    }
    // 版本位必须是 4；variant 位必须是 8/9/a/b
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}
