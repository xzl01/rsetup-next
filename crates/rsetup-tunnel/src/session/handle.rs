//! 会话句柄、状态、handler 契约与 owner 内部消息类型。
//!
//! 句柄是 `Clone + Send + Sync` 的瘦外壳：协议状态只由一个 owner 任务持有。句柄
//! 对 owner 只有三条有界通道：
//! * **业务**（含 Event 与 RPC 入队）：容量小、满则 `Busy`，不重试；
//! * **System**：独立有界通道，不能被业务队列拥堵挡在 owner 之前；
//! * **关停**：`try_send` 幂等命令，句柄侧绝不等待网络。
use super::pending::next_trace_id;
use super::probe::ProbeRequest;
use super::types::{Reply, RequestError, StreamEpoch};
use crate::envelope::{PacketType, StreamKind, TunnelPacket};
use crate::session::streams::{StreamError, StreamReceiver, StreamSender};
use bytes::Bytes;
use http::Request;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// 会话对外可见的状态机；`Closed` 为终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 尚未满足「本端两条流各自首探测成功」。
    Connecting,
    /// 本端 Control 与 Data 都已收到本端首 probe 的有效 pong（§6.6 ready 判据）。
    Ready,
    /// 已发出或观察到 GOAWAY：停止新业务与新探测，只维持已接纳的操作。
    Draining,
    Closed,
}

/// 会话结束原因。固定枚举，不含对端可控文本或 payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCloseReason {
    /// 本端 `graceful_shutdown`，排空期限走完后正常结束。
    Local,
    /// 观察到对端 GOAWAY 后排空结束。
    PeerGoaway,
    /// 收到 `tunnel.kick`。
    Kicked,
    /// 收到 `tunnel.revoke`。
    Revoked,
    /// 本端 `abort`，或最后一个句柄被 drop。
    Abort,
    /// 连接级故障：h2 连接结束、加密记录层致命、连接级 PING 判死。
    Transport,
    /// 单流 3 次重开仍未达首次健康，升级为连接级重建。
    StreamFlapping,
    /// 协议违规（例如 System 出现在数据流、未知 System action）。
    Protocol,
}

/// owner 任务退出结果：原因 + 本端计数 + 资源释放确认，无对端 payload 文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionOutcome {
    pub reason: SessionCloseReason,
    /// 本端收到的 `tunnel.ping` 请求数（Control + Data 合计）。
    pub probes_served: u32,
    /// 本端发出的探测窗口中未获有效 pong 的次数。
    pub probe_missions: u32,
    /// handler future panic 次数。
    pub handler_panics: u64,
    /// 事件回调队列有界丢弃的事件数。
    pub dropped_events: u64,
    /// **因有界背压被丢弃的入站请求回包数**（byte 预算不足或业务队列已满）。
    ///
    /// 与 `dropped_events` 严格分开：后者是 handler 回调队列的丢弃，前者是回给
    /// 对端的响应被丢弃。丢弃是真实的过载行为，绝不伪装成已送达，也绝不因此
    /// 伪造响应；只计数，不带 payload/action 等对端可控文本（sanitized）。
    /// 注意不含「旧代际 handler 的回包」那类非过载丢弃：那不是资源耗尽信号。
    pub dropped_replies: u64,
    /// 退出时已确认清理全部附属任务与两条流的两半。
    pub cleaned: bool,
}

/// `TYPE_SYSTEM` 信令（§6.3：只允许在控制流上）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemSignal {
    Kick,
    Revoke,
}

impl SystemSignal {
    pub(crate) fn action(self) -> &'static str {
        match self {
            Self::Kick => "tunnel.kick",
            Self::Revoke => "tunnel.revoke",
        }
    }

    pub(crate) fn from_action(action: &str) -> Option<Self> {
        match action {
            "tunnel.kick" => Some(Self::Kick),
            "tunnel.revoke" => Some(Self::Revoke),
            _ => None,
        }
    }
}

/// handler future：装箱后由会话的 `JoinSet` 与并发预算托管。
pub type HandlerFuture = Pin<Box<dyn Future<Output = Reply> + Send + 'static>>;

/// 交给 handler 的请求上下文。连接 peer 身份由外层握手 manager 绑定，
/// 本类型不提供、也不从 packet 自报字段推断设备身份。
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub stream: StreamKind,
    pub epoch: StreamEpoch,
    pub packet: TunnelPacket,
}

