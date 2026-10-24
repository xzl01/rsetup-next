//! 真实双流会话驱动：`SecureIo` → `H2KeepaliveIo` → h2c 双长流。
//!
//! 任务拓扑（owner 是唯一的协议状态持有者，返回给调用方的 `JoinHandle` 就是它）：
//!
//! * **owner 任务**：未决表、逐流 [`ProbeState`]、代际登记、handler `JoinSet`、
//!   事件/回调队列、排空计时。它**从不**直接 poll h2 对象，也从不在 `await` 上
//!   持有协议状态——所有网络 I/O 都在附属任务里，owner 只与有界通道交换消息。
//!   因此「业务写阻塞」不可能让 h2 无人 poll。
//! * **连接驱动任务**：client 持续 poll `h2::client::Connection` 并串行开流
//!   （Control 先于 Data）；server 持续 `poll_accept` + `poll_close`，收到
//!   `Graceful` 才走 h2 官方 graceful。它退出即连接级结束。h2 结束后本任务
//!   **park 住不释放 io**，由 owner 退出时才销毁 socket，保证排空期内连接不被
//!   提前掐断。
//! * **每代际每条流的 reader/writer 任务**：writer 先看控制小队列再看业务队列
//!   （probe/pong/System 优先），reader 只转发不解释业务。代际切换时两半与两个
//!   任务一起销毁（writer 卡在流控里时由 owner abort，`SendGuard` drop 会 reset
//!   该流）。
//! * **回调任务**：`on_event`/`on_system`/`on_ready`/`on_closed` 串行执行，队列
//!   有界、满则丢弃并计数，因此任何 handler 同步回调都阻塞不了 reader。
//!
//! 关停语义：`graceful_shutdown` 立即生效——client 恰好一条 GOAWAY(last 0)、
//! server 走 h2 官方 graceful——然后维持**至少** `drain_timeout`（默认 5s）已接纳
//! 的操作；不等「所有业务响应先做完」（RFC9113 §6.8 允许 GOAWAY 后继续完成既有
//! 流），也不把 h2 的 SHUTDOWN PING 挡掉。
use super::config::{
    H2_INITIAL_CONNECTION_WINDOW, H2_INITIAL_WINDOW, H2_MAX_CONCURRENT_STREAMS, H2_MAX_FRAME_SIZE,
    H2_MAX_HEADER_LIST_SIZE, SessionConfig, SessionError,
};
use super::handle::{
    ConnCommand, HandlerDone, HandlerPanic, SessionCloseReason, SessionCommand, SessionHandle,
    SessionHandler, SessionOutcome, SessionShared, SessionState, SessionStatus, Slot,
    StreamCommand, StreamEvent, SystemCommand, SystemSignal, Waiter,
};
use super::pending::{PROBE_RESERVATION, PendingKind, PendingTable, RequestKey};
use super::probe::{ProbeDecision, ProbeState};
use super::streams::{
    CONTROL_PATH, DATA_PATH, StreamError, StreamReceiver, StreamSender, accept_server_stream,
    open_client_stream,
};
use super::types::{Reply, RequestError, StreamEpoch};
use crate::envelope::{PacketType, StreamKind, TunnelPacket};
use crate::h2_keepalive::{H2KeepaliveIo, H2Role, PeerGoaway};
use crate::secure_io::SecureIo;
use bytes::Bytes;
use std::collections::{BTreeMap, VecDeque};
use std::future::poll_fn;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until};

/// handler panic 的固定响应。
const PANIC_STATUS: i32 = 1009;
const PANIC_MESSAGE: &str = "handler panicked";
/// handler 并发预算耗尽的固定响应（不排队、不重试）。
const OVERLOAD_STATUS: i32 = 1002;
const OVERLOAD_MESSAGE: &str = "handler concurrency exhausted";
const PING_ACTION: &str = "tunnel.ping";
/// §6.7.2：`tunnel.ping` 的 nonce 必须恰好 16B（接收端复核用）。
const PROBE_NONCE_LEN: usize = 16;
/// System 回调的有界送达宽限：通知先于关闭，但不无限等慢回调。
const SYSTEM_GRACE: Duration = Duration::from_millis(500);
/// 事件通道深度：结构事件稀少，reader 靠它做背压。
const EVENT_QUEUE: usize = 128;
/// System 专用通道深度（kick/revoke 幂等，不需要更深）。
const SYSTEM_QUEUE: usize = 4;
/// 开流命令投递失败后的退避。
const OPEN_RETRY_BACKOFF: Duration = Duration::from_millis(50);
/// 退出时确认附属任务结束的宽限。
const CLEANUP_GRACE: Duration = Duration::from_secs(2);
/// 确认被 abort 的任务真正结束时的轮询间隔（也是让出调度权的凭据）。
const CLEANUP_POLL: Duration = Duration::from_millis(1);
/// 完全空闲时的巡检间隔：只为让 owner 有节奏地重新计算期限。
const IDLE_TICK: Duration = Duration::from_millis(50);

type SlotTable = [RwLock<Option<Slot>>; 2];

fn kinds() -> [StreamKind; 2] {
    [StreamKind::Control, StreamKind::Data]
}

fn index_of(kind: StreamKind) -> usize {
    match kind {
        StreamKind::Control => 0,
        StreamKind::Data => 1,
    }
}

fn ping_packet(trace_id: u64, nonce: &[u8; 16]) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: PacketType::Request,
        action: PING_ACTION.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: nonce.to_vec(),
        metadata: BTreeMap::new(),
    }
}

fn pong_packet(trace_id: u64, nonce: &[u8]) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: PacketType::Response,
        action: PING_ACTION.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: nonce.to_vec(),
        metadata: BTreeMap::new(),
    }
}

fn reply_from_packet(packet: &TunnelPacket) -> Reply {
    Reply {
        status_code: packet.status_code,
        error_message: packet.error_message.clone(),
        payload: Bytes::copy_from_slice(&packet.payload),
    }
}

fn client_builder() -> h2::client::Builder {
    let mut builder = h2::client::Builder::new();
    builder
        .initial_window_size(H2_INITIAL_WINDOW)
        .initial_connection_window_size(H2_INITIAL_CONNECTION_WINDOW)
        .max_frame_size(H2_MAX_FRAME_SIZE)
        .max_header_list_size(H2_MAX_HEADER_LIST_SIZE)
        // client 绝不接收 push：显式关闭，避免对端用 PUSH_PROMISE 建立第三条流。
        .enable_push(false)
        .max_concurrent_streams(H2_MAX_CONCURRENT_STREAMS);
    builder
}

fn server_builder() -> h2::server::Builder {
    let mut builder = h2::server::Builder::new();
    builder
        .initial_window_size(H2_INITIAL_WINDOW)
        .initial_connection_window_size(H2_INITIAL_CONNECTION_WINDOW)
        .max_frame_size(H2_MAX_FRAME_SIZE)
        .max_header_list_size(H2_MAX_HEADER_LIST_SIZE)
        .max_concurrent_streams(H2_MAX_CONCURRENT_STREAMS);
    // server 侧 h2 不提供 push 开关：本端从不主动开流，也就没有 PUSH_PROMISE。
    builder
}

// ---------------------------------------------------------------- 公共入口

/// 以中控（client）角色启动会话：内部先包一层 `H2KeepaliveIo`，并在把 io 交给
/// h2 **之前**取得 GOAWAY 控制句柄。
pub fn start_client_session<S>(
    io: SecureIo<S>,
    handler: Arc<dyn SessionHandler>,
    config: SessionConfig,
) -> Result<(SessionHandle, JoinHandle<SessionOutcome>), SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    start(H2Role::Client, io, handler, config)
}

/// 以设备（server）角色启动会话。server 永不创建流，只路由与顶替 client 建来的流。
pub fn start_server_session<S>(
    io: SecureIo<S>,
    handler: Arc<dyn SessionHandler>,
    config: SessionConfig,
) -> Result<(SessionHandle, JoinHandle<SessionOutcome>), SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    start(H2Role::Server, io, handler, config)
}

fn start<S>(
    role: H2Role,
    io: SecureIo<S>,
    handler: Arc<dyn SessionHandler>,
    config: SessionConfig,
) -> Result<(SessionHandle, JoinHandle<SessionOutcome>), SessionError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    config.validate()?;
    // 先占回调追踪名额，再 spawn 回调任务：顺序反过来就等于先制造一个「可能没人追踪」
    // 的任务。名额耗尽（并发会话 > CALLBACK_TRACK_CAPACITY）时直接拒绝启动这条会话，
    // io 随本函数返回被 drop（明确失败关闭），不静默放宽、不无界增长。
    let mut callback = CallbackTracker::reserve(callback_registry()).ok_or(
        // 复用既有入口错误变体（本任务不得改 config.rs 的公共枚举）：reason 文本如实
        // 写出真实原因，不伪装成网络故障。
        SessionError::InvalidConfig("callback tracking slots exhausted"),
    )?;
    let keepalive = H2KeepaliveIo::new(io, role);
    // 必须在 io 移交 h2 之前取句柄：之后再也拿不到第二次。
    let wire = Arc::new(keepalive.control_handle());
    let peer_goaway = wire.subscribe_peer_goaway();
    // 出站 GOAWAY「整帧写出 + 底层 flush 确认」的观察者：两种角色都需要，
    // 因为 server 的 GOAWAY 是 h2 自己写出的。owner 用它把排空计时锚定在
    // GOAWAY 真的上线之后，而不是「请求发 GOAWAY」那一刻。
    let goaway_flush = wire.subscribe_outbound_goaway();
    let is_client = matches!(role, H2Role::Client);

    // 命令通道**只承载业务投递**（`request`/`send_event`）。关停与终止走独立的
    // `SessionStatus::intent` watch，探测由 owner 自己按窗口经每流 ctl 小队列发出：
    // 两者都不占本通道，所以业务队列被压到 1 也堵不住关停/探测。
    // `business_queue.max(4)` 只是给业务投递留一点基本余量；真正的硬上限仍是每条流的
    // biz_tx（容量 business_queue）与整连接 byte 预算，两处都以 Busy 结清。
    let (cmd_tx, cmd_rx) = mpsc::channel(config.business_queue.max(4));
    let (system_tx, system_rx) = mpsc::channel(SYSTEM_QUEUE);
    let (ev_tx, ev_rx) = mpsc::channel(EVENT_QUEUE);
    let (conn_tx, conn_rx) = mpsc::channel(8);
    let (done_tx, done_rx) = mpsc::channel(config.handler_concurrency + 8);
    let (notify_tx, notify_rx) = mpsc::channel(config.event_queue + config.notify_queue);
    let (hold_tx, hold_rx) = oneshot::channel();

    let status = Arc::new(SessionStatus::new());
    let slots: Arc<SlotTable> = Arc::new([RwLock::new(None), RwLock::new(None)]);
    let now = Instant::now();
    let streams = [
        StreamState::new(StreamKind::Control, StreamEpoch(1), now, is_client),
        StreamState::new(StreamKind::Data, StreamEpoch(1), now, is_client),
    ];

    // 回调任务的「自己已经返回」信号：finish 用它做有界等待。JoinHandle 不可
    // Clone，直接 `timeout(.., handle)` 一旦超时就等于丢掉句柄（脱离追踪），因此
    // 句柄从一开始就挂在预留好的追踪名额上（`CallbackTracker`），确认不了也只能转交，
    // 不能丢弃。
    let (callback_done_tx, callback_done) = oneshot::channel();
    let callback_handler = Arc::clone(&handler);
    let callback_join = tokio::spawn(async move {
        run_callback(callback_handler, notify_rx).await;
        let _ = callback_done_tx.send(());
    });
    callback.attach(callback_join);
    let conn_join = if is_client {
        tokio::spawn(client_connection(
            keepalive,
            conn_rx,
            ev_tx.clone(),
            hold_rx,
        ))
    } else {
        tokio::spawn(server_connection(
            keepalive,
            conn_rx,
            ev_tx.clone(),
            hold_rx,
        ))
    };
    // 事件推送之外再留一份 watch 副本：GOAWAY 与连接结束几乎同时到达时，
    // 事件顺序不再决定结论。
    let peer_seen = peer_goaway.clone();
    let watch_join = tokio::spawn(watch_peer_goaway(peer_goaway, ev_tx.clone()));

    // sender 交给 owner，句柄只留订阅端。
    let state_rx = status.state.subscribe();
    let timeout = config.ordinary_timeout;
    let driver = Driver::new(
        config,
        handler,
        Arc::clone(&status),
        slots,
        streams,
        wire,
        is_client,
        goaway_flush,
        conn_tx,
        ev_tx,
        done_tx,
        notify_tx,
        peer_seen,
        status.intent.subscribe(),
        vec![conn_join, watch_join],
        Vec::new(),
    );
    let outcome = tokio::spawn(run_driver(
        driver,
        cmd_rx,
        system_rx,
        ev_rx,
        done_rx,
        hold_tx,
        callback,
        callback_done,
    ));
    let shared = Arc::new(SessionShared {
        timeout,
        cmd_tx,
        system_tx,
        status,
        state_rx,
    });
    Ok((SessionHandle::new(shared), outcome))
}

