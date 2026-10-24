//! v3 前置安全握手（Tokio 传输层，protocol_spec.md §4.1–§4.6、计划 Task 2）。
//!
//! 本模块把 `rsetup-protocol` 的纯字节契约（帧边界、严格 wire 审计、签名输入、
//! X25519/HKDF）接到真实 Tokio 传输上，只负责「一条连接从 TCP 到会话密钥」：
//! 不查库、不做 h2/gRPC、不持有准入状态——准入由调用方以 [`AdmissionPolicy`] 注入。
//!
//! 关键规则（逐条由 `tests/handshake.rs` 实测）：
//!
//! * **无 TOFU**：板端必须至少配置一个可信中控公钥，否则在发出任何字节之前失败
//!   （[`HandshakeError::NoTrustedServers`]）。
//! * **审计先于副作用**：帧头 v3 校验 + `handshake_msg` 逐条严格 wire 审计全部通过
//!   之前，不调用准入策略、不交付任何握手结果；客户端签名失败直接关闭连接，
//!   不回 `REJECTED(SIGNATURE_INVALID)`，也不写任何永久决定。
//! * **`descriptor_raw` 直接签名**：签名输入只用帧内 `device_descriptor` 的内容
//!   原字节；本模块从不重编码后验签（客户端也只编码一次，见
//!   `ClientAuthRequest::new` 的同源约束）。
//! * **每次连接全新随机与临时密钥**：`client_random`/`server_random` 取自 ring
//!   CSPRNG，X25519 临时密钥对一次会话一对（[`EphemeralKeyPair::agree`] 消耗自身）。
//! * **超时**：初始步骤 10s（含服务端验签与准入工作）；进入 PENDING 后 probe
//!   周期 5s、单在途 pong 期限 10s、板端接收静默期限 15s，且**不设审批总期限**。
//!   两个期限都是**绝对期限**：pong 期限锚在 probe 提交的时刻（写出被背压挡住的耗时
//!   算在 10s 内），板端静默期限锚在最后一条合法帧到达的时刻（回 pong 的耗时不能
//!   把它推后）。批准只会发生在在途 probe 结清之后，并在最终批准前重新核验准入策略
//!   （撤销优先）；准入策略长时间不返回时，中控侧把被 pin 的准入 future 与 probe
//!   调度并发等待，探测节奏绝不被 `evaluate` 拖住。挂起期内**全程**读取入站帧：
//!   在途 probe 已结清后到达的任何帧都是当场关闭的协议违例，绝不随批准交付；
//!   pong 只有严格早于原 `probe 提交 + 10s` 到达才结清（整点及以后即超时，
//!   与 `select!` 分支就绪顺序无关）。回到 Pending 的每种转换都走同一个有界
//!   复查退避，不存在 0ns 连轴转。
//! * **未认证信息不得影响重连**：只有**验签通过**的 `REJECTED` 才能映射为
//!   [`HandshakeError::SignedRejection`]；`REVOKED(2)`/`APPROVAL_DENIED(3)` 才让
//!   调用方停止自动重连（[`HandshakeError::stops_auto_reconnect`]）。坏签名或
//!   不受信公钥签的拒绝一律按 [`HandshakeError::UntrustedServer`] 处理。
//!
//! 错误与 `Debug` 全程去敏：只含 [`std::io::ErrorKind`]、阶段/字段名与协议错误
//! 变体，不回显 payload、公钥字节、身份或路径。

use std::fmt;
use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use ring::rand::{SecureRandom, SystemRandom};
use rsetup_protocol::crypto::{CryptoError, EphemeralKeyPair, SessionKeys};
use rsetup_protocol::ed25519::verify_ed25519;
use rsetup_protocol::frame::{self, FRAME_HEADER_LEN, FrameType};
use rsetup_protocol::handshake_adapter::{
    AdapterError, HandshakeMessage, decode_handshake_frame, encode_handshake_frame,
};
use rsetup_protocol::handshake_msg::{
    ClientAuthRequest, ClientHello, DeviceDescriptor, HandshakeMsgError, PendingPong,
    ServerAuthResponse, ServerChallenge, ServerPending,
};
use rsetup_protocol::sig::{
    REASON_APPROVAL_DENIED, REASON_HANDSHAKE_TIMEOUT, REASON_REVOKED, REASON_SERVER_ERROR,
    REASON_SIGNATURE_INVALID, REASON_UNSPECIFIED, STATUS_REJECTED, server_signature_input,
};
use thiserror::Error;

use crate::identity::IdentityKey;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout, timeout_at};

/// §4.1：非 PENDING 的初始握手步骤期限，包含服务端验签与准入检查。
pub const INITIAL_STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// §4.1：PENDING probe 名义周期（下一轮不早于上轮发送后 5s）。
pub const PROBE_PERIOD: Duration = Duration::from_secs(5);
/// §4.1：单在途 probe 的 pong 期限。
pub const PROBE_PONG_TIMEOUT: Duration = Duration::from_secs(10);
/// §4.1：板端进入 PENDING 后的接收静默期限（发送 pong 不刷新）。
pub const CLIENT_SILENCE_TIMEOUT: Duration = Duration::from_secs(15);
/// Pending 结果之后允许再次查询准入状态的最短间隔（= 名义 probe 周期）。
///
/// 这不是协议超时，也不改变任何 §4.1 期限：它存在的唯一理由是「`evaluate` 立刻返回
/// Pending」时不能连轴转地重查。批准仍会在在途 probe 结清的那一刻立即生效。
pub const ADMISSION_RECHECK_DELAY: Duration = PROBE_PERIOD;

// ================================================================ 公开类型

/// 准入策略返回的未来。调用方可挂起等待人工审批，不设总期限。
pub type AdmissionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AdmissionDecision, HandshakeError>> + Send + 'a>>;

/// 中控侧准入决定。`Rejected` 携带 §4.3 的 `reason_code`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// 新身份挂起待审批：本模块立即进入 probe 流程，不占用审批总期限。
    Pending,
    /// 已批准。
    Approved,
    /// 已拒绝。仅允许 `0/2/3/4/5`；`1`（SIGNATURE_INVALID）在 v3 绝不发出。
    Rejected(u32),
}

/// 已通过严格 wire 审计与客户端签名验证的对端身份，交给准入策略的输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPeer {
    /// 已验签的板端 Ed25519 公钥。
    pub public_key: [u8; 32],
    /// 从帧内原字节解析出的设备描述符。
    pub descriptor: DeviceDescriptor,
}

/// 中控侧准入判定注入点。
///
/// 每次需要「最新状态」时调用一次：进入握手时的初判、每轮 probe 结清后的复查，
/// 以及最终批准前的临界区核验（撤销优先）。
pub trait AdmissionPolicy: Send + Sync {
    /// 对已验签的 `peer` 作出决定。错误直接终止本连接且不写永久决定。
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a>;
}

/// 板端握手配置。
pub struct ClientHandshakeConfig {
    /// 板端长期身份密钥。
    pub identity: Arc<IdentityKey>,
    /// 可信中控 Ed25519 公钥列表，至少一个（禁止隐式 TOFU）。
    pub trusted_servers: Vec<[u8; 32]>,
    /// 上报的设备描述符。
    pub descriptor: DeviceDescriptor,
}

