//! 握手消息 Protobuf 编解码（protocol_spec.md §4.2）：手写 proto3 canonical wire
//! 编解码 + §4.2「严格 wire 校验契约」的逐条审计，不引入 protobuf/prost 依赖，不使用 `unsafe`。
//!
//! 覆盖 §4.2 的 7 条消息：`ClientHello`、`ServerChallenge`、`DeviceDescriptor`
//! （嵌套于 `ClientAuthRequest`）、`ClientAuthRequest`、`ServerPending`、
//! `PendingPong`、`ServerAuthResponse`。本模块只做**纯字节**编解码与校验：
//! 不解析帧头（见 [`crate::frame`]）、不构造签名输入（见 [`crate::sig`]）、
//! 不做状态机迁移（见 [`crate::handshake`]）。
//!
//! # canonical 编码（确定性）
//!
//! * 字段号升序写出；proto3 隐式缺省（数值 0、空串）不写；
//! * 必要 `bytes`（32B/16B/64B）由定长数组表达，恒非空，因此恒写出；
//! * 嵌套 `device_descriptor` 写出**保留的原字节**（见下），外层字段号仍升序、
//!   外层长度前缀仍按原字节数最短编码，因此客户端构造的消息满足
//!   `encode(decode(b)) == b`；
//! * varint（key、长度前缀、整数值）一律最短编码。
//!
//! # `descriptor_raw` 原字节保留（§4.2 提取规则）
//!
//! 签名引用的是 `CLIENT_AUTH_REQUEST` Payload 内 `device_descriptor`
//! length-delimited 嵌套消息的**内容原字节**（不含外层 key/长度 varint/帧头）。
//! 解码把它原样存入 [`ClientAuthRequest::descriptor_raw`]，编码时**逐字节原样写出**：
//! 因此即使入站嵌套字节非 canonical（字段乱序、显式写出缺省值），验签所用的
//! `descriptor_raw` 也不会被重编码改变，服务端不存在「重编码后验签」的代码路径。
//! [`ClientAuthRequest::with_descriptor_raw`] 用于把从原始帧截取的字节挂到已解析的
//! 请求上，并强校验该原字节解析结果与 `descriptor` 完全一致。
//!
//! # 严格解码（全部 fail-closed，错误固定去敏）
//!
//! 逐条拒绝：未知字段号、已知 singular 字段重复、非最短 varint（**字段 key
//! varint、长度前缀 varint 与整数值 varint 三类**）、错误 wire type（含
//! `bool`/`uint32` 的取值越界）、截断、长度前缀越过剩余字节、必要 bytes 缺失或
//! 长度非 32/16/64B、未消费的尾随字节、非法 UTF-8、字段号 0 与越出 `u32`、
//! 消息超过 16 KiB 上限、嵌套 `DeviceDescriptor` 超过 16 247B、以及 §4.3 的
//! status/reason_code/服务端临时公钥组合约束与 §4.1 的 `wait_hint_ms = 5000`、
//! `probe_seq != 0`。缺失的 proto3 scalar 一律按 0 严格解释（不推导为另一状态），
//! 因此「缺失 `wait_hint_ms`」表现为 `BadWaitHintMs(0)`、
//! 「缺失 `probe_seq`」表现为 `ZeroProbeSeq`。
//!
//! 错误变体只携带**字段号、字节长度、枚举取值**等结构信息，绝不回显 payload
//! 内容、公钥字节或 descriptor 文本。

use thiserror::Error;

use crate::sig::{
    MAX_DESCRIPTOR_WIRE_BYTES, REASON_APPROVAL_DENIED, REASON_HANDSHAKE_TIMEOUT, REASON_REVOKED,
    REASON_SERVER_ERROR, REASON_UNSPECIFIED, STATUS_APPROVED, STATUS_REJECTED,
};

/// 顶层握手消息（即握手帧 Payload）字节上限：16 KiB，与
/// [`crate::frame::MAX_PAYLOAD_LEN`] 同值，二者必须一致。
pub const MAX_HANDSHAKE_MESSAGE_BYTES: usize = crate::frame::MAX_PAYLOAD_LEN;

/// 嵌套 `DeviceDescriptor` 内容原字节上限 16 247B：由「ClientAuthRequest 固定开销
/// 134B + 嵌套 tag 1B + 2B 长度 varint ≤ 16 384」推出，与
/// [`crate::sig::MAX_DESCRIPTOR_WIRE_BYTES`] 同值，二者必须一致。
pub const MAX_DEVICE_DESCRIPTOR_BYTES: usize = MAX_DESCRIPTOR_WIRE_BYTES;