// ---------------------------------------------------------------- 连接驱动

/// client：持续驱动 `h2::client::Connection`，并串行执行开流命令。
async fn client_connection<S>(
    keepalive: H2KeepaliveIo<SecureIo<S>>,
    mut cmd_rx: mpsc::Receiver<ConnCommand>,
    ev: mpsc::Sender<StreamEvent>,
    hold: oneshot::Receiver<()>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handshake = client_builder().handshake(keepalive).await;
    let reason = match handshake {
        Ok((mut send_request, conn)) => {
            let mut conn = std::pin::pin!(conn);
            loop {
                tokio::select! {
                    biased;
                    Some(command) = cmd_rx.recv() => match command {
                        ConnCommand::Open { kind, epoch } => {
                            // 开流期间连接必须由同一任务继续驱动，否则 response
                            // headers 永远收不到（这就是「inline await 无人 poll h2」）。
                            let opened = tokio::select! {
                                biased;
                                result = open_client_stream(&mut send_request, kind, epoch) => result,
                                finished = conn.as_mut() => break conn_reason(finished),
                            };
                            let event = StreamEvent::Opened {
                                kind,
                                epoch,
                                result: opened,
                            };
                            tokio::select! {
                                _ = ev.send(event) => {}
                                finished = conn.as_mut() => break conn_reason(finished),
                            }
                        }
                        // client 的 GOAWAY 由 wire 注入，不走 h2 graceful。
                        ConnCommand::Graceful => {}
                    },
                    finished = conn.as_mut() => break conn_reason(finished),
                }
            }
        }
        Err(_) => SessionCloseReason::Transport,
    };
    park_and_report(reason, ev, hold).await;
}

/// server：持续 accept + `poll_close`；只有 owner 明确要求才走官方 graceful。
async fn server_connection<S>(
    keepalive: H2KeepaliveIo<SecureIo<S>>,
    mut cmd_rx: mpsc::Receiver<ConnCommand>,
    ev: mpsc::Sender<StreamEvent>,
    hold: oneshot::Receiver<()>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut conn = std::pin::pin!(match server_builder().handshake(keepalive).await {
        Ok(conn) => conn,
        Err(_) => {
            park_and_report(SessionCloseReason::Transport, ev, hold).await;
            return;
        }
    });
    let mut accepted: Option<Result<_, h2::Error>> = None;
    let reason = loop {
        // accept、poll_close 与关停命令都在这一处驱动：命令必须在同一个 poll 里
        // 注册 waker，否则任务已经挂在 accept 上时，graceful 只能等下一个无关事件
        // 才被看见——GOAWAY 会永远停在 h2 内部，一个字节都写不出去。
        let event = poll_fn(|cx| {
            while let std::task::Poll::Ready(Some(ConnCommand::Graceful)) = cmd_rx.poll_recv(cx) {
                // h2 官方 graceful：由它自己写 GOAWAY（本层不替它写、不改帧）。
                conn.as_mut().graceful_shutdown();
            }
            if let Some(first) = accepted.take() {
                return std::task::Poll::Ready(ServerPoll::Accepted(first.map(Box::new)));
            }
            match conn.as_mut().poll_accept(cx) {
                std::task::Poll::Ready(Some(result)) => {
                    return std::task::Poll::Ready(ServerPoll::Accepted(result.map(Box::new)));
                }
                std::task::Poll::Ready(None) => {
                    return std::task::Poll::Ready(ServerPoll::Closed(Ok(())));
                }
                std::task::Poll::Pending => {}
            }
            conn.as_mut().poll_closed(cx).map(ServerPoll::Closed)
        })
        .await;
        match event {
            ServerPoll::Accepted(Ok(pair)) => {
                let (request, response) = *pair;
                let event = StreamEvent::Incoming { request, response };
                let sent = tokio::select! {
                    _ = ev.send(event) => Forwarded::Sent,
                    closed = poll_fn(|cx| conn.as_mut().poll_closed(cx)) => Forwarded::Done(closed),
                };
                if let Forwarded::Done(closed) = sent {
                    break conn_reason(closed);
                }
            }
            ServerPoll::Accepted(Err(_)) => break SessionCloseReason::Transport,
            ServerPoll::Closed(result) => break conn_reason(result),
        }
    };
    park_and_report(reason, ev, hold).await;
}

type IncomingRequest = http::Request<h2::RecvStream>;
type IncomingResponse = h2::server::SendResponse<Bytes>;

enum Forwarded {
    Sent,
    Done(Result<(), h2::Error>),
}

enum ServerPoll {
    // 装箱：接受到的流句柄远大于关闭分支，不装箱会让每次 poll 都搬运 264B。
    Accepted(Result<Box<(IncomingRequest, IncomingResponse)>, h2::Error>),
    Closed(Result<(), h2::Error>),
}

/// 摘掉已确认结束的任务句柄，未结束的一律留在表里。
///
/// 「确认结束」只以 `is_finished()`（调度器已 poll 到完成/取消）为准：
/// 未完成的句柄绝不能丢出表外，否则等于放弃追踪（detach）。
fn prune_finished(handles: &mut Vec<JoinHandle<()>>) {
    handles.retain(|handle| !handle.is_finished());
}

// -------------------------------------------------- 未确认结束的回调句柄追踪表

/// 进程级「仍未确认结束的回调任务」追踪名额上限。
///
/// **每条会话在启动回调任务之前预留 1 个名额**，因此表长硬上界就是本进程的并发
/// 会话数（≤1024）：既保证「超时的原句柄一定要能转交进来」（转交永不因表满而被迫
/// 丢弃句柄），也保证不会无限增长。名额耗尽时新会话启动**明确失败**（fail closed），
/// 而不是静默放宽或悄悄脱离追踪。
pub const CALLBACK_TRACK_CAPACITY: usize = 1024;

/// 追踪表里的一个名额。
enum TrackSlot {
    /// 空位。
    Free,
    /// 会话已预留（回调任务可能尚未启动，也可能仍在运行）。
    Reserved,
    /// 已转交、仍未确认结束的回调任务句柄。
    Tracked(JoinHandle<()>),
}

/// 进程级、硬容量、**无后台轮询任务**的回调句柄追踪表（quarantine）。
///
/// 只跟踪「还没法确认结束」的回调：会话退出时确认不了的**原句柄**转交进来，之后
/// 由三类既有事件顺带回收——新会话启动（预留时 `prune`）、会话退出（`commit` 前
/// `prune`）、外部观察 [`quarantined_callbacks`]。已确认结束的句柄在回收那一刻被
/// 丢弃，因此不会长期替已完成的回调扣着 handler。
///
/// 边界（必须如实理解）：这张表**不是强制取消**，也不能让不可抢占的同步回调停下来；
/// 它只保证「本进程仍在追踪这个任务」，从而 `cleaned=false` 是可核对的事实而非空话。
/// 用户代码永久阻塞时，该任务会一直占着一个名额（占用可见、有上界），但也仅此而已：
/// 名额耗尽后新会话直接启动失败，不会演变成无限增长。runtime 关停/进程退出是另一层
/// 边界，由外层 manager 负责，本表不假装能跨越它。
struct CallbackRegistry {
    slots: Vec<TrackSlot>,
}

impl CallbackRegistry {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: (0..capacity).map(|_| TrackSlot::Free).collect(),
        }
    }

    /// 回收已确认结束的句柄（`is_finished()`：调度器已 poll 到完成/取消）。
    fn prune(&mut self) {
        for slot in self.slots.iter_mut() {
            let TrackSlot::Tracked(handle) = slot else {
                continue;
            };
            if handle.is_finished() {
                *slot = TrackSlot::Free;
            }
        }
    }

    /// 预留一个名额；满则 `None`，调用方必须拒绝启动新会话。
    fn reserve(&mut self) -> Option<usize> {
        self.prune();
        let index = self
            .slots
            .iter()
            .position(|slot| matches!(slot, TrackSlot::Free))?;
        self.slots[index] = TrackSlot::Reserved;
        Some(index)
    }

    /// 归还自己预留的名额；只对 `Reserved` 生效，因此可重复调用。
    fn release(&mut self, index: usize) {
        if let Some(slot @ TrackSlot::Reserved) = self.slots.get_mut(index) {
            *slot = TrackSlot::Free;
        }
    }

    /// 把未能确认结束的**原句柄**放进自己预留的名额。
    ///
    /// 预留成功后这一步不可能失败，这正是「先预留、后 spawn」的全部意义：`prune`
    /// 只回收 `Tracked`，绝不动 `Reserved`，所以名额不会在别人手里凭空消失。
    fn commit(&mut self, index: usize, handle: JoinHandle<()>) {
        self.prune();
        if let Some(slot @ TrackSlot::Reserved) = self.slots.get_mut(index) {
            *slot = TrackSlot::Tracked(handle);
        }
    }

    /// 仍未确认结束、已转交的句柄数（调用前应先 `prune`）。
    fn tracked(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| matches!(slot, TrackSlot::Tracked(_)))
            .count()
    }
}

