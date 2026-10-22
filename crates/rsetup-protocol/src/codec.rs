//! 业务 Protobuf codec（03 §2 schema，手写 proto3 canonical wire 编解码）。
//!
//! 字节格式严格按 [`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`] §2
//! 的 proto3 schema 字段号与类型实现，不引入 protobuf/prost 依赖，不使用 `unsafe`。
//!
//! 启用条件：`Codec` 是 sealed trait，且编解码都要求 [`ApprovedCodecWitness`]；
//! 该见证只能由 [`crate::wire::approved_codec_witness`]（项目所有者授权）产出。
//! 无见证时 `encode_business`/`decode_business` 仍按原语义 fail-closed。
//!
//! # canonical 编码（确定性）
//!
//! * 字段号升序写出，重复字段按 `Vec` 顺序连续写出；
//! * proto3 隐式缺省（数值 0、`false`、空串、空 bytes、空 repeated）不写；
//! * `optional` 字段仅 `Some(..)` 才写，且 `Some(0)`/`Some("")` 也必须写（presence 语义）；
//! * varint 最短编码，`optional double` 用 fixed64 小端。
//!
//! 因此同一 DTO 恒得同一字节串；合规 canonical 字节串满足 `encode(decode(b)) == b`。
//!
//! # 严格解码（全部 fail-closed，不回显 payload 内容）
//!
//! 拒绝：未知字段号、重复 singular/optional 字段、错误 wire type、截断的
//! varint/长度/长度前缀、长度超过剩余字节、非最短 varint、u32/varint 溢出、
//! 字段号 > `u32::MAX`、非法 UTF-8、`execution_token` 长度非 32B、顶层消息超过
//! 512 KiB（`LengthLimit`）、`metrics` 数量超过 128（`TooManyMetrics`）、
//! `supported_actions` 数量超过白名单长度或单元素超长（固定去敏的 `InvalidDto`）、
//! 缺失 `schema_version`（03 §1「缺失/0 不作默认兼容」）。
//!
//! 非 optional 字段缺省按 proto3 默认值（空串/0/`false`）填充，其合法性由 DTO
//! 自校验决定；DTO 失败经 [`dto_rejected`] 分类：版本不受支持 →
//! [`CodecError::UnsupportedSchemaVersion`]（可协商的版本错误），其余 →
//! 去敏的 [`CodecError::InvalidDto`] 固定标签。

use crate::wire::{
    ApprovedCodecWitness, BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind,
    Capabilities, ClockSample, CodecError, DeviceStatus, DtoError, MAX_ACTION_BYTES,
    MAX_ENVELOPE_BYTES, MAX_METRICS, Metric, Query, RebootExecute, RebootPrepare, RebootTicket,
    SUPPORTED_ACTIONS, TaskQuery, TaskRecord, sealed,
};

/// `supported_actions` 的数量上限：入站动作白名单长度（当前 6 项）。
/// 解码端在分配 `String` 之前就用它封顶，避免重复字段被用来放大分配。
const MAX_SUPPORTED_ACTIONS: usize = SUPPORTED_ACTIONS.len();

/// 顶层业务消息字节上限（与 03 §2「所有消息仍受传输整包 ≤512KiB」一致）。
pub const MAX_BUSINESS_MESSAGE_BYTES: usize = MAX_ENVELOPE_BYTES;

/// `Metric.value` 的 wire type 简写。
mod wt {
    pub const VARINT: u8 = 0;
    pub const FIXED64: u8 = 1;
    pub const LEN: u8 = 2;
    // wire type 3/4/5/6/7（groups、fixed32、保留）全部由 `read_tag` 拒绝。
}

/// `execution_token` 在 wire 上是 `bytes`，Rust 侧为定长 `[u8; 32]`（03 §4 B-03）。
const EXECUTION_TOKEN_LEN: usize = 32;

/// 已批准的业务 Protobuf codec（所有者授权，见 `wire::approved_codec_witness`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusinessCodec;

impl sealed::Sealed for BusinessCodec {}

// ====================================================================
// 写入端
// ====================================================================