/// 业务分发入口。dyn-safe：只有 `on_request` 必须实现，其余默认 no-op。
pub trait SessionHandler: Send + Sync + 'static {
    /// 请求处理。future 由会话托管；panic 只回固定 1009 并清任务。
    fn on_request(&self, ctx: RequestContext) -> HandlerFuture;
    /// 单向事件/遥测；在专用回调任务上执行，不阻塞任何 reader。
    fn on_event(&self, _stream: StreamKind, _packet: TunnelPacket) {}
    /// System 信令；在同一回调任务上按序执行，之后整连接关闭。
    fn on_system(&self, _signal: SystemSignal) {}
    /// 本端两条流首次双双健康时调用一次。
    fn on_ready(&self) {}
    /// 会话结束时调用一次。
    fn on_closed(&self, _reason: SessionCloseReason) {}

    /// 让外部 `Arc<Self>` 直接升格为 trait object（不破坏 dyn 兼容）。
    fn into_shared(self: Arc<Self>) -> Arc<dyn SessionHandler>
    where
        Self: Sized,
    {
        self
    }
}

impl Reply {
    /// 组装同 trace、同 action 的响应信封（回源流由调用方决定；探测不走这里）。
    pub(crate) fn to_packet(&self, trace_id: u64, action: &str) -> TunnelPacket {
        TunnelPacket {
            trace_id,
            packet_type: PacketType::Response,
            action: action.to_string(),
            status_code: self.status_code,
            error_message: self.error_message.clone(),
            payload: self.payload.to_vec(),
            metadata: BTreeMap::new(),
        }
    }
}

/// 每连接业务队列字节预算：整连接共享，出队或丢弃自动归还。
#[derive(Clone)]
pub(crate) struct ByteBudget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

/// 一次预留的守卫：随队列条目一起存活，drop 即归还额度。
pub(crate) struct ByteGuard {
    used: Arc<AtomicUsize>,
    bytes: usize,
}

impl ByteBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    /// 非阻塞预留；超过预算（或溢出）直接失败，由调用方回 `Busy`。
    pub(crate) fn try_reserve(&self, bytes: usize) -> Option<ByteGuard> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(bytes)?;
            if next > self.limit {
                return None;
            }
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ByteGuard {
                        used: Arc::clone(&self.used),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for ByteGuard {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::Release);
    }
}

/// 业务队列条目：持有字节预算额度，出队或丢弃即归还。
pub(crate) struct QueuedPacket {
    pub packet: TunnelPacket,
    pub epoch: StreamEpoch,
    pub _budget: ByteGuard,
}

/// 一条当前代际流的共享发送槽；代际切换时整体替换。
#[derive(Clone)]
pub(crate) struct Slot {
    pub epoch: StreamEpoch,
    pub biz_tx: mpsc::Sender<QueuedPacket>,
    pub ctl_tx: mpsc::Sender<StreamCommand>,
    pub budget: ByteBudget,
}

/// 句柄 → owner：业务与关停通道。
/// owner 必须看到的关停意图。它与业务通道彻底解耦：
/// 句柄侧只是写一个单调升级的原子值，队列满、owner 正在别处 await 都丢不了。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ControlIntent {
    None,
    /// 优雅排空：owner 收到后必须真的开始发 GOAWAY 并继续驱动满排空期限。
    Shutdown,
    /// 立即中止：结清一切、取消 handler 与附属任务、释放连接。
    Abort,
}

impl ControlIntent {
    /// 单调升级：Abort 压过 Shutdown，Shutdown 压过 None，永不降级。
    fn rank(self) -> u8 {
        match self {
            ControlIntent::None => 0,
            ControlIntent::Shutdown => 1,
            ControlIntent::Abort => 2,
        }
    }
}

pub(crate) enum SessionCommand {
    /// RPC 请求入队（未决登记与入队在同一次 owner 迭代内原子完成）。
    Enqueue {
        stream: StreamKind,
        packet: TunnelPacket,
        deadline: tokio::time::Instant,
        waiter: Waiter,
        ack: oneshot::Sender<Result<(), RequestError>>,
    },
    /// 单向 Event 入队：不建未决项、不等响应、不重试。
    SendEvent {
        stream: StreamKind,
        packet: TunnelPacket,
        ack: oneshot::Sender<Result<(), RequestError>>,
    },
}