/// 进程级唯一实例。用 `OnceLock` 而不是 `thread_local`：会话任务可能落在任意
/// worker 上，追踪必须跨任务、跨线程成立。
fn callback_registry() -> &'static Mutex<CallbackRegistry> {
    static REGISTRY: OnceLock<Mutex<CallbackRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(CallbackRegistry::with_capacity(CALLBACK_TRACK_CAPACITY)))
}

/// 取表。表内不持锁跨 `await`，也不在持锁期间执行用户代码；中毒只可能来自测试里的
/// panic，此时数组内容依旧自洽，恢复比 panic 更符合「绝不丢句柄」的目标。
fn lock_registry(
    registry: &Mutex<CallbackRegistry>,
) -> std::sync::MutexGuard<'_, CallbackRegistry> {
    registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 观察当前**仍未确认结束**的会话回调任务数（进程级）。
///
/// 读取前先按 `JoinHandle::is_finished()` 顺带回收，因此本数只表示「还没法确认
/// 结束的回调」，不是历史累计；已完成的回调既不扣名额，也不扣 handler。
/// 这是运维观察/回归用的小接口，不是下游必须配合的构造参数。
pub fn quarantined_callbacks() -> usize {
    let mut registry = lock_registry(callback_registry());
    registry.prune();
    registry.tracked()
}

/// 一条会话为其**唯一**回调任务持有的追踪名额。
///
/// 预留发生在回调任务 `spawn` **之前**：会话既然启动成功，超时转交就一定有名额，
/// 于是任何退出路径都不需要靠丢弃句柄来脱身。
struct CallbackTracker<'a> {
    registry: &'a Mutex<CallbackRegistry>,
    index: usize,
    /// `None` = 已确认了结（句柄可安全丢弃），或已由 `Drop` 转交追踪表。
    handle: Option<JoinHandle<()>>,
}

impl<'a> CallbackTracker<'a> {
    /// 预留名额。`None` 表示追踪名额已满：调用方必须拒绝启动这条会话。
    fn reserve(registry: &'a Mutex<CallbackRegistry>) -> Option<Self> {
        let index = lock_registry(registry).reserve()?;
        Some(Self {
            registry,
            index,
            handle: None,
        })
    }

    /// 把刚 spawn 的回调任务句柄挂到预留名额上。
    fn attach(&mut self, handle: JoinHandle<()>) {
        self.handle = Some(handle);
    }

    fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// 请求取消。同步回调不可抢占，因此这只是一个**尽力而为**的信号。
    fn abort(&self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    /// 已确认了结：立刻丢弃句柄并归还名额。
    ///
    /// 只允许两种来源——回调任务自己报告 handler 已 return（完成信号是任务在
    /// return **之后**发出的），或该任务已消亡（panic/取消，其 future 连同捕获的
    /// handler `Arc` 一起被 drop）。两者都不再承载用户代码，丢弃句柄不构成 detach。
    fn release_confirmed(mut self) {
        self.handle = None;
    }

    /// 在 `grace` 内有界确认「该任务真的结束了」。
    ///
    /// 返回 `false` 时本类型的 `Drop` 会把**原句柄**转交追踪表：owner 有界返回与
    /// 不丢句柄同时成立，代价只是 `cleaned=false` 这一如实结论。
    async fn settle(mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.is_finished() {
                // 已确认结束：丢弃句柄不构成 detach，Drop 负责归还名额。
                self.handle = None;
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            // 交出调度权：is_finished 只有在任务真的被 poll 到之后才会翻真。
            tokio::time::sleep(CLEANUP_POLL).await;
        }
    }
}

impl Drop for CallbackTracker<'_> {
    fn drop(&mut self) {
        match self.handle.take() {
            // 句柄还在手上 = 没能确认它结束。直接 drop(JoinHandle) 等于让任务脱离
            // 追踪（它照样继续跑，但本进程再也不知道它何时结束，`cleaned` 就成了
            // 空话）。名额是预留过的，转交不可能因「表满」失败。
            Some(handle) => lock_registry(self.registry).commit(self.index, handle),
            // 已确认了结（或从未挂上句柄）：把名额还回去。
            None => lock_registry(self.registry).release(self.index),
        }
    }
}

fn reject(ack: oneshot::Sender<Result<(), RequestError>>, error: RequestError) {
    let _ = ack.send(Err(error));
}

fn conn_reason(_result: Result<(), h2::Error>) -> SessionCloseReason {
    // 连接自行结束（EOF、对端关闭、h2 致命错、保活判死）统一记 Transport；
    // 若已在排空，owner 会用 drain_reason（Local/PeerGoaway）覆盖。
    SessionCloseReason::Transport
}

/// h2 结束后不立即释放 io：owner 退出（drop hold_tx）才真正关闭 socket，
/// 这样排空期限内不会因为 h2 Connection 提前 Ready 就把已接纳的活跃流掐死。
async fn park_and_report(
    reason: SessionCloseReason,
    ev: mpsc::Sender<StreamEvent>,
    hold: oneshot::Receiver<()>,
) {
    let _ = ev.send(StreamEvent::ConnClosed { reason }).await;
    let _ = hold.await;
}

/// 观察对端 GOAWAY：无需新请求即可让 owner 进入排空。
async fn watch_peer_goaway(
    mut rx: tokio::sync::watch::Receiver<Option<PeerGoaway>>,
    ev: mpsc::Sender<StreamEvent>,
) {
    loop {
        if rx.changed().await.is_err() {
            return;
        }
        let observed = *rx.borrow_and_update();
        if let Some(goaway) = observed {
            let _ = ev
                .send(StreamEvent::PeerGoaway {
                    error_code: goaway.error_code,
                })
                .await;
            return;
        }
    }
}

// ---------------------------------------------------------------- 回调任务

enum Callback {
    Ready,
    Event {
        kind: StreamKind,
        packet: TunnelPacket,
    },
    System {
        signal: SystemSignal,
        ack: oneshot::Sender<()>,
    },
    Closed(SessionCloseReason),
}

async fn run_callback(handler: Arc<dyn SessionHandler>, mut rx: mpsc::Receiver<Callback>) {
    while let Some(item) = rx.recv().await {
        match item {
            Callback::Ready => handler.on_ready(),
            Callback::Event { kind, packet } => handler.on_event(kind, packet),
            Callback::System { signal, ack } => {
                handler.on_system(signal);
                let _ = ack.send(());
            }
            Callback::Closed(reason) => {
                handler.on_closed(reason);
                return;
            }
        }
    }
}

// ---------------------------------------------------------------- reader/writer

async fn run_reader(mut receiver: StreamReceiver, ev: mpsc::Sender<StreamEvent>) {
    let kind = receiver.kind();
    let epoch = receiver.epoch();
    loop {
        match receiver.recv_packet().await {
            Ok(Some(packet)) => {
                let event = StreamEvent::Packet {
                    kind,
                    epoch,
                    packet,
                };
                if ev.send(event).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = ev
                    .send(StreamEvent::StreamClosed {
                        kind,
                        epoch,
                        error: None,
                    })
                    .await;
                return;
            }
            Err(error) => {
                // 把失败原因**原样按类型**交给 owner：连接级还是流级由
                // StreamError::connection_fatal 判定，reader 不做二次归类。
                let _ = ev
                    .send(StreamEvent::StreamClosed {
                        kind,
                        epoch,
                        error: Some(error),
                    })
                    .await;
                return;
            }
        }
    }
}

async fn run_writer(
    mut sender: StreamSender,
    mut ctl_rx: mpsc::Receiver<StreamCommand>,
    mut biz_rx: mpsc::Receiver<super::handle::QueuedPacket>,
    ev: mpsc::Sender<StreamEvent>,
) {
    let kind = sender.kind();
    let epoch = sender.epoch();
    let mut finish = false;
    let mut biz_open = true;
    loop {
        // biased：控制小队列优先，业务拥堵时 probe/pong/System 仍能上线。
        tokio::select! {
            biased;
            command = ctl_rx.recv() => match command {
                Some(StreamCommand::Probe(request)) => {
                    let packet = ping_packet(request.trace_id, &request.nonce);
                    if sender.send_packet(&packet).await.is_err() {
                        break;
                    }
                }
                Some(StreamCommand::Pong { trace_id, nonce }) => {
                    let packet = pong_packet(trace_id, &nonce);
                    if sender.send_packet(&packet).await.is_err() {
                        break;
                    }
                }
                Some(StreamCommand::System(packet)) => {
                    if sender.send_packet(&packet).await.is_err() {
                        break;
                    }
                }
                Some(StreamCommand::Reset(reason)) => {
                    sender.reset(reason);
                    finish = true;
                    break;
                }
                Some(StreamCommand::Finish) => {
                    finish = true;
                    break;
                }
                None => {
                    finish = true;
                    break;
                }
            },
            item = biz_rx.recv(), if biz_open => match item {
                Some(queued) => {
                    if queued.epoch != epoch {
                        // 旧代际包不得进新流。
                        continue;
                    }
                    if sender.send_packet(&queued.packet).await.is_err() {
                        break;
                    }
                }
                None => {
                    // 业务侧再无包可发：关掉这一路，继续等控制命令。
                    biz_open = false;
                }
            }
        }
    }
    if finish {
        // 半关闭：server 发 grpc-status trailers，client 发空 EOS。
        let _ = sender.finish();
    }
    // writer 侧结束是本地收尾（Finish/Reset/通道关闭），不是对端协议违规。
    let _ = ev
        .send(StreamEvent::StreamClosed {
            kind,
            epoch,
            error: None,
        })
        .await;
}

/// handler future 的兜底回报：panic/取消也必须有一条消息，否则调用方挂到超时。
struct DoneGuard {
    tx: mpsc::Sender<HandlerDone>,
    kind: StreamKind,
    epoch: StreamEpoch,
    trace_id: u64,
    action: String,
    done: bool,
}

