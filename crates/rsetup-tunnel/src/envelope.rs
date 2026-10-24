//! TunnelPacket 信封编解码与校验。
//!
//! 安全边界（与 §6.4 一致）：
//! - 公共入口 `encode` / `decode` 自身即安全：`encode` 先做字段预算与 checked 长度计算，
//!   再分配/复制；`decode` 在构造任何字符串之前先校验对端声明长度的预算，并在返回前执行
//!   与 `validate_fields` 相同的语义校验。**不依赖调用方记得再调用 `validate`。**
//! - `validate(stream)` = 公共字段校验 + 按流种类（Control/Data）的路由规则。
//! - 错误枚举不保存也不 Display 对端 metadata / 字符串原文，只携带受限数字。

use std::collections::BTreeMap;
use thiserror::Error;

/// 信封与编解码错误。
///
/// 该枚举刻意只携带 `&'static str` 或数字：任何来自对端的字节（metadata key/value、
/// action、error_message）都不会进入错误文案，避免控制字符与敏感内容回显。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("message too large: {0} bytes (max 524288)")]
    MessageTooLarge(usize),

    #[error("truncated wire format")]
    Truncated,

    #[error("malformed wire format: {0}")]
    Malformed(&'static str),

    #[error("non-canonical varint")]
    NonCanonicalVarint,

    #[error("varint overflows u64")]
    VarintOverflow,

    #[error("varint too long")]
    VarintTooLong,

    #[error("invalid wire type: {0}")]
    InvalidWireType(u8),

    #[error("unknown protobuf field: {0}")]
    UnknownField(u32),

    #[error("duplicate field: {0}")]
    DuplicateField(u32),

    /// 重复 metadata key。只携带出现序号（0 基，必 < `MAX_METADATA_ENTRIES`），不回显 key。
    #[error("duplicate metadata key at entry index {0}")]
    DuplicateMetadataKey(usize),

    #[error("invalid utf-8 string")]
    InvalidUtf8,

    #[error("invalid packet type: {0}")]
    InvalidPacketType(u32),

    #[error("int32 value out of range for field status_code")]
    Int32OutOfRange,

    #[error("action length {0} exceeds budget {1}")]
    ActionTooLarge(usize, usize),

    #[error("metadata entry count exceeds budget {0}")]
    MetadataTooManyEntries(usize),

    #[error("metadata key length {0} exceeds budget {1}")]
    MetadataKeyTooLarge(usize, usize),

    #[error("metadata value length {0} exceeds budget {1}")]
    MetadataValueTooLarge(usize, usize),

    #[error("metadata total size exceeds budget {0}")]
    MetadataTotalTooLarge(usize),

    #[error("field length {0} exceeds budget {1}")]
    FieldTooLarge(usize, usize),

    #[error("encoded size computation overflowed")]
    SizeOverflow,

    #[error("validation error: {0}")]
    Validation(&'static str),
}

/// 流种类：控制流或数据流。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Control,
    Data,
}

/// 信元类型 (proto3 enum PacketType)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Request = 1,
    Response = 2,
    Event = 3,
    System = 4,
}

impl PacketType {
    pub fn from_u32(val: u32) -> Result<Self, EnvelopeError> {
        match val {
            1 => Ok(PacketType::Request),
            2 => Ok(PacketType::Response),
            3 => Ok(PacketType::Event),
            4 => Ok(PacketType::System),
            _ => Err(EnvelopeError::InvalidPacketType(val)),
        }
    }

    pub fn to_u32(self) -> u32 {
        self as u32
    }
}

/// 通用信元信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelPacket {
    pub trace_id: u64,
    pub packet_type: PacketType,
    pub action: String,
    pub status_code: i32,
    pub error_message: String,
    pub payload: Vec<u8>,
    pub metadata: BTreeMap<String, String>,
}