/// `client_random`/`server_random`/两个 Ed25519/X25519 公钥的恰字节数。
pub const RANDOM_BYTES_LEN: usize = 32;
/// `pending_token` 的恰字节数。
pub const PENDING_TOKEN_LEN: usize = 16;
/// `status_nonce` 的恰字节数。
pub const STATUS_NONCE_LEN: usize = 16;
/// `server_x25519_eph_pub` 的恰字节数（REJECTED 时为全零占位）。
pub const SERVER_EPHEMERAL_LEN: usize = 32;
/// Ed25519 签名的恰字节数。
pub const SIGNATURE_LEN: usize = 64;
/// §4.1：`ServerPending.wait_hint_ms` 本版固定 5000（仅名义周期提示，
/// 接收方不得据此改变超时规则）。
pub const PENDING_WAIT_HINT_MS: u32 = 5_000;

/// 本 schema 用到的 wire type。3/4/5/6/7（group、fixed32、保留位）一律拒绝。
mod wt {
    pub const VARINT: u8 = 0;
    pub const LEN: u8 = 2;
}

/// 严格 wire 校验错误。变体集合固定且去敏：只带字段号、长度、枚举取值，
/// 不回显任何 payload 字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandshakeMsgError {
    /// 字节流在某字段中途结束（含 varint 未闭合、长度前缀后负载不足、定长负载不足）。
    #[error("truncated wire bytes")]
    Truncated,
    /// varint 不是最短编码（末字节贡献 0 个信息位），覆盖字段 key、长度前缀与整数值。
    #[error("non-canonical varint")]
    NonCanonicalVarint,
    /// varint 超过 10 字节，或 10 字节仍溢出 `u64`。
    #[error("varint out of range or too long")]
    VarintOverflow,
    /// `uint32` 字段的 varint 越出 `u32`。
    #[error("varint out of range for uint32")]
    Uint32OutOfRange,
    /// 字段号 0：protobuf 与本协议都不存在该字段号。
    #[error("zero field number")]
    ZeroFieldNumber,
    /// 字段号越出 `u32`：不得谎报成「字段号 0」或 `UnknownField(0)`。
    #[error("field number out of range")]
    FieldNumberOutOfRange,
    /// schema 之外的字段号：首版不做 unknown-field 保留，直接拒绝。
    #[error("unknown field number: {0}")]
    UnknownField(u32),
    /// singular 字段出现第二次：canonical 编码不可能产生。
    #[error("duplicate field number: {0}")]
    DuplicateField(u32),
    /// 已知字段的 wire type 与 schema 不符（含 3/4/5/6/7 保留 wire type）。
    #[error("field {field} has unexpected wire type {wire_type}")]
    WrongWireType { field: u32, wire_type: u8 },
    /// 必要 `bytes` 字段长度不是规定的 32/16/64B。
    #[error("field {field} has {len} bytes, expected exactly {expected}")]
    BadFieldLength {
        field: u32,
        len: usize,
        expected: usize,
    },
    /// 长度前缀越过消息尾（或与外层声明长度自相矛盾）。
    #[error("length prefix exceeds remaining bytes")]
    LengthOutOfBounds,
    /// 全部已知字段消费完毕后仍有未消费字节。
    #[error("unconsumed trailing bytes")]
    TrailingBytes,
    /// `string` 字段不是合法 UTF-8（只带字段号，不回显内容）。
    #[error("string field {0} is not valid UTF-8")]
    InvalidUtf8(u32),
    /// 必要字段缺失（`bytes` 与嵌套 `device_descriptor`）。
    #[error("missing required field: {0}")]
    MissingField(u32),
    /// 顶层消息（握手帧 Payload）超过 16 KiB。
    #[error("message too large: {len} > {max}")]
    MessageTooLarge { len: usize, max: usize },
    /// 嵌套 `DeviceDescriptor` 原字节超过 16 247B。
    #[error("descriptor too large: {len} > {max}")]
    DescriptorTooLarge { len: usize, max: usize },
    /// §4.3：`status` 只允许 0/1。
    #[error("bad status: {0} (only 0 or 1 allowed)")]
    BadStatus(u32),
    /// §4.3：`reason_code` 与 `status` 的组合非法
    /// （APPROVED 只允许 0；REJECTED 只允许 `{0,2,3,4,5}`，`1=SIGNATURE_INVALID` v3 绝不发出）。
    #[error("bad reason code for status: {0}")]
    BadReasonCode(u32),
    /// §4.3：APPROVED 的服务端临时公钥必须非全零；REJECTED 必须恰为 32B 全零占位。
    #[error("server ephemeral public key violates the status-dependent zero constraint")]
    BadServerEphemeral,
    /// §4.1：`wait_hint_ms` 必须恰为 5000（含缺失按 0 解释的情形）。
    #[error("bad wait_hint_ms: {0} (expected 5000)")]
    BadWaitHintMs(u32),
    /// §4.1：`probe_seq` 从 1 开始，不得为 0（含缺失按 0 解释的情形）。
    #[error("probe_seq must be non-zero")]
    ZeroProbeSeq,
    /// `with_descriptor_raw` 提供的原字节解析结果与 `descriptor` 不一致。
    #[error("descriptor raw bytes do not match the parsed descriptor")]
    DescriptorRawMismatch,
}