/// 确定性写入器：调用顺序即字节顺序，故编码方必须按字段号升序调用。
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    fn with_capacity(cap: usize) -> Self {
        Writer {
            buf: Vec::with_capacity(cap),
        }
    }

    fn varint(&mut self, mut value: u64) {
        loop {
            if value < 0x80 {
                self.buf.push(value as u8);
                return;
            }
            self.buf.push(((value as u8) & 0x7f) | 0x80);
            value >>= 7;
        }
    }

    fn tag(&mut self, field: u32, wire_type: u8) {
        self.varint((u64::from(field) << 3) | u64::from(wire_type));
    }

    /// proto3 隐式缺省：0 不写。
    fn uint32(&mut self, field: u32, value: u32) {
        if value != 0 {
            self.tag(field, wt::VARINT);
            self.varint(u64::from(value));
        }
    }

    fn uint64(&mut self, field: u32, value: u64) {
        if value != 0 {
            self.tag(field, wt::VARINT);
            self.varint(value);
        }
    }

    /// int64 负数按 64 位二补码写成（最长）10 字节 varint，protobuf 规范允许。
    fn int64(&mut self, field: u32, value: i64) {
        if value != 0 {
            self.tag(field, wt::VARINT);
            self.varint(value as u64);
        }
    }

    fn bool(&mut self, field: u32, value: bool) {
        if value {
            self.tag(field, wt::VARINT);
            self.varint(1);
        }
    }

    /// 非 optional string：空串按 proto3 缺省不写。
    fn string(&mut self, field: u32, value: &str) {
        if value.is_empty() {
            return;
        }
        self.bytes(field, value.as_bytes());
    }

    /// 非 optional bytes：空 bytes 按 proto3 缺省不写。
    fn bytes(&mut self, field: u32, value: &[u8]) {
        if value.is_empty() {
            return;
        }
        self.tag(field, wt::LEN);
        self.varint(value.len() as u64);
        self.buf.extend_from_slice(value);
    }

    /// `optional string`：presence 语义，`Some("")` 也必须写（0 长度）。
    fn opt_string(&mut self, field: u32, value: Option<&str>) {
        if let Some(value) = value {
            self.tag(field, wt::LEN);
            self.varint(value.len() as u64);
            self.buf.extend_from_slice(value.as_bytes());
        }
    }

    /// `optional int64`：`Some(0)` 也必须写。
    fn opt_int64(&mut self, field: u32, value: Option<i64>) {
        if let Some(value) = value {
            self.tag(field, wt::VARINT);
            self.varint(value as u64);
        }
    }

    /// `optional double`：fixed64 小端；`Some(..)` 才写。
    fn opt_double(&mut self, field: u32, value: Option<f64>) {
        if let Some(value) = value {
            self.tag(field, wt::FIXED64);
            self.buf.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// 嵌套 message：空子消息也要写（0 长度）。
    fn message(&mut self, field: u32, encoded: &[u8]) {
        self.tag(field, wt::LEN);
        self.varint(encoded.len() as u64);
        self.buf.extend_from_slice(encoded);
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

// ====================================================================
// 读取端
// ====================================================================

/// 只前进、零拷贝借用的 wire 读取器。所有失败都返回去敏的固定原因。
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn byte(&mut self) -> Result<u8, CodecError> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(CodecError::Malformed("truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    /// 严格 varint：≤10 字节、最短编码、不溢出 u64。
    fn varint(&mut self) -> Result<u64, CodecError> {
        let mut acc: u64 = 0;
        let mut index = 0usize;
        loop {
            let b = self.byte()?;
            let payload = u64::from(b & 0x7f);
            if index == 9 && payload > 1 {
                return Err(CodecError::Malformed("varint overflows u64"));
            }
            acc |= payload << (7 * index as u32);
            index += 1;
            if b & 0x80 == 0 {
                // 非最短编码：末字节贡献 0 个信息位（等价于多写补零的高位组）。
                // 只判末字节：0x80 0x80 0x01（16384）是最短编码，中间 0 载荷合法。
                if index > 1 && payload == 0 {
                    return Err(CodecError::Malformed("non-canonical varint"));
                }
                return Ok(acc);
            }
            if index == 10 {
                // 第 10 字节仍带 continuation：varint 超过 10 字节。
                return Err(CodecError::Malformed("varint too long"));
            }
        }
    }

    fn uint32(&mut self) -> Result<u32, CodecError> {
        let v = self.varint()?;
        u32::try_from(v).map_err(|_| CodecError::Malformed("varint out of range for uint32"))
    }

    fn uint64(&mut self) -> Result<u64, CodecError> {
        self.varint()
    }

    fn int64(&mut self) -> Result<i64, CodecError> {
        Ok(self.varint()? as i64)
    }

    fn bool(&mut self) -> Result<bool, CodecError> {
        match self.varint()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CodecError::Malformed("invalid bool")),
        }
    }

    fn fixed64_bits(&mut self) -> Result<[u8; 8], CodecError> {
        let end = self
            .pos
            .checked_add(8)
            .ok_or(CodecError::Malformed("truncated"))?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(CodecError::Malformed("truncated"))?;
        self.pos = end;
        let mut out = [0u8; 8];
        out.copy_from_slice(slice);
        Ok(out)
    }

    fn double(&mut self) -> Result<f64, CodecError> {
        let bits = self.fixed64_bits()?;
        Ok(f64::from_le_bytes(bits))
    }

    /// 长度前缀 + 负载；拒绝长度越界与超过剩余字节。
    fn length_delimited(&mut self) -> Result<&'a [u8], CodecError> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| CodecError::Malformed("length out of range"))?;
        if len > self.remaining() {
            return Err(CodecError::Malformed("length exceeds remaining bytes"));
        }
        let end = self.pos + len;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn string(&mut self) -> Result<String, CodecError> {
        let slice = self.length_delimited()?;
        String::from_utf8(slice.to_vec()).map_err(|_| CodecError::InvalidUtf8)
    }

    /// 校验 wire type 并推进到 payload。
    fn expect(&mut self, wire_type: u8, want: u8) -> Result<(), CodecError> {
        if wire_type == want {
            Ok(())
        } else {
            Err(CodecError::Malformed("unexpected wire type"))
        }
    }
}

/// singular/optional 字段重复检测位图（本 schema 字段号均 ≤ 10）。
#[derive(Default)]
struct Seen(u32);

impl Seen {
    fn mark(&mut self, field: u32) -> Result<(), CodecError> {
        // 本 schema 字段号 ≤ 10；更大字段号一律由调用方先判为 UnknownField，
        // 这里的位图容量守卫只是兜底（不会 panic、不会移位溢出）。
        if field == 0 || field > u32::BITS {
            return Err(CodecError::UnknownField(field));
        }
        let mask = 1u32 << (field - 1);
        if self.0 & mask != 0 {
            return Err(CodecError::DuplicateField(field));
        }
        self.0 |= mask;
        Ok(())
    }
}

/// 每消息统一的 tag 读取：字段号 0、字段号越过 `u32::MAX`、保留 wire type 直接拒绝。
fn read_tag(reader: &mut Reader<'_>) -> Result<(u32, u8), CodecError> {
    let tag = reader.varint()?;
    let wire_type = (tag & 0b111) as u8;
    let field = tag >> 3;
    if field == 0 {
        return Err(CodecError::Malformed("zero field number"));
    }
    // 字段号越界是**字节流不合法**，不得谎报成 `UnknownField(0)`：那会同时
    // 伪造出「字段号 0」这个本应被拒绝的取值，并掩盖真实的越界原因。
    let field =
        u32::try_from(field).map_err(|_| CodecError::Malformed("field number out of range"))?;
    match wire_type {
        wt::VARINT | wt::FIXED64 | wt::LEN => Ok((field, wire_type)),
        _ => Err(CodecError::Malformed("unsupported wire type")),
    }
}

/// 顶层消息统一的长度上限守卫：**只**表示消息（含嵌套子消息）字节数越过
/// 512 KiB 整包上限；集合条目数量上限另见 `CodecError::TooManyMetrics`。
fn check_size(bytes: &[u8]) -> Result<(), CodecError> {
    if bytes.len() > MAX_BUSINESS_MESSAGE_BYTES {
        return Err(CodecError::LengthLimit);
    }
    Ok(())
}

/// `schema_version` 缺失即拒绝：03 §1「缺失/0 不作默认兼容」。
fn require_version(version: Option<u32>) -> Result<u32, CodecError> {
    version.ok_or(CodecError::MissingField("schema_version"))
}

/// `supported_actions` 单元素读取：**先**做长度守卫再分配 `String`，
/// 使超长元素根本不会进入堆分配；返回固定去敏标签，不回显内容。
/// 上限取 `MAX_ACTION_BYTES`（业务 action 的 wire 合同上限），比入站白名单
/// 最长项更宽松，因此不会误拒任何合法动作。
fn read_supported_action(reader: &mut Reader<'_>) -> Result<String, CodecError> {
    let raw = reader.length_delimited()?;
    if raw.len() > MAX_ACTION_BYTES {
        return Err(CodecError::InvalidDto("action_too_long"));
    }
    String::from_utf8(raw.to_vec()).map_err(|_| CodecError::InvalidUtf8)
}

/// DTO 自校验失败 → 分类后的去敏 `CodecError`（不带出 payload 内容、不回显取值）。
///
/// * 版本不受支持 → [`CodecError::UnsupportedSchemaVersion`]：这是可协商的**版本**
///   错误（上层据此回「版本/能力不支持」），必须与「参数非法」区分开；
/// * 其余契约违规 → [`CodecError::InvalidDto`] 的固定标签。
///
/// `pub(crate)`：`wire::encode_business`/`decode_business` 的前置 DTO 校验必须走
/// **同一套**映射，否则同一条违规在两条路径上会得到不同分类。
pub(crate) fn dto_rejected(error: &DtoError) -> CodecError {
    if let DtoError::UnsupportedVersion(version) = error {
        return CodecError::UnsupportedSchemaVersion(*version);
    }
    let tag: &'static str = match error {
        DtoError::EmptyField(_) => "empty_field",
        DtoError::FieldTooLong { .. } => "field_too_long",
        DtoError::InvalidUuid(_) => "invalid_uuid",
        DtoError::InvalidLiteral { .. } => "invalid_literal",
        DtoError::ValueOutOfContract(_) => "value_out_of_contract",
        DtoError::DuplicateMetricKey(_) => "duplicate_metric_key",
        // 版本不受支持已在上方单独分流；此分支只为穷尽性存在，永不可达。
        DtoError::UnsupportedVersion(_) => "unsupported_version",
    };
    CodecError::InvalidDto(tag)
}

fn check_payload(
    expected: BusinessPayloadKind,
    payload: BusinessPayload,
) -> Result<BusinessPayload, CodecError> {
    if payload.kind() != expected {
        return Err(CodecError::Malformed("payload kind mismatch"));
    }
    payload.validate().map_err(|error| dto_rejected(&error))?;
    Ok(payload)
}

// ====================================================================
// 逐消息编码
// ====================================================================

fn encode_query(message: &Query) -> Vec<u8> {
    let mut w = Writer::new();
    w.uint32(1, message.schema_version);
    w.finish()
}

fn encode_capabilities(message: &Capabilities) -> Vec<u8> {
    let mut w = Writer::with_capacity(128);
    w.uint32(1, message.schema_version);
    w.string(2, &message.boot_id);
    w.string(3, &message.agent_version);
    for action in &message.supported_actions {
        w.string(4, action);
    }
    w.bool(5, message.durable_task_journal);
    w.uint32(6, message.max_status_bytes);
    w.string(7, &message.journal_epoch);
    w.finish()
}

fn encode_metric(message: &Metric) -> Vec<u8> {
    let mut w = Writer::with_capacity(48);
    w.string(1, &message.key);
    w.opt_double(2, message.value);
    w.string(3, &message.unit);
    w.string(4, &message.quality);
    w.finish()
}

fn encode_device_status(message: &DeviceStatus) -> Vec<u8> {
    let mut w = Writer::with_capacity(256);
    w.uint32(1, message.schema_version);
    w.string(2, &message.boot_id);
    w.string(3, &message.agent_epoch);
    w.uint64(4, message.sample_seq);
    w.int64(5, message.sampled_unix_ms);
    w.uint64(6, message.sampled_monotonic_ns);
    w.string(7, &message.clock_epoch);
    for metric in &message.metrics {
        w.message(8, &encode_metric(metric));
    }
    w.string(9, &message.clock_quality);
    w.finish()
}

fn encode_clock_sample(message: &ClockSample) -> Vec<u8> {
    let mut w = Writer::with_capacity(128);
    w.uint32(1, message.schema_version);
    w.string(2, &message.boot_id);
    w.string(3, &message.clock_epoch);
    w.int64(4, message.receive_unix_ms);
    w.int64(5, message.send_unix_ms);
    w.uint64(6, message.receive_monotonic_ns);
    w.uint64(7, message.send_monotonic_ns);
    w.string(8, &message.quality);
    w.finish()
}

fn encode_reboot_prepare(message: &RebootPrepare) -> Vec<u8> {
    let mut w = Writer::with_capacity(96);
    w.uint32(1, message.schema_version);
    w.string(2, &message.sub_task_id);
    w.string(3, &message.expected_boot_id);
    w.finish()
}

fn encode_reboot_ticket(message: &RebootTicket) -> Vec<u8> {
    let mut w = Writer::with_capacity(128);
    w.uint32(1, message.schema_version);
    w.string(2, &message.sub_task_id);
    w.string(3, &message.boot_id);
    w.bytes(4, &message.execution_token);
    w.uint32(5, message.valid_for_ms);
    w.finish()
}

fn encode_reboot_execute(message: &RebootExecute) -> Vec<u8> {
    let mut w = Writer::with_capacity(128);
    w.uint32(1, message.schema_version);
    w.string(2, &message.sub_task_id);
    w.string(3, &message.expected_boot_id);
    w.bytes(4, &message.execution_token);
    w.finish()
}

fn encode_task_query(message: &TaskQuery) -> Vec<u8> {
    let mut w = Writer::with_capacity(64);
    w.uint32(1, message.schema_version);
    w.string(2, &message.sub_task_id);
    w.finish()
}

fn encode_task_record(message: &TaskRecord) -> Vec<u8> {
    let mut w = Writer::with_capacity(192);
    w.uint32(1, message.schema_version);
    w.string(2, &message.sub_task_id);
    w.string(3, &message.journal_epoch);
    w.uint64(4, message.record_version);
    w.string(5, &message.state);
    w.string(6, &message.pre_boot_id);
    w.opt_string(7, message.observed_boot_id.as_deref());
    w.opt_string(8, message.reason_code.as_deref());
    w.opt_int64(9, message.updated_unix_ms);
    // evidence_kind 在 wire 上是非 optional string（账本决议②的唯一兼容映射）：
    // None 与 Some("") 都写空串（即按 proto3 缺省省略），Some(非空) 原样写出。
    w.string(10, message.evidence_kind.as_deref().unwrap_or(""));
    w.finish()
}

// ====================================================================
// 逐消息解码
// ====================================================================

fn decode_query(bytes: &[u8]) -> Result<Query, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
            }
            other => return Err(CodecError::UnknownField(other)),
        }
        seen.mark(field)?;
    }
    Ok(Query {
        schema_version: require_version(schema_version)?,
    })
}

