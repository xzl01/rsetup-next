//! 握手帧与加密记录的编排适配层（§4.1/§4.2/§5.2）。
//!
//! 本模块只做纯字节适配，不建立 socket、不做签名编排、不持有准入状态：
//! * 把 [`FrameType`] 与 [`crate::handshake_msg`] 的消息严格配对并加/去 8B 帧头；
//! * 提供有界的 4B 记录长度重组器，保证 `open` 只收到恰好一条完整记录；
//! * 用 [`EstablishedTunnel`] 唯一持有 [`RecordLayerPair`]，只暴露 pair 级
//!   `send`/`recv`，避免调用方误用 `tx`/`rx` 造成同 key/nonce 重用。

use thiserror::Error;

use crate::crypto::{
    CryptoError, MAX_RECORD_BODY_LEN, MIN_RECORD_BODY_LEN, PreparedRecord, RecordLayerPair,
};
use crate::frame::{self, FrameType};
use crate::handshake_msg::{
    ClientAuthRequest, ClientHello, HandshakeMsgError, PendingPong, ServerAuthResponse,
    ServerChallenge, ServerPending,
};

/// 一条握手帧对应的 typed 消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeMessage {
    ClientHello(ClientHello),
    ServerChallenge(ServerChallenge),
    ClientAuthRequest(ClientAuthRequest),
    ServerPending(ServerPending),
    PendingPong(PendingPong),
    ServerAuthResponse(ServerAuthResponse),
}

impl HandshakeMessage {
    fn frame_type(&self) -> FrameType {
        match self {
            Self::ClientHello(_) => FrameType::ClientHello,
            Self::ServerChallenge(_) => FrameType::ServerChallenge,
            Self::ClientAuthRequest(_) => FrameType::ClientAuthRequest,
            Self::ServerPending(_) => FrameType::ServerPending,
            Self::PendingPong(_) => FrameType::PendingPong,
            Self::ServerAuthResponse(_) => FrameType::ServerAuthResponse,
        }
    }

    fn encode(&self) -> Result<Vec<u8>, HandshakeMsgError> {
        match self {
            Self::ClientHello(m) => m.encode(),
            Self::ServerChallenge(m) => m.encode(),
            Self::ClientAuthRequest(m) => m.encode(),
            Self::ServerPending(m) => m.encode(),
            Self::PendingPong(m) => m.encode(),
            Self::ServerAuthResponse(m) => m.encode(),
        }
    }

    fn decode(frame_type: FrameType, payload: &[u8]) -> Result<Self, HandshakeMsgError> {
        Ok(match frame_type {
            FrameType::ClientHello => Self::ClientHello(ClientHello::decode(payload)?),
            FrameType::ServerChallenge => Self::ServerChallenge(ServerChallenge::decode(payload)?),
            FrameType::ClientAuthRequest => {
                Self::ClientAuthRequest(ClientAuthRequest::decode(payload)?)
            }
            FrameType::ServerPending => Self::ServerPending(ServerPending::decode(payload)?),
            FrameType::PendingPong => Self::PendingPong(PendingPong::decode(payload)?),
            FrameType::ServerAuthResponse => {
                Self::ServerAuthResponse(ServerAuthResponse::decode(payload)?)
            }
        })
    }
}