/// 整包上限：512 KiB = 524,288 字节。
pub const MAX_ENVELOPE_SIZE: usize = 512 * 1024;
pub const MAX_ACTION_LEN: usize = 64;
pub const MAX_METADATA_ENTRIES: usize = 16;
pub const MAX_METADATA_KEY_LEN: usize = 64;
pub const MAX_METADATA_VALUE_LEN: usize = 256;
/// metadata 累计字节预算（小常量）：任何一侧都不得分配超过该值的 metadata。
pub const MAX_METADATA_TOTAL_BYTES: usize =
    MAX_METADATA_ENTRIES * (MAX_METADATA_KEY_LEN + MAX_METADATA_VALUE_LEN);
/// 单个 map entry 的 wire 预算：
/// key 字段 `1 tag + 1 长度 + 64`，value 字段 `1 tag + 2 长度 + 256`（256 需 2 字节 varint）。
pub const MAX_METADATA_ENTRY_BYTES: usize =
    (1 + 1 + MAX_METADATA_KEY_LEN) + (1 + 2 + MAX_METADATA_VALUE_LEN);

impl TunnelPacket {
    /// 公共字段/语义校验：与流种类无关的全部规则，`encode` 与 `decode` 都会执行。
    ///
    /// 规则顺序固定：通用 `status_code`/`error_message` 规则先于 `tunnel.ping` 专用规则，
    /// 以免 ping 分支提前返回而绕过非 Response 禁止携带错误字段的约束。
    pub fn validate_fields(&self) -> Result<(), EnvelopeError> {
        // 1. trace_id 必须非零
        if self.trace_id == 0 {
            return Err(EnvelopeError::Validation("trace_id must be non-zero"));
        }

        // 2. action：非空、≤64B、小写字母/数字、单点分隔
        if self.action.is_empty() {
            return Err(EnvelopeError::Validation("action cannot be empty"));
        }
        if self.action.len() > MAX_ACTION_LEN {
            return Err(EnvelopeError::ActionTooLarge(
                self.action.len(),
                MAX_ACTION_LEN,
            ));
        }
        if !is_valid_action_format(&self.action) {
            return Err(EnvelopeError::Validation(
                "action must consist of lowercase letters, digits, separated by single dots",
            ));
        }

        // 3. metadata 预算：≤16 项、key ≤64B、value ≤256B、累计 ≤ MAX_METADATA_TOTAL_BYTES
        self.validate_metadata_budgets()?;

        // 4. 通用 status_code / error_message 规则（先于任何 action 专用规则）
        //    非 Response（Request/Event/System）不得携带错误字段；Response 成功不得携带 error_message。
        if self.packet_type != PacketType::Response {
            if self.status_code != 0 {
                return Err(EnvelopeError::Validation(
                    "non-response packet cannot have non-zero status_code",
                ));
            }
            if !self.error_message.is_empty() {
                return Err(EnvelopeError::Validation(
                    "non-response packet cannot have error_message",
                ));
            }
        } else if self.status_code == 0 && !self.error_message.is_empty() {
            return Err(EnvelopeError::Validation(
                "successful response (status 0) cannot have error_message",
            ));
        }

        // 5. tunnel. 前缀与 TYPE_SYSTEM 归属（与流种类无关的部分）
        let is_tunnel_kick = self.action == "tunnel.kick";
        let is_tunnel_revoke = self.action == "tunnel.revoke";
        let is_tunnel_system_signal = is_tunnel_kick || is_tunnel_revoke;

        if self.action.starts_with("tunnel.")
            && !is_tunnel_system_signal
            && self.action != "tunnel.ping"
        {
            return Err(EnvelopeError::Validation("unknown tunnel. prefix action"));
        }
        if is_tunnel_system_signal && self.packet_type != PacketType::System {
            return Err(EnvelopeError::Validation(
                "tunnel.kick/revoke must be System packet type",
            ));
        }
        if self.packet_type == PacketType::System && !is_tunnel_system_signal {
            return Err(EnvelopeError::Validation(
                "System packet only allowed for tunnel.kick or tunnel.revoke",
            ));
        }

        // 6. tunnel.ping 专用规则（通用错误字段规则已在上一步执行完毕）
        if self.action == "tunnel.ping" {
            match self.packet_type {
                PacketType::Request | PacketType::Response => {}
                _ => {
                    return Err(EnvelopeError::Validation(
                        "tunnel.ping must be Request or Response",
                    ));
                }
            }
            if self.payload.len() != 16 {
                return Err(EnvelopeError::Validation(
                    "tunnel.ping payload must be exactly 16 bytes",
                ));
            }
            if !self.metadata.is_empty() {
                return Err(EnvelopeError::Validation(
                    "tunnel.ping metadata must be empty",
                ));
            }
            if self.packet_type == PacketType::Response {
                if self.status_code != 0 {
                    return Err(EnvelopeError::Validation(
                        "tunnel.ping response status_code must be 0",
                    ));
                }
                if !self.error_message.is_empty() {
                    return Err(EnvelopeError::Validation(
                        "tunnel.ping response error_message must be empty",
                    ));
                }
            }
            return Ok(());
        }

        Ok(())
    }