fn decode_capabilities(bytes: &[u8]) -> Result<Capabilities, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut boot_id = String::new();
    let mut agent_version = String::new();
    let mut supported_actions: Vec<String> = Vec::new();
    let mut durable_task_journal = false;
    let mut max_status_bytes: u32 = 0;
    let mut journal_epoch = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                boot_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                agent_version = reader.string()?;
                seen.mark(field)?;
            }
            // repeated：允许多次出现，不做 singular 重复检测；但**数量与单元素
            // 长度都在 `String` 分配之前封顶**——入站白名单只有
            // `MAX_SUPPORTED_ACTIONS` 项，重复字段不得被用来放大分配。
            4 => {
                reader.expect(wire_type, wt::LEN)?;
                if supported_actions.len() >= MAX_SUPPORTED_ACTIONS {
                    return Err(CodecError::InvalidDto("too_many_actions"));
                }
                supported_actions.push(read_supported_action(&mut reader)?);
            }
            5 => {
                reader.expect(wire_type, wt::VARINT)?;
                durable_task_journal = reader.bool()?;
                seen.mark(field)?;
            }
            6 => {
                reader.expect(wire_type, wt::VARINT)?;
                max_status_bytes = reader.uint32()?;
                seen.mark(field)?;
            }
            7 => {
                reader.expect(wire_type, wt::LEN)?;
                journal_epoch = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(Capabilities {
        schema_version: require_version(schema_version)?,
        boot_id,
        agent_version,
        supported_actions,
        durable_task_journal,
        max_status_bytes,
        journal_epoch,
    })
}