/// 句柄 → owner：System 专用通道（与业务解耦，不能被业务背压挡在前面）。
pub(crate) struct SystemCommand {
    pub packet: TunnelPacket,
    pub ack: oneshot::Sender<Result<(), RequestError>>,
}

/// owner → 连接驱动任务。
pub(crate) enum ConnCommand {
    /// client：开一条新流（首连、顶替或重开）。
    Open {
        kind: StreamKind,
        epoch: StreamEpoch,
    },
    /// server：h2 官方 graceful（GOAWAY → 排空已接纳流 → PING 确认）。
    Graceful,
}

/// 控制/探测小队列命令（独立于业务队列，业务拥堵不得饿死健康检查与 System）。
pub(crate) enum StreamCommand {
    Probe(ProbeRequest),
    Pong { trace_id: u64, nonce: Vec<u8> },
    System(TunnelPacket),
    Reset(h2::Reason),
    Finish,
}

/// 附属任务 → owner。
pub(crate) enum StreamEvent {
    /// server：accept 到一条待路由的流。
    Incoming {
        request: Request<h2::RecvStream>,
        response: h2::server::SendResponse<Bytes>,
    },
    /// client：开流结果；错误同样交 owner 计入重开失败。
    Opened {
        kind: StreamKind,
        epoch: StreamEpoch,
        result: Result<(StreamSender, StreamReceiver), StreamError>,
    },
    /// reader 转发的一包（分类由 owner 做，reader 不解释业务）。
    Packet {
        kind: StreamKind,
        epoch: StreamEpoch,
        packet: TunnelPacket,
    },
    /// reader/writer 上报：该代际流已结束（EOF/RST/协议错）。
    ///
    /// `error` 是**类型化**的失败原因，归属由 [`StreamError::connection_fatal`] 给出：
    /// 只有结构性路由违规终结整条连接，应用包非法（含 §6.7.2 的非法探测）只终结本流。
    /// owner 侧不得再用 bool/文本猜测这一级别。
    StreamClosed {
        kind: StreamKind,
        epoch: StreamEpoch,
        error: Option<StreamError>,
    },
    /// 入站 GOAWAY（只有结构化字段）。
    PeerGoaway { error_code: u32 },
    /// 连接驱动任务结束。
    ConnClosed { reason: SessionCloseReason },
    /// GOAWAY 是否真正写入内核（失败不得谎称已发）。
    GoawaySent { ok: bool },
}

/// handler future 的完成消息（panic 由 drop guard 也发一条，保证不悬挂）。
pub(crate) struct HandlerDone {
    pub kind: StreamKind,
    pub epoch: StreamEpoch,
    pub trace_id: u64,
    pub action: String,
    pub reply: Result<Reply, HandlerPanic>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HandlerPanic;

/// 未决表 value：业务响应回投通道。
pub(crate) struct Waiter(pub oneshot::Sender<Result<Reply, RequestError>>);

impl fmt::Debug for Waiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Waiter(<one-shot reply channel>)")
    }
}

/// 每流的「首次健康」证据槽索引。
pub(crate) fn slot_index(kind: StreamKind) -> usize {
    match kind {
        StreamKind::Control => 0,
        StreamKind::Data => 1,
    }
}

/// owner 拥有、句柄可读的会话状态。
pub(crate) struct SessionStatus {
    pub state: watch::Sender<SessionState>,
    /// 每流首探测成功的代际；两条都非 0 才 Ready。
    pub ready_epoch: [AtomicU64; 2],
    /// 排空已开始（本端请求或对端 GOAWAY）。句柄可同步置位。
    pub draining: AtomicBool,
    /// 句柄 → owner 的关停意图（单调升级，绝不走业务队列）。
    pub intent: watch::Sender<ControlIntent>,
}

impl SessionStatus {
    pub(crate) fn new() -> Self {
        Self {
            state: watch::channel(SessionState::Connecting).0,
            ready_epoch: [const { AtomicU64::new(0) }; 2],
            draining: AtomicBool::new(false),
            intent: watch::channel(ControlIntent::None).0,
        }
    }