/// 中控端握手配置。
pub struct ServerHandshakeConfig {
    /// 中控长期身份密钥。
    pub identity: Arc<IdentityKey>,
}

/// 握手成功后的交付物：原始连接 + 会话密钥 + 对端身份。
#[must_use = "握手结果包含会话密钥，忽略即丢弃已建立的加密通道"]
pub struct HandshakeResult<S> {
    /// 已完成握手、可直接接入记录层（`SecureIo`）的连接。
    pub io: S,
    /// §5.1 派生的会话密钥。一次性消耗品，只能产出一个 `RecordLayerPair`。
    pub session_keys: SessionKeys,
    /// 对端长期公钥：板端看到中控公钥（必为可信列表中被匹配的那一个），
    /// 中控看到板端公钥。
    pub peer_public_key: [u8; 32],
    /// 设备描述符：仅中控侧为 `Some`，板端为 `None`。
    pub descriptor: Option<DeviceDescriptor>,
}

impl<S: fmt::Debug> fmt::Debug for HandshakeResult<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HandshakeResult")
            .field("io", &self.io)
            .field("session_keys", &"SessionKeys(<redacted>)")
            .field("peer_public_key", &hex::encode(self.peer_public_key))
            .field("descriptor", &self.descriptor)
            .finish()
    }
}

/// 握手阶段标签，用于去敏的超时错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeStage {
    /// 发送 CLIENT_HELLO。
    ClientHello,
    /// 等待 SERVER_CHALLENGE。
    ServerChallenge,
    /// 发送 CLIENT_AUTH_REQUEST。
    ClientAuthRequest,
    /// 等待最终 SERVER_AUTH_RESPONSE（非 PENDING 路径）。
    ServerAuthResponse,
    /// 等待准入决定或首个 SERVER_PENDING（含策略工作）。
    AdmissionDecision,
    /// 等待匹配 pong；也用于板端回送 pong 的写出。
    PendingPong,
    /// 板端 PENDING 期间的接收静默期限。
    ClientSilence,
}

/// 经**合法签名**认证的中控拒绝原因（§4.3/§4.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticatedRejection {
    /// `0` 未说明。
    Unspecified,
    /// `2` 已吊销。
    Revoked,
    /// `3` 审批被拒。
    ApprovalDenied,
    /// `4` 握手步超时。
    HandshakeTimeout,
    /// `5` 服务端错误（过载等），应退避重试。
    ServerError,
}

impl AuthenticatedRejection {
    /// §4.6 第 4 条：仅 `REVOKED` 与 `APPROVAL_DENIED` 让板端停止自动重连。
    #[must_use]
    pub fn stops_auto_reconnect(self) -> bool {
        matches!(self, Self::Revoked | Self::ApprovalDenied)
    }

    fn from_reason(code: u32) -> Option<Self> {
        match code {
            REASON_UNSPECIFIED => Some(Self::Unspecified),
            REASON_REVOKED => Some(Self::Revoked),
            REASON_APPROVAL_DENIED => Some(Self::ApprovalDenied),
            REASON_HANDSHAKE_TIMEOUT => Some(Self::HandshakeTimeout),
            REASON_SERVER_ERROR => Some(Self::ServerError),
            _ => None,
        }
    }

    fn reason_code(self) -> u32 {
        match self {
            Self::Unspecified => REASON_UNSPECIFIED,
            Self::Revoked => REASON_REVOKED,
            Self::ApprovalDenied => REASON_APPROVAL_DENIED,
            Self::HandshakeTimeout => REASON_HANDSHAKE_TIMEOUT,
            Self::ServerError => REASON_SERVER_ERROR,
        }
    }
}