fn decode_metric(bytes: &[u8]) -> Result<Metric, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut key = String::new();
    let mut value: Option<f64> = None;
    let mut unit = String::new();
    let mut quality = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::LEN)?;
                key = reader.string()?;
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::FIXED64)?;
                value = Some(reader.double()?);
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                unit = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::LEN)?;
                quality = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(Metric {
        key,
        value,
        unit,
        quality,
    })
}

fn decode_device_status(bytes: &[u8]) -> Result<DeviceStatus, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut boot_id = String::new();
    let mut agent_epoch = String::new();
    let mut sample_seq: u64 = 0;
    let mut sampled_unix_ms: i64 = 0;
    let mut sampled_monotonic_ns: u64 = 0;
    let mut clock_epoch = String::new();
    let mut metrics: Vec<Metric> = Vec::new();
    let mut clock_quality = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                boot_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                agent_epoch = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::VARINT)?;
                sample_seq = reader.uint64()?;
                seen.mark(field)?;
            }
            5 => {
                reader.expect(wire_type, wt::VARINT)?;
                sampled_unix_ms = reader.int64()?;
                seen.mark(field)?;
            }
            6 => {
                reader.expect(wire_type, wt::VARINT)?;
                sampled_monotonic_ns = reader.uint64()?;
                seen.mark(field)?;
            }
            7 => {
                reader.expect(wire_type, wt::LEN)?;
                clock_epoch = reader.string()?;
                seen.mark(field)?;
            }
            8 => {
                reader.expect(wire_type, wt::LEN)?;
                // 数量上限是独立的**资源数量**约束：第 MAX_METRICS+1 条即使在
                // 512 KiB 以内也必须拒，且不得借用只表示整包长度的 `LengthLimit`。
                if metrics.len() >= MAX_METRICS {
                    return Err(CodecError::TooManyMetrics);
                }
                metrics.push(decode_metric(reader.length_delimited()?)?);
            }
            9 => {
                reader.expect(wire_type, wt::LEN)?;
                clock_quality = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(DeviceStatus {
        schema_version: require_version(schema_version)?,
        boot_id,
        agent_epoch,
        sample_seq,
        sampled_unix_ms,
        sampled_monotonic_ns,
        clock_epoch,
        metrics,
        clock_quality,
    })
}