    /// 按流种类校验：公共字段校验 + Control/Data 路由规则。
    pub fn validate(&self, stream: StreamKind) -> Result<(), EnvelopeError> {
        self.validate_fields()?;

        // TYPE_SYSTEM 仅允许在控制流（§6.3）
        if self.packet_type == PacketType::System && stream != StreamKind::Control {
            return Err(EnvelopeError::Validation(
                "System packet only allowed on Control stream",
            ));
        }

        Ok(())
    }

    fn validate_metadata_budgets(&self) -> Result<(), EnvelopeError> {
        if self.metadata.len() > MAX_METADATA_ENTRIES {
            return Err(EnvelopeError::MetadataTooManyEntries(MAX_METADATA_ENTRIES));
        }
        let mut total = 0usize;
        for (k, v) in &self.metadata {
            if k.len() > MAX_METADATA_KEY_LEN {
                return Err(EnvelopeError::MetadataKeyTooLarge(
                    k.len(),
                    MAX_METADATA_KEY_LEN,
                ));
            }
            if v.len() > MAX_METADATA_VALUE_LEN {
                return Err(EnvelopeError::MetadataValueTooLarge(
                    v.len(),
                    MAX_METADATA_VALUE_LEN,
                ));
            }
            total = total
                .checked_add(k.len())
                .and_then(|t| t.checked_add(v.len()))
                .ok_or(EnvelopeError::SizeOverflow)?;
            if total > MAX_METADATA_TOTAL_BYTES {
                return Err(EnvelopeError::MetadataTotalTooLarge(
                    MAX_METADATA_TOTAL_BYTES,
                ));
            }
        }
        Ok(())
    }

    /// 预计算 proto3 序列化长度：全程 checked，超限直接报错，**不复制任何 payload**。
    ///
    /// 该方法只做尺寸/溢出判定，不做语义校验；`encode` 会先 `validate_fields`。
    pub fn serialized_len(&self) -> Result<usize, EnvelopeError> {
        let mut n = 0usize;
        let mut add = |delta: usize| -> Result<(), EnvelopeError> {
            n = n.checked_add(delta).ok_or(EnvelopeError::SizeOverflow)?;
            if n > MAX_ENVELOPE_SIZE {
                return Err(EnvelopeError::MessageTooLarge(n));
            }
            Ok(())
        };

        let pt_num = self.packet_type.to_u32();
        if self.trace_id != 0 {
            add(1 + varint_len_u64(self.trace_id))?;
        }
        if pt_num != 0 {
            add(1 + varint_len_u64(u64::from(pt_num)))?;
        }
        if !self.action.is_empty() {
            add(1 + varint_len_u64(self.action.len() as u64) + self.action.len())?;
        }
        if self.status_code != 0 {
            // 负数 int32 以 64 位符号扩展编码，varint_len 自然给出 10 字节
            add(1 + varint_len_u64(self.status_code as i64 as u64))?;
        }
        if !self.error_message.is_empty() {
            add(1 + varint_len_u64(self.error_message.len() as u64) + self.error_message.len())?;
        }
        if !self.payload.is_empty() {
            add(1 + varint_len_u64(self.payload.len() as u64) + self.payload.len())?;
        }
        for (k, v) in &self.metadata {
            // 与 encode 写头使用同一函数，避免长度计算与实写产生偏差
            let entry = metadata_entry_len(k, v);
            add(1 + varint_len_u64(entry as u64) + entry)?;
        }

        Ok(n)
    }