// ====================================================================
// 写入端
// ====================================================================

/// 确定性写入器：调用顺序即字节顺序，故调用方必须按字段号升序调用。
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
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

    /// 非 optional string：空串按 proto3 缺省不写。
    fn string(&mut self, field: u32, value: &str) {
        if !value.is_empty() {
            self.bytes(field, value.as_bytes());
        }
    }

    /// `bytes`：空值按 proto3 缺省不写（本模块的必要 bytes 由定长数组表达，不会为空）。
    fn bytes(&mut self, field: u32, value: &[u8]) {
        if value.is_empty() {
            return;
        }
        self.raw(field, value);
    }

    /// 长度定界的原字节：即使 0 长度也写出（嵌套 message 的 presence 语义，
    /// 或必须逐字节保真的 `descriptor_raw`）。
    fn raw(&mut self, field: u32, value: &[u8]) {
        self.tag(field, wt::LEN);
        self.varint(value.len() as u64);
        self.buf.extend_from_slice(value);
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

// ====================================================================
// 读取端
// ====================================================================

/// 只前进、零拷贝借用的 wire 读取器。所有失败都返回固定去敏错误。
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

    fn byte(&mut self) -> Result<u8, HandshakeMsgError> {
        let b = *self.buf.get(self.pos).ok_or(HandshakeMsgError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    /// 严格 varint：≤10 字节、最短编码、不溢出 `u64`。key、长度前缀与整数值共用本函数，
    /// 因此三类非最短编码由同一处拒绝。
    fn varint(&mut self) -> Result<u64, HandshakeMsgError> {
        let mut acc: u64 = 0;
        let mut index = 0usize;
        loop {
            let b = self.byte()?;
            let payload = u64::from(b & 0x7f);
            if index == 9 && payload > 1 {
                return Err(HandshakeMsgError::VarintOverflow);
            }
            acc |= payload << (7 * index as u32);
            index += 1;
            if b & 0x80 == 0 {
                // 非最短编码：末字节贡献 0 个信息位（等价于多写补零的高位组）。
                // 只判末字节：`88 88 01` 这类中间 0 载荷组仍是最短编码。
                if index > 1 && payload == 0 {
                    return Err(HandshakeMsgError::NonCanonicalVarint);
                }
                return Ok(acc);
            }
            if index == 10 {
                // 第 10 字节仍带 continuation：varint 超过 10 字节。
                return Err(HandshakeMsgError::VarintOverflow);
            }
        }
    }

    fn uint32(&mut self) -> Result<u32, HandshakeMsgError> {
        let v = self.varint()?;
        u32::try_from(v).map_err(|_| HandshakeMsgError::Uint32OutOfRange)
    }

    fn uint64(&mut self) -> Result<u64, HandshakeMsgError> {
        self.varint()
    }

    /// 长度前缀 + 负载；先判长度可用性再判是否越过剩余字节，不提前分配。
    fn length_delimited(&mut self) -> Result<&'a [u8], HandshakeMsgError> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| HandshakeMsgError::LengthOutOfBounds)?;
        if len > self.remaining() {
            return Err(HandshakeMsgError::LengthOutOfBounds);
        }
        let end = self.pos + len;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// 恰 `N` 字节的必要 `bytes` 字段：长度不符即拒绝（不截断、不补齐）。
    fn fixed_bytes<const N: usize>(&mut self, field: u32) -> Result<[u8; N], HandshakeMsgError> {
        let raw = self.length_delimited()?;
        if raw.len() != N {
            return Err(HandshakeMsgError::BadFieldLength {
                field,
                len: raw.len(),
                expected: N,
            });
        }
        let mut out = [0u8; N];
        out.copy_from_slice(raw);
        Ok(out)
    }

    /// `string` 字段：UTF-8 校验在分配前完成，失败只带字段号。
    fn string(&mut self, field: u32) -> Result<String, HandshakeMsgError> {
        let raw = self.length_delimited()?;
        let s = std::str::from_utf8(raw).map_err(|_| HandshakeMsgError::InvalidUtf8(field))?;
        Ok(s.to_string())
    }
}

/// singular 字段重复检测位图（本 schema 字段号均 ≤ 4）。
#[derive(Default)]
struct Seen(u32);

impl Seen {
    /// 先判重复再判 wire type：已知字段号的第二次出现恒为 `DuplicateField`。
    fn mark(&mut self, field: u32) -> Result<(), HandshakeMsgError> {
        if field == 0 || field > u32::BITS {
            return Err(HandshakeMsgError::UnknownField(field));
        }
        let mask = 1u32 << (field - 1);
        if self.0 & mask != 0 {
            return Err(HandshakeMsgError::DuplicateField(field));
        }
        self.0 |= mask;
        Ok(())
    }
}

/// 统一的 tag 读取：字段号 0 与字段号越出 `u32::MAX` 直接拒绝；
/// wire type 的匹配由各消息在**确认字段号已知之后**执行，
/// 使 schema 外字段号恒报 `UnknownField`（含 wire type 3/4/5/6/7 的探测字节）。
fn read_tag(reader: &mut Reader<'_>) -> Result<(u32, u8), HandshakeMsgError> {
    let tag = reader.varint()?;
    let wire_type = (tag & 0b111) as u8;
    let field = tag >> 3;
    if field == 0 {
        return Err(HandshakeMsgError::ZeroFieldNumber);
    }
    let field = u32::try_from(field).map_err(|_| HandshakeMsgError::FieldNumberOutOfRange)?;
    Ok((field, wire_type))
}

fn expect_wire(field: u32, wire_type: u8, want: u8) -> Result<(), HandshakeMsgError> {
    if wire_type == want {
        Ok(())
    } else {
        Err(HandshakeMsgError::WrongWireType { field, wire_type })
    }
}

/// 顶层消息（握手帧 Payload）上限守卫。
fn check_message_size(bytes: &[u8]) -> Result<(), HandshakeMsgError> {
    if bytes.len() > MAX_HANDSHAKE_MESSAGE_BYTES {
        return Err(HandshakeMsgError::MessageTooLarge {
            len: bytes.len(),
            max: MAX_HANDSHAKE_MESSAGE_BYTES,
        });
    }
    Ok(())
}

/// 嵌套 `DeviceDescriptor` 原字节上限守卫。
fn check_descriptor_size(len: usize) -> Result<(), HandshakeMsgError> {
    if len > MAX_DEVICE_DESCRIPTOR_BYTES {
        return Err(HandshakeMsgError::DescriptorTooLarge {
            len,
            max: MAX_DEVICE_DESCRIPTOR_BYTES,
        });
    }
    Ok(())
}

/// 每个消息解码末尾的统一尾随字节守卫。字段循环每轮恒消费完整一个字段，
/// 因此正常路径不会触发；它保证「只消费前缀即返回 Ok」不可能成为代码路径。
fn check_consumed(reader: &Reader<'_>) -> Result<(), HandshakeMsgError> {
    if reader.eof() {
        Ok(())
    } else {
        Err(HandshakeMsgError::TrailingBytes)
    }
}

fn is_all_zero(value: &[u8; SERVER_EPHEMERAL_LEN]) -> bool {
    value.iter().all(|b| *b == 0)
}

/// §4.3 的 status/reason_code/服务端临时公钥组合约束，检查顺序固定为
/// `status → reason_code → server_x25519_eph_pub`，多项违例时报首个类别。
/// 编解码两侧共用，因此同一违例在 `encode` 与 `decode` 上得到同一变体。
fn check_server_auth_status(
    status: u32,
    reason_code: u32,
    server_x25519_eph_pub: &[u8; SERVER_EPHEMERAL_LEN],
) -> Result<(), HandshakeMsgError> {
    if status == u32::from(STATUS_APPROVED) {
        if reason_code != REASON_UNSPECIFIED {
            return Err(HandshakeMsgError::BadReasonCode(reason_code));
        }
        if is_all_zero(server_x25519_eph_pub) {
            return Err(HandshakeMsgError::BadServerEphemeral);
        }
        Ok(())
    } else if status == u32::from(STATUS_REJECTED) {
        // 1 = SIGNATURE_INVALID 在 §4.2 定义但 v3 绝不发出（验签失败直接关闭 TCP）。
        let allowed = [
            REASON_UNSPECIFIED,
            REASON_REVOKED,
            REASON_APPROVAL_DENIED,
            REASON_HANDSHAKE_TIMEOUT,
            REASON_SERVER_ERROR,
        ];
        if !allowed.contains(&reason_code) {
            return Err(HandshakeMsgError::BadReasonCode(reason_code));
        }
        if !is_all_zero(server_x25519_eph_pub) {
            return Err(HandshakeMsgError::BadServerEphemeral);
        }
        Ok(())
    } else {
        Err(HandshakeMsgError::BadStatus(status))
    }
}

/// §4.1/§4.2 的 PENDING probe 语义约束，检查顺序固定为 `wait_hint_ms → probe_seq`。
fn check_pending_semantics(wait_hint_ms: u32, probe_seq: u64) -> Result<(), HandshakeMsgError> {
    if wait_hint_ms != PENDING_WAIT_HINT_MS {
        return Err(HandshakeMsgError::BadWaitHintMs(wait_hint_ms));
    }
    if probe_seq == 0 {
        return Err(HandshakeMsgError::ZeroProbeSeq);
    }
    Ok(())
}

// ====================================================================
// ClientHello / ServerChallenge
// ====================================================================

/// `ClientHello { bytes client_random = 1; }`（§4.2，恰 32B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHello {
    pub client_random: [u8; RANDOM_BYTES_LEN],
}

/// `ServerChallenge { bytes server_random = 1; }`（§4.2，恰 32B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerChallenge {
    pub server_random: [u8; RANDOM_BYTES_LEN],
}