fn decode_clock_sample(bytes: &[u8]) -> Result<ClockSample, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut boot_id = String::new();
    let mut clock_epoch = String::new();
    let mut receive_unix_ms: i64 = 0;
    let mut send_unix_ms: i64 = 0;
    let mut receive_monotonic_ns: u64 = 0;
    let mut send_monotonic_ns: u64 = 0;
    let mut quality = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                boot_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                clock_epoch = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::VARINT)?;
                receive_unix_ms = reader.int64()?;
                seen.mark(field)?;
            }
            5 => {
                reader.expect(wire_type, wt::VARINT)?;
                send_unix_ms = reader.int64()?;
                seen.mark(field)?;
            }
            6 => {
                reader.expect(wire_type, wt::VARINT)?;
                receive_monotonic_ns = reader.uint64()?;
                seen.mark(field)?;
            }
            7 => {
                reader.expect(wire_type, wt::VARINT)?;
                send_monotonic_ns = reader.uint64()?;
                seen.mark(field)?;
            }
            8 => {
                reader.expect(wire_type, wt::LEN)?;
                quality = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(ClockSample {
        schema_version: require_version(schema_version)?,
        boot_id,
        clock_epoch,
        receive_unix_ms,
        send_unix_ms,
        receive_monotonic_ns,
        send_monotonic_ns,
        quality,
    })
}