    /// 序列化为 proto3 二进制。
    ///
    /// 顺序固定为：语义校验 → checked 长度计算（超 512KiB 立即失败）→ 一次性分配与复制。
    /// 因此超大 payload 只会触发一次错误返回，不会产生与输入等大的第二次分配。
    pub fn encode(&self) -> Result<Vec<u8>, EnvelopeError> {
        self.validate_fields()?;
        let total = self.serialized_len()?;

        let mut buf = Vec::with_capacity(total);

        // 字段 1: uint64 trace_id
        if self.trace_id != 0 {
            encode_tag(&mut buf, 1, WIRE_VARINT);
            encode_varint(&mut buf, self.trace_id);
        }

        // 字段 2: PacketType type
        let pt_num = self.packet_type.to_u32();
        if pt_num != 0 {
            encode_tag(&mut buf, 2, WIRE_VARINT);
            encode_varint(&mut buf, u64::from(pt_num));
        }

        // 字段 3: string action
        if !self.action.is_empty() {
            encode_tag(&mut buf, 3, WIRE_LEN);
            encode_varint(&mut buf, self.action.len() as u64);
            buf.extend_from_slice(self.action.as_bytes());
        }

        // 字段 4: int32 status_code
        // 负数按 proto3 规则编码为 10 字节 sign-extended varint
        if self.status_code != 0 {
            encode_tag(&mut buf, 4, WIRE_VARINT);
            encode_varint(&mut buf, self.status_code as i64 as u64);
        }

        // 字段 5: string error_message
        if !self.error_message.is_empty() {
            encode_tag(&mut buf, 5, WIRE_LEN);
            encode_varint(&mut buf, self.error_message.len() as u64);
            buf.extend_from_slice(self.error_message.as_bytes());
        }

        // 字段 6: bytes payload
        if !self.payload.is_empty() {
            encode_tag(&mut buf, 6, WIRE_LEN);
            encode_varint(&mut buf, self.payload.len() as u64);
            buf.extend_from_slice(&self.payload);
        }

        // 字段 7: map<string, string> metadata
        for (k, v) in &self.metadata {
            let entry_len = metadata_entry_len(k, v);
            encode_tag(&mut buf, 7, WIRE_LEN);
            encode_varint(&mut buf, entry_len as u64);
            if !k.is_empty() {
                encode_tag(&mut buf, 1, WIRE_LEN);
                encode_varint(&mut buf, k.len() as u64);
                buf.extend_from_slice(k.as_bytes());
            }
            if !v.is_empty() {
                encode_tag(&mut buf, 2, WIRE_LEN);
                encode_varint(&mut buf, v.len() as u64);
                buf.extend_from_slice(v.as_bytes());
            }
        }

        debug_assert_eq!(buf.len(), total, "serialized_len 与实写长度必须一致");
        Ok(buf)
    }