impl ClientHello {
    pub fn new(client_random: [u8; RANDOM_BYTES_LEN]) -> Self {
        ClientHello { client_random }
    }

    /// canonical 编码：单字段 `0a 20 || client_random`，共 34B。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        let mut w = Writer::new();
        w.bytes(1, &self.client_random);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut client_random: Option<[u8; RANDOM_BYTES_LEN]> = None;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    client_random = Some(r.fixed_bytes::<{ RANDOM_BYTES_LEN }>(field)?);
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        Ok(ClientHello {
            client_random: client_random.ok_or(HandshakeMsgError::MissingField(1))?,
        })
    }
}

impl ServerChallenge {
    pub fn new(server_random: [u8; RANDOM_BYTES_LEN]) -> Self {
        ServerChallenge { server_random }
    }

    /// canonical 编码：单字段 `0a 20 || server_random`，共 34B。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        let mut w = Writer::new();
        w.bytes(1, &self.server_random);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut server_random: Option<[u8; RANDOM_BYTES_LEN]> = None;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    server_random = Some(r.fixed_bytes::<{ RANDOM_BYTES_LEN }>(field)?);
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        Ok(ServerChallenge {
            server_random: server_random.ok_or(HandshakeMsgError::MissingField(1))?,
        })
    }
}