fn decode_reboot_prepare(bytes: &[u8]) -> Result<RebootPrepare, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut sub_task_id = String::new();
    let mut expected_boot_id = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                sub_task_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                expected_boot_id = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(RebootPrepare {
        schema_version: require_version(schema_version)?,
        sub_task_id,
        expected_boot_id,
    })
}

/// `bytes execution_token` → 定长 `[u8; 32]`；长度不符即拒绝。
fn decode_execution_token(raw: &[u8]) -> Result<[u8; 32], CodecError> {
    if raw.len() != EXECUTION_TOKEN_LEN {
        return Err(CodecError::Malformed("execution_token length"));
    }
    let mut out = [0u8; EXECUTION_TOKEN_LEN];
    out.copy_from_slice(raw);
    Ok(out)
}

fn decode_reboot_ticket(bytes: &[u8]) -> Result<RebootTicket, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut sub_task_id = String::new();
    let mut boot_id = String::new();
    let mut execution_token = [0u8; EXECUTION_TOKEN_LEN];
    let mut valid_for_ms: u32 = 0;
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                sub_task_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                boot_id = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::LEN)?;
                execution_token = decode_execution_token(reader.length_delimited()?)?;
                seen.mark(field)?;
            }
            5 => {
                reader.expect(wire_type, wt::VARINT)?;
                valid_for_ms = reader.uint32()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(RebootTicket {
        schema_version: require_version(schema_version)?,
        sub_task_id,
        boot_id,
        execution_token,
        valid_for_ms,
    })
}