    pub(crate) fn set(&self, state: SessionState) {
        self.state.send_if_modified(|current| {
            if *current == state {
                false
            } else {
                *current = state;
                true
            }
        });
    }

    pub(crate) fn mark_ready(&self, kind: StreamKind, epoch: StreamEpoch) {
        self.ready_epoch[slot_index(kind)].store(epoch.0.max(1), Ordering::Release);
    }

    pub(crate) fn clear_ready(&self, kind: StreamKind) {
        self.ready_epoch[slot_index(kind)].store(0, Ordering::Release);
    }

    pub(crate) fn both_ready(&self) -> bool {
        self.ready_epoch
            .iter()
            .all(|slot| slot.load(Ordering::Acquire) != 0)
    }

    /// 记录关停意图并唤醒 owner（幂等，Abort 一旦置位不被降级）。
    pub(crate) fn request(&self, intent: ControlIntent) {
        self.intent.send_if_modified(|current| {
            if current.rank() >= intent.rank() {
                false
            } else {
                *current = intent;
                true
            }
        });
    }

    /// 读取当前意图并标记已见（配合 `changed()` 不会丢唤醒）。
    pub(crate) fn take_intent(&self) -> ControlIntent {
        *self.intent.borrow()
    }

    pub(crate) fn begin_drain(&self) {
        self.draining.store(true, Ordering::Release);
    }

    pub(crate) fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }
}

/// owner 与句柄共享的运行时入口。
pub(crate) struct SessionShared {
    pub timeout: Duration,
    pub cmd_tx: mpsc::Sender<SessionCommand>,
    pub system_tx: mpsc::Sender<SystemCommand>,
    pub status: Arc<SessionStatus>,
    pub state_rx: watch::Receiver<SessionState>,
}

/// 会话句柄：`Clone + Send + Sync`，不含任何 key 或设备私钥。
#[derive(Clone)]
pub struct SessionHandle {
    shared: Arc<SessionShared>,
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionHandle")
            .field("state", &*self.shared.state_rx.borrow())
            .finish_non_exhaustive()
    }
}

impl SessionHandle {
    pub(crate) fn new(shared: Arc<SessionShared>) -> Self {
        Self { shared }
    }