impl Drop for DoneGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let _ = self.tx.try_send(HandlerDone {
            kind: self.kind,
            epoch: self.epoch,
            trace_id: self.trace_id,
            action: std::mem::take(&mut self.action),
            reply: Err(HandlerPanic),
        });
    }
}

// ---------------------------------------------------------------- owner

/// 未决表 value。
enum Pending {
    Business(Waiter),
    Probe,
}

impl Pending {
    fn settle(self, result: Result<Reply, RequestError>) {
        if let Self::Business(Waiter(tx)) = self {
            let _ = tx.send(result);
        }
    }
}

struct Generation {
    epoch: StreamEpoch,
    slot: Slot,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
}

struct StreamState {
    probe: ProbeState,
    generation: Option<Generation>,
    /// 已就该代际发出开流命令，正在等流出现。
    opening: bool,
    /// client：会话存续期间应始终保有该流。
    desired: bool,
}

impl StreamState {
    fn new(kind: StreamKind, epoch: StreamEpoch, now: Instant, desired: bool) -> Self {
        Self {
            probe: ProbeState::new(kind, epoch, now),
            generation: None,
            opening: false,
            desired,
        }
    }

    fn epoch(&self) -> StreamEpoch {
        self.probe.epoch()
    }
}

struct Driver {
    config: SessionConfig,
    handler: Arc<dyn SessionHandler>,
    status: Arc<SessionStatus>,
    slots: Arc<[RwLock<Option<Slot>>; 2]>,
    streams: [StreamState; 2],
    /// GOAWAY/排空控制句柄。client 额外可以请求注入 GOAWAY；server 只用它
    /// 冻结 wrapper 周期 PING 并观察出站 GOAWAY 是否真的上线。
    wire: Arc<crate::h2_keepalive::H2WireControl>,
    is_client: bool,
    /// 出站 GOAWAY 上线（写出 + flush 确认）的观察计数（latest）。
    goaway_flush: tokio::sync::watch::Receiver<u64>,
    conn_tx: mpsc::Sender<ConnCommand>,
    ev_tx: mpsc::Sender<StreamEvent>,
    done_tx: mpsc::Sender<HandlerDone>,
    notify_tx: mpsc::Sender<Callback>,
    /// 关停意图订阅：pre_work 每轮都读，select 里另有一份用于即时唤醒。
    intent_rx: tokio::sync::watch::Receiver<super::handle::ControlIntent>,
    /// 对端 GOAWAY 的结构化观察（latest）：用于连接同时结束时给出正确结论。
    peer_goaway: tokio::sync::watch::Receiver<Option<crate::h2_keepalive::PeerGoaway>>,
    attached: Vec<JoinHandle<()>>,
    /// 已 abort 但需要确认结束的附属任务。
    retired: Vec<JoinHandle<()>>,
    pending: PendingTable<Pending>,
    business_deadlines: VecDeque<Instant>,
    handlers: JoinSet<()>,
    drain_started_at: Option<Instant>,
    drain_deadline: Option<Instant>,
    drain_reason: Option<SessionCloseReason>,
    goaway_requested: bool,
    /// 本地排空：等待「GOAWAY 已上线」确认的期限。确认后才起 drain_timeout；
    /// 期限到点仍未确认 => 本端 GOAWAY 从未上线，结论必须是 Transport。
    goaway_wait: Option<Instant>,
    /// 已观察到出站 GOAWAY 的 flush 确认。
    goaway_confirmed: bool,
    goaway_failed: bool,
    streams_finished: bool,
    /// server：本连接上出现过 Control 流（Data 必须晚于它）。
    control_seen: bool,
    system_pending: Option<SystemSignal>,
    system_deadline: Option<Instant>,
    system_ack: Option<oneshot::Receiver<()>>,
    open_retry_at: Option<Instant>,
    probes_served: u32,
    probe_missions: u32,
    handler_panics: u64,
    dropped_events: u64,
    dropped_replies: u64,
    /// **整连接唯一**的业务/回包 byte 预算：两条流、所有代际共用同一份配额。
    /// 每代际新建一份等于把配置承诺的上限悄悄乘上（流数 × 重连次数）。
    reply_budget: super::handle::ByteBudget,
}