/// 协议违例。变体只携带结构信息，绝不携带 payload 或密钥。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProtocolViolation {
    /// 帧头非法：magic/version/帧类型/payload 上限。
    #[error("bad v3 frame header: {0}")]
    Frame(#[from] frame::FrameError),
    /// §4.2 严格 wire 审计失败。
    #[error("strict handshake message audit failed: {0}")]
    Message(#[from] HandshakeMsgError),
    /// 帧类型与当前握手阶段不匹配（乱序、角色颠倒、终态后重发）。
    #[error("unexpected frame in this phase: expected {expected}, got {got}")]
    UnexpectedFrame {
        expected: &'static str,
        got: &'static str,
    },
    /// PENDING 三元组（token / status_nonce / probe_seq）失配。
    #[error("pending probe mismatch: {0}")]
    Pending(&'static str),
    /// 帧与消息类型不配对，或输入不是恰好一条完整帧。
    #[error("frame/message mismatch: {0}")]
    Framing(&'static str),
    /// 本模块自身的同源约束被破坏（对端无法触发）。
    #[error("internal handshake invariant violated: {0}")]
    Internal(&'static str),
}

/// 握手错误。全部变体去敏；判定是否应停止自动重连请用
/// [`HandshakeError::stops_auto_reconnect`]。
#[derive(Debug, Error)]
pub enum HandshakeError {
    /// 传输层失败（含对端关闭：`ErrorKind::UnexpectedEof`）。只保留 `ErrorKind`。
    #[error("transport failure: {0:?}")]
    Io(ErrorKind),

    /// 指定步骤超出期限。
    #[error("handshake step {0:?} exceeded its deadline")]
    Timeout(HandshakeStage),

    /// 帧/消息/序列违例。
    #[error("v3 handshake protocol violation: {0}")]
    Protocol(#[from] ProtocolViolation),

    /// 板端身份签名与握手转录不符：直接关闭，不回拒绝、不写永久决定。
    #[error("client identity signature does not match the handshake transcript")]
    ClientSignature,

    /// 中控响应签名无效，或不是可信列表内公钥所签：**未认证**信息。
    #[error("server response signature invalid or not issued by a trusted server key")]
    UntrustedServer,

    /// 板端未配置任何可信中控公钥（禁止隐式 TOFU）。
    #[error("no trusted controller public key configured")]
    NoTrustedServers,

    /// 准入策略自身失败，或给出了 v3 不允许的 reason。
    #[error("admission policy failed")]
    Policy,

    /// X25519 协商或会话密钥派生失败（含全零共享密钥）。
    #[error("key agreement or session key derivation failed")]
    KeyAgreement,

    /// **验签通过**的 REJECTED。只有它的 `reason` 可用于停止自动重连。
    #[error("peer sent an authenticated rejection: {reason:?}")]
    SignedRejection { reason: AuthenticatedRejection },
}

impl HandshakeError {
    /// §4.6 第 4 条：只有可信签名的 `REVOKED`/`APPROVAL_DENIED` 才停止自动重连；
    /// 任何未认证信息（坏签名、不受信公钥）与超时/传输错误都可重试。
    #[must_use]
    pub fn stops_auto_reconnect(&self) -> bool {
        match self {
            Self::SignedRejection { reason } => reason.stops_auto_reconnect(),
            _ => false,
        }
    }
}

impl From<frame::FrameError> for HandshakeError {
    fn from(err: frame::FrameError) -> Self {
        Self::Protocol(ProtocolViolation::Frame(err))
    }
}

impl From<std::io::Error> for HandshakeError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.kind())
    }
}

impl From<AdapterError> for HandshakeError {
    fn from(err: AdapterError) -> Self {
        match err {
            AdapterError::Frame(e) => Self::Protocol(ProtocolViolation::Frame(e)),
            AdapterError::Message(e) => Self::Protocol(ProtocolViolation::Message(e)),
            AdapterError::TypeMismatch { .. } => {
                Self::Protocol(ProtocolViolation::Framing("frame/message type mismatch"))
            }
            AdapterError::NotExactlyOneFrame => {
                Self::Protocol(ProtocolViolation::Framing("not exactly one frame"))
            }
            other => Self::Protocol(ProtocolViolation::Framing(other.kind())),
        }
    }
}

impl From<HandshakeMsgError> for HandshakeError {
    fn from(err: HandshakeMsgError) -> Self {
        Self::Protocol(ProtocolViolation::Message(err))
    }
}

impl From<CryptoError> for HandshakeError {
    fn from(_: CryptoError) -> Self {
        Self::KeyAgreement
    }
}

/// `AdapterError` 的其余变体只可能来自记录层路径，握手用不到。
trait AdapterErrorKind {
    fn kind(&self) -> &'static str;
}

impl AdapterErrorKind for AdapterError {
    fn kind(&self) -> &'static str {
        match self {
            Self::RecordTooSmall(_) => "record too small",
            Self::RecordTooLarge(_) => "record too large",
            Self::BufferLimit => "record buffer limit",
            Self::Crypto(_) => "record crypto",
            Self::TunnelFatal => "tunnel fatal",
            _ => "adapter error",
        }
    }
}

// ================================================================ 帧 I/O

/// 带绝对期限地写出一帧（写出耗时算进期限，不重开计时）。
async fn send_frame<S>(
    stage: HandshakeStage,
    deadline: Instant,
    io: &mut S,
    frame_type: FrameType,
    message: &HandshakeMessage,
) -> Result<(), HandshakeError>
where
    S: AsyncWrite + Unpin,
{
    with_deadline(stage, deadline, write_frame(io, frame_type, message)).await?
}

/// 可安全取消的握手帧读取器。
///
/// 不能直接用 `AsyncReadExt::read_exact`：`select!` 会在帧还没读完时因为别的分支
/// 先就绪而丢弃这个 future，`read_exact` 会把「已经读走的字节」一起丢掉，破坏后续帧
/// 的边界。这里把字节直接读进随调用方存活的 `buf`，future 被丢弃也不丢字节，
/// `next_frame()` 因此可以安全重入。字节数按已解析出的帧声明推进，不信任任何长度。
struct FrameReader<S> {
    io: S,
    buf: BytesMut,
}

impl<S> FrameReader<S>
where
    S: AsyncRead + Unpin,
{
    fn new(io: S) -> Self {
        Self {
            io,
            buf: BytesMut::new(),
        }
    }

    /// 写侧借用：只在没有读 future 存活的时刻使用（`select!` 之外）。
    fn io_mut(&mut self) -> &mut S {
        &mut self.io
    }

    /// 交还连接前复核缓冲区已空：握手结束后不应再有本模块未消费的字节，
    /// 非空说明对端在多发内容，按协议违例 fail closed，绝不把残包带进加密通道。
    fn into_inner_checked(self) -> Result<S, HandshakeError> {
        if !self.buf.is_empty() {
            return Err(HandshakeError::Protocol(ProtocolViolation::Framing(
                "unexpected trailing bytes after handshake",
            )));
        }
        Ok(self.io)
    }

    /// 批准线性化点的有界检查：用户缓冲为空仍不代表底层没有已排队字节。
    /// 只poll一次、最多消费1B；Pending立即表示此刻无可观察输入，不等待静默。
    /// 此phase客户端唯一合法动作是等待最终响应，所以任何额外首byte都应拒绝。
    /// 检查之后才到达的未来输入不在此保证内；不在最终响应之后预读合法加密数据。
    async fn reject_observable_trailing_input(&mut self) -> Result<(), HandshakeError> {
        if !self.buf.is_empty() {
            return Err(HandshakeError::Protocol(ProtocolViolation::Framing(
                "unexpected trailing input before approval",
            )));
        }
        std::future::poll_fn(|cx| {
            let mut byte = [0u8; 1];
            let mut buf = tokio::io::ReadBuf::new(&mut byte);
            match Pin::new(&mut self.io).poll_read(cx, &mut buf) {
                std::task::Poll::Pending => std::task::Poll::Ready(Ok(())),
                std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Err(error.into())),
                std::task::Poll::Ready(Ok(())) if buf.filled().is_empty() => {
                    std::task::Poll::Ready(Err(HandshakeError::Io(ErrorKind::UnexpectedEof)))
                }
                std::task::Poll::Ready(Ok(())) => {
                    std::task::Poll::Ready(Err(HandshakeError::Protocol(
                        ProtocolViolation::Framing("unexpected trailing input before approval"),
                    )))
                }
            }
        })
        .await
    }

    /// 取下一帧；每次 `Ok` 恰好消耗一帧，`Err` 即连接应关闭。
    async fn next_frame(&mut self) -> Result<(FrameType, HandshakeMessage), HandshakeError> {
        loop {
            if let Some(frame) = self.take_frame()? {
                return Ok(frame);
            }
            let read = self.io.read_buf(&mut self.buf).await?;
            if read == 0 {
                return Err(HandshakeError::Io(ErrorKind::UnexpectedEof));
            }
        }
    }

    /// `next_frame` 的装箱版本，供 `select!` 分支使用。
    async fn next_frame_boxed(
        &mut self,
    ) -> Result<(FrameType, Box<HandshakeMessage>), HandshakeError> {
        let (frame_type, message) = self.next_frame().await?;
        Ok((frame_type, Box::new(message)))
    }

    /// 非阻塞地从未消费字节里切出一帧；不足则 `Ok(None)`。
    fn take_frame(&mut self) -> Result<Option<(FrameType, HandshakeMessage)>, HandshakeError> {
        if self.buf.len() < FRAME_HEADER_LEN {
            return Ok(None);
        }
        let header = frame::parse_frame_header(&self.buf[..FRAME_HEADER_LEN])?;
        let total = FRAME_HEADER_LEN
            + usize::try_from(header.payload_len).map_err(|_| {
                HandshakeError::Protocol(ProtocolViolation::Frame(
                    frame::FrameError::PayloadTooLarge {
                        len: u32::MAX,
                        max: frame::MAX_PAYLOAD_LEN as u32,
                    },
                ))
            })?;
        if self.buf.len() < total {
            return Ok(None);
        }
        let bytes = self.buf.split_to(total).freeze();
        let (frame_type, message) = decode_handshake_frame(&bytes)?;
        if frame_type != header.frame_type {
            return Err(HandshakeError::Protocol(ProtocolViolation::Framing(
                "frame type does not match header",
            )));
        }
        Ok(Some((frame_type, message)))
    }
}