// ====================================================================
// DeviceDescriptor
// ====================================================================

/// `DeviceDescriptor { string device_sn = 1; device_model = 2; fw_version = 3; extra_info = 4; }`。
///
/// 4 个字段均为非 optional `string`：缺失按 proto3 缺省解释为空串，写出时不写；
/// 每个字段至多出现一次，出现时必须是合法 UTF-8。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub device_sn: String,
    pub device_model: String,
    pub fw_version: String,
    pub extra_info: String,
}

impl DeviceDescriptor {
    pub fn new(
        device_sn: impl Into<String>,
        device_model: impl Into<String>,
        fw_version: impl Into<String>,
        extra_info: impl Into<String>,
    ) -> Self {
        DeviceDescriptor {
            device_sn: device_sn.into(),
            device_model: device_model.into(),
            fw_version: fw_version.into(),
            extra_info: extra_info.into(),
        }
    }

    /// canonical 编码：字段升序、空串不写，因此 4 个空串编码为 0 字节。
    /// 长度上限由 [`ClientAuthRequest`]（嵌套处）与本类型的 [`Self::decode`] 强制。
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.string(1, &self.device_sn);
        w.string(2, &self.device_model);
        w.string(3, &self.fw_version);
        w.string(4, &self.extra_info);
        w.finish()
    }

    /// 严格解析 descriptor **内容原字节**（不含外层 key/长度 varint/帧头）。
    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_descriptor_size(bytes.len())?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut device_sn: Option<String> = None;
        let mut device_model: Option<String> = None;
        let mut fw_version: Option<String> = None;
        let mut extra_info: Option<String> = None;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    device_sn = Some(r.string(field)?);
                }
                2 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    device_model = Some(r.string(field)?);
                }
                3 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    fw_version = Some(r.string(field)?);
                }
                4 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    extra_info = Some(r.string(field)?);
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        Ok(DeviceDescriptor {
            device_sn: device_sn.unwrap_or_default(),
            device_model: device_model.unwrap_or_default(),
            fw_version: fw_version.unwrap_or_default(),
            extra_info: extra_info.unwrap_or_default(),
        })
    }
}

// ====================================================================
// ClientAuthRequest
// ====================================================================