impl Driver {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: SessionConfig,
        handler: Arc<dyn SessionHandler>,
        status: Arc<SessionStatus>,
        slots: Arc<[RwLock<Option<Slot>>; 2]>,
        streams: [StreamState; 2],
        wire: Arc<crate::h2_keepalive::H2WireControl>,
        is_client: bool,
        goaway_flush: tokio::sync::watch::Receiver<u64>,
        conn_tx: mpsc::Sender<ConnCommand>,
        ev_tx: mpsc::Sender<StreamEvent>,
        done_tx: mpsc::Sender<HandlerDone>,
        notify_tx: mpsc::Sender<Callback>,
        peer_goaway: tokio::sync::watch::Receiver<Option<crate::h2_keepalive::PeerGoaway>>,
        intent_rx: tokio::sync::watch::Receiver<super::handle::ControlIntent>,
        attached: Vec<JoinHandle<()>>,
        retired: Vec<JoinHandle<()>>,
    ) -> Self {
        let pending = PendingTable::new(config.max_pending)
            .expect("validated config keeps max_pending within pending bounds");
        // 预算在连接建立时创建一次；之后每次 register 只 clone 同一个 Arc。
        let reply_budget = super::handle::ByteBudget::new(config.business_queue_bytes);
        Self {
            config,
            handler,
            status,
            slots,
            streams,
            wire,
            is_client,
            goaway_flush,
            conn_tx,
            ev_tx,
            done_tx,
            notify_tx,
            peer_goaway,
            intent_rx,
            attached,
            retired,
            pending,
            business_deadlines: VecDeque::new(),
            handlers: JoinSet::new(),
            drain_started_at: None,
            drain_deadline: None,
            drain_reason: None,
            goaway_requested: false,
            goaway_wait: None,
            goaway_confirmed: false,
            goaway_failed: false,
            streams_finished: false,
            control_seen: false,
            system_pending: None,
            system_deadline: None,
            system_ack: None,
            open_retry_at: None,
            probes_served: 0,
            probe_missions: 0,
            handler_panics: 0,
            dropped_events: 0,
            dropped_replies: 0,
            reply_budget,
        }
    }

    fn stream(&self, kind: StreamKind) -> &StreamState {
        &self.streams[index_of(kind)]
    }

    fn stream_mut(&mut self, kind: StreamKind) -> &mut StreamState {
        &mut self.streams[index_of(kind)]
    }

    // -------------------------------------------------- 每轮必做的巡检

    fn pre_work(&mut self, now: Instant) -> Option<SessionCloseReason> {
        // 关停意图最先处理：它不经过业务队列，因此也不受业务背压影响。
        match self.status.take_intent() {
            super::handle::ControlIntent::Abort => return Some(SessionCloseReason::Abort),
            super::handle::ControlIntent::Shutdown => {
                self.begin_drain(SessionCloseReason::Local, now);
            }
            super::handle::ControlIntent::None => {}
        }
        self.expire_business(now);
        if let Err(reason) = self.tick_probes(now) {
            return Some(reason);
        }
        // 收割完成的 handler 任务：结果已经走 done 通道，这里只回收 JoinSet 预算。
        while self.handlers.try_join_next().is_some() {}
        if let Some(reason) = self.settle_goaway_anchor(now) {
            return Some(reason);
        }
        if let Some(deadline) = self.drain_deadline
            && now >= deadline
        {
            let reason = self
                .drain_reason
                .take()
                .unwrap_or(SessionCloseReason::Local);
            // 本端 GOAWAY 没能真正上线时，不得声称是本地优雅排空结束。
            return Some(
                if self.goaway_failed && reason == SessionCloseReason::Local {
                    SessionCloseReason::Transport
                } else {
                    reason
                },
            );
        }
        if self.system_pending.is_some() {
            // 通知已按序送达回调任务，或宽限到点：两者都推进到关闭。
            let notified = match &mut self.system_ack {
                Some(rx) => matches!(rx.try_recv(), Ok(())),
                None => true,
            };
            if notified {
                return Some(self.take_system_reason());
            }
            if let Some(deadline) = self.system_deadline
                && now >= deadline
            {
                return Some(self.take_system_reason());
            }
        }
        self.ensure_open(now);
        None
    }

    /// 本地排空的计时锚点：只有「GOAWAY 整帧写出且底层 flush 已确认」之后才起
    /// `drain_timeout`；等待确认本身也是有界的。
    ///
    /// 返回 `Some(reason)` 表示必须立刻收场——GOAWAY 永远上不去了（适配器终态，
    /// 或写方向被按住到期限），此时绝不能声称「本地优雅排空」。
    fn settle_goaway_anchor(&mut self, now: Instant) -> Option<SessionCloseReason> {
        if let Some(deadline) = self.goaway_wait {
            if self.goaway_confirmed {
                // 真正上线的一刻才是排空起点。
                self.goaway_wait = None;
                self.drain_started_at = Some(now);
                self.drain_deadline = Some(now + self.config.drain_timeout);
            } else if now >= deadline {
                self.goaway_wait = None;
                self.goaway_failed = true;
            }
        }
        if self.goaway_failed && self.drain_deadline.is_none() {
            let reason = self
                .drain_reason
                .take()
                .unwrap_or(SessionCloseReason::Local);
            return Some(if reason == SessionCloseReason::Local {
                SessionCloseReason::Transport
            } else {
                reason
            });
        }
        None
    }

    fn take_system_reason(&mut self) -> SessionCloseReason {
        match self.system_pending.take() {
            Some(SystemSignal::Kick) => SessionCloseReason::Kicked,
            Some(SystemSignal::Revoke) => SessionCloseReason::Revoked,
            None => SessionCloseReason::Protocol,
        }
    }

    /// client 需要但当前没有的流：Control 先于 Data；排空期不再新建。
    fn ensure_open(&mut self, now: Instant) {
        self.open_retry_at = None;
        if self.status.is_draining() {
            return;
        }
        for kind in kinds() {
            let control_live = self.stream(StreamKind::Control).generation.is_some();
            let state = self.stream(kind);
            if !state.desired || state.opening || state.generation.is_some() {
                continue;
            }
            if kind == StreamKind::Data && !control_live {
                // 初连顺序：Control 先落地，再开 Data。
                continue;
            }
            let epoch = state.epoch();
            if self
                .conn_tx
                .try_send(ConnCommand::Open { kind, epoch })
                .is_ok()
            {
                self.stream_mut(kind).opening = true;
            } else {
                // 命令通道暂时不可用：退避后重试，避免 owner 空转。
                self.open_retry_at = Some(now + OPEN_RETRY_BACKOFF);
            }
        }
    }

    fn expire_business(&mut self, now: Instant) {
        while let Some(front) = self.business_deadlines.front() {
            if *front > now {
                break;
            }
            self.business_deadlines.pop_front();
        }
        for entry in self.pending.expire(now) {
            entry.settle(Err(RequestError::Timeout));
        }
    }

    fn next_timer(&self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut bump = |at: Instant| next = Some(next.map_or(at, |prev| prev.min(at)));
        for state in &self.streams {
            if let Some(at) = state.probe.next_deadline() {
                bump(at);
            }
        }
        if let Some(front) = self.business_deadlines.front() {
            bump(*front);
        }
        if let Some(at) = self.drain_deadline {
            bump(at);
        }
        if let Some(at) = self.goaway_wait {
            bump(at);
        }
        if let Some(at) = self.system_deadline {
            bump(at);
        }
        if let Some(at) = self.open_retry_at {
            bump(at);
        }
        // 没有可推进的期限时不返回「立刻」：否则 select 分支会空转饿死其他任务。
        Some(next.filter(|at| *at > now).unwrap_or(now + IDLE_TICK))
    }

    // -------------------------------------------------- 探测

    fn tick_probes(&mut self, now: Instant) -> Result<(), SessionCloseReason> {
        for kind in kinds() {
            let decision = {
                let state = self.stream_mut(kind);
                if !state.probe.next_deadline().is_some_and(|at| now >= at) {
                    continue;
                }
                state.probe.tick(now)
            };
            match decision {
                ProbeDecision::Idle => {}
                ProbeDecision::Send(request) => {
                    let ctl = match self.stream(kind).generation.as_ref() {
                        Some(slot_owner) if slot_owner.epoch == request.epoch => {
                            slot_owner.slot.ctl_tx.clone()
                        }
                        _ => {
                            // 该代际已无流：不登记也不发，窗口由 ProbeState 计 miss。
                            continue;
                        }
                    };
                    let key = RequestKey {
                        trace_id: request.trace_id,
                        stream: kind,
                        epoch: request.epoch,
                        action: PING_ACTION.to_string(),
                    };
                    if self
                        .pending
                        .insert_probe(key, request.deadline, request.nonce, Pending::Probe)
                        .is_err()
                    {
                        self.probe_missions += 1;
                        continue;
                    }
                    if ctl.try_send(StreamCommand::Probe(request)).is_err() {
                        self.probe_missions += 1;
                    }
                }
                ProbeDecision::Reopen { epoch } => self.reopen(kind, epoch, now),
                ProbeDecision::CloseConnection => {
                    return Err(SessionCloseReason::StreamFlapping);
                }
            }
        }
        Ok(())
    }

    /// 单流重开：先结清旧代际条目与任务，再由 `ensure_open` 以新代际重开。
    fn reopen(&mut self, kind: StreamKind, epoch: StreamEpoch, now: Instant) {
        self.teardown(kind, epoch, Some(RequestError::StreamClosed));
        {
            let state = self.stream_mut(kind);
            state.opening = false;
        }
        self.status.clear_ready(kind);
        self.update_state();
        if self.status.is_draining() {
            return;
        }
        let _ = now;
    }

    // -------------------------------------------------- 入站

    fn on_opened(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        result: Result<(StreamSender, StreamReceiver), StreamError>,
        now: Instant,
    ) -> Option<SessionCloseReason> {
        match result {
            Ok((sender, receiver)) => self.register(kind, epoch, sender, receiver, now).err(),
            Err(_) => {
                // 开流失败不改判：15s 开流窗口到点由 ProbeState 计一次重开失败。
                None
            }
        }
    }

    fn register(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        sender: StreamSender,
        receiver: StreamReceiver,
        now: Instant,
    ) -> Result<(), SessionCloseReason> {
        let opened = {
            let state = self.stream_mut(kind);
            state.opening = false;
            if state.epoch() != epoch {
                // 迟到/超前代际：直接销毁两半，不影响当前代际。
                drop((sender, receiver));
                return Ok(());
            }
            state.probe.on_opened(epoch, now)
        };
        if !opened {
            drop((sender, receiver));
            return Ok(());
        }
        let (biz_tx, biz_rx) = mpsc::channel(self.config.business_queue);
        let (ctl_tx, ctl_rx) = mpsc::channel(2);
        // 同一连接的预算只有一份：重开/换流只是再借同一个 Arc，绝不重新发配额。
        let budget = self.reply_budget.clone();
        let slot = Slot {
            epoch,
            biz_tx,
            ctl_tx,
            budget,
        };
        let writer = tokio::spawn(run_writer(sender, ctl_rx, biz_rx, self.ev_tx.clone()));
        let reader = tokio::spawn(run_reader(receiver, self.ev_tx.clone()));
        {
            let mut guard = self.write_slot(kind);
            *guard = Some(slot.clone());
        }
        self.stream_mut(kind).generation = Some(Generation {
            epoch,
            slot,
            reader,
            writer,
        });
        Ok(())
    }

    /// 已观察到的对端 GOAWAY error code（没有则 None）。
    fn peer_goaway_code(&self) -> Option<u32> {
        self.peer_goaway
            .borrow()
            .as_ref()
            .map(|seen| seen.error_code)
    }

    fn write_slot(&self, kind: StreamKind) -> std::sync::RwLockWriteGuard<'_, Option<Slot>> {
        match self.slots[index_of(kind)].write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn read_slot(&self, kind: StreamKind) -> Option<Slot> {
        match self.slots[index_of(kind)].read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn on_stream_closed(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        error: Option<StreamError>,
        now: Instant,
    ) -> Option<SessionCloseReason> {
        let current = self
            .stream(kind)
            .generation
            .as_ref()
            .map(|slot_owner| slot_owner.epoch);
        if current != Some(epoch) {
            // 旧代际的迟到上报：只结清自己的条目。
            for entry in self.pending.close_stream(kind, epoch) {
                entry.settle(Err(RequestError::StreamClosed));
            }
            return None;
        }
        if error.is_some_and(|error| error.connection_fatal()) {
            // 只有结构性路由违规（§6.3：System 不在控制流上）才终结整条连接。
            self.teardown(kind, epoch, Some(RequestError::StreamClosed));
            self.status.clear_ready(kind);
            self.update_state();
            return Some(SessionCloseReason::Protocol);
        }
        // 其余一切（含 StreamError::StreamProtocol：本流应用包非法，如 §6.7.2 的
        // 非 16B 探测请求）都只终结本流：RST 该流、结清本代际未决、按 ProbeState 重开。
        let decision = if self.status.is_draining() {
            // 排空期不重开任何流。
            None
        } else {
            Some(self.stream_mut(kind).probe.on_reset(epoch, now))
        };
        self.teardown(kind, epoch, Some(RequestError::StreamClosed));
        self.status.clear_ready(kind);
        self.update_state();
        match decision {
            Some(ProbeDecision::Reopen { epoch }) => {
                self.reopen(kind, epoch, now);
                None
            }
            Some(ProbeDecision::CloseConnection) => Some(SessionCloseReason::StreamFlapping),
            _ => None,
        }
    }

    fn on_packet(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        packet: TunnelPacket,
        now: Instant,
    ) -> Option<SessionCloseReason> {
        match packet.packet_type {
            PacketType::Response => {
                self.on_response(kind, epoch, packet, now);
                None
            }
            PacketType::Request => {
                if packet.action == PING_ACTION {
                    // §6.7.2 接收端职责：核验类型、action、字段约束与**当前流代际**。
                    // 违规只按流级协议错误终结本流——既不升级成连接级结论，也绝不
                    // 给非法/迟到的探测代答 pong 或计入已服务探测。
                    if !self.probe_request_current(kind, epoch, &packet) {
                        return self.fail_stream(kind, epoch);
                    }
                    // 探测在接收端立即回 pong，且不进业务 handler（§6.7.2）。
                    self.probes_served += 1;
                    self.send_pong(kind, &packet);
                    return None;
                }
                self.dispatch_request(kind, epoch, packet);
                None
            }
            PacketType::Event => {
                if self
                    .notify_tx
                    .try_send(Callback::Event { kind, packet })
                    .is_err()
                {
                    self.dropped_events += 1;
                }
                None
            }
            PacketType::System => {
                // TYPE_SYSTEM 只允许出现在 Control 流（§6 路由规则）。reader 侧
                // `packet.validate(kind)` 已按同一规则把它归为连接级（StreamError::
                // Protocol → connection_fatal），这里的分支是**已解码路由**上的纵深
                // 防御，不引入第二套分类。
                if kind != StreamKind::Control {
                    return Some(SessionCloseReason::Protocol);
                }
                let Some(signal) = SystemSignal::from_action(&packet.action) else {
                    // 未知 System：**当前公共 decode 不会让它到这里**（decode 的
                    // validate_fields 先拒，归流级 StreamProtocol）。留着只为「已解码
                    // 但路由非法」这一防御性前提兜底：不执行、不刷新健康。
                    return Some(SessionCloseReason::Protocol);
                };
                if self.stream(kind).generation.is_none() {
                    return Some(SessionCloseReason::Protocol);
                }
                self.notify_system(signal);
                None
            }
        }
    }

    /// 入站探测请求是否可代答：字段约束（§6.7.2：恰好 16B nonce、无 metadata、
    /// 非 0 trace）+ 必须来自本代际的流。
    ///
    /// 公共 `TunnelPacket::decode` 已经拒绝字段违规的探测；这里再按 §6.7.2 复核一次
    /// 是接收端自身的职责（不依赖上游恰好先拒），且**不放宽**任何既有校验。
    fn probe_request_current(
        &self,
        kind: StreamKind,
        epoch: StreamEpoch,
        packet: &TunnelPacket,
    ) -> bool {
        if packet.packet_type != PacketType::Request
            || packet.payload.len() != PROBE_NONCE_LEN
            || !packet.metadata.is_empty()
            || packet.trace_id == 0
        {
            return false;
        }
        matches!(
            self.stream(kind).generation.as_ref(),
            Some(current) if current.epoch == epoch
        )
    }

    /// 只终结本流的协议错误：RST/销毁该代际两半、结清本代际未决一次、按 ProbeState
    /// 走既有重开路径。绝不返回连接级结论（那是 `connection_fatal` 的专属）。
    fn fail_stream(&mut self, kind: StreamKind, epoch: StreamEpoch) -> Option<SessionCloseReason> {
        if self.status.is_draining() {
            // 排空期不重开：只结清本流未决。
            self.teardown(kind, epoch, Some(RequestError::StreamClosed));
            return None;
        }
        let decision = self.stream_mut(kind).probe.on_reset(epoch, Instant::now());
        self.teardown(kind, epoch, Some(RequestError::StreamClosed));
        self.status.clear_ready(kind);
        self.update_state();
        match decision {
            ProbeDecision::Reopen { epoch } => {
                self.reopen(kind, epoch, Instant::now());
                None
            }
            // 沿用既有 ProbeState 升级路径：反复失败仍按 StreamFlapping 收场。
            ProbeDecision::CloseConnection => Some(SessionCloseReason::StreamFlapping),
            _ => None,
        }
    }

    fn on_response(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        packet: TunnelPacket,
        now: Instant,
    ) {
        let Some(matched) = self.pending.match_response(&packet, kind, epoch, now) else {
            return; // 未知/迟到/错代际/错 action/错 probe：静默丢弃
        };
        match matched.kind {
            PendingKind::Business => {
                matched.value.settle(Ok(reply_from_packet(&packet)));
            }
            PendingKind::Probe { nonce } => {
                // 先由 PendingTable 认定这是本窗口的正确 probe，才刷新健康。
                let accepted =
                    self.stream_mut(kind)
                        .probe
                        .accept_pong(packet.trace_id, &nonce, epoch, now);
                if accepted {
                    self.status.mark_ready(kind, epoch);
                    self.update_state();
                }
            }
        }
    }

    fn send_pong(&mut self, kind: StreamKind, packet: &TunnelPacket) {
        let ctl = match self.stream(kind).generation.as_ref() {
            Some(slot_owner) => slot_owner.slot.ctl_tx.clone(),
            None => return,
        };
        // 控制队列满（只可能是正在拆除）：丢弃 pong，等下一次窗口。
        let _ = ctl.try_send(StreamCommand::Pong {
            trace_id: packet.trace_id,
            nonce: packet.payload.clone(),
        });
    }

    fn update_state(&mut self) {
        if self.status.is_draining() {
            self.status.set(SessionState::Draining);
            return;
        }
        let previous = *self.status.state.borrow();
        if matches!(previous, SessionState::Closed | SessionState::Draining) {
            return;
        }
        if self.status.both_ready() {
            self.status.set(SessionState::Ready);
            if previous != SessionState::Ready {
                // on_ready 每轮 Ready 只通知一次。
                let _ = self.notify_tx.try_send(Callback::Ready);
            }
        } else {
            self.status.set(SessionState::Connecting);
        }
    }

    // -------------------------------------------------- 出站

    fn enqueue(
        &mut self,
        stream: StreamKind,
        packet: TunnelPacket,
        deadline: Instant,
        waiter: Option<Waiter>,
        ack: oneshot::Sender<Result<(), RequestError>>,
    ) {
        if self.status.is_draining() {
            return reject(ack, RequestError::Closed);
        }
        let Some(slot) = self.read_slot(stream) else {
            return reject(ack, RequestError::StreamClosed);
        };
        if waiter.is_some() && Instant::now() >= deadline {
            return reject(ack, RequestError::Timeout);
        }
        if packet.validate(stream).is_err() {
            return reject(ack, RequestError::ProtocolMismatch);
        }
        // 入队前先用精确的 proto3 长度做信封门禁：超限包不得上线，
        // 否则 writer 编码失败会把整条流带下去。
        if packet.serialized_len().is_err() {
            return reject(ack, RequestError::ProtocolMismatch);
        }
        // 预留 2 个 probe 名额：业务最多 max_pending - 2。
        if waiter.is_some() && self.pending.len() >= self.config.max_pending - PROBE_RESERVATION {
            return reject(ack, RequestError::Busy);
        }
        let Some(_guard) = slot
            .budget
            .try_reserve(SessionConfig::queued_bytes(packet.payload.len()))
        else {
            return reject(ack, RequestError::Busy);
        };
        let epoch = slot.epoch;
        let queued = super::handle::QueuedPacket {
            packet: packet.clone(),
            epoch,
            _budget: _guard,
        };
        if slot.biz_tx.try_send(queued).is_err() {
            return reject(ack, RequestError::Busy);
        }
        if let Some(waiter) = waiter {
            let key = RequestKey {
                trace_id: packet.trace_id,
                stream,
                epoch,
                action: packet.action.clone(),
            };
            if self
                .pending
                .insert_business(key, deadline, Pending::Business(waiter))
                .is_err()
            {
                // 包已上线但登记失败：调用方得到 Busy，结果未知且不重放。
                return reject(ack, RequestError::Busy);
            }
            self.business_deadlines.push_back(deadline);
        }
        let _ = ack.send(Ok(()));
    }

    /// System 走控制小队列：业务写阻塞时 kick/revoke 仍能上线。
    fn send_system(
        &mut self,
        packet: TunnelPacket,
        ack: oneshot::Sender<Result<(), RequestError>>,
    ) {
        let Some(slot) = self.read_slot(StreamKind::Control) else {
            return reject(ack, RequestError::StreamClosed);
        };
        if packet.packet_type != PacketType::System
            || SystemSignal::from_action(&packet.action).is_none()
            || !packet.payload.is_empty()
        {
            return reject(ack, RequestError::ProtocolMismatch);
        }
        if slot.ctl_tx.try_send(StreamCommand::System(packet)).is_err() {
            return reject(ack, RequestError::Busy);
        }
        let _ = ack.send(Ok(()));
    }

    /// System 入站：先按序通知 handler，再整连接关闭（kick/revoke 不受排空限制）。
    fn notify_system(&mut self, signal: SystemSignal) {
        if self.system_pending.is_some() {
            return;
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        self.system_pending = Some(signal);
        self.system_deadline = Some(Instant::now() + SYSTEM_GRACE);
        self.system_ack = Some(ack_rx);
        // 与 on_event 共用同一条有序回调队列：通知一定排在关闭之前。
        if self
            .notify_tx
            .try_send(Callback::System {
                signal,
                ack: ack_tx,
            })
            .is_err()
        {
            // 回调通道已不可用：不再等待确认，由期限推进关闭。
            self.system_ack = None;
        }
    }

    /// server 入站流路由：初连 Data-first 拒绝、未知路径交给 streams 层回
    /// grpc-status 12、同类新流顶替旧代际（RST + 作废 epoch）。
    fn route_incoming(
        &mut self,
        request: IncomingRequest,
        mut response: IncomingResponse,
        now: Instant,
    ) -> Option<SessionCloseReason> {
        let path = request.uri().path();
        let kind = match path {
            CONTROL_PATH => StreamKind::Control,
            DATA_PATH => StreamKind::Data,
            _ => {
                // 未知路径由 accept_server_stream 统一回 12，本端不建流也不刷新健康。
                let epoch = self.stream(StreamKind::Control).epoch();
                let _ = accept_server_stream(request, response, epoch);
                return None;
            }
        };
        if kind == StreamKind::Data && !self.control_seen {
            // 初连顺序：Control 之后才允许 Data（§6.6）。
            response.send_reset(h2::Reason::PROTOCOL_ERROR);
            return None;
        }
        if self.status.is_draining() {
            // 排空期不接纳新流。
            response.send_reset(h2::Reason::REFUSED_STREAM);
            return None;
        }
        let epoch = if self.stream(kind).generation.is_some() {
            // 同类顶替：RST 旧流 + 结清旧代际，再用 ProbeState 推进的新代际接纳。
            let old = self.stream(kind).epoch();
            self.rst(kind, h2::Reason::NO_ERROR);
            let decision = self.stream_mut(kind).probe.on_reset(old, now);
            self.teardown(kind, old, Some(RequestError::StreamClosed));
            self.status.clear_ready(kind);
            self.update_state();
            match decision {
                ProbeDecision::Reopen { epoch } => epoch,
                ProbeDecision::CloseConnection => {
                    response.send_reset(h2::Reason::NO_ERROR);
                    return Some(SessionCloseReason::StreamFlapping);
                }
                _ => {
                    response.send_reset(h2::Reason::NO_ERROR);
                    return None;
                }
            }
        } else {
            self.stream(kind).epoch()
        };
        match accept_server_stream(request, response, epoch) {
            Ok(Some((accepted, sender, receiver))) => {
                if accepted == StreamKind::Control {
                    self.control_seen = true;
                }
                let _ = self.register(accepted, epoch, sender, receiver, now);
                None
            }
            Ok(None) => None,
            Err(_) => Some(SessionCloseReason::Protocol),
        }
    }

    /// 对当前代际流显式发 RST_STREAM（服务端只发 RST，等 client 重开）。
    fn rst(&mut self, kind: StreamKind, reason: h2::Reason) {
        if let Some(slot_owner) = self.stream(kind).generation.as_ref() {
            let _ = slot_owner
                .slot
                .ctl_tx
                .try_send(StreamCommand::Reset(reason));
        }
    }

    fn dispatch_request(&mut self, kind: StreamKind, epoch: StreamEpoch, packet: TunnelPacket) {
        if self.handlers.len() >= self.config.handler_concurrency {
            // 并发预算耗尽：立即回忙，不排队（§6.7.2 接收端处理队列必须有限）。
            let reply = Reply {
                status_code: OVERLOAD_STATUS,
                error_message: OVERLOAD_MESSAGE.to_string(),
                payload: Bytes::new(),
            };
            self.send_reply(kind, epoch, packet.trace_id, &packet.action, &reply);
            return;
        }
        let trace_id = packet.trace_id;
        let action = packet.action.clone();
        let future = self.handler.on_request(super::handle::RequestContext {
            stream: kind,
            epoch,
            packet,
        });
        let tx = self.done_tx.clone();
        self.handlers.spawn(async move {
            let mut guard = DoneGuard {
                tx: tx.clone(),
                kind,
                epoch,
                trace_id,
                action: action.clone(),
                done: false,
            };
            let reply = future.await;
            guard.done = true;
            let _ = tx
                .send(HandlerDone {
                    kind,
                    epoch,
                    trace_id,
                    action,
                    reply: Ok(reply),
                })
                .await;
        });
    }

    fn on_handler_done(&mut self, done: HandlerDone) {
        match done.reply {
            Ok(reply) => {
                self.send_reply(done.kind, done.epoch, done.trace_id, &done.action, &reply);
            }
            Err(_) => {
                self.handler_panics += 1;
                let reply = Reply {
                    status_code: PANIC_STATUS,
                    error_message: PANIC_MESSAGE.to_string(),
                    payload: Bytes::new(),
                };
                self.send_reply(done.kind, done.epoch, done.trace_id, &done.action, &reply);
            }
        }
    }

    fn send_reply(
        &mut self,
        kind: StreamKind,
        epoch: StreamEpoch,
        trace_id: u64,
        action: &str,
        reply: &Reply,
    ) {
        // 旧代际 handler 的回包不得进入新流；直接丢弃，由对端期限自行结清。
        let Some(slot) = self.read_slot(kind) else {
            return;
        };
        if slot.epoch != epoch {
            return;
        }
        let mut packet = reply.to_packet(trace_id, action);
        if packet.status_code == 0 && !packet.error_message.is_empty() {
            // 协议规定 status_code=0 不得携带 error_message：清空而非丢包。
            packet.error_message.clear();
        }
        let Some(_guard) = slot
            .budget
            .try_reserve(SessionConfig::queued_bytes(packet.payload.len()))
        else {
            // 背压是真的丢了回包：如实计数（sanitized，不带任何对端文本），
            // 绝不伪造「已发送」。
            self.dropped_replies = self.dropped_replies.saturating_add(1);
            return;
        };
        if slot
            .biz_tx
            .try_send(super::handle::QueuedPacket {
                packet,
                epoch,
                _budget: _guard,
            })
            .is_err()
        {
            self.dropped_replies = self.dropped_replies.saturating_add(1);
        }
    }

    // -------------------------------------------------- 关停

    /// 排空入口：幂等，且**不**等待任何业务响应完成（RFC9113 §6.8）。
    ///
    /// 计时口径（本端发起 vs 对端发起）：
    /// * 对端发起（`PeerGoaway`/带码 GOAWAY）：GOAWAY 已在对端上线并被本端**观察到**，
    ///   因此以观察到的此刻起算 `drain_timeout`；
    /// * 本端发起（`Local`）：请求发 GOAWAY **不是**排空起点。起点是 GOAWAY 整帧
    ///   写出且底层 flush 得到确认的那一刻（`settle_goaway_anchor`）；确认之前的
    ///   等待同样有界，超时即按 Transport 收场，绝不谎报 Local。
    fn begin_drain(&mut self, reason: SessionCloseReason, now: Instant) {
        self.status.begin_drain();
        self.status.set(SessionState::Draining);
        if self.drain_started_at.is_none() {
            self.drain_started_at = Some(now);
            if reason != SessionCloseReason::Local {
                // 对端发起：锚点就是「看到 GOAWAY」。
                self.drain_deadline = Some(now + self.config.drain_timeout);
            }
        }
        if self.drain_reason.is_none() {
            self.drain_reason = Some(reason);
        }
        // 「是否已排空」不能用共享状态判定：graceful_shutdown 会在句柄侧同步
        // 置位，让订阅方立刻看到 Draining；owner 侧的幂等只能看自己的记账。
        if self.goaway_requested {
            return;
        }
        // 冻结 wrapper 周期 PING（h2 内部 SHUTDOWN PING 仍放行）与两条流的 probe。
        let _ = self.wire.begin_drain();
        if self.is_client {
            // client：由适配器注入 GOAWAY（last_stream_id 语义不变）。
            self.goaway_requested = self.wire.request_client_goaway().is_ok();
            let wire = Arc::clone(&self.wire);
            let ev = self.ev_tx.clone();
            // 独立任务观察适配器终态：owner 绝不能 inline 等它（自锁）。
            self.attached.retain(|handle| !handle.is_finished());
            self.attached.push(tokio::spawn(async move {
                let ok = wire.wait_goaway_sent().await.is_ok();
                let _ = ev.send(StreamEvent::GoawaySent { ok }).await;
            }));
        } else {
            // server：GOAWAY 由 h2 官方 graceful 写出（本组件不替它写）。
            self.goaway_requested = true;
            let tx = self.conn_tx.clone();
            self.attached.retain(|handle| !handle.is_finished());
            self.attached.push(tokio::spawn(async move {
                let _ = tx.send(ConnCommand::Graceful).await;
            }));
        }
        if self.drain_deadline.is_none() {
            // 本端发起：等待「已上线」确认，等待本身有界。
            self.goaway_wait = Some(now + self.config.drain_timeout);
        }
        for kind in kinds() {
            self.stream_mut(kind).probe.begin_drain();
            self.stream_mut(kind).desired = false;
        }
    }

    fn settle_all(&mut self, error: RequestError) {
        for entry in self.pending.close_all() {
            entry.settle(Err(error));
        }
        self.business_deadlines.clear();
    }

    fn finish_streams(&mut self) {
        if self.streams_finished {
            return;
        }
        self.streams_finished = true;
        for kind in kinds() {
            if let Some(slot_owner) = self.stream(kind).generation.as_ref() {
                let _ = slot_owner.slot.ctl_tx.try_send(StreamCommand::Finish);
            }
        }
    }

    /// 统一清理：两半、任务、通道。返回是否确认无残留。
    async fn cleanup(&mut self) -> bool {
        for kind in kinds() {
            let epoch = self.stream(kind).epoch();
            self.teardown(kind, epoch, None);
        }
        // handler 任务：abort 后必须被再次 poll 才算结束，
        // 因此先取出 JoinSet，在自己的任务里等它排空。
        let mut handlers = std::mem::take(&mut self.handlers);
        handlers.abort_all();
        let handlers_clean = tokio::time::timeout(CLEANUP_GRACE, async move {
            while handlers.join_next().await.is_some() {}
        })
        .await
        .is_ok();
        for handle in self.attached.iter() {
            handle.abort();
        }
        // abort 只在下一次被 poll 时生效：这里持续交出调度权，
        // cleaned 因此是「确认无残留」而不是「已发出取消」。
        let deadline = Instant::now() + CLEANUP_GRACE;
        loop {
            let streams_clean = self.streams.iter().all(|state| state.generation.is_none());
            let retired_clean = self.retired.iter().all(JoinHandle::is_finished);
            let attached_clean = self.attached.iter().all(JoinHandle::is_finished);
            if handlers_clean && streams_clean && retired_clean && attached_clean {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(CLEANUP_POLL).await;
        }
    }

    /// 销毁某代际流：结清该代际未决一次、停两半与两任务、摘掉共享槽。
    fn teardown(&mut self, kind: StreamKind, epoch: StreamEpoch, settle: Option<RequestError>) {
        {
            let mut guard = self.write_slot(kind);
            if guard.as_ref().is_some_and(|slot| slot.epoch == epoch) {
                let slot = std::mem::take(&mut *guard);
                drop(slot);
            }
        }
        let generation = std::mem::take(&mut self.stream_mut(kind).generation);
        if let Some(generation) = generation {
            generation.writer.abort();
            generation.reader.abort();
            // 退役任务留在观察表里：cleanup 必须确认它们真的结束，
            // 而不是「已经发出 abort」就当作清理完成。
            // 表本身只保留「还没结束」的句柄：已 poll 到完成的直接摘掉，
            // 否则长期重连会让它按 (2 × 重连次数) 无界增长；未确认结束的
            // 句柄绝不丢出表外（丢=脱离追踪，等于 detach）。
            prune_finished(&mut self.retired);
            self.retired.push(generation.writer);
            self.retired.push(generation.reader);
        }
        if let Some(error) = settle {
            for entry in self.pending.close_stream(kind, epoch) {
                entry.settle(Err(error));
            }
        }
    }

    fn outcome(&mut self, reason: SessionCloseReason, cleaned: bool) -> SessionOutcome {
        SessionOutcome {
            reason,
            probes_served: self.probes_served,
            probe_missions: self.probe_missions,
            handler_panics: self.handler_panics,
            dropped_events: self.dropped_events,
            dropped_replies: self.dropped_replies,
            cleaned,
        }
    }
}

// ---------------------------------------------------------------- owner 主循环

#[allow(clippy::too_many_arguments)]
async fn run_driver(
    mut driver: Driver,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    mut system_rx: mpsc::Receiver<SystemCommand>,
    mut ev_rx: mpsc::Receiver<StreamEvent>,
    mut done_rx: mpsc::Receiver<HandlerDone>,
    hold_tx: oneshot::Sender<()>,
    callback: CallbackTracker<'static>,
    callback_done: oneshot::Receiver<()>,
) -> SessionOutcome {
    // 事件循环里的四个接收端都是本函数局部变量：owner 状态在 `driver` 里，
    // 这样 select! 的分支体可以对 driver 做可变借用。
    let mut intent_rx = driver.intent_rx.clone();
    let mut goaway_flush = driver.goaway_flush.clone();
    let reason = loop {
        let now = Instant::now();
        if let Some(reason) = driver.pre_work(now) {
            break reason;
        }
        let timer = driver.next_timer(now).unwrap_or(now);
        tokio::select! {
            biased;
            Some(command) = cmd_rx.recv() => match command {
                SessionCommand::Enqueue { stream, packet, deadline, waiter, ack } => {
                    driver.enqueue(stream, packet, deadline, Some(waiter), ack);
                }
                SessionCommand::SendEvent { stream, packet, ack } => {
                    driver.enqueue(stream, packet, Instant::now(), None, ack);
                }
            },
            Some(command) = system_rx.recv() => {
                driver.send_system(command.packet, command.ack);
            },
            Some(event) = ev_rx.recv() => {
                let now = Instant::now();
                match event {
                    StreamEvent::Incoming { request, response } => {
                        if let Some(reason) = driver.route_incoming(request, response, now) {
                            break reason;
                        }
                    }
                    StreamEvent::Opened { kind, epoch, result } => {
                        if let Some(reason) = driver.on_opened(kind, epoch, result, now) {
                            break reason;
                        }
                    }
                    StreamEvent::Packet { kind, epoch, packet } => {
                        if let Some(reason) = driver.on_packet(kind, epoch, packet, now) {
                            break reason;
                        }
                    }
                    StreamEvent::StreamClosed { kind, epoch, error } => {
                        if let Some(reason) = driver.on_stream_closed(kind, epoch, error, now) {
                            break reason;
                        }
                    }
                    StreamEvent::PeerGoaway { error_code } => {
                        // 只有 error_code=0 才算对端优雅排空；带错误码的 GOAWAY
                        // 同样进入排空，但结论记为传输故障。
                        let reason = if error_code == 0 {
                            SessionCloseReason::PeerGoaway
                        } else {
                            SessionCloseReason::Transport
                        };
                        driver.begin_drain(reason, now);
                    }
                    StreamEvent::ConnClosed { reason } => {
                        if driver.status.is_draining() {
                            // h2 已收尾：继续走完排空期限，不提前销毁。
                        } else if let Some(code) = driver.peer_goaway_code() {
                            // 对端 GOAWAY 让 h2 连接同时收尾：两条事件几乎同时到，
                            // 这里按「对端优雅排空」定性，不得谎报成传输故障。
                            let drain = if code == 0 {
                                SessionCloseReason::PeerGoaway
                            } else {
                                SessionCloseReason::Transport
                            };
                            driver.begin_drain(drain, now);
                        } else {
                            break reason;
                        }
                    }
                    StreamEvent::GoawaySent { ok } => {
                        // 适配器进入终态：GOAWAY 再也没机会上线，摘掉等待，
                        // 由 pre_work 立刻按非 Local 收场。
                        driver.goaway_failed = !ok;
                        if !ok {
                            driver.goaway_wait = None;
                        }
                    }
                }
            },
            Some(done) = done_rx.recv() => driver.on_handler_done(done),
            // 出站 GOAWAY 的 flush 确认：排空计时到这一刻才起算。
            _ = goaway_flush.changed() => {
                let _ = goaway_flush.borrow_and_update();
                driver.goaway_confirmed = true;
            },
            // 关停意图走独立 watch：业务队列打满时也立刻醒。
            _ = intent_rx.changed() => {},
            _ = sleep_until(timer) => {}
        }
    };
    finish(driver, reason, hold_tx, callback, callback_done).await
}

/// 收尾：结清未决 → 半关闭两流 → 通知 on_closed → 清理任务。
///
/// 阶段上界（真实值，别简写成「0.5s + 2s」）：通知投递 ≤`SYSTEM_GRACE` 0.5s，等完成
/// 信号 ≤0.5s，`cleanup()` 最坏 ~4s（handler JoinSet 排空 ≤`CLEANUP_GRACE` 2s **之后**，
/// 附属/退役句柄的确认轮询再给 ≤2s 一段），回调确认 ≤`CLEANUP_GRACE` 2s ⇒ 最坏 ≈7s；
/// 无堆积时 cleanup 一轮 1ms 就返回，典型 ≈3s。以上都再加调度余量。
/// 这里没有任何一段是「等回调跑完」：不可抢占的同步回调最多只能让 owner 等满上面的
/// 期限，之后要么被确认结束、要么带着**原句柄**转交追踪表（见 [`CallbackTracker`]）。
async fn finish(
    mut driver: Driver,
    reason: SessionCloseReason,
    hold_tx: oneshot::Sender<()>,
    callback: CallbackTracker<'static>,
    callback_done: oneshot::Receiver<()>,
) -> SessionOutcome {
    driver.status.set(SessionState::Closed);
    driver.settle_all(RequestError::Closed);
    driver.finish_streams();
    // on_closed 的「入队」与「回调返回」两步各自有界：慢回调既不能把 owner 钉在
    // 这里，也不能靠丢弃 JoinHandle 蒙混过关（丢句柄=脱离追踪）。
    // 投递结论按**真实结果**判定：`Ok(Err(..))` 是接收端已关闭（回调任务早已消亡），
    // 不是「已送达」；把它当成功会跳过后面的完成确认，让 cleaned 失去依据。
    let delivered = matches!(
        tokio::time::timeout(
            SYSTEM_GRACE,
            driver.notify_tx.send(Callback::Closed(reason))
        )
        .await,
        Ok(Ok(())),
    );
    // 等的是回调任务自己报告的完成信号，而不是 JoinHandle 本身：超时只放弃这一次
    // 等待，句柄始终挂在追踪名额上，后面还能 abort 并有界确认。
    let mut confirmed = false;
    if delivered {
        match tokio::time::timeout(SYSTEM_GRACE, callback_done).await {
            // on_closed 正常返回。
            Ok(Ok(())) => confirmed = true,
            // 任务已消亡（panic/取消）：它的 future 连同捕获的 handler 已被 drop，
            // 不会留孤儿。
            Ok(Err(_)) => confirmed = true,
            // 还在跑：下面必须 abort 并有界确认；确认不了就转交追踪表。
            Err(_elapsed) => {}
        }
    }
    // cleaned 只回答「资源是否确认无残留」。
    let mut cleaned = driver.cleanup().await;
    if confirmed {
        callback.release_confirmed();
    } else {
        // 回调任务仍在跑（handler 的同步回调不可抢占）或通知压根没送到：先请求取消，
        // 再有界确认。确认不了也**不丢弃句柄**——CallbackTracker 的 Drop 把它转交给
        // 进程级追踪表，owner 照样有界返回，`cleaned` 如实为 false。
        // 注意这不是强制取消：任务可能永远跑不完，届时它占着一个可见的名额，
        // 而进程/runtime 退出才是它的终点（外层 manager 的边界）。
        callback.abort();
        cleaned &= callback.settle(CLEANUP_GRACE).await;
    }
    drop(hold_tx);
    driver.outcome(reason, cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 名额口径：`(已预留未转交, 已转交未确认)`。
    fn slot_counts(registry: &CallbackRegistry) -> (usize, usize) {
        let reserved = registry
            .slots
            .iter()
            .filter(|slot| matches!(slot, TrackSlot::Reserved))
            .count();
        (reserved, registry.tracked())
    }

    /// 追踪名额是**硬上界**：满员后新会话必须被拒绝启动（fail closed），而不是
    /// 静默放宽或悄悄脱离追踪；正常了结当场归还名额，不依赖任何后台回收。
    #[tokio::test(start_paused = true)]
    async fn callback_track_quota_is_hard_bounded_and_returned() {
        let registry = Mutex::new(CallbackRegistry::with_capacity(2));
        let first = CallbackTracker::reserve(&registry).expect("第 1 个名额");
        let _second = CallbackTracker::reserve(&registry).expect("第 2 个名额");
        assert!(
            CallbackTracker::reserve(&registry).is_none(),
            "名额满时必须拒绝新会话启动"
        );
        assert_eq!(slot_counts(&lock_registry(&registry)), (2, 0));

        // 确认了结 → 名额立刻可复用（会话数上界不因退出而漏额）。
        first.release_confirmed();
        let third = CallbackTracker::reserve(&registry).expect("归还后可再预留");
        assert_eq!(slot_counts(&lock_registry(&registry)), (2, 0));
        third.release_confirmed();
        drop(_second);
        assert_eq!(slot_counts(&lock_registry(&registry)), (0, 0));
        let reused = CallbackTracker::reserve(&registry);
        assert!(reused.is_some(), "空表必须可再用");
    }

    /// 有界确认失败时，**原句柄**必须留在追踪表里（丢弃=detach）；任务真的结束后由
    /// 下一次 prune 回收，回收前名额仍然是满的——占用可见，但不无限增长。
    #[tokio::test(start_paused = true)]
    async fn unconfirmed_callback_handle_is_handed_over_then_reclaimed() {
        let registry = Mutex::new(CallbackRegistry::with_capacity(1));
        let (tx, rx) = oneshot::channel::<()>();
        let running = tokio::spawn(async move {
            let _ = rx.await;
        });
        let mut tracker = CallbackTracker::reserve(&registry).expect("空表可预留");
        tracker.attach(running);

        // 任务还卡在 await 上：有界确认必须如实失败。
        assert!(
            !tracker.settle(CLEANUP_POLL).await,
            "未确认结束时 settle 必须返回 false"
        );
        assert_eq!(
            slot_counts(&lock_registry(&registry)),
            (0, 1),
            "超时的原句柄必须转交追踪表，不能丢弃后当作没发生"
        );
        assert!(
            CallbackTracker::reserve(&registry).is_none(),
            "未回收前名额仍是满的（占用可见，不允许绕过）"
        );

        // 回调真的结束 → 下一次 prune 回收名额。
        drop(tx);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let reclaimed = CallbackTracker::reserve(&registry);
        assert!(reclaimed.is_some(), "任务结束后名额必须被回收");
        assert_eq!(lock_registry(&registry).tracked(), 0);
    }

    /// owner 自己被取消（连 `finish` 都没走到）时，`Drop` 兜底也必须转交句柄。
    #[tokio::test(start_paused = true)]
    async fn tracker_dropped_without_settling_still_tracks_handle() {
        let registry = Mutex::new(CallbackRegistry::with_capacity(1));
        let (tx, rx) = oneshot::channel::<()>();
        let mut tracker = CallbackTracker::reserve(&registry).expect("预留名额");
        tracker.attach(tokio::spawn(async move {
            let _ = rx.await;
        }));
        // 模拟 owner 被取消：没有任何有界确认，也没有 abort。
        drop(tracker);
        assert_eq!(
            slot_counts(&lock_registry(&registry)),
            (0, 1),
            "丢弃未确认的句柄等于 detach，必须由 Drop 转交追踪表"
        );
        drop(tx);
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            CallbackTracker::reserve(&registry).is_some(),
            "回收后名额必须可再用"
        );
    }

    /// 正常路径：确认结束的回调当场丢句柄 + 还名额，表里一个都不留，也不替已完成的
    /// 回调扣着 handler。
    #[tokio::test(start_paused = true)]
    async fn confirmed_callback_releases_its_slot_immediately() {
        let registry = Mutex::new(CallbackRegistry::with_capacity(1));
        let mut tracker = CallbackTracker::reserve(&registry).expect("预留名额");
        tracker.attach(tokio::spawn(async {}));
        tokio::task::yield_now().await;

        assert!(
            tracker.settle(CLEANUP_POLL).await,
            "已结束的任务必须被确认，从而当场归还名额"
        );
        assert_eq!(slot_counts(&lock_registry(&registry)), (0, 0));
        assert!(
            CallbackTracker::reserve(&registry).is_some(),
            "确认结束后名额必须立即可复用"
        );
    }

    /// 退役表 `retired` 只允许摘掉**已确认结束**的任务：
    /// 未结束的直接丢出表外等于「脱离」——它还会继续跑，但 owner 再也不知道它结束没结束，
    /// `cleaned` 就变成谎话。本用例把这条不变量钉住。
    #[tokio::test(start_paused = true)]
    async fn prune_finished_only_drops_confirmed_handles() {
        let finished = tokio::spawn(async {});
        let (tx, rx) = oneshot::channel::<()>();
        let unfinished = tokio::spawn(async move {
            let _ = rx.await;
        });
        // 让 finished 真的被 poll 到完成：is_finished 只在此之后才为真。
        tokio::task::yield_now().await;
        assert!(finished.is_finished(), "前置条件：该任务必须已确认结束");

        let mut retired = vec![finished, unfinished];
        prune_finished(&mut retired);

        assert_eq!(retired.len(), 1, "只允许摘掉已确认结束的任务");
        assert!(
            !retired[0].is_finished(),
            "未确认结束的任务必须留在表内，否则就成了脱离的孤儿"
        );

        drop(tx);
        for handle in retired {
            let _ = handle.await;
        }
    }

    /// 清理阶段必须**先摘退役表**再往里塞被 abort 的句柄：
    /// 表里若长期只增不减，长会话会无界堆积。
    #[tokio::test(start_paused = true)]
    async fn prune_finished_bounds_the_retired_table() {
        let mut retired: Vec<JoinHandle<()>> = Vec::new();
        for _ in 0..64 {
            retired.push(tokio::spawn(async {}));
        }
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        prune_finished(&mut retired);
        assert!(retired.is_empty(), "全部结束的退役项必须收敛到空表");
    }
}