/// 适配层错误：固定、去敏，不回显 payload/密钥。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdapterError {
    #[error("frame error: {0}")]
    Frame(#[from] frame::FrameError),
    #[error("handshake message error: {0}")]
    Message(#[from] HandshakeMsgError),
    #[error("frame/message type mismatch: frame={frame:?}, message={message}")]
    TypeMismatch {
        frame: FrameType,
        message: &'static str,
    },
    #[error("input is not exactly one complete handshake frame")]
    NotExactlyOneFrame,
    #[error("record body too small: {0} < 16")]
    RecordTooSmall(u32),
    #[error("record body too large: {0} > 65536")]
    RecordTooLarge(u32),
    /// 重组器已进入致命状态（非法长度字段之后）：本对象**不可复用**，任何
    /// 后续输入一律返回本变体，绝不交付任何记录（§5.2 一条坏记录 = 整条 TCP
    /// 致命，不允许"丢掉这一批继续收"）。
    #[error("record reassembler is fatal; the whole TCP connection must be closed")]
    ReassemblerFatal,
    /// ⚠️ **保留但不再产生**：初版按"已缓冲 + 本批"总量卡 65540，两三条完整
    /// 记录一起到达就会被误杀。现在 [`RecordReassembler`] 改为边到达边成帧，
    /// 未完成的残留字节由构造恒 ≤ 65540（见其文档），因此不存在"批量过大"
    /// 这一类错误。本变体仅为既有 `match` 的兼容性保留。
    #[error("record reassembly buffer would exceed 65540 bytes")]
    BufferLimit,
    #[error("record authentication/decryption failed or tunnel is fatal: {0}")]
    Crypto(#[from] CryptoError),
    #[error("tunnel is already fatal; the whole TCP connection must be closed")]
    TunnelFatal,
}

/// 把 typed 握手消息编码为一条完整握手帧。
pub fn encode_handshake_frame(
    frame_type: FrameType,
    message: &HandshakeMessage,
) -> Result<Vec<u8>, AdapterError> {
    if message.frame_type() != frame_type {
        return Err(AdapterError::TypeMismatch {
            frame: frame_type,
            message: message_type_name(message),
        });
    }
    let payload = message.encode()?;
    Ok(frame::encode_frame(frame_type, &payload)?)
}

/// 解析一条完整握手帧为 typed 消息；拒绝不足/多余字节与类型不匹配。
pub fn decode_handshake_frame(bytes: &[u8]) -> Result<(FrameType, HandshakeMessage), AdapterError> {
    let header = frame::parse_frame_header(bytes)?;
    let expected = frame::FRAME_HEADER_LEN
        .checked_add(header.payload_len as usize)
        .ok_or(AdapterError::NotExactlyOneFrame)?;
    if bytes.len() != expected {
        return Err(AdapterError::NotExactlyOneFrame);
    }
    let payload = &bytes[frame::FRAME_HEADER_LEN..];
    let message = HandshakeMessage::decode(header.frame_type, payload)?;
    Ok((header.frame_type, message))
}

fn message_type_name(message: &HandshakeMessage) -> &'static str {
    match message {
        HandshakeMessage::ClientHello(_) => "ClientHello",
        HandshakeMessage::ServerChallenge(_) => "ServerChallenge",
        HandshakeMessage::ClientAuthRequest(_) => "ClientAuthRequest",
        HandshakeMessage::ServerPending(_) => "ServerPending",
        HandshakeMessage::PendingPong(_) => "PendingPong",
        HandshakeMessage::ServerAuthResponse(_) => "ServerAuthResponse",
    }
}

/// 4B BE 长度前缀记录重组器（§5.2）。
///
/// 接收缓冲上限 65540 字节（4B 前缀 + 最大 65536B 记录体），`push` 只交付
/// **恰好一条完整记录**（含 4B 前缀）；半条记录留在内部，不向上层交付任何字节。
///
/// 三条硬约束（初版三条都违反过，均由本类型的测试钉死）：
/// 1. **上限不削弱**：不保留任何"已到达但未成帧"的多余字节——输入被边到达边
///    成帧，`buffer` 只容纳"当前这一条尚未收完的记录"，因此
///    [`Self::buffered_len`] 由构造恒 ≤ [`MAX_RECORD_BUFFER`]。初版按
///    "已缓冲 + 本批长度"卡上限，把合法的多条完整记录批量到达误判为超限；
///    现在改为按"当前记录剩余需求"取用输入，上限只约束真正的残留字节。
/// 2. **超限前缀到达即拒绝**：长度字段一旦凑满 4B 立刻判定 16..=65536，不等
///    payload（长度 65537 的记录不必先收 65537 字节再报错）。
/// 3. **致命粘滞**：非法长度 ⇒ 进入 fatal 态并清空缓冲，此后任何输入都返回
///    [`AdapterError::ReassemblerFatal`]，绝不"丢掉这批继续收"（§5.2：任何
///    记录级失败都是连接级致命，不得在原连接重同步）。
///
/// 长度解析不使用 `expect`/panic：任何字节序列都只能返回 `Ok`/`Err`。
#[derive(Debug, Default)]
pub struct RecordReassembler {
    /// 当前这条**尚未收完**的记录字节（含已到达的前缀）。
    buffer: Vec<u8>,
    /// 当前记录的总长度（4B 前缀 + 记录体），仅在 `buffer` 达到 4B 时有效。
    expect_total: usize,
    /// 致命粘滞位：一旦 true 永不复位。
    fatal: bool,
}

/// 接收缓冲上限：4B 长度前缀 + 最大 65536B 记录体。
pub const MAX_RECORD_BUFFER: usize = 4 + 65_536;

/// 长度前缀字节数。
const LEN_PREFIX: usize = 4;

impl RecordReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前未交付的字节数（= 当前未完成记录已到达的字节），仅用于测试/观测。
    ///
    /// 由构造保证恒 ≤ [`MAX_RECORD_BUFFER`]。
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    /// 本重组器是否已进入致命态（一旦 true 永不复位）。
    pub fn is_fatal(&self) -> bool {
        self.fatal
    }

    /// 下一次底层读取**不越过当前记录边界**时可安全取用的最大字节数。
    ///
    /// 供 `SecureIo` 这类"每次 read 都对应一条记录"的读侧使用：把 read 上限
    /// 收到当前记录的剩余字节数，同一个 read 就不会同时带来"上一条记录的
    /// 最后一个字节"和"下一条记录的首字节"，§5.2 的 30s 部分记录期限因此
    /// 只有一条记录的时间归属可言（首字节到达时刻唯一）。
    ///
    /// * 当前没有未完成记录（残留 0 字节）⇒ 返回 `cap`：新记录可以从本 chunk
    ///   起头，其首字节时刻就是"现在"；
    /// * 长度前缀还没凑齐（1..3 字节残留）⇒ 返回 `cap`（此时无法预判边界，
    ///   由调用方在成帧后按记录边界重新锚定计时）；
    /// * 前缀已知 ⇒ 返回"当前记录还缺的字节数"与 `cap` 的较小值。
    pub fn read_limit(&self, cap: usize) -> usize {
        if self.buffer.len() < LEN_PREFIX {
            return cap;
        }
        (self.expect_total - self.buffer.len()).min(cap)
    }

    /// 喂入任意长度的到达字节，返回本次凑齐的**完整记录**（可能 0..N 条）。
    ///
    /// 出错时本对象进入 fatal 态、清空缓冲并返回 `Err`，已交付的记录不回退
    /// （错误总是发生在所有已完整记录**之后**，所以调用方拿到的记录都是
    /// 合法的；随后必须关闭整条 TCP）。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, AdapterError> {
        if self.fatal {
            return Err(AdapterError::ReassemblerFatal);
        }
        let mut records: Vec<Vec<u8>> = Vec::new();
        let mut cursor = 0usize;
        while cursor < bytes.len() {
            if self.buffer.len() < LEN_PREFIX {
                // 前缀未凑满：只补到 4B 就停下判定，绝不先把 payload 吞进缓冲。
                let take = (bytes.len() - cursor).min(LEN_PREFIX - self.buffer.len());
                self.buffer.extend_from_slice(&bytes[cursor..cursor + take]);
                cursor += take;
                if self.buffer.len() < LEN_PREFIX {
                    break;
                }
                // 长度字段刚到齐就判定（约束 2）。
                let declared = u32::from_be_bytes([
                    self.buffer[0],
                    self.buffer[1],
                    self.buffer[2],
                    self.buffer[3],
                ]);
                if declared < MIN_RECORD_BODY_LEN as u32 {
                    return self.fail(AdapterError::RecordTooSmall(declared));
                }
                if declared > MAX_RECORD_BODY_LEN as u32 {
                    return self.fail(AdapterError::RecordTooLarge(declared));
                }
                self.expect_total = LEN_PREFIX + declared as usize;
                continue;
            }
            // 已知合法长度：只取到"当前记录结束点"，多余字节留给下一条成帧，
            // 因此缓冲永不越过单条记录上界（约束 1）。
            let need = self.expect_total - self.buffer.len();
            let take = need.min(bytes.len() - cursor);
            self.buffer.extend_from_slice(&bytes[cursor..cursor + take]);
            cursor += take;
            if self.buffer.len() < self.expect_total {
                break;
            }
            debug_assert_eq!(self.buffer.len(), self.expect_total);
            debug_assert!(self.buffer.len() <= MAX_RECORD_BUFFER);
            records.push(core::mem::take(&mut self.buffer));
        }
        Ok(records)
    }

    /// 记录级致命：置粘滞位、清空缓冲后返回错误（约束 3）。
    fn fail(&mut self, err: AdapterError) -> Result<Vec<Vec<u8>>, AdapterError> {
        self.fatal = true;
        self.buffer.clear();
        self.expect_total = 0;
        Err(err)
    }
}