    /// 从 proto3 二进制严格解码。
    ///
    /// - 输入整包 ≤ 512KiB；
    /// - 每个变长字段先按预算检查对端声明长度，**再**分配字符串（action ≤64B、
    ///   metadata key ≤64B、value ≤256B、entry ≤`MAX_METADATA_ENTRY_BYTES`）；
    /// - metadata 第 17 项在构造/插入之前即被拒绝；
    /// - 返回前执行 `validate_fields`，因此公共解码入口不会产出语义非法的信元。
    pub fn decode(bytes: &[u8]) -> Result<Self, EnvelopeError> {
        if bytes.len() > MAX_ENVELOPE_SIZE {
            return Err(EnvelopeError::MessageTooLarge(bytes.len()));
        }

        let mut reader = ProtoReader::new(bytes);

        let mut trace_id = 0u64;
        let mut packet_type: Option<PacketType> = None;
        let mut action = String::new();
        let mut status_code = 0i32;
        let mut error_message = String::new();
        let mut payload = Vec::new();
        let mut metadata = BTreeMap::new();
        let mut metadata_total = 0usize;

        let mut seen = SeenFields::default();

        while !reader.is_empty() {
            let (field, wire_type) = reader.read_tag()?;
            match field {
                1 => {
                    seen.mark(1)?;
                    reader.expect_wire_type(wire_type, WIRE_VARINT)?;
                    trace_id = reader.read_uint64()?;
                }
                2 => {
                    seen.mark(2)?;
                    reader.expect_wire_type(wire_type, WIRE_VARINT)?;
                    let pt_val = reader.read_uint32()?;
                    packet_type = Some(PacketType::from_u32(pt_val)?);
                }
                3 => {
                    seen.mark(3)?;
                    reader.expect_wire_type(wire_type, WIRE_LEN)?;
                    let len = reader.read_length()?;
                    if len > MAX_ACTION_LEN {
                        return Err(EnvelopeError::ActionTooLarge(len, MAX_ACTION_LEN));
                    }
                    action = reader.read_string_of(len)?;
                }
                4 => {
                    seen.mark(4)?;
                    reader.expect_wire_type(wire_type, WIRE_VARINT)?;
                    status_code = reader.read_int32()?;
                }
                5 => {
                    seen.mark(5)?;
                    reader.expect_wire_type(wire_type, WIRE_LEN)?;
                    // error_message 无独立规范上限，仅受整包 512KiB 预算约束
                    let len = reader.read_length()?;
                    if len > MAX_ENVELOPE_SIZE {
                        return Err(EnvelopeError::FieldTooLarge(len, MAX_ENVELOPE_SIZE));
                    }
                    error_message = reader.read_string_of(len)?;
                }
                6 => {
                    seen.mark(6)?;
                    reader.expect_wire_type(wire_type, WIRE_LEN)?;
                    let len = reader.read_length()?;
                    if len > MAX_ENVELOPE_SIZE {
                        return Err(EnvelopeError::FieldTooLarge(len, MAX_ENVELOPE_SIZE));
                    }
                    payload = reader.take(len)?.to_vec();
                }
                7 => {
                    // map<string, string>：字段 7 repeated。第 17 项必须在构造之前拒绝。
                    reader.expect_wire_type(wire_type, WIRE_LEN)?;
                    if metadata.len() >= MAX_METADATA_ENTRIES {
                        return Err(EnvelopeError::MetadataTooManyEntries(MAX_METADATA_ENTRIES));
                    }
                    let entry_index = metadata.len();
                    let len = reader.read_length()?;
                    if len > MAX_METADATA_ENTRY_BYTES {
                        return Err(EnvelopeError::FieldTooLarge(len, MAX_METADATA_ENTRY_BYTES));
                    }
                    let entry = reader.take(len)?;
                    let (k, v) = parse_map_entry(entry)?;
                    let total = metadata_total
                        .checked_add(k.len())
                        .and_then(|t| t.checked_add(v.len()))
                        .ok_or(EnvelopeError::SizeOverflow)?;
                    if total > MAX_METADATA_TOTAL_BYTES {
                        return Err(EnvelopeError::MetadataTotalTooLarge(
                            MAX_METADATA_TOTAL_BYTES,
                        ));
                    }
                    metadata_total = total;
                    if metadata.insert(k, v).is_some() {
                        return Err(EnvelopeError::DuplicateMetadataKey(entry_index));
                    }
                }
                _ => {
                    return Err(EnvelopeError::UnknownField(field));
                }
            }
        }

        // proto3 缺省值：若 packet_type 未出现，则为 0 (TYPE_UNKNOWN)，非法！
        let packet_type = packet_type.ok_or(EnvelopeError::InvalidPacketType(0))?;

        let packet = TunnelPacket {
            trace_id,
            packet_type,
            action,
            status_code,
            error_message,
            payload,
            metadata,
        };

        // 公共解码入口自身即做语义校验，不依赖调用方
        packet.validate_fields()?;

        Ok(packet)
    }
}