async fn read_frame<S>(io: &mut S) -> Result<(FrameType, HandshakeMessage), HandshakeError>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0u8; FRAME_HEADER_LEN];
    io.read_exact(&mut header).await?;
    // 先解析头部：payload 上限 16 KiB 在此校验，绝不按对端声明无界分配。
    let payload_len =
        usize::try_from(frame::parse_frame_header(&header)?.payload_len).map_err(|_| {
            ProtocolViolation::Frame(frame::FrameError::PayloadTooLarge {
                len: u32::MAX,
                max: frame::MAX_PAYLOAD_LEN as u32,
            })
        })?;

    let mut frame_bytes = vec![0u8; FRAME_HEADER_LEN + payload_len];
    frame_bytes[..FRAME_HEADER_LEN].copy_from_slice(&header);
    io.read_exact(&mut frame_bytes[FRAME_HEADER_LEN..]).await?;
    Ok(decode_handshake_frame(&frame_bytes)?)
}

async fn write_frame<S>(
    io: &mut S,
    frame_type: FrameType,
    message: &HandshakeMessage,
) -> Result<(), HandshakeError>
where
    S: AsyncWrite + Unpin,
{
    let bytes = encode_handshake_frame(frame_type, message)?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}

fn expect_frame(
    expected: FrameType,
    frame_type: FrameType,
    message: HandshakeMessage,
) -> Result<HandshakeMessage, HandshakeError> {
    if frame_type == expected {
        Ok(message)
    } else {
        Err(HandshakeError::Protocol(
            ProtocolViolation::UnexpectedFrame {
                expected: frame_name(expected),
                got: frame_name(frame_type),
            },
        ))
    }
}

const fn frame_name(frame_type: FrameType) -> &'static str {
    match frame_type {
        FrameType::ClientHello => "client-hello",
        FrameType::ServerChallenge => "server-challenge",
        FrameType::ClientAuthRequest => "client-auth-request",
        FrameType::ServerAuthResponse => "server-auth-response",
        FrameType::ServerPending => "server-pending",
        FrameType::PendingPong => "pending-pong",
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N], HandshakeError> {
    let rng = SystemRandom::new();
    let mut out = [0u8; N];
    rng.fill(&mut out)
        .map_err(|_| HandshakeError::KeyAgreement)?;
    Ok(out)
}

// ================================================================ 板端