fn decode_reboot_execute(bytes: &[u8]) -> Result<RebootExecute, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut sub_task_id = String::new();
    let mut expected_boot_id = String::new();
    let mut execution_token = [0u8; EXECUTION_TOKEN_LEN];
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                sub_task_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                expected_boot_id = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::LEN)?;
                execution_token = decode_execution_token(reader.length_delimited()?)?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(RebootExecute {
        schema_version: require_version(schema_version)?,
        sub_task_id,
        expected_boot_id,
        execution_token,
    })
}

fn decode_task_query(bytes: &[u8]) -> Result<TaskQuery, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut sub_task_id = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                sub_task_id = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(TaskQuery {
        schema_version: require_version(schema_version)?,
        sub_task_id,
    })
}

fn decode_task_record(bytes: &[u8]) -> Result<TaskRecord, CodecError> {
    check_size(bytes)?;
    let mut reader = Reader::new(bytes);
    let mut seen = Seen::default();
    let mut schema_version: Option<u32> = None;
    let mut sub_task_id = String::new();
    let mut journal_epoch = String::new();
    let mut record_version: u64 = 0;
    let mut state = String::new();
    let mut pre_boot_id = String::new();
    let mut observed_boot_id: Option<String> = None;
    let mut reason_code: Option<String> = None;
    let mut updated_unix_ms: Option<i64> = None;
    let mut evidence_kind = String::new();
    while !reader.eof() {
        let (field, wire_type) = read_tag(&mut reader)?;
        match field {
            1 => {
                reader.expect(wire_type, wt::VARINT)?;
                schema_version = Some(reader.uint32()?);
                seen.mark(field)?;
            }
            2 => {
                reader.expect(wire_type, wt::LEN)?;
                sub_task_id = reader.string()?;
                seen.mark(field)?;
            }
            3 => {
                reader.expect(wire_type, wt::LEN)?;
                journal_epoch = reader.string()?;
                seen.mark(field)?;
            }
            4 => {
                reader.expect(wire_type, wt::VARINT)?;
                record_version = reader.uint64()?;
                seen.mark(field)?;
            }
            5 => {
                reader.expect(wire_type, wt::LEN)?;
                state = reader.string()?;
                seen.mark(field)?;
            }
            6 => {
                reader.expect(wire_type, wt::LEN)?;
                pre_boot_id = reader.string()?;
                seen.mark(field)?;
            }
            7 => {
                reader.expect(wire_type, wt::LEN)?;
                observed_boot_id = Some(reader.string()?);
                seen.mark(field)?;
            }
            8 => {
                reader.expect(wire_type, wt::LEN)?;
                reason_code = Some(reader.string()?);
                seen.mark(field)?;
            }
            9 => {
                reader.expect(wire_type, wt::VARINT)?;
                updated_unix_ms = Some(reader.int64()?);
                seen.mark(field)?;
            }
            10 => {
                reader.expect(wire_type, wt::LEN)?;
                evidence_kind = reader.string()?;
                seen.mark(field)?;
            }
            other => return Err(CodecError::UnknownField(other)),
        }
    }
    Ok(TaskRecord {
        schema_version: require_version(schema_version)?,
        sub_task_id,
        journal_epoch,
        record_version,
        state,
        pre_boot_id,
        observed_boot_id,
        reason_code,
        updated_unix_ms,
        // evidence_kind 的非 optional↔Option 唯一映射：缺省/空串 → None，非空 → Some。
        evidence_kind: (!evidence_kind.is_empty()).then_some(evidence_kind),
    })
}