    /// 同步 RPC（§6.5 模式 1）：分配 trace、按当前代际入队、登记未决。
    /// 期限（默认 5s）起算于本调用；队列满 `Busy`；不自动重发结果未知的变更。
    pub async fn request(
        &self,
        stream: StreamKind,
        action: &str,
        payload: Vec<u8>,
    ) -> Result<Reply, RequestError> {
        let packet = TunnelPacket {
            trace_id: next_trace_id()?,
            packet_type: PacketType::Request,
            action: action.to_string(),
            status_code: 0,
            error_message: String::new(),
            payload,
            metadata: BTreeMap::new(),
        };
        if packet.validate(stream).is_err() {
            return Err(RequestError::ProtocolMismatch);
        }
        let shared = Arc::clone(&self.shared);
        // 期限锚定在入队这一刻：之后排队与写阻塞都不暂停。
        let deadline = tokio::time::Instant::now() + shared.timeout;
        if *shared.state_rx.borrow() == SessionState::Closed || shared.status.is_draining() {
            return Err(RequestError::Closed);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let (ack_tx, ack_rx) = oneshot::channel();
        shared
            .cmd_tx
            .clone()
            .try_send(SessionCommand::Enqueue {
                stream,
                packet,
                deadline,
                waiter: Waiter(reply_tx),
                ack: ack_tx,
            })
            .map_err(|_| RequestError::Busy)?;
        // ack 由 owner 在同一次迭代内给出：Closed / StreamClosed / Busy / Protocol。
        ack_rx.await.map_err(|_| RequestError::Closed)??;
        // owner 的到期巡检负责结清未决表；这里保证调用方按时返回。
        // 同一个 entry 只会被移除一次，两条路径不会重复结清。
        tokio::time::timeout_at(deadline, reply_rx)
            .await
            .map_err(|_| RequestError::Timeout)?
            .map_err(|_| RequestError::Closed)?
    }

    /// 单向 Event（§6.5 模式 3）：独立 trace、不建未决项、不等响应、不重试。
    /// 与 RPC 共用该流的字节预算与发送背压；排空后拒绝。
    /// **成功只表示已交给本端发送队列**，不代表对端已处理。
    pub async fn send_event(
        &self,
        stream: StreamKind,
        action: &str,
        payload: Vec<u8>,
    ) -> Result<(), RequestError> {
        let packet = TunnelPacket {
            trace_id: next_trace_id()?,
            packet_type: PacketType::Event,
            action: action.to_string(),
            status_code: 0,
            error_message: String::new(),
            payload,
            metadata: BTreeMap::new(),
        };
        if packet.validate(stream).is_err() {
            return Err(RequestError::ProtocolMismatch);
        }
        let shared = Arc::clone(&self.shared);
        if *shared.state_rx.borrow() == SessionState::Closed || shared.status.is_draining() {
            return Err(RequestError::Closed);
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        shared
            .cmd_tx
            .clone()
            .try_send(SessionCommand::SendEvent {
                stream,
                packet,
                ack: ack_tx,
            })
            .map_err(|_| RequestError::Busy)?;
        ack_rx.await.map_err(|_| RequestError::Closed)?
    }

    /// System 信令（§6.3/§6.6）：只走控制流、payload 必须为空、独立 trace。
    /// 走与业务解耦的有界优先路径，因此不会被业务背压堵死；成功同样只表示
    /// 已交给本端发送队列，外层 manager 仍需自己的 timer 关闭连接。
    pub async fn send_system(&self, signal: SystemSignal) -> Result<(), RequestError> {
        let packet = TunnelPacket {
            trace_id: next_trace_id()?,
            packet_type: PacketType::System,
            action: signal.action().to_string(),
            status_code: 0,
            error_message: String::new(),
            payload: Vec::new(),
            metadata: BTreeMap::new(),
        };
        // System 只允许在控制流上，且不得携带 payload。
        if packet.validate(StreamKind::Control).is_err() || !packet.payload.is_empty() {
            return Err(RequestError::ProtocolMismatch);
        }
        let shared = Arc::clone(&self.shared);
        if *shared.state_rx.borrow() == SessionState::Closed {
            return Err(RequestError::Closed);
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        shared
            .system_tx
            .clone()
            .try_send(SystemCommand {
                packet,
                ack: ack_tx,
            })
            .map_err(|_| RequestError::Busy)?;
        ack_rx.await.map_err(|_| RequestError::Closed)?
    }

    /// 等到本端两条流各自首 probe 有效 pong。
    pub async fn wait_ready(&self) -> Result<(), RequestError> {
        let mut rx = self.shared.state_rx.clone();
        loop {
            match *rx.borrow_and_update() {
                SessionState::Ready => return Ok(()),
                SessionState::Closed | SessionState::Draining => {
                    return Err(RequestError::Closed);
                }
                SessionState::Connecting => {}
            }
            if rx.changed().await.is_err() {
                return Err(RequestError::Closed);
            }
        }
    }

    /// 非阻塞、幂等请求优雅排空：client 恰好一条 GOAWAY(last_stream_id=0)，
    /// server 交给 h2 官方 graceful。返回时只保证「意图已登记」。
    pub fn graceful_shutdown(&self) {
        // 意图落在独立的 watch 上：业务队列满也丢不了，且 owner 收到后必须真的
        // 继续驱动排空（发 GOAWAY、走完 >=5s），不是只把状态改成 Draining。
        self.shared.status.request(ControlIntent::Shutdown);
        // watch 是同步的：订阅方立即看到 Draining，不必等 owner 被调度。
        self.shared.status.begin_drain();
        self.shared.status.set(SessionState::Draining);
    }

    /// 立即中止：不排空、不重放任何已发出的变更。
    pub fn abort(&self) {
        // 与 graceful_shutdown 同理：终止意图不得依赖业务队列的余量。
        self.shared.status.request(ControlIntent::Abort);
    }

    /// 订阅状态变化（含 Draining，供外层 manager 立即摘除会话）。
    pub fn subscribe_state(&self) -> watch::Receiver<SessionState> {
        self.shared.state_rx.clone()
    }
}

impl Drop for SessionShared {
    fn drop(&mut self) {
        // 最后一个句柄消失：让 owner 走 Abort 结清，不留孤儿会话。
        // 这里同样不能 try_send：队列满时终止会被静默丢弃。
        self.status.request(ControlIntent::Abort);
    }
}