/// 握手完成后的加密隧道端点：唯一持有 [`RecordLayerPair`]。
///
/// 不暴露 `tx`/`rx`，调用方只能通过 `send`/`recv` 使用；任何失败都会经
/// 共享熔断位把整对置为 fatal。`send` 返回后若写出失败/部分写入，必须调用
/// [`Self::abort`] 并关闭整条 TCP，禁止重试已 seal 的记录。
#[derive(Debug)]
pub struct EstablishedTunnel {
    pair: RecordLayerPair,
}

impl EstablishedTunnel {
    pub fn new(pair: RecordLayerPair) -> Self {
        Self { pair }
    }

    pub fn is_fatal(&self) -> bool {
        self.pair.is_fatal()
    }

    /// 加密一条记录（原子的 prepare+commit）；返回的字节必须**完整**写出，
    /// 否则调用 [`Self::abort`]。
    ///
    /// 需要严格"整条写出确认后才推进序号"的网络写出侧请用
    /// [`Self::prepare_record`] + [`Self::commit_record`]。
    pub fn send(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, AdapterError> {
        if self.pair.is_fatal() {
            return Err(AdapterError::TunnelFatal);
        }
        Ok(self.pair.tx.seal(plaintext)?)
    }

    /// 解密一条完整记录；输入必须由 [`RecordReassembler`] 保证恰好一条。
    pub fn recv(&mut self, record: &[u8]) -> Result<Vec<u8>, AdapterError> {
        if self.pair.is_fatal() {
            return Err(AdapterError::TunnelFatal);
        }
        Ok(self.pair.rx.open(record)?)
    }

    /// 写出失败/部分写入或连接级错误：整对作废，调用方关闭 TCP。
    pub fn abort(&mut self) {
        self.pair.mark_write_failed();
    }

    /// **网络写出侧**的两阶段入口之一：预封箱一条记录，**不**推进序号。
    ///
    /// 返回的 [`PreparedRecord::wire`] 必须**完整**写进 socket；得到确认后调用
    /// [`Self::commit_record`] 才推进 `TxSeq`（§5.2）。写出结果不确定时**直接
    /// drop 这条 pending**（或 [`Self::abort_pending_record`]）——整对随即作废，
    /// 不存在"再 seal 一条重发"的路径。在途已有一条时 [`Self::send`] 与本方法
    /// 都会返回 [`CryptoError::PendingOutstanding`]。
    pub fn prepare_record(&mut self, plaintext: &[u8]) -> Result<PreparedRecord, AdapterError> {
        if self.pair.is_fatal() {
            return Err(AdapterError::TunnelFatal);
        }
        Ok(self.pair.tx.prepare(plaintext)?)
    }

    /// 两阶段入口之二：确认整条记录已完整写出，推进 `TxSeq` 并返回该序号。
    /// 跨 pair/越序/重复提交一律拉闸整对（[`CryptoError::CommitMismatch`]）。
    pub fn commit_record(&mut self, pending: PreparedRecord) -> Result<u64, AdapterError> {
        Ok(self.pair.tx.commit(pending)?)
    }

    /// 放弃在途 pending 并拉闸整对；无在途记录时返回 false。
    pub fn abort_pending_record(&mut self) -> bool {
        self.pair.tx.abort_pending()
    }
}