fn is_valid_action_format(action: &str) -> bool {
    // 小写字母/数字/点分隔：每个 segment 非空，且仅包含 [a-z0-9]
    let segments: Vec<&str> = action.split('.').collect();
    if segments.is_empty() {
        return false;
    }
    for seg in segments {
        if seg.is_empty() {
            return false;
        }
        for b in seg.bytes() {
            if !b.is_ascii_lowercase() && !b.is_ascii_digit() {
                return false;
            }
        }
    }
    true
}

const WIRE_VARINT: u8 = 0;
const WIRE_LEN: u8 = 2;

/// varint 编码所需字节数（1..=10）。
fn varint_len_u64(mut val: u64) -> usize {
    let mut n = 1;
    while val >= 0x80 {
        val >>= 7;
        n += 1;
    }
    n
}

fn encode_varint(buf: &mut Vec<u8>, mut val: u64) {
    loop {
        if val < 0x80 {
            buf.push(val as u8);
            return;
        }
        buf.push(((val as u8) & 0x7f) | 0x80);
        val >>= 7;
    }
}

fn encode_tag(buf: &mut Vec<u8>, field: u32, wire_type: u8) {
    encode_varint(buf, ((field as u64) << 3) | (wire_type as u64));
}

fn metadata_entry_len(k: &str, v: &str) -> usize {
    let mut entry = 0usize;
    if !k.is_empty() {
        entry += 1 + varint_len_u64(k.len() as u64) + k.len();
    }
    if !v.is_empty() {
        entry += 1 + varint_len_u64(v.len() as u64) + v.len();
    }
    entry
}

/// 解析 map entry：key/value 均在分配之前检查声明长度预算。
fn parse_map_entry(bytes: &[u8]) -> Result<(String, String), EnvelopeError> {
    let mut reader = ProtoReader::new(bytes);
    let mut key = String::new();
    let mut value = String::new();
    let mut seen_key = false;
    let mut seen_val = false;

    while !reader.is_empty() {
        let (field, wire_type) = reader.read_tag()?;
        match field {
            1 => {
                if seen_key {
                    return Err(EnvelopeError::DuplicateField(1));
                }
                seen_key = true;
                reader.expect_wire_type(wire_type, WIRE_LEN)?;
                let len = reader.read_length()?;
                if len > MAX_METADATA_KEY_LEN {
                    return Err(EnvelopeError::MetadataKeyTooLarge(
                        len,
                        MAX_METADATA_KEY_LEN,
                    ));
                }
                key = reader.read_string_of(len)?;
            }
            2 => {
                if seen_val {
                    return Err(EnvelopeError::DuplicateField(2));
                }
                seen_val = true;
                reader.expect_wire_type(wire_type, WIRE_LEN)?;
                let len = reader.read_length()?;
                if len > MAX_METADATA_VALUE_LEN {
                    return Err(EnvelopeError::MetadataValueTooLarge(
                        len,
                        MAX_METADATA_VALUE_LEN,
                    ));
                }
                value = reader.read_string_of(len)?;
            }
            _ => return Err(EnvelopeError::UnknownField(field)),
        }
    }

    Ok((key, value))
}

#[derive(Default)]
struct SeenFields(u32);

impl SeenFields {
    fn mark(&mut self, field: u32) -> Result<(), EnvelopeError> {
        if field == 0 || field > 31 {
            return Err(EnvelopeError::UnknownField(field));
        }
        let mask = 1u32 << (field - 1);
        if self.0 & mask != 0 {
            return Err(EnvelopeError::DuplicateField(field));
        }
        self.0 |= mask;
        Ok(())
    }
}