/// `ClientAuthRequest { bytes client_ed25519_pub = 1; bytes client_x25519_eph_pub = 2;
/// DeviceDescriptor device_descriptor = 3; bytes signature = 4; }`（§4.2）。
///
/// 字段私有：`descriptor_raw` 与 `descriptor` 必须始终一致（构造与
/// [`Self::with_descriptor_raw`] 共同维护该不变量），它是 §4.3 验签唯一可引用的
/// 原字节，因此 [`Self::encode`] 原样写出该字段而不重编码 `descriptor`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAuthRequest {
    client_ed25519_pub: [u8; RANDOM_BYTES_LEN],
    client_x25519_eph_pub: [u8; RANDOM_BYTES_LEN],
    descriptor: DeviceDescriptor,
    descriptor_raw: Vec<u8>,
    signature: [u8; SIGNATURE_LEN],
}

impl ClientAuthRequest {
    /// 客户端构造路径：`descriptor_raw` 取 `descriptor` 的 canonical 编码并保留，
    /// 与签名输入使用的原字节同源（§4.2「客户端只编码一次并保留」）。
    pub fn new(
        client_ed25519_pub: [u8; RANDOM_BYTES_LEN],
        client_x25519_eph_pub: [u8; RANDOM_BYTES_LEN],
        descriptor: DeviceDescriptor,
        signature: [u8; SIGNATURE_LEN],
    ) -> Result<Self, HandshakeMsgError> {
        let descriptor_raw = descriptor.encode();
        let request = ClientAuthRequest {
            client_ed25519_pub,
            client_x25519_eph_pub,
            descriptor,
            descriptor_raw,
            signature,
        };
        request.check_semantics()?;
        Ok(request)
    }

    pub fn client_ed25519_pub(&self) -> &[u8; RANDOM_BYTES_LEN] {
        &self.client_ed25519_pub
    }

    pub fn client_x25519_eph_pub(&self) -> &[u8; RANDOM_BYTES_LEN] {
        &self.client_x25519_eph_pub
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.descriptor
    }

    /// `device_descriptor` 嵌套消息的**内容原字节**（不含外层 key/长度 varint/帧头），
    /// 即 §4.3 客户端签名输入里的 `descriptor_raw`。
    pub fn descriptor_raw(&self) -> &[u8] {
        &self.descriptor_raw
    }

    pub fn signature(&self) -> &[u8; SIGNATURE_LEN] {
        &self.signature
    }

    /// 把从原始帧截取的 `descriptor_raw` 挂到本请求上（服务端验签路径）。
    ///
    /// 强校验：`raw` 必须能通过 §4.2 严格解析，且解析结果与当前 `descriptor`
    /// **完全一致**；否则返回 [`HandshakeMsgError::DescriptorRawMismatch`]
    /// （或解析自身的错误），绝不接受「先替换再靠业务字段兜底」。
    pub fn with_descriptor_raw(mut self, raw: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_descriptor_size(raw.len())?;
        let parsed = DeviceDescriptor::decode(raw)?;
        if parsed != self.descriptor {
            return Err(HandshakeMsgError::DescriptorRawMismatch);
        }
        self.descriptor_raw = raw.to_vec();
        Ok(self)
    }

    /// canonical 编码：外层字段升序、必要 bytes 恒写出；`device_descriptor` 写出
    /// 保留的原字节（presence 语义，空 descriptor 也写出 0 长度）。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        self.check_semantics()?;
        let mut w = Writer::new();
        w.bytes(1, &self.client_ed25519_pub);
        w.bytes(2, &self.client_x25519_eph_pub);
        w.raw(3, &self.descriptor_raw);
        w.bytes(4, &self.signature);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut client_ed25519_pub: Option<[u8; RANDOM_BYTES_LEN]> = None;
        let mut client_x25519_eph_pub: Option<[u8; RANDOM_BYTES_LEN]> = None;
        let mut descriptor: Option<DeviceDescriptor> = None;
        let mut descriptor_raw: Option<&[u8]> = None;
        let mut signature: Option<[u8; SIGNATURE_LEN]> = None;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    client_ed25519_pub = Some(r.fixed_bytes::<{ RANDOM_BYTES_LEN }>(field)?);
                }
                2 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    client_x25519_eph_pub = Some(r.fixed_bytes::<{ RANDOM_BYTES_LEN }>(field)?);
                }
                3 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    let raw = r.length_delimited()?;
                    // 上限先于内层解析与任何分配，超长的 descriptor 不进入字符串构造。
                    check_descriptor_size(raw.len())?;
                    descriptor = Some(DeviceDescriptor::decode(raw)?);
                    descriptor_raw = Some(raw);
                }
                4 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    signature = Some(r.fixed_bytes::<{ SIGNATURE_LEN }>(field)?);
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        let (descriptor, descriptor_raw) = match (descriptor, descriptor_raw) {
            (Some(descriptor), Some(raw)) => (descriptor, raw.to_vec()),
            _ => return Err(HandshakeMsgError::MissingField(3)),
        };
        Ok(ClientAuthRequest {
            client_ed25519_pub: client_ed25519_pub.ok_or(HandshakeMsgError::MissingField(1))?,
            client_x25519_eph_pub: client_x25519_eph_pub
                .ok_or(HandshakeMsgError::MissingField(2))?,
            descriptor,
            descriptor_raw,
            signature: signature.ok_or(HandshakeMsgError::MissingField(4))?,
        })
    }

    /// 结构外的约束：descriptor 原字节上限与整包上限。
    fn check_semantics(&self) -> Result<(), HandshakeMsgError> {
        check_descriptor_size(self.descriptor_raw.len())
    }
}