// ====================================================================
// Codec 实现
// ====================================================================

impl BusinessCodec {
    /// 编码为 03 §2 的 canonical 字节（不做 DTO 校验；校验由
    /// `encode_business`/`decode` 负责）。
    fn encode_payload(message: &BusinessPayload) -> Vec<u8> {
        match message {
            BusinessPayload::Query(m) => encode_query(m),
            BusinessPayload::Capabilities(m) => encode_capabilities(m),
            BusinessPayload::DeviceStatus(m) => encode_device_status(m),
            BusinessPayload::ClockSample(m) => encode_clock_sample(m),
            BusinessPayload::RebootPrepare(m) => encode_reboot_prepare(m),
            BusinessPayload::RebootTicket(m) => encode_reboot_ticket(m),
            BusinessPayload::RebootExecute(m) => encode_reboot_execute(m),
            BusinessPayload::TaskQuery(m) => encode_task_query(m),
            BusinessPayload::TaskRecord(m) => encode_task_record(m),
        }
    }

    fn decode_payload(
        expected: BusinessPayloadKind,
        bytes: &[u8],
    ) -> Result<BusinessPayload, CodecError> {
        let payload = match expected {
            BusinessPayloadKind::Query => BusinessPayload::Query(decode_query(bytes)?),
            BusinessPayloadKind::Capabilities => {
                BusinessPayload::Capabilities(decode_capabilities(bytes)?)
            }
            BusinessPayloadKind::DeviceStatus => {
                BusinessPayload::DeviceStatus(decode_device_status(bytes)?)
            }
            BusinessPayloadKind::ClockSample => {
                BusinessPayload::ClockSample(decode_clock_sample(bytes)?)
            }
            BusinessPayloadKind::RebootPrepare => {
                BusinessPayload::RebootPrepare(decode_reboot_prepare(bytes)?)
            }
            BusinessPayloadKind::RebootTicket => {
                BusinessPayload::RebootTicket(decode_reboot_ticket(bytes)?)
            }
            BusinessPayloadKind::RebootExecute => {
                BusinessPayload::RebootExecute(decode_reboot_execute(bytes)?)
            }
            BusinessPayloadKind::TaskQuery => BusinessPayload::TaskQuery(decode_task_query(bytes)?),
            BusinessPayloadKind::TaskRecord => {
                BusinessPayload::TaskRecord(decode_task_record(bytes)?)
            }
        };
        Ok(payload)
    }
}

impl crate::wire::Codec for BusinessCodec {
    fn encode(
        &self,
        _witness: &ApprovedCodecWitness,
        message: &BusinessPayload,
    ) -> Result<Vec<u8>, CodecError> {
        // DTO 必须自洽才允许产出字节：`encode_business` 已前置校验，此处再兜一次。
        message.validate().map_err(|error| dto_rejected(&error))?;
        // 与解码端对称：不得产出解码端必然拒绝的字节。`supported_actions` 的分配
        // 守卫在 `decode_capabilities` 里，编码端同步封顶（DTO 词表已保证取值合法，
        // 越界的只可能是重复项）。
        if let BusinessPayload::Capabilities(message) = message {
            if message.supported_actions.len() > MAX_SUPPORTED_ACTIONS {
                return Err(CodecError::InvalidDto("too_many_actions"));
            }
        }
        let bytes = Self::encode_payload(message);
        check_size(&bytes)?;
        Ok(bytes)
    }

    fn decode(
        &self,
        _witness: &ApprovedCodecWitness,
        expected: BusinessPayloadKind,
        bytes: &[u8],
    ) -> Result<BusinessPayload, CodecError> {
        check_size(bytes)?;
        let payload = Self::decode_payload(expected, bytes)?;
        check_payload(expected, payload)
    }
}

/// 版本自检：常量与 DTO 契约同步（避免 schema_version 常量漂移）。
const _: () = assert!(BUSINESS_SCHEMA_VERSION == 1);