struct ProtoReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        ProtoReader { buf, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_byte(&mut self) -> Result<u8, EnvelopeError> {
        if self.pos >= self.buf.len() {
            return Err(EnvelopeError::Truncated);
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn read_varint(&mut self) -> Result<u64, EnvelopeError> {
        let mut acc: u64 = 0;
        let mut index = 0usize;
        loop {
            let b = self.read_byte()?;
            let payload = u64::from(b & 0x7f);
            if index == 9 && payload > 1 {
                return Err(EnvelopeError::VarintOverflow);
            }
            acc |= payload << (7 * index as u32);
            index += 1;
            if b & 0x80 == 0 {
                if index > 1 && payload == 0 {
                    return Err(EnvelopeError::NonCanonicalVarint);
                }
                return Ok(acc);
            }
            if index == 10 {
                return Err(EnvelopeError::VarintTooLong);
            }
        }
    }

    fn read_tag(&mut self) -> Result<(u32, u8), EnvelopeError> {
        let tag = self.read_varint()?;
        let wire_type = (tag & 0b111) as u8;
        let field = tag >> 3;
        if field == 0 {
            return Err(EnvelopeError::Malformed("zero field number"));
        }
        let field =
            u32::try_from(field).map_err(|_| EnvelopeError::Malformed("field number overflow"))?;
        match wire_type {
            WIRE_VARINT | WIRE_LEN => Ok((field, wire_type)),
            other => Err(EnvelopeError::InvalidWireType(other)),
        }
    }

    fn expect_wire_type(&self, got: u8, want: u8) -> Result<(), EnvelopeError> {
        if got != want {
            Err(EnvelopeError::InvalidWireType(got))
        } else {
            Ok(())
        }
    }

    fn read_uint64(&mut self) -> Result<u64, EnvelopeError> {
        self.read_varint()
    }

    fn read_uint32(&mut self) -> Result<u32, EnvelopeError> {
        let v = self.read_varint()?;
        u32::try_from(v).map_err(|_| EnvelopeError::Malformed("uint32 overflow"))
    }

    /// proto3 int32：只接受 `0..=i32::MAX`，或按 64 位二补码符号扩展得到的负值。
    /// 绝不使用 `as i32` 截断（`2^32`、5 字节 `0xffffffff`、`0x7fffffffffffffff` 均拒绝）。
    fn read_int32(&mut self) -> Result<i32, EnvelopeError> {
        let v = self.read_varint()?;
        if v <= i32::MAX as u64 {
            return Ok(v as i32);
        }
        // 负数只接受完整的 64 位符号扩展：v 的低 64 位按 i64 解释为负，
        // 且它必须正好是某个 i32 的符号扩展（否则即为截断/溢出）。
        let wide = v as i64;
        if wide < 0 && (wide as i32) as i64 == wide {
            return Ok(wide as i32);
        }
        Err(EnvelopeError::Int32OutOfRange)
    }

    /// 读取长度前缀；调用方必须先按字段预算判定再分配。
    fn read_length(&mut self) -> Result<usize, EnvelopeError> {
        let len = self.read_varint()?;
        usize::try_from(len).map_err(|_| EnvelopeError::Malformed("length overflow"))
    }

    /// 按已确认的长度取切片（零拷贝）。
    fn take(&mut self, len: usize) -> Result<&'a [u8], EnvelopeError> {
        if len > self.remaining() {
            return Err(EnvelopeError::Truncated);
        }
        let start = self.pos;
        let end = self.pos + len;
        self.pos = end;
        Ok(&self.buf[start..end])
    }

    /// 长度必须已由调用方按预算校验过，此处的分配上限即该预算。
    fn read_string_of(&mut self, len: usize) -> Result<String, EnvelopeError> {
        let slice = self.take(len)?;
        std::str::from_utf8(slice)
            .map(|s| s.to_string())
            .map_err(|_| EnvelopeError::InvalidUtf8)
    }
}