// ====================================================================
// ServerPending / PendingPong
// ====================================================================

/// `ServerPending { uint32 wait_hint_ms = 1; bytes pending_token = 2;
/// bytes status_nonce = 3; uint64 probe_seq = 4; }`（§4.2/§4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPending {
    pub wait_hint_ms: u32,
    pub pending_token: [u8; PENDING_TOKEN_LEN],
    pub status_nonce: [u8; STATUS_NONCE_LEN],
    pub probe_seq: u64,
}

/// `PendingPong { bytes pending_token = 1; bytes status_nonce = 2; uint64 probe_seq = 3; }`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPong {
    pub pending_token: [u8; PENDING_TOKEN_LEN],
    pub status_nonce: [u8; STATUS_NONCE_LEN],
    pub probe_seq: u64,
}

impl ServerPending {
    /// 构造并校验 §4.1 语义：`wait_hint_ms` 固定 5000、`probe_seq` 非 0。
    pub fn new(
        pending_token: [u8; PENDING_TOKEN_LEN],
        status_nonce: [u8; STATUS_NONCE_LEN],
        probe_seq: u64,
    ) -> Result<Self, HandshakeMsgError> {
        let message = ServerPending {
            wait_hint_ms: PENDING_WAIT_HINT_MS,
            pending_token,
            status_nonce,
            probe_seq,
        };
        check_pending_semantics(message.wait_hint_ms, message.probe_seq)?;
        Ok(message)
    }

    /// canonical 编码（字段升序）；语义违例按解码同一顺序报同一变体。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        check_pending_semantics(self.wait_hint_ms, self.probe_seq)?;
        let mut w = Writer::new();
        w.uint32(1, self.wait_hint_ms);
        w.bytes(2, &self.pending_token);
        w.bytes(3, &self.status_nonce);
        w.uint64(4, self.probe_seq);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut wait_hint_ms = 0u32;
        let mut pending_token: Option<[u8; PENDING_TOKEN_LEN]> = None;
        let mut status_nonce: Option<[u8; STATUS_NONCE_LEN]> = None;
        let mut probe_seq = 0u64;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::VARINT)?;
                    wait_hint_ms = r.uint32()?;
                }
                2 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    pending_token = Some(r.fixed_bytes::<{ PENDING_TOKEN_LEN }>(field)?);
                }
                3 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    status_nonce = Some(r.fixed_bytes::<{ STATUS_NONCE_LEN }>(field)?);
                }
                4 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::VARINT)?;
                    probe_seq = r.uint64()?;
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        let message = ServerPending {
            wait_hint_ms,
            pending_token: pending_token.ok_or(HandshakeMsgError::MissingField(2))?,
            status_nonce: status_nonce.ok_or(HandshakeMsgError::MissingField(3))?,
            probe_seq,
        };
        check_pending_semantics(message.wait_hint_ms, message.probe_seq)?;
        Ok(message)
    }
}

impl PendingPong {
    /// 构造并校验 §4.1 语义：客户端逐字回显，`probe_seq` 非 0。
    pub fn new(
        pending_token: [u8; PENDING_TOKEN_LEN],
        status_nonce: [u8; STATUS_NONCE_LEN],
        probe_seq: u64,
    ) -> Result<Self, HandshakeMsgError> {
        let message = PendingPong {
            pending_token,
            status_nonce,
            probe_seq,
        };
        if message.probe_seq == 0 {
            return Err(HandshakeMsgError::ZeroProbeSeq);
        }
        Ok(message)
    }

    /// canonical 编码（字段升序）。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        if self.probe_seq == 0 {
            return Err(HandshakeMsgError::ZeroProbeSeq);
        }
        let mut w = Writer::new();
        w.bytes(1, &self.pending_token);
        w.bytes(2, &self.status_nonce);
        w.uint64(3, self.probe_seq);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut pending_token: Option<[u8; PENDING_TOKEN_LEN]> = None;
        let mut status_nonce: Option<[u8; STATUS_NONCE_LEN]> = None;
        let mut probe_seq = 0u64;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    pending_token = Some(r.fixed_bytes::<{ PENDING_TOKEN_LEN }>(field)?);
                }
                2 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    status_nonce = Some(r.fixed_bytes::<{ STATUS_NONCE_LEN }>(field)?);
                }
                3 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::VARINT)?;
                    probe_seq = r.uint64()?;
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        let message = PendingPong {
            pending_token: pending_token.ok_or(HandshakeMsgError::MissingField(1))?,
            status_nonce: status_nonce.ok_or(HandshakeMsgError::MissingField(2))?,
            probe_seq,
        };
        if message.probe_seq == 0 {
            return Err(HandshakeMsgError::ZeroProbeSeq);
        }
        Ok(message)
    }
}