/// 板端（TCP 客户端）握手。
pub async fn client_handshake<S>(
    mut io: S,
    config: &ClientHandshakeConfig,
) -> Result<HandshakeResult<S>, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 无 TOFU：没有可信中控公钥就在发出任何字节之前失败。
    if config.trusted_servers.is_empty() {
        return Err(HandshakeError::NoTrustedServers);
    }

    let client_random = random_bytes::<32>()?;
    with_timeout(
        HandshakeStage::ClientHello,
        INITIAL_STEP_TIMEOUT,
        write_frame(
            &mut io,
            FrameType::ClientHello,
            &HandshakeMessage::ClientHello(ClientHello::new(client_random)),
        ),
    )
    .await??;

    let server_random = {
        let message = with_timeout(
            HandshakeStage::ServerChallenge,
            INITIAL_STEP_TIMEOUT,
            read_frame(&mut io),
        )
        .await?;
        let (frame_type, message) = message?;
        match expect_frame(FrameType::ServerChallenge, frame_type, message)? {
            HandshakeMessage::ServerChallenge(challenge) => challenge.server_random,
            _ => return Err(unreachable_frame(FrameType::ServerChallenge)),
        }
    };

    // 每连接全新的 X25519 临时密钥对。
    let ephemeral = EphemeralKeyPair::generate()?;
    let client_public = config.identity.public_key();
    let client_ephemeral = ephemeral.public_key();
    // 客户端只编码一次并保留：签名与传输使用同一串原字节。
    let descriptor_raw = config.descriptor.encode();
    let signature = sign_client_auth_request(
        config.identity.as_ref(),
        &client_random,
        &server_random,
        &client_public,
        &client_ephemeral,
        &descriptor_raw,
    )?;
    let request = ClientAuthRequest::new(
        client_public,
        client_ephemeral,
        config.descriptor.clone(),
        signature,
    )?;
    if request.descriptor_raw() != descriptor_raw.as_slice() {
        return Err(HandshakeError::Protocol(ProtocolViolation::Internal(
            "descriptor raw bytes changed between signing and encoding",
        )));
    }
    with_timeout(
        HandshakeStage::ClientAuthRequest,
        INITIAL_STEP_TIMEOUT,
        write_frame(
            &mut io,
            FrameType::ClientAuthRequest,
            &HandshakeMessage::ClientAuthRequest(request),
        ),
    )
    .await??;

    // 非 PENDING 的初始等待 10s；进入 PENDING 后改为 15s 接收静默期限。
    // 静默期限是**绝对期限**：锚在最后一条合法 SERVER_PENDING 的到达时刻，
    // 回 pong（含写出被背压挡住的耗时）都在期限内进行，因此发送侧不能刷新它，
    // 只有新的合法 SERVER_PENDING 才重新锚定（§4.1）。
    let mut status_nonce: Option<[u8; 16]> = None;
    let mut last_probe_seq = 0u64;
    let mut silence_deadline: Option<Instant> = None;
    loop {
        let message = match silence_deadline {
            Some(deadline) => {
                with_deadline(HandshakeStage::ClientSilence, deadline, read_frame(&mut io))
                    .await??
                    .1
            }
            None => {
                with_timeout(
                    HandshakeStage::ServerAuthResponse,
                    INITIAL_STEP_TIMEOUT,
                    read_frame(&mut io),
                )
                .await??
                .1
            }
        };

        match message {
            HandshakeMessage::ServerPending(pending) => {
                let nonce = pending.status_nonce;
                match status_nonce {
                    None => {
                        if pending.probe_seq != 1 {
                            return Err(pending_violation("first probe_seq must be 1"));
                        }
                    }
                    Some(previous) => {
                        if previous != nonce {
                            return Err(pending_violation("status_nonce changed during pending"));
                        }
                        if pending.probe_seq <= last_probe_seq {
                            return Err(pending_violation("probe_seq must strictly increase"));
                        }
                    }
                }
                status_nonce = Some(nonce);
                last_probe_seq = pending.probe_seq;

                // 合法 SERVER_PENDING 到达：静默期限从这一刻重新锚定。
                let received_at = Instant::now();
                let silence = received_at + CLIENT_SILENCE_TIMEOUT;
                silence_deadline = Some(silence);

                // 立即逐字回显三元组；写出期限同样落在这 15s 之内。
                let pong = PendingPong::new(pending.pending_token, nonce, pending.probe_seq)?;
                let pong_deadline = (received_at + PROBE_PONG_TIMEOUT).min(silence);
                if let Err(err) = send_frame(
                    HandshakeStage::PendingPong,
                    pong_deadline,
                    &mut io,
                    FrameType::PendingPong,
                    &HandshakeMessage::PendingPong(pong),
                )
                .await
                {
                    // 静默期限先到就是失联，不是「pong 还没写完」：写出被挡住
                    // 永远不能把 15s 静默推后。
                    if Instant::now() >= silence {
                        return Err(HandshakeError::Timeout(HandshakeStage::ClientSilence));
                    }
                    return Err(err);
                }
            }
            HandshakeMessage::ServerAuthResponse(response) => {
                let peer_public_key = verify_server_response(
                    config,
                    &client_random,
                    &server_random,
                    &client_public,
                    &client_ephemeral,
                    &descriptor_raw,
                    &response,
                )?;
                if response.status != 0 {
                    // 已验签，才让 reason 影响调用方。
                    let reason = AuthenticatedRejection::from_reason(response.reason_code)
                        .ok_or(HandshakeError::UntrustedServer)?;
                    return Err(HandshakeError::SignedRejection { reason });
                }
                // 先协商再交付：协商失败绝不交付半截会话。
                let shared = ephemeral.agree(&response.server_x25519_eph_pub)?;
                let session_keys = SessionKeys::derive(&shared, &client_random, &server_random)?;
                return Ok(HandshakeResult {
                    io,
                    session_keys,
                    peer_public_key,
                    descriptor: None,
                });
            }
            other => {
                let got = message_frame_type(&other);
                return Err(HandshakeError::Protocol(
                    ProtocolViolation::UnexpectedFrame {
                        expected: "server-auth-response|server-pending",
                        got: frame_name(got),
                    },
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn sign_client_auth_request(
    identity: &IdentityKey,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_public: &[u8; 32],
    client_ephemeral: &[u8; 32],
    descriptor_raw: &[u8],
) -> Result<[u8; 64], HandshakeError> {
    let input = rsetup_protocol::sig::client_signature_input(
        client_random,
        server_random,
        client_public,
        client_ephemeral,
        descriptor_raw,
    )
    .map_err(|_| {
        HandshakeError::Protocol(ProtocolViolation::Internal(
            "client signature input rejected",
        ))
    })?;
    Ok(identity.sign(&input))
}

/// 用可信列表逐个验签；返回**被匹配到的**可信公钥。
#[allow(clippy::too_many_arguments)]
fn verify_server_response(
    config: &ClientHandshakeConfig,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    client_public: &[u8; 32],
    client_ephemeral: &[u8; 32],
    descriptor_raw: &[u8],
    response: &ServerAuthResponse,
) -> Result<[u8; 32], HandshakeError> {
    // v3 绝不发出 SIGNATURE_INVALID；出现即视为伪造/篡改。
    if response.status != u32::from(rsetup_protocol::sig::STATUS_APPROVED)
        && response.status != u32::from(STATUS_REJECTED)
    {
        return Err(HandshakeError::UntrustedServer);
    }
    if response.reason_code == REASON_SIGNATURE_INVALID {
        return Err(HandshakeError::UntrustedServer);
    }
    let status = response.status as u8;
    let input = server_signature_input(
        server_random,
        client_random,
        client_public,
        client_ephemeral,
        descriptor_raw,
        &response.server_x25519_eph_pub,
        status,
        response.reason_code,
    )
    .map_err(|_| HandshakeError::UntrustedServer)?;

    for trusted in &config.trusted_servers {
        if verify_ed25519(&input, &response.signature, trusted).is_ok() {
            return Ok(*trusted);
        }
    }
    Err(HandshakeError::UntrustedServer)
}

// ================================================================ 中控端

/// 中控端（TCP 服务端）握手。`policy` 负责准入；本模块只负责协议与密码学。
pub async fn server_handshake<S>(
    io: S,
    config: &ServerHandshakeConfig,
    policy: &dyn AdmissionPolicy,
) -> Result<HandshakeResult<S>, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // 读侧统一走可安全取消的 FrameReader：PENDING 循环里 `select!` 会因为
    // 「准入决定先到」而丢弃尚未完成的读帧 future，缓冲区必须随循环存活才不丢字节。
    let mut frames = FrameReader::new(io);
    // 初始步骤（含验签与准入工作）共享一个 10s 期限。
    let step_deadline = Instant::now() + INITIAL_STEP_TIMEOUT;

    let client_random = {
        let (frame_type, message) = with_deadline(
            HandshakeStage::ClientHello,
            step_deadline,
            frames.next_frame(),
        )
        .await??;
        match expect_frame(FrameType::ClientHello, frame_type, message)? {
            HandshakeMessage::ClientHello(hello) => hello.client_random,
            _ => return Err(unreachable_frame(FrameType::ClientHello)),
        }
    };

    let server_random = random_bytes::<32>()?;
    send_frame(
        HandshakeStage::ServerChallenge,
        step_deadline,
        frames.io_mut(),
        FrameType::ServerChallenge,
        &HandshakeMessage::ServerChallenge(ServerChallenge::new(server_random)),
    )
    .await?;

    let request = {
        let (frame_type, message) = with_deadline(
            HandshakeStage::ClientAuthRequest,
            step_deadline,
            frames.next_frame(),
        )
        .await??;
        match expect_frame(FrameType::ClientAuthRequest, frame_type, message)? {
            HandshakeMessage::ClientAuthRequest(request) => request,
            _ => return Err(unreachable_frame(FrameType::ClientAuthRequest)),
        }
    };

    // 严格审计（上面已完成）之后、任何准入副作用之前，先验板端签名。
    let client_public = *request.client_ed25519_pub();
    let client_ephemeral = *request.client_x25519_eph_pub();
    let descriptor_raw = request.descriptor_raw().to_vec();
    let input = rsetup_protocol::sig::client_signature_input(
        &client_random,
        &server_random,
        &client_public,
        &client_ephemeral,
        &descriptor_raw,
    )
    .map_err(|_| HandshakeError::ClientSignature)?;
    if verify_ed25519(&input, request.signature(), &client_public).is_err() {
        // §4.5：直接关闭 TCP，不回 REJECTED(SIGNATURE_INVALID)，不写永久决定。
        return Err(HandshakeError::ClientSignature);
    }

    let peer = VerifiedPeer {
        public_key: client_public,
        descriptor: request.descriptor().clone(),
    };

    // 初判必须落在 10s 初始步骤期限内（§4.1：验签与准入检查要在期限内给出直接响应
    // 或首个 SERVER_PENDING）；进入挂起后不再有这一步骤期限。
    let mut decision = evaluate(policy, &peer, Some(step_deadline)).await?;
    if matches!(decision, AdmissionDecision::Pending) {
        return drive_pending(
            frames,
            config,
            policy,
            &peer,
            &request,
            &client_random,
            &server_random,
            step_deadline,
        )
        .await;
    }

    loop {
        match decision {
            AdmissionDecision::Approved => {
                frames.reject_observable_trailing_input().await?;
                // 最终接纳前在短临界区重新核验（撤销优先）。
                let recheck = evaluate(policy, &peer, Some(step_deadline)).await?;
                if matches!(recheck, AdmissionDecision::Pending) {
                    // 状态回退到未决定：不接纳，转入挂起探测。
                    return drive_pending(
                        frames,
                        config,
                        policy,
                        &peer,
                        &request,
                        &client_random,
                        &server_random,
                        step_deadline,
                    )
                    .await;
                }
                if recheck != AdmissionDecision::Approved {
                    decision = recheck;
                    continue;
                }
                frames.reject_observable_trailing_input().await?;
                return finalize_approved(
                    frames.into_inner_checked()?,
                    config,
                    &request,
                    &peer,
                    &client_random,
                    &server_random,
                )
                .await;
            }
            AdmissionDecision::Rejected(code) => {
                let reason = rejection_reason(code)?;
                send_signed_rejection(
                    frames.io_mut(),
                    config,
                    &request,
                    &peer,
                    &client_random,
                    &server_random,
                    reason,
                )
                .await?;
                return Err(HandshakeError::SignedRejection { reason });
            }
            AdmissionDecision::Pending => unreachable!("进入循环前已分流 Pending"),
        }
    }
}

/// 一轮在途 probe 的全部状态（`Copy`，便于在 `select!` 前后复制）。
#[derive(Debug, Clone, Copy)]
struct InflightProbe {
    token: [u8; 16],
    nonce: [u8; 16],
    seq: u64,
    /// probe 提交时刻（写出**之前**取样）。
    sent_at: Instant,
    /// pong 期限：`sent_at + PROBE_PONG_TIMEOUT`。
    deadline: Instant,
}

/// `select!` 的一次唤醒事件；分支体只做调度，不碰 `io`（借用冲突）。
enum PendingEvent {
    /// 读到一帧（`Err` 已是最终错误原因）。消息体装箱，避免整个枚举按最大 variant 计。
    Frame(Result<(FrameType, Box<HandshakeMessage>), HandshakeError>),
    /// 在途 probe 的原 pong 期限已到。
    PongExpired,
    /// 被 pin 的准入 future 给出结果。
    Admission(Result<AdmissionDecision, HandshakeError>),
    /// 下一轮 probe 的最早发送时刻已到。
    ProbeDue,
    /// 有界复查时刻已到。
    RecheckDue,
}

/// §4.1 PENDING 挂起态调度中心：把「等准入决定」和「维持 probe 节奏」真正并发起来。
///
/// 必须同时成立的规则（`tests/handshake.rs` 逐条实测）：
///
/// * **准入 future 被 pin 且并发**：至多一个在途 `evaluate` future，存在局部变量里
///   跨 `select!` 轮次存活；它**没有决定期间，5s 名义 probe 必须照常发出**，
///   否则板端会在 15s 静默上误断线（F1）。
/// * **单在途**：上一轮未按 `sent_at + 10s` 结清就不再发新 probe。
/// * **pong 期限含写出耗时**：`sent_at` 在写出**之前**取样，pong 期限用
///   `sent_at + 10s` 的绝对期限；写出被背压挡住的时间算在这 10s 内，
///   不能写完再新给 10s（F2）。
/// * **Pending 不忙等（统一退避）**：**所有**回到 Pending 的状态转换——无论是
///   `evaluate` 返回 Pending，还是最终临界区复查回退 Pending——都安排
///   `ADMISSION_RECHECK_DELAY` 的有界复查并恢复 probe 调度；只清决定不设退避会在
///   「Pending→Approved→(复查)Pending→…」交替下于同一 0ns 瞬时无限连轴转。
///   不靠限制循环次数掩盖计时，也不借 `AdmissionPolicy` 强制策略立即返回。
/// * **挂起期全程读帧（R1）**：读分支不随「无在途 probe」关闭；已结清后到达的
///   任何帧（旧 pong 等错 phase 帧）当场 `UnexpectedFrame` 违例，绝不留给随后
///   交付给加密层的 io。泛型 socket 无法证明「将来没有字节」，本保证只覆盖
///   交付前已可观察的帧；批准点一到即停止读，不预读最终响应之后的记录层数据，
///   也不做无界 drain 或静默等待窗口。
/// * **pong 记账前复核原绝对期限（R2）**：帧通过三元组核验后、结清之前，必须用
///   当前 `Instant` 与 `sent_at + 10s` 的**原**期限核对（`now >= deadline` 即超时）；
///   `select!` 分支的就绪顺序不是时间证明。
/// * **决定就绪不再发新 probe**：只等在途 probe 按原期限结清再发最终响应；
///   吊销/拒绝可立即终止，不占用剩余 probe 期限。
/// * **无审批总期限**：挂起多久都不因等待时长拒绝；最终接纳前再核验一次，fail closed。
// 挂起态需要的全部上下文：连接、配置、策略、已验证对端、握手三要素与初始步骤期限。
#[allow(clippy::too_many_arguments)]
async fn drive_pending<S>(
    mut frames: FrameReader<S>,
    config: &ServerHandshakeConfig,
    policy: &dyn AdmissionPolicy,
    peer: &VerifiedPeer,
    request: &ClientAuthRequest,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    step_deadline: Instant,
) -> Result<HandshakeResult<S>, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    // status_nonce 挂起期内不变；probe_seq 从 1 严格递增。
    let mut status_nonce = [0u8; 16];
    let mut probe_seq: u64 = 0u64;

    // §4.1：首个 SERVER_PENDING 立即发送，兼作进入 PENDING 的通知与第一轮 ping。
    // 它必须**先于任何准入结果**送达，否则决定来得太快会把挂起通知整个吞掉。
    let first = send_probe(
        frames.io_mut(),
        &mut status_nonce,
        &mut probe_seq,
        step_deadline,
    )
    .await?;
    let mut next_probe_at = Some(first.sent_at + PROBE_PERIOD);
    let mut inflight = Some(first);
    // 至多一个被 pin 的准入 future。
    let mut admission: Option<AdmissionFuture<'_>> = None;
    // 下一次允许发起 evaluate 的最早时刻（Pending 结果的有界延迟复查）。
    let mut recheck_at: Option<Instant> = None;
    // 已就绪的最终决定；就绪后停止发新 probe。
    let mut final_decision: Option<AdmissionDecision> = None;

    loop {
        // (1) 决定已就绪：绝不再发新 probe。批准要等原期限结清；拒绝立即终止。
        if let Some(decision) = final_decision {
            match decision {
                AdmissionDecision::Rejected(code) => {
                    let reason = rejection_reason(code)?;
                    send_signed_rejection(
                        frames.io_mut(),
                        config,
                        request,
                        peer,
                        client_random,
                        server_random,
                        reason,
                    )
                    .await?;
                    return Err(HandshakeError::SignedRejection { reason });
                }
                AdmissionDecision::Pending => {
                    // 回到 Pending 的**统一**处置：清掉已消费的决定，安排有界复查，
                    // probe 调度随之恢复（ProbeDue 分支只在无最终决定时开）。
                    // 不这样统一退避的话，「Approved → 最终复查 Pending → 立即重查
                    // → Approved → …」会在同一个 0ns 瞬时无限连轴转。
                    final_decision = None;
                    recheck_at.get_or_insert(Instant::now() + ADMISSION_RECHECK_DELAY);
                    continue;
                }
                AdmissionDecision::Approved => {
                    if inflight.is_none() {
                        // 交付边界（R1）：缓冲区里仍坐着整帧说明对端在挂起期乱序
                        // 多发，当场违例，绝不随批准带进加密通道。
                        if let Some((frame_type, _)) = frames.take_frame()? {
                            return Err(unexpected_after_settlement(frame_type));
                        }
                        frames.reject_observable_trailing_input().await?;
                        // 真正接纳前在短临界区复查（撤销优先）；这一跳是状态读取，
                        // 不是人工审批，因此允许直接 await 并 fail closed。
                        let recheck = evaluate(policy, peer, None).await?;
                        if recheck != AdmissionDecision::Approved {
                            if matches!(recheck, AdmissionDecision::Pending) {
                                // 回退到未决定：与 Admission 分支的 Pending 走同一条
                                // 有界复查路径，不得立刻重新 evaluate。
                                recheck_at = Some(Instant::now() + ADMISSION_RECHECK_DELAY);
                            }
                            final_decision = Some(recheck);
                            continue;
                        }
                        // final SQL复查期间也可能到达额外帧，签发响应前再取一次
                        // 不等待的观察点；不把用户缓冲为空当作socket空。
                        frames.reject_observable_trailing_input().await?;
                        return finalize_approved(
                            frames.into_inner_checked()?,
                            config,
                            request,
                            peer,
                            client_random,
                            server_random,
                        )
                        .await;
                    }
                }
            }
        }

        // (2) 需要最新决定、当前没有在途准入 future、且已过有界复查时刻 → 起一个。
        if admission.is_none()
            && final_decision.is_none()
            && recheck_at.is_none_or(|at| Instant::now() >= at)
        {
            admission = Some(policy.evaluate(peer));
            recheck_at = None;
        }

        // (3) 并发等待：pong 读取、pong 原期限、准入决定、probe 调度、复查调度。
        let event = {
            let pong_deadline = inflight.map(|probe| probe.deadline);
            tokio::select! {
                biased;
                // R1：挂起期**全程**读帧——「无在途 probe」不是关读的理由。关读会把
                // 已到达的错 phase 帧（如旧 pong）原封留给随后交付的加密/h2 通道；
                // 全程读只消费本 phase 合法的入站帧，不预读最终响应之后的字节
                // （批准交付点一到就不再读，见 (1) Approved 分支与 into_inner_checked）。
                frame = frames.next_frame_boxed() => PendingEvent::Frame(frame),
                _ = tokio::time::sleep_until(pong_deadline.unwrap_or_else(Instant::now)),
                    if inflight.is_some() => PendingEvent::PongExpired,
                outcome = wait_admission(&mut admission), if admission.is_some() =>
                    PendingEvent::Admission(outcome),
                _ = tokio::time::sleep_until(next_probe_at.unwrap_or_else(Instant::now)),
                    if inflight.is_none() && final_decision.is_none() => PendingEvent::ProbeDue,
                _ = tokio::time::sleep_until(recheck_at.unwrap_or_else(Instant::now)),
                    if recheck_at.is_some() && admission.is_none() && final_decision.is_none() =>
                        PendingEvent::RecheckDue,
            }
        };

        match event {
            PendingEvent::Frame(result) => {
                let (frame_type, message) = result?;
                let message = *message;
                // 无在途 probe 时到达的任何入站帧都是协议错误：当场违例，既不
                // panic（不假设这里必然有在途），也不吞进缓冲区留给交付时刻。
                let Some(probe) = inflight else {
                    return Err(unexpected_after_settlement(frame_type));
                };
                let pong = match expect_frame(FrameType::PendingPong, frame_type, message)? {
                    HandshakeMessage::PendingPong(pong) => pong,
                    _ => return Err(unreachable_frame(FrameType::PendingPong)),
                };
                // 延迟/错代际 pong 一律视为协议违例并关闭，绝不覆盖最新决定。
                if pong.pending_token != probe.token {
                    return Err(pending_violation("pending_token mismatch"));
                }
                if pong.status_nonce != probe.nonce {
                    return Err(pending_violation("status_nonce mismatch"));
                }
                if pong.probe_seq != probe.seq {
                    return Err(pending_violation("probe_seq mismatch"));
                }
                // R2：帧已通过全部字段核验，但 select 分支「就绪顺序」不是时间证明：
                // biased 下读分支会与已到期的原期限同刻抢跑。结清（信用）之前必须
                // 用当前 Instant 复核**原绝对**期限；超时就是超时，边界取
                // `now >= deadline`（与 timeout_at 语义一致）。
                if Instant::now() >= probe.deadline {
                    return Err(HandshakeError::Timeout(HandshakeStage::PendingPong));
                }
                inflight = None;
                // 结清在途 probe 后立刻允许取最新决定（批准不必等下一个 5s 周期）。
                recheck_at = Some(Instant::now());
            }
            PendingEvent::PongExpired => {
                // 含「写出被挡住后剩余窗口耗尽」：期限锚在提交时刻，不重开。
                return Err(HandshakeError::Timeout(HandshakeStage::PendingPong));
            }
            PendingEvent::Admission(outcome) => {
                admission = None;
                match outcome? {
                    AdmissionDecision::Pending => {
                        // Pending 立即返回也不能连轴转：下一次复查推后有界延迟。
                        recheck_at = Some(Instant::now() + ADMISSION_RECHECK_DELAY);
                    }
                    other => {
                        // Approved/Rejected 已持久化：停止发新 probe（拒绝在下一轮立即终止）。
                        final_decision = Some(other);
                    }
                }
            }
            PendingEvent::ProbeDue => {
                let probe = send_probe(
                    frames.io_mut(),
                    &mut status_nonce,
                    &mut probe_seq,
                    step_deadline,
                )
                .await?;
                // 名义 5s 周期锚在提交时刻：写出被挡住的时间也算在周期里。
                next_probe_at = Some(probe.sent_at + PROBE_PERIOD);
                inflight = Some(probe);
            }
            PendingEvent::RecheckDue => {
                // 回到 (2) 起一个新的 evaluate；本身不改变任何决定。
            }
        }
    }
}

/// 提交一轮 probe：`pending_token` 每轮重新随机，`status_nonce` 首轮生成后不变，
/// `probe_seq` 严格递增。
///
/// F2 的口径在这里：`sent_at` 在**调用写出之前**取样，pong 期限与「下一轮不早于
/// 5s 后」都锚在这个取样点上，因此写出被背压挡住多久都算在这 10s 之内。
async fn send_probe<S>(
    io: &mut S,
    status_nonce: &mut [u8; 16],
    probe_seq: &mut u64,
    step_deadline: Instant,
) -> Result<InflightProbe, HandshakeError>
where
    S: AsyncWrite + Unpin,
{
    *probe_seq =
        probe_seq
            .checked_add(1)
            .ok_or(HandshakeError::Protocol(ProtocolViolation::Pending(
                "probe_seq exhausted",
            )))?;
    if *probe_seq == 1 {
        *status_nonce = random_bytes::<16>()?;
    }
    let pending = ServerPending::new(random_bytes::<16>()?, *status_nonce, *probe_seq)?;
    let sent_at = Instant::now();
    let pong_deadline = sent_at + PROBE_PONG_TIMEOUT;
    let (write_deadline, write_stage) = if *probe_seq == 1 {
        // 首轮 SERVER_PENDING 仍受初始 10s 步骤期限约束（§4.1 验签+准入）。
        (
            step_deadline.min(pong_deadline),
            HandshakeStage::AdmissionDecision,
        )
    } else {
        (pong_deadline, HandshakeStage::PendingPong)
    };
    send_frame(
        write_stage,
        write_deadline,
        io,
        FrameType::ServerPending,
        &HandshakeMessage::ServerPending(pending),
    )
    .await?;
    Ok(InflightProbe {
        token: pending.pending_token,
        nonce: *status_nonce,
        seq: *probe_seq,
        sent_at,
        deadline: pong_deadline,
    })
}

/// 轮询被 pin 的准入 future；没有在途 future 时永远 Pending（分支由 `if` 关闭）。
async fn wait_admission(
    slot: &mut Option<AdmissionFuture<'_>>,
) -> Result<AdmissionDecision, HandshakeError> {
    match slot.as_mut() {
        Some(future) => future.as_mut().await,
        None => std::future::pending().await,
    }
}

/// 把准入决定的 `reason_code` 映射成可送达的签名拒绝原因。
/// 1（SIGNATURE_INVALID）与未知枚举绝不发出，直接 fail closed。
fn rejection_reason(code: u32) -> Result<AuthenticatedRejection, HandshakeError> {
    AuthenticatedRejection::from_reason(code)
        .filter(|_| code != REASON_SIGNATURE_INVALID)
        .ok_or(HandshakeError::Policy)
}

async fn evaluate(
    policy: &dyn AdmissionPolicy,
    peer: &VerifiedPeer,
    budget: Option<Instant>,
) -> Result<AdmissionDecision, HandshakeError> {
    let future = policy.evaluate(peer);
    match budget {
        Some(deadline) => timeout_at(deadline, future)
            .await
            .map_err(|_| HandshakeError::Timeout(HandshakeStage::AdmissionDecision))?,
        None => future.await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_approved<S>(
    mut io: S,
    config: &ServerHandshakeConfig,
    request: &ClientAuthRequest,
    peer: &VerifiedPeer,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
) -> Result<HandshakeResult<S>, HandshakeError>
where
    S: AsyncWrite + Unpin,
{
    // 每连接全新的服务端临时密钥。
    let ephemeral = EphemeralKeyPair::generate()?;
    let server_ephemeral = ephemeral.public_key();
    let signature = server_signature(
        config,
        server_random,
        client_random,
        peer,
        request,
        &server_ephemeral,
        0,
        REASON_UNSPECIFIED,
    )?;
    let response = ServerAuthResponse::approved(server_ephemeral, signature)?;

    // 先协商（全零共享密钥等违例必须在发出批准之前失败）。
    let shared = ephemeral.agree(request.client_x25519_eph_pub())?;
    let session_keys = SessionKeys::derive(&shared, client_random, server_random)?;

    write_frame(
        &mut io,
        FrameType::ServerAuthResponse,
        &HandshakeMessage::ServerAuthResponse(response),
    )
    .await?;

    Ok(HandshakeResult {
        io,
        session_keys,
        peer_public_key: peer.public_key,
        descriptor: Some(peer.descriptor.clone()),
    })
}

#[allow(clippy::too_many_arguments)]
async fn send_signed_rejection<S>(
    io: &mut S,
    config: &ServerHandshakeConfig,
    request: &ClientAuthRequest,
    peer: &VerifiedPeer,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    reason: AuthenticatedRejection,
) -> Result<(), HandshakeError>
where
    S: AsyncWrite + Unpin,
{
    // REJECTED 不交付会话密钥，但签名仍覆盖转录与 reason_code；临时公钥位取全零占位。
    let signature = server_signature(
        config,
        server_random,
        client_random,
        peer,
        request,
        &[0u8; 32],
        u32::from(STATUS_REJECTED),
        reason.reason_code(),
    )?;
    let response = ServerAuthResponse::rejected(reason.reason_code(), signature)?;
    write_frame(
        io,
        FrameType::ServerAuthResponse,
        &HandshakeMessage::ServerAuthResponse(response),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
fn server_signature(
    config: &ServerHandshakeConfig,
    server_random: &[u8; 32],
    client_random: &[u8; 32],
    peer: &VerifiedPeer,
    request: &ClientAuthRequest,
    server_ephemeral: &[u8; 32],
    status: u32,
    reason_code: u32,
) -> Result<[u8; 64], HandshakeError> {
    let input = server_signature_input(
        server_random,
        client_random,
        &peer.public_key,
        request.client_x25519_eph_pub(),
        request.descriptor_raw(),
        server_ephemeral,
        status as u8,
        reason_code,
    )
    .map_err(|_| HandshakeError::Policy)?;
    Ok(config.identity.sign(&input))
}

fn pending_violation(reason: &'static str) -> HandshakeError {
    HandshakeError::Protocol(ProtocolViolation::Pending(reason))
}

/// 挂起期内所有在途 probe 都已结清：此后再来的任何帧都不属于可等待的 phase，
/// 当场按 `UnexpectedFrame` 违例关闭，绝不缓冲后随批准交付（R1）。
fn unexpected_after_settlement(frame_type: FrameType) -> HandshakeError {
    HandshakeError::Protocol(ProtocolViolation::UnexpectedFrame {
        expected: "no-frame (all probes settled)",
        got: frame_name(frame_type),
    })
}

fn unreachable_frame(expected: FrameType) -> HandshakeError {
    HandshakeError::Protocol(ProtocolViolation::UnexpectedFrame {
        expected: frame_name(expected),
        got: "mismatched-typed-message",
    })
}

fn message_frame_type(message: &HandshakeMessage) -> FrameType {
    match message {
        HandshakeMessage::ClientHello(_) => FrameType::ClientHello,
        HandshakeMessage::ServerChallenge(_) => FrameType::ServerChallenge,
        HandshakeMessage::ClientAuthRequest(_) => FrameType::ClientAuthRequest,
        HandshakeMessage::ServerPending(_) => FrameType::ServerPending,
        HandshakeMessage::PendingPong(_) => FrameType::PendingPong,
        HandshakeMessage::ServerAuthResponse(_) => FrameType::ServerAuthResponse,
    }
}

async fn with_timeout<T, F>(
    stage: HandshakeStage,
    limit: Duration,
    future: F,
) -> Result<T, HandshakeError>
where
    F: Future<Output = T>,
{
    timeout(limit, future)
        .await
        .map_err(|_| HandshakeError::Timeout(stage))
}

async fn with_deadline<T, F>(
    stage: HandshakeStage,
    deadline: Instant,
    future: F,
) -> Result<T, HandshakeError>
where
    F: Future<Output = T>,
{
    timeout_at(deadline, future)
        .await
        .map_err(|_| HandshakeError::Timeout(stage))
}