// ====================================================================
// ServerAuthResponse
// ====================================================================

/// `ServerAuthResponse { uint32 status = 1; uint32 reason_code = 2;
/// bytes server_x25519_eph_pub = 3; bytes signature = 4; }`（§4.2/§4.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerAuthResponse {
    pub status: u32,
    pub reason_code: u32,
    pub server_x25519_eph_pub: [u8; SERVER_EPHEMERAL_LEN],
    pub signature: [u8; SIGNATURE_LEN],
}

impl ServerAuthResponse {
    /// 构造并校验 §4.3 的 status/reason_code/临时公钥组合约束。
    pub fn new(
        status: u32,
        reason_code: u32,
        server_x25519_eph_pub: [u8; SERVER_EPHEMERAL_LEN],
        signature: [u8; SIGNATURE_LEN],
    ) -> Result<Self, HandshakeMsgError> {
        let message = ServerAuthResponse {
            status,
            reason_code,
            server_x25519_eph_pub,
            signature,
        };
        check_server_auth_status(
            message.status,
            message.reason_code,
            &message.server_x25519_eph_pub,
        )?;
        Ok(message)
    }

    /// APPROVED：`status = 0`、`reason_code = 0`、`server_x25519_eph_pub` 必须非全零。
    pub fn approved(
        server_x25519_eph_pub: [u8; SERVER_EPHEMERAL_LEN],
        signature: [u8; SIGNATURE_LEN],
    ) -> Result<Self, HandshakeMsgError> {
        Self::new(
            u32::from(STATUS_APPROVED),
            REASON_UNSPECIFIED,
            server_x25519_eph_pub,
            signature,
        )
    }

    /// REJECTED：`status = 1`、`server_x25519_eph_pub` 为 32B 全零占位，
    /// `reason_code ∈ {0,2,3,4,5}`。
    pub fn rejected(
        reason_code: u32,
        signature: [u8; SIGNATURE_LEN],
    ) -> Result<Self, HandshakeMsgError> {
        Self::new(
            u32::from(STATUS_REJECTED),
            reason_code,
            [0u8; SERVER_EPHEMERAL_LEN],
            signature,
        )
    }

    /// canonical 编码（字段升序；`reason_code = 0` 按缺省不写）。
    pub fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        check_server_auth_status(self.status, self.reason_code, &self.server_x25519_eph_pub)?;
        let mut w = Writer::new();
        w.uint32(1, self.status);
        w.uint32(2, self.reason_code);
        w.bytes(3, &self.server_x25519_eph_pub);
        w.bytes(4, &self.signature);
        let out = w.finish();
        check_message_size(&out)?;
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, HandshakeMsgError> {
        check_message_size(bytes)?;
        let mut r = Reader::new(bytes);
        let mut seen = Seen::default();
        let mut status = 0u32;
        let mut reason_code = 0u32;
        let mut server_x25519_eph_pub: Option<[u8; SERVER_EPHEMERAL_LEN]> = None;
        let mut signature: Option<[u8; SIGNATURE_LEN]> = None;
        while !r.eof() {
            let (field, wire_type) = read_tag(&mut r)?;
            match field {
                1 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::VARINT)?;
                    status = r.uint32()?;
                }
                2 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::VARINT)?;
                    reason_code = r.uint32()?;
                }
                3 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    server_x25519_eph_pub = Some(r.fixed_bytes::<{ SERVER_EPHEMERAL_LEN }>(field)?);
                }
                4 => {
                    seen.mark(field)?;
                    expect_wire(field, wire_type, wt::LEN)?;
                    signature = Some(r.fixed_bytes::<{ SIGNATURE_LEN }>(field)?);
                }
                other => return Err(HandshakeMsgError::UnknownField(other)),
            }
        }
        check_consumed(&r)?;
        let message = ServerAuthResponse {
            status,
            reason_code,
            server_x25519_eph_pub: server_x25519_eph_pub
                .ok_or(HandshakeMsgError::MissingField(3))?,
            signature: signature.ok_or(HandshakeMsgError::MissingField(4))?,
        };
        check_server_auth_status(
            message.status,
            message.reason_code,
            &message.server_x25519_eph_pub,
        )?;
        Ok(message)
    }
}
