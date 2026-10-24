//! HTTP/2 连接级唯一 token 保活适配器（协议规范 §6.7.1）。
//!
//! 位置：`SecureIo` 之上、h2 会话之下。它对上层 h2 与下层字节都保持透明：
//! 连接前导（客户端出站 24B / 服务端入站 24B）与全部普通帧逐字转发，只在
//! **可注入帧边界**额外注入连接级 `PING`（9B 帧头 + 8B opaque token，
//! type=6、stream=0、flags=0）。可注入边界要求：前导已完整、不在任何帧的
//! 帧头/payload 中间，且**不在 header block 序列内部**——HEADERS/PUSH_PROMISE
//! 未置 `END_HEADERS` 时，后续必须紧跟 CONTINUATION 直到 END_HEADERS，中间不得
//! 插入任何其它帧（<https://www.rfc-editor.org/rfc/rfc9113.html#section-4.3>，
//! §4.3/§6.10 的 MUST NOT interleaving）。
//!
//! 为什么要独立组件：h2 0.4.20 的公开 `PingPong::send_ping` 忽略调用方传入的
//! payload，内部固定复用 `Ping::USER` token（`h2/src/proto/ping_pong.rs`），
//! 因此无法把一次探测的往返绑定到唯一 token，也就无法隔离迟到/重复 ACK。
//! 用固定 token 伪装唯一 token 不符合 §6.7.1，所以这里自己发帧。
//!
//! 计时与调度：连续 15s 窗口，每窗口开始提交一个 CSPRNG 唯一 token 的探测，
//! 至窗口结束等待匹配 ACK；至多一个逻辑在途；连续 3 个窗口没有匹配 ACK 即
//! 释放整个底层 io 并报错。不启后台 task：窗口推进只发生在 `poll_read` /
//! `poll_write` / `poll_flush` 内，同时注册定时器 waker，使驱动 h2 Connection
//! 的任务到点能正常醒来。发送阻塞不暂停窗口计时，过期窗口的探测不补发。
//!
//! **在途 wire 帧与逻辑窗口是两件事**：逻辑窗口可以照常前进、照常计失败，但
//! 已经写到一半的探测帧属于字节流状态，必须原样写完才允许出现任何其它字节；
//! 绝不因为“新窗口开了”就覆盖剩余字节或往旧帧 payload 里插新帧。从未上线的
//! 过期探测直接丢弃（不积压历史窗口），旧帧写完后最多再发“当前仍有效窗口”的
//! 那一条探测；旧探测对应的 ACK 属迟到 ACK，不 credit 新窗口。
//!
//! 边界与安全：
//! * 只有当前窗口 token 的首个 ACK 清除连续失败数，并且被拦截不交 h2；迟到
//!   （含在途旧帧对应窗口）、重复、陌生 ACK 一律原样交给 h2（h2 自行忽略）。
//!   对端 PING 请求同样原样交 h2 应答，本组件绝不代答，避免双重 ACK，也不干扰
//!   h2 原生 shutdown PING。
//! * 业务帧不能代替 ACK。`KeepaliveStats` 只做诊断，不导出 token，`Debug` 去敏。
//! * token 只能来自 CSPRNG：`SystemRandom::fill` 失败或去重重采样耗尽都**失败
//!   关闭**（去敏错误 + 释放底层），绝不退化成确定性递增或固定 token。
//! * 额外暂存均为固定上限（待探测 17B、在途探测 17B、入站 PING 检视 17B、帧头
//!   9B），普通帧 payload 流式转发，不囤积 peer 指定的大帧。
//! * 底层错误、`Ok(0)`、死链、token 源不可用：**所有底层 read Err（帧头/暂存/
//!   payload）与 write Err 同口径**——首次判定即记录、当次释放底层 io 的所有权
//!   （`S::drop` 立刻关 socket），错误 sticky，四类 poll 一律同口径；不做重同步，
//!   也不重发已交出的字节。这里**不**用 `poll_shutdown` 表达“关闭整条连接”——
//!   它需要被继续轮询到完成，而报错后本组件不再保留任何状态机去续它。
//! * 底层 EOF 落在帧头/连接级 PING 中间时，把已收到的残缺字节按截断语义原样
//!   交给 h2（由 h2 报协议错误），随后才报干净 EOF，绝不吞字节后假装 EOF。
//!
//! 连接级排空控制（§6.6、计划 Task4 裁决、RFC9113 §3.4/§6.8）：上层在把 io
//! 交给 `h2::handshake` **之前**用 [`H2KeepaliveIo::control_handle`] 取出
//! [`H2WireControl`]，之后即便具体泛型 `S` 已随 io 被移走，仍能：
//! * [`H2WireControl::begin_drain`]：幂等停止新窗口自发探测（在途半帧原样写完），
//!   并冻结窗口失败计数——排空 ≥5s 不得被保活假阳性杀死；不影响业务帧与
//!   h2 原生 SHUTDOWN PING/ACK，不绕过任何加密层；
//! * [`H2WireControl::request_client_goaway`]：单槽幂等排队恰好一条 client
//!   GOAWAY（type=7、stream=0、payload 8B 全零）。RFC9113 §6.8 的 last ID 指
//!   **peer-initiated** 流，本协议禁 server push ⇒ client 恒 0，绝不取自己最大
//!   奇数 ID；server 角色返回 [`WireControlError::WrongRole`]，由 h2 官方
//!   graceful 三步处理，本组件不代 server 发 GOAWAY；
//! * [`H2WireControl::wait_goaway_sent`]：只有整条 GOAWAY 通过底层 `poll_write`
//!   且 `poll_flush` 确认后才成功；fatal / `poll_shutdown` / 适配器 Drop 都会
//!   把未完成的 waiter 唤醒为 [`WireControlError::Closed`]，但**驱动任务必须
//!   持续 poll**——本组件不开后台 task，没人 poll 就没人写；
//! * [`H2WireControl::subscribe_peer_goaway`]：入站 GOAWAY 只观察**结构化 8B**
//!   字段（latest watch、sequence 单调），peer 可控的 debug 变长尾不进通知、
//!   不进日志，原帧逐字交 h2，不吞帧、不代 ACK。
//!
//! 句柄不 clone 底层 io、不持任何 key；它只把一个 waker 叫醒，注入仍发生在
//! driver 的正常 poll 里。所有自发帧（PING 与 GOAWAY）都必须在 24B 前导与
//! **首条 SETTINGS**（§3.4：HTTP/2 首帧必须是 SETTINGS）之后，且只落在完整帧
//! 与 HEADERS/CONTINUATION 块结束边界（§4.3）。

use std::error::Error;
use std::fmt;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker, ready};
use std::time::Duration;

use ring::rand::{SecureRandom, SystemRandom};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;
use tokio::time::{Instant, Sleep};

/// 单个探测窗口长度：每窗口一个 token，至窗口结束等待匹配 ACK（§6.7.1）。
pub const KEEPALIVE_WINDOW: Duration = Duration::from_secs(15);

/// 连续多少个窗口没有匹配 ACK 即判死链。
pub const MAX_CONSECUTIVE_WINDOW_FAILURES: u32 = 3;

/// HTTP/2 客户端连接前导（24B）：不是帧，必须原样透传。
pub const H2_CLIENT_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// 本适配器判死链时给出的标记错误，可用 [`is_keepalive_timeout`] 识别。
#[derive(Debug, Clone, Copy)]
pub struct KeepaliveTimeout;

impl fmt::Display for KeepaliveTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HTTP/2 连接级 PING 连续 {} 个窗口未收到匹配 ACK",
            MAX_CONSECUTIVE_WINDOW_FAILURES
        )
    }
}

impl Error for KeepaliveTimeout {}

/// 判断 io 错误是否由本适配器判定的死链引起。
pub fn is_keepalive_timeout(err: &io::Error) -> bool {
    self_err_is::<KeepaliveTimeout>(err)
}

/// token 源不可用（CSPRNG 失败或去重重采样耗尽）时的标记错误。
///
/// 去敏：固定文案，不含底层错误细节、不含 token、不含重试线索。语义是**失败
/// 关闭**，不是“换个 token 继续跑”，因此也不提供任何活性兜底开关。
#[derive(Debug, Clone, Copy)]
pub struct KeepaliveTokenError;

impl fmt::Display for KeepaliveTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "连接保活 token 的 CSPRNG 不可用，已按失败关闭终止连接")
    }
}

impl Error for KeepaliveTokenError {}

/// 判断 io 错误是否由 token 源不可用引起（与死链错误互斥）。
pub fn is_keepalive_token_error(err: &io::Error) -> bool {
    self_err_is::<KeepaliveTokenError>(err)
}

fn self_err_is<T: 'static + Error>(err: &io::Error) -> bool {
    match err.get_ref() {
        Some(inner) => inner.is::<T>(),
        None => false,
    }
}

/// 适配器角色：决定 24B 连接前导出现在哪一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H2Role {
    /// HTTP/2 客户端：出站方向以 24B 前导开始。
    Client,
    /// HTTP/2 服务端：入站方向以 24B 前导开始。
    Server,
}

/// 入站 peer GOAWAY 的有界观察结果。
///
/// 只含帧首 8B 的**结构化**字段（去保留位后的 last-stream-id、error code）与
/// 本连接内的单调序号；peer 可控的 debug 变长尾**绝不**进入通知（流式原样交
/// h2），也不进日志——避免无界内存与把 peer 数据注入日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerGoaway {
    /// GOAWAY last-stream-id（已去 1 位保留位）。语义是 **peer-initiated** 流
    /// 的上界（RFC9113 §6.8），不是本端创建过的流。
    pub last_stream_id: u32,
    /// GOAWAY error code 原样 u32（NO_ERROR 为 0）。
    pub error_code: u32,
    /// 本连接内第几次通知（从 1 起单调递增）；watch 只保留最新值，不累积历史。
    pub sequence: u64,
}

/// wire 控制面错误。固定文案，不含底层细节，可安全进日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireControlError {
    /// 该操作只对 HTTP/2 client 角色有效；server 的 GOAWAY 走 h2 官方 graceful。
    WrongRole,
    /// 连接已进入终态（底层已释放、已 shutdown，或适配器 Drop 时 GOAWAY 尚未
    /// flush），请求不可能再完成。
    Closed,
}

impl fmt::Display for WireControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireControlError::WrongRole => {
                write!(
                    f,
                    "该角色不支持自发 GOAWAY：server 端应使用 h2 官方 graceful 关闭"
                )
            }
            WireControlError::Closed => write!(f, "wire 控制面已关闭：连接进入终态"),
        }
    }
}

impl Error for WireControlError {}

/// `wait_goaway_sent` 观察到的终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoawayWait {
    /// 尚无确认结果（未请求，或已排队未 flush）。
    Pending,
    /// 整条 GOAWAY 已通过底层 `poll_write` 且 `poll_flush` 确认。
    Sent,
    /// fatal、shutdown 或适配器 Drop：不会再有确认。
    Closed,
}

/// 控制面共享状态的互斥部分（全部是短临界区，绝不跨 poll/await 持锁）。
struct WireInner {
    /// 已排空：不再开新窗口、不再自发探测（在途半帧仍原样写完）。
    draining: bool,
    /// client GOAWAY 已排队（单槽：置位后幂等，直到写出确认）。
    goaway_queued: bool,
    /// GOAWAY 已整帧写出且 flush 确认。
    goaway_sent: bool,
    /// 连接终态：后续请求一律 Closed。
    terminal: bool,
    /// driver 最近一次 poll 注册的 waker；句柄请求后唤醒它。
    driver_waker: Option<Waker>,
    /// peer GOAWAY 通知序号（单调）。
    peer_sequence: u64,
}

struct WireShared {
    role: H2Role,
    inner: Mutex<WireInner>,
    peer_tx: watch::Sender<Option<PeerGoaway>>,
    wait_tx: watch::Sender<GoawayWait>,
    /// 出站 GOAWAY「整帧已写出且底层 flush 已确认」的单调计数（0 = 从未）。
    /// 与角色无关的**纯观察**通道：本组件绝不因为它而多写/少写一个字节。
    outbound_tx: watch::Sender<u64>,
}

impl WireShared {
    fn new(role: H2Role) -> Self {
        let (wait_tx, _wait_rx0) = watch::channel(GoawayWait::Pending);
        let (peer_tx, _peer_rx0) = watch::channel(None);
        let (outbound_tx, _outbound_rx0) = watch::channel(0u64);
        Self {
            role,
            inner: Mutex::new(WireInner {
                draining: false,
                goaway_queued: false,
                goaway_sent: false,
                terminal: false,
                driver_waker: None,
                peer_sequence: 0,
            }),
            peer_tx,
            wait_tx,
            outbound_tx,
        }
    }

    /// 锁中毒只可能来自同组件临界区 panic（这里不重入、不跨 await），一律恢复
    /// 继续，不把一次 panic 放大成二次 panic。
    fn lock(&self) -> std::sync::MutexGuard<'_, WireInner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 唤醒当前挂起的 driver（h2 Connection 的 poll_read/write/flush）。
    /// 只在锁**外**调用。
    fn wake_driver(&self, waker: Option<Waker>) {
        if let Some(w) = waker {
            w.wake();
        }
    }

    fn mark_sent(&self) {
        self.lock().goaway_sent = true;
        // 自发 GOAWAY 不经出站帧记账（是适配器自己写的），因此在这里补记一次
        // 「整帧写出 + flush 确认」，让出站观察对两种角色同构。
        self.note_outbound_goaway();
        self.wait_tx.send_if_modified(|s| {
            if *s == GoawayWait::Pending {
                *s = GoawayWait::Sent;
                true
            } else {
                false
            }
        });
    }

    /// 记录「一条完整 GOAWAY 帧已经过底层写出且 flush 已确认」。
    /// latest 单调计数；无接收者时 `send_modify` 仍写入最新值。
    fn note_outbound_goaway(&self) {
        self.outbound_tx
            .send_modify(|sequence| *sequence = sequence.saturating_add(1));
    }

    /// 终态：之后的请求一律 Closed；Pending 的 waiter 当次唤醒。已 Sent 不回退。
    fn mark_closed(&self) {
        self.lock().terminal = true;
        self.wait_tx.send_if_modified(|s| {
            if *s == GoawayWait::Pending {
                *s = GoawayWait::Closed;
                true
            } else {
                false
            }
        });
    }

    fn note_peer_goaway(&self, last_stream_id: u32, error_code: u32) {
        let sequence = {
            let mut inner = self.lock();
            inner.peer_sequence = inner.peer_sequence.saturating_add(1);
            inner.peer_sequence
        };
        // 有界 latest：新值覆盖旧值。必须用 send_modify 而不是 send——
        // `send` 在没有任何接收者时会**丢弃**新值（tokio 文档明示），那样
        // “先观察、后订阅”的调用方会永远看不到 GOAWAY；send_modify 无接收者
        // 也写入 latest，有接收者时照常通知。
        let note = PeerGoaway {
            last_stream_id,
            error_code,
            sequence,
        };
        self.peer_tx.send_modify(|slot| *slot = Some(note));
    }
}

/// 连接级排空控制句柄（Clone + Send + Sync，不持有底层 `S`、不 clone io、不持 key）。
///
/// **必须在 io 被交给 `h2::handshake` 之前**用 [`H2KeepaliveIo::control_handle`]
/// 取得；之后即便具体泛型 `S` 已随 io 移走，也仍能请求 GOAWAY、观察 peer
/// GOAWAY。不启后台 task：GOAWAY 的写出只发生在 driver 任务的正常 poll 内，
/// 句柄负责把挂起的 driver 唤醒。**驱动任务必须持续 poll**，否则
/// [`H2WireControl::wait_goaway_sent`] 只能等到 fatal/Drop 的终态为止。
#[derive(Clone)]
pub struct H2WireControl {
    shared: Arc<WireShared>,
}

impl H2WireControl {
    /// 幂等进入排空：停止**新窗口**的自发探测（从未上线的排队探测直接丢弃，
    /// 已在途的半帧原样写完），并冻结窗口失败计数（排空 ≥5s 不得被保活假阳性
    /// 判死）。不阻止已接业务流的 DATA/trailers，也不干预 h2 原生 SHUTDOWN
    /// PING/ACK；任何排空操作都不绕过完整加密。已终态返回 Closed。
    pub fn begin_drain(&self) -> Result<(), WireControlError> {
        let waker = {
            let mut inner = self.shared.lock();
            if inner.terminal {
                return Err(WireControlError::Closed);
            }
            inner.draining = true;
            inner.driver_waker.take()
        };
        self.shared.wake_driver(waker);
        Ok(())
    }

    /// 排队**恰好一条** client GOAWAY（单槽幂等：重复调用不插多条）：
    /// type=7、stream=0、payload 恰 8B 全零（NO_ERROR、debug 空）。
    ///
    /// last_stream_id 固定 0：RFC9113 §6.8 的 last ID 是发送方处理过的
    /// **peer-initiated** 流，本协议禁 server push、server 不创建流，所以
    /// client 恒 0——**不**取自己最大奇数 ID。
    ///
    /// server 角色返回 [`WireControlError::WrongRole`]（server 走 h2 官方
    /// graceful 三步，本组件不代发）。请求成功入队后唤醒挂起的 driver。
    pub fn request_client_goaway(&self) -> Result<(), WireControlError> {
        if self.shared.role != H2Role::Client {
            return Err(WireControlError::WrongRole);
        }
        let waker = {
            let mut inner = self.shared.lock();
            if inner.terminal {
                return Err(WireControlError::Closed);
            }
            if !inner.goaway_queued && !inner.goaway_sent {
                inner.goaway_queued = true;
            }
            inner.driver_waker.take()
        };
        self.shared.wake_driver(waker);
        Ok(())
    }

    /// 订阅入站 peer GOAWAY 的结构化观察（latest + 单调 sequence）。
    /// 初值 `None`；不需要未来再 `send_request` 报错也能即时感知。
    pub fn subscribe_peer_goaway(&self) -> watch::Receiver<Option<PeerGoaway>> {
        self.shared.peer_tx.subscribe()
    }

    /// 订阅**出站** GOAWAY 的「整帧写出且底层 flush 已确认」事件（latest 单调计数，
    /// 初值 0 表示从未发生）。
    ///
    /// 用途只有一个：让调用方把排空计时锚定在 GOAWAY **真的上线**之后，而不是
    /// 锚定在「请求发 GOAWAY」那一刻。角色无关——client 由本组件自发的那一帧，与
    /// h2 为 server graceful 自己写出的那一帧，都算；本组件不因此注入、延迟或改写
    /// 任何帧，也不改动 client GOAWAY 的 last_stream_id。
    ///
    /// 必须如实理解的边界：它证明的是「字节已通过底层 `poll_write` 并被
    /// `poll_flush` 确认」，即已离开本进程的发送路径，不等于对端已经收到。
    /// 写方向被按住时该计数**不增长**，这正是调用方需要的负向信号。
    pub fn subscribe_outbound_goaway(&self) -> watch::Receiver<u64> {
        self.shared.outbound_tx.subscribe()
    }

    /// 等待 GOAWAY **整帧通过底层 `poll_write` 并被 `poll_flush` 确认**。
    /// 只入队不算已写；fatal、`poll_shutdown` 或适配器 Drop 唤醒为
    /// [`WireControlError::Closed`]。若从未请求或 driver 停止 poll，本 future
    /// 会一直 Pending——契约是驱动任务持续 poll（见类型文档）。
    pub async fn wait_goaway_sent(&self) -> Result<(), WireControlError> {
        let mut rx = self.shared.wait_tx.subscribe();
        loop {
            match *rx.borrow_and_update() {
                GoawayWait::Sent => return Ok(()),
                GoawayWait::Closed => return Err(WireControlError::Closed),
                GoawayWait::Pending => {}
            }
            if rx.changed().await.is_err() {
                // watch 发送端整体被丢弃（适配器与句柄全部退出）：终态。
                return Err(WireControlError::Closed);
            }
        }
    }
}

impl fmt::Debug for H2WireControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 去敏：只暴露角色与粗粒度状态位，不导出 waker/watch 内容。
        let inner = self.shared.lock();
        f.debug_struct("H2WireControl")
            .field("role", &self.shared.role)
            .field("draining", &inner.draining)
            .field("goaway_queued", &inner.goaway_queued)
            .field("goaway_sent", &inner.goaway_sent)
            .field("terminal", &inner.terminal)
            .finish()
    }
}

/// 诊断快照；只含计数与状态，不含 token（观察 API 仅用于诊断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveStats {
    /// 已开启的窗口数。
    pub windows_opened: u64,
    /// 完整写出的探测帧数。
    pub probes_sent: u64,
    /// 收到匹配 ACK 的窗口数（每窗口至多一次）。
    pub acked_windows: u64,
    /// 被拦截消费、未交付 h2 的匹配 ACK 帧数。
    pub intercepted_acks: u64,
    /// 当前连续未获匹配 ACK 的窗口数。
    pub consecutive_failures: u32,
    /// 是否已因连续失败判定死链。
    pub failed: bool,
    /// 是否已关闭（`poll_shutdown`、底层错误或死链）；之后不再探测、不再计失败。
    pub stopped: bool,
}

/// 终止原因，用于在后续每次 poll 上重建同一 io 错误。
#[derive(Debug, Clone)]
enum Failure {
    Timeout,
    Token,
    Broken { kind: io::ErrorKind, msg: String },
}

impl Failure {
    fn err(&self) -> io::Error {
        match self {
            Failure::Timeout => io::Error::new(io::ErrorKind::TimedOut, KeepaliveTimeout),
            Failure::Token => io::Error::other(KeepaliveTokenError),
            Failure::Broken { kind, msg } => io::Error::new(*kind, msg.clone()),
        }
    }
}

/// 待写出/正在写出的自发 PING 帧（固定 17B）。
#[derive(Clone, Copy)]
struct Probe {
    bytes: [u8; PROBE_FRAME_LEN],
    written: usize,
}

impl Probe {
    fn new(token: &[u8; TOKEN_LEN]) -> Self {
        let mut bytes = [0u8; PROBE_FRAME_LEN];
        // 长度 8、type=6 PING、flags=0、stream=0
        bytes[2] = TOKEN_LEN as u8;
        bytes[3] = FRAME_TYPE_PING;
        bytes[9..].copy_from_slice(token);
        Self { bytes, written: 0 }
    }
}

/// `push_probe` 的一次推进结果。
enum Pushed {
    /// 无事可做，或探测帧已完整写出。
    Done,
    /// 底层暂时不可写，且该帧已部分上线：必须原样交回，下次 poll 继续写完。
    /// （窗口计时不因此暂停。）
    NotYet(Probe),
    /// 底层报错：已由 [`H2KeepaliveIo::fail`] 记录原因并释放底层。
    Failed(io::Error),
}

/// 正在写出/排队的自发 GOAWAY 帧（固定 17B）。
///
/// 与 [`Probe`] 同理：一旦有字节上线就必须原样写完；且它占用字节流期间，h2 的
/// 帧也必须等待（返回 Pending），绝不把 GOAWAY 插进半帧中间。
#[derive(Clone, Copy)]
struct GoawayFrame {
    bytes: [u8; GOAWAY_FRAME_LEN],
    written: usize,
}

impl GoawayFrame {
    fn new() -> Self {
        let mut bytes = [0u8; GOAWAY_FRAME_LEN];
        // 长度 8、type=7 GOAWAY、flags=0、stream=0；payload 全零：
        // last_stream_id=0（peer-initiated 语义，禁 push）+ NO_ERROR + debug 空。
        bytes[2] = GOAWAY_PAYLOAD_LEN as u8;
        bytes[3] = FRAME_TYPE_GOAWAY;
        Self { bytes, written: 0 }
    }
}

/// 入站 GOAWAY 的结构观察器：**固定 8B**，只收 payload 首 8 字节；debug 变长尾
/// 流式透传，绝不整帧留存。凑满即通知一次并释放观察位。
#[derive(Clone, Copy, Default)]
struct GoawayObserve {
    captured: [u8; GOAWAY_PAYLOAD_LEN],
    filled: usize,
}

impl GoawayObserve {
    /// 喂入该 GOAWAY payload 的一段；返回 true 表示 8B 结构化字段已凑满。
    fn feed(&mut self, bytes: &[u8]) -> bool {
        let need = GOAWAY_PAYLOAD_LEN - self.filled;
        let n = bytes.len().min(need);
        self.captured[self.filled..self.filled + n].copy_from_slice(&bytes[..n]);
        self.filled += n;
        self.filled == GOAWAY_PAYLOAD_LEN
    }
}

/// 入站定长暂存：要么是一段 1..9B 帧头（普通帧透传），要么是一整条 17B 连接级 PING。
///
/// 这是入站方向唯一的额外缓冲；业务帧体绝不进这里，只按剩余计数字节流式转发。
#[derive(Clone, Copy)]
struct Staged {
    bytes: [u8; PROBE_FRAME_LEN],
    /// 需要收满的字节数：9（普通帧头）或 17（连接级 PING）。
    total: usize,
    filled: usize,
    delivered: usize,
    is_ack: bool,
    decided: bool,
    swallow: bool,
    /// 底层提前 EOF 导致帧残缺：只能把已收到的字节原样交给 h2 处理。
    truncated: bool,
}

const FRAME_HEADER_LEN: usize = 9;
const TOKEN_LEN: usize = 8;
const PROBE_FRAME_LEN: usize = FRAME_HEADER_LEN + TOKEN_LEN;
const FRAME_TYPE_HEADERS: u8 = 0x1;
const FRAME_TYPE_SETTINGS: u8 = 0x4;
const FRAME_TYPE_PUSH_PROMISE: u8 = 0x5;
const FRAME_TYPE_PING: u8 = 0x6;
const FRAME_TYPE_GOAWAY: u8 = 0x7;
const FRAME_TYPE_CONTINUATION: u8 = 0x9;
/// 自发 GOAWAY 的固定 payload：last_stream_id(4B)=0 + error(4B)=0，debug 空。
const GOAWAY_PAYLOAD_LEN: usize = 8;
/// 自发 GOAWAY 整帧长（9B 帧头 + 8B payload）；恰与探测帧同长。
const GOAWAY_FRAME_LEN: usize = FRAME_HEADER_LEN + GOAWAY_PAYLOAD_LEN;
const FLAG_ACK: u8 = 0x1;
const FLAG_END_HEADERS: u8 = 0x4;
const PREFACE_LEN: usize = 24;
/// 单次转发上限：不囤积 peer 指定的大帧，也不谎报巨大写出量。
const MAX_WRITE_CHUNK: usize = 16 * 1024;
/// 新 token 与近期 token 冲突时的重采样上限（固定上限，用尽即失败关闭）。
const TOKEN_RESAMPLE_LIMIT: usize = 16;
/// 近期 token 历史条数（固定上限，仅用于跨窗口唯一性）。
const TOKEN_HISTORY: usize = 4;
/// 单次 drive 最多结算的窗口数，防御虚拟时钟异常导致的长循环。
const WINDOW_CATCH_UP_LIMIT: usize = 4096;

/// token 随机源。
///
/// 生产侧只有 [`TokenRng::System`] 一种实现；两个 `#[cfg(test)]` 变体只为 crate
/// 内单测注入“CSPRNG 失败/重采样耗尽”，其类型、变体与构造函数均不对外可见，
/// 因此**不存在**可在生产里替换或关闭 CSPRNG 的入口（也没有 feature 开关）。
enum TokenRng {
    System,
    #[cfg(test)]
    TestFail {
        remaining_ok: usize,
    },
    #[cfg(test)]
    TestScript {
        tokens: Vec<[u8; TOKEN_LEN]>,
        idx: usize,
    },
}

impl TokenRng {
    /// 填充 8B 候选 token；返回 `false` 表示随机源不可用（调用方必须失败关闭）。
    fn fill(&mut self, dst: &mut [u8]) -> bool {
        match self {
            TokenRng::System => SystemRandom::new().fill(dst).is_ok(),
            #[cfg(test)]
            TokenRng::TestFail { remaining_ok } => {
                if *remaining_ok == 0 {
                    return false;
                }
                *remaining_ok -= 1;
                for (i, byte) in dst.iter_mut().enumerate() {
                    *byte = 0x5au8.wrapping_add(i as u8);
                }
                true
            }
            #[cfg(test)]
            TokenRng::TestScript { tokens, idx } => {
                if tokens.is_empty() {
                    return false;
                }
                // 脚本耗尽后恒返回最后一个 token：用于稳定构造“连续冲突”
                let token = tokens[(*idx).min(tokens.len() - 1)];
                *idx = idx.saturating_add(1);
                dst.copy_from_slice(&token);
                true
            }
        }
    }
}

/// 透明 HTTP/2 连接级 PING 保活适配器。
///
/// 不启后台任务：调用方照常驱动 `h2::client::Connection` /
/// `h2::server::Connection` 即可，窗口调度在每次 poll 中推进。
pub struct H2KeepaliveIo<S> {
    /// 底层 io。终态（死链、token 源不可用、底层错误）下被释放，因此全部访问器
    /// 返回 `Option`，终态不得 panic。
    io: Option<S>,
    role: H2Role,
    rng: TokenRng,

    // ===== 窗口调度 =====
    window_opened_at: Option<Instant>,
    window_token: [u8; TOKEN_LEN],
    window_token_set: bool,
    window_acked: bool,
    recent_tokens: [[u8; TOKEN_LEN]; TOKEN_HISTORY],
    recent_len: usize,
    recent_next: usize,
    timer: Option<Pin<Box<Sleep>>>,
    stats: KeepaliveStats,
    failure: Option<Failure>,
    stopped: bool,

    // ===== 自发探测 =====
    /// 当前窗口尚未上线的探测（可被新窗口顶掉，不积压历史窗口）。
    pending: Option<Probe>,
    /// 已经写到一半、必须原样写完的探测帧（wire 上唯一在途的自发帧）。
    inflight: Option<Probe>,

    // ===== 出站帧边界（只计剩余，不保存 peer 帧）=====
    out_preface_left: usize,
    out_payload_left: Option<u64>,
    out_hdr: [u8; FRAME_HEADER_LEN],
    out_hdr_filled: usize,
    /// 正在记账这一帧的类型/flags（帧头收满时写入，帧结束时判定 header block 边界）。
    out_frame_type: u8,
    out_frame_flags: u8,
    /// header block 序列未结束（HEADERS/PUSH_PROMISE 无 END_HEADERS）：期间不得
    /// 插入任何其它帧（RFC 9113 §4.3 / §6.10）。
    out_in_header_block: bool,

    // ===== 入站帧边界 =====
    in_preface_left: usize,
    in_payload_left: Option<u64>,
    in_hdr: [u8; FRAME_HEADER_LEN],
    in_hdr_filled: usize,
    staged: Option<Staged>,
    /// 入站 GOAWAY 结构观察（固定 8B，逐帧 arm；debug 不在此处）。
    in_goaway: Option<GoawayObserve>,

    // ===== 连接级排空控制（Task 4c）=====
    /// 与控制句柄共享的有界状态；不导出底层 `S`。
    shared: Arc<WireShared>,
    /// 正在写出的自发 GOAWAY（在途半帧必须原样写完，期间占用字节流）。
    out_goaway: Option<GoawayFrame>,
    /// GOAWAY 整帧已被底层 `poll_write` 接受，等待 `poll_flush` 确认。
    goaway_flush_pending: bool,
    /// 出站方向刚写完一整条 GOAWAY 帧（由 `poll_write` 的帧记账识别，含 h2 自己
    /// 为 graceful 写出的那一帧），等待下一次底层 `poll_flush` 确认。
    /// **纯观察位**：不参与任何写出/注入决策，也不因观察而多写一个字节。
    out_goaway_flush_pending: bool,
    /// 首条 SETTINGS 帧已完整记账：在它之前禁止任何自发帧注入（§3.4）。
    out_first_settings_done: bool,
}

impl<S> H2KeepaliveIo<S> {
    /// 包装底层 io。`H2Role::Client` 表示本端是 HTTP/2 客户端（出站含 24B 前导）。
    ///
    /// 构造不依赖 Tokio 运行时；但必须在运行时上下文里被 poll（窗口用
    /// `tokio::time` 单调虚拟时钟，测试可 `pause`/`advance`）。
    pub fn new(io: S, role: H2Role) -> Self {
        Self {
            io: Some(io),
            role,
            rng: TokenRng::System,
            window_opened_at: None,
            window_token: [0u8; TOKEN_LEN],
            window_token_set: false,
            window_acked: false,
            recent_tokens: [[0u8; TOKEN_LEN]; TOKEN_HISTORY],
            recent_len: 0,
            recent_next: 0,
            timer: None,
            stats: KeepaliveStats {
                windows_opened: 0,
                probes_sent: 0,
                acked_windows: 0,
                intercepted_acks: 0,
                consecutive_failures: 0,
                failed: false,
                stopped: false,
            },
            failure: None,
            stopped: false,
            pending: None,
            inflight: None,
            out_preface_left: match role {
                H2Role::Client => PREFACE_LEN,
                H2Role::Server => 0,
            },
            out_payload_left: None,
            out_hdr: [0u8; FRAME_HEADER_LEN],
            out_hdr_filled: 0,
            out_frame_type: 0,
            out_frame_flags: 0,
            out_in_header_block: false,
            in_preface_left: match role {
                H2Role::Server => PREFACE_LEN,
                H2Role::Client => 0,
            },
            in_payload_left: None,
            in_hdr: [0u8; FRAME_HEADER_LEN],
            in_hdr_filled: 0,
            staged: None,
            in_goaway: None,
            shared: Arc::new(WireShared::new(role)),
            out_goaway: None,
            goaway_flush_pending: false,
            out_goaway_flush_pending: false,
            out_first_settings_done: false,
        }
    }

    /// 仅 crate 内单测使用：注入受控 token 源，覆盖“CSPRNG 失败/重采样耗尽”分支。
    /// 私有可见性，生产侧无从调用，因此不构成可配置随机的绕过入口。
    #[cfg(test)]
    fn with_token_rng(io: S, role: H2Role, rng: TokenRng) -> Self {
        let mut me = Self::new(io, role);
        me.rng = rng;
        me
    }

    /// 本端 HTTP/2 角色。
    pub fn role(&self) -> H2Role {
        self.role
    }

    /// 诊断快照（不导出 token）。
    pub fn stats(&self) -> KeepaliveStats {
        let mut stats = self.stats;
        stats.stopped = self.stopped || self.failure.is_some();
        stats.failed = self.stats.failed;
        stats
    }

    /// 借用底层 io；终态（已因死链/token 源/底层错误释放所有权）返回 `None`。
    pub fn get_ref(&self) -> Option<&S> {
        self.io.as_ref()
    }

    /// 可变借用底层 io；终态返回 `None`。
    ///
    /// **风险**：绕过本组件直接写底层字节流会破坏帧边界与 header block 记账
    /// （出站边界只由 [`AsyncWrite::poll_write`] 的记账维护），后果可能是在非帧
    /// 边界注入 `PING`、上层看到错位帧，或把外部写入误当成某个帧的 payload，
    /// 从而同时破坏保活与 h2 会话。仅用于取底层元数据或 poll 之外的只读诊断；
    /// 任何写出都必须走 `H2KeepaliveIo` 自己的 `AsyncWrite`。
    pub fn get_mut(&mut self) -> Option<&mut S> {
        self.io.as_mut()
    }

    /// 交还底层 io（丢弃保活调度器）。
    ///
    /// 返回 `None` 表示已进入终态、底层所有权早先已被释放（死链/token 源不可用/
    /// 底层错误）；不得 panic，也无法再拿回底层。交还即视为本组件不再驱动任何
    /// 自发帧：未完成的 `wait_goaway_sent` waiter 会在 Drop 时被唤醒为 Closed。
    pub fn into_inner(mut self) -> Option<S> {
        self.io.take()
    }

    /// 取出连接级排空控制句柄（见 [`H2WireControl`]）。
    ///
    /// **必须在 io 被交给 `h2::handshake` 之前调用**：句柄不持有也不 clone 底层
    /// `S`，io 移交后这是唯一还能驱动 GOAWAY / 观察 peer GOAWAY 的入口。
    /// 可多次调用、可 clone、可跨任务持有。
    pub fn control_handle(&self) -> H2WireControl {
        H2WireControl {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<S> Drop for H2KeepaliveIo<S> {
    fn drop(&mut self) {
        // 终态契约：适配器被丢弃后不会再有人把 GOAWAY flush 上线，Pending 的
        // waiter 必须当次被唤醒为 Closed，不得永挂。
        self.shared.mark_closed();
    }
}

impl<S> H2KeepaliveIo<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // ===== 底层 io =====

    /// 底层已释放时统一用的错误：优先复现 sticky 的终止原因。
    fn inner_released_err(&self) -> io::Error {
        self.failure_err().unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "保活适配器已释放底层 io")
        })
    }

    /// 取可变底层；终态返回 sticky 错误。
    fn io_mut(&mut self) -> Result<&mut S, io::Error> {
        if self.io.is_none() {
            let err = self.inner_released_err();
            return Err(err);
        }
        match self.io.as_mut() {
            Some(io) => Ok(io),
            // 不可达：上面已判过；用固定错误而不是 panic
            None => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "保活适配器已释放底层 io",
            )),
        }
    }

    // ===== 帧边界 =====

    /// 出站是否处于可插入自发帧的边界：前导已完整、不在任何帧的帧头/payload 内，
    /// 且不在 header block 序列内部（HEADERS/PUSH_PROMISE 到 END_HEADERS 之间
    /// 不得插帧，即使单帧 payload 已经结束）。
    fn outbound_boundary(&self) -> bool {
        self.out_preface_left == 0
            && self.out_payload_left.is_none()
            && self.out_hdr_filled == 0
            && !self.out_in_header_block
    }

    /// 自发帧（PING 与 GOAWAY 共用）的可注入边界：在 [`Self::outbound_boundary`]
    /// 之外还要求**首条 SETTINGS 已完整写出**（RFC9113 §3.4）。
    fn injection_allowed(&self) -> bool {
        self.out_first_settings_done && self.outbound_boundary()
    }

    /// 一帧记账结束：更新 header block 状态；识别首条 SETTINGS（§3.4 注入门）。
    fn out_finish_frame(&mut self) {
        let ftype = self.out_frame_type;
        if ftype == FRAME_TYPE_GOAWAY {
            // 一整条 GOAWAY 的字节已全部交给底层：还差 flush 确认，先挂观察位。
            self.out_goaway_flush_pending = true;
        }
        if ftype == FRAME_TYPE_SETTINGS {
            // 自发起（PING / GOAWAY）必须等首条 SETTINGS 完整写出之后（§3.4：
            // HTTP/2 双方首帧必须是 SETTINGS），否则就是在 preface 与 SETTINGS
            // 之间插帧——不能依赖 h2 当前调度“碰巧先发了 SETTINGS”。
            self.out_first_settings_done = true;
        }
        let ends_headers = self.out_frame_flags & FLAG_END_HEADERS != 0;
        if (ftype == FRAME_TYPE_HEADERS || ftype == FRAME_TYPE_PUSH_PROMISE) && !ends_headers {
            self.out_in_header_block = true;
        } else if ftype == FRAME_TYPE_CONTINUATION && ends_headers {
            self.out_in_header_block = false;
        }
        // 其余帧（含尚未 END_HEADERS 的 CONTINUATION）不改变状态：序列要么已开始，
        // 要么本来就与 header block 无关。
    }

    /// 记账已交给底层的出站字节以维护帧边界：只数长度，不缓存 peer 帧。
    fn out_account(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.out_preface_left > 0 {
                let n = rest.len().min(self.out_preface_left);
                self.out_preface_left -= n;
                rest = &rest[n..];
                continue;
            }
            match self.out_payload_left {
                None => {
                    let need = FRAME_HEADER_LEN - self.out_hdr_filled;
                    let n = rest.len().min(need);
                    let filled = self.out_hdr_filled;
                    self.out_hdr[filled..filled + n].copy_from_slice(&rest[..n]);
                    self.out_hdr_filled += n;
                    rest = &rest[n..];
                    if self.out_hdr_filled == FRAME_HEADER_LEN {
                        let len = frame_len(&self.out_hdr);
                        self.out_frame_type = self.out_hdr[3];
                        self.out_frame_flags = self.out_hdr[4];
                        self.out_hdr_filled = 0;
                        if len == 0 {
                            self.out_finish_frame();
                        } else {
                            self.out_payload_left = Some(len);
                        }
                    }
                }
                Some(left) => {
                    let n = rest.len().min(left as usize);
                    let rem = left - n as u64;
                    if rem == 0 {
                        self.out_payload_left = None;
                        self.out_finish_frame();
                    } else {
                        self.out_payload_left = Some(rem);
                    }
                    rest = &rest[n..];
                }
            }
        }
    }

    // ===== token =====

    /// 生成与近期窗口不重复的 CSPRNG token。
    ///
    /// `None` 表示 CSPRNG 不可用或去重重采样耗尽：调用方**必须失败关闭**。
    /// 这里没有任何确定性替代——继续发一个可预测 token 会让迟到/重复 ACK 无法
    /// 绑定到某次往返，等于放弃 §6.7.1 的唯一性前提，也等于谎称 token 来自 CSPRNG。
    fn next_token(&mut self) -> Option<[u8; TOKEN_LEN]> {
        for _ in 0..TOKEN_RESAMPLE_LIMIT {
            let mut token = [0u8; TOKEN_LEN];
            if !self.rng.fill(&mut token) {
                return None;
            }
            if token_is_empty(&token) || self.recent_contains(&token) {
                continue;
            }
            self.recent_push(token);
            return Some(token);
        }
        None
    }

    fn recent_contains(&self, token: &[u8; TOKEN_LEN]) -> bool {
        self.recent_tokens
            .iter()
            .take(self.recent_len)
            .any(|t| t == token)
    }

    fn recent_push(&mut self, token: [u8; TOKEN_LEN]) {
        self.recent_tokens[self.recent_next] = token;
        self.recent_next = (self.recent_next + 1) % TOKEN_HISTORY;
        // recent_len 饱和增长到 TOKEN_HISTORY，与环形写入位置无关：4 个槽位必须
        // 全部参与查重（早期实现按 recent_next 推导，实际只用满 3 槽）。
        self.recent_len = (self.recent_len + 1).min(TOKEN_HISTORY);
    }

    // ===== 失败与调度 =====

    /// 记录终止原因并**当场**释放底层 io 的所有权；返回本次要上报的错误。
    ///
    /// 首次判定即释放，是“关闭整个底层连接”的唯一可靠实现：`poll_shutdown` 需要
    /// 被继续轮询才能完成，而本组件在报错后不会保留任何状态机去续它。
    /// 释放的同时清掉定时器与全部在途/暂存帧：终态不再产生任何字节。
    fn fail(&mut self, failure: Failure) -> io::Error {
        let err = failure.err();
        if self.failure.is_none() {
            self.failure = Some(failure);
            self.stats.failed = true;
            self.window_opened_at = None;
            self.io = None;
            self.timer = None;
            self.staged = None;
            self.pending = None;
            self.inflight = None;
            self.in_goaway = None;
            self.out_goaway = None;
            self.stopped = true;
            self.stats.stopped = true;
            // 终态同步到控制面：Pending 的 waiter 当次唤醒为 Closed。
            self.shared.mark_closed();
        }
        err
    }

    /// P3 同口径：底层 read Err（帧头 / 暂存检视 / payload 流式段）与写 Err 一样
    /// **当次记录、释放底层、四类 poll sticky**；不得留下活的 `get_ref` 让后续
    /// 探测/调度继续跑。EOF（`Ok(0)`）不在此列——它走截断透传语义。
    fn fail_read(&mut self, e: io::Error) -> io::Error {
        let kind = e.kind();
        let msg = format!("底层读取失败: {e}");
        self.fail(Failure::Broken { kind, msg })
    }

    fn failure_err(&self) -> Option<io::Error> {
        self.failure.as_ref().map(Failure::err)
    }

    /// 开启新窗口并登记一条探测（尚未上线）。
    fn open_window(&mut self, at: Instant) -> Result<(), io::Error> {
        let token = match self.next_token() {
            Some(token) => token,
            None => return Err(self.fail(Failure::Token)),
        };
        self.window_opened_at = Some(at);
        self.window_token = token;
        self.window_token_set = true;
        self.window_acked = false;
        self.stats.windows_opened += 1;
        // 只保留当前窗口的探测：上一窗口从未上线的探测直接顶掉，不积压历史窗口。
        self.pending = Some(Probe::new(&token));
        Ok(())
    }

    /// 结算到期的窗口。
    ///
    /// 逻辑窗口照常前进、照常计失败（写阻塞不暂停计时）；从未上线的过期探测由
    /// `open_window` 直接顶掉。**已经写到一半的探测帧是字节流状态，不属于窗口**：
    /// 它留在 `inflight` 里由 `push_probe` 继续原样写完，窗口推进绝不覆盖它，
    /// 也不会在旧帧 payload 中间插新帧。
    fn close_due_windows(&mut self, now: Instant) -> Result<(), io::Error> {
        let mut guard = 0usize;
        while !self.stopped && self.failure.is_none() && guard < WINDOW_CATCH_UP_LIMIT {
            let opened = match self.window_opened_at {
                Some(at) => at,
                None => break,
            };
            let deadline = opened + KEEPALIVE_WINDOW;
            if now < deadline {
                break;
            }
            if !self.window_acked {
                self.stats.consecutive_failures += 1;
            }
            if self.stats.consecutive_failures >= MAX_CONSECUTIVE_WINDOW_FAILURES {
                return Err(self.fail(Failure::Timeout));
            }
            self.open_window(deadline)?;
            guard += 1;
        }
        Ok(())
    }

    /// 注册下一次窗口边界的定时器，使空闲的上层任务（h2 Connection）到点能醒。
    fn arm_timer(&mut self, cx: &mut Context<'_>) {
        if self.stopped || self.failure.is_some() {
            self.timer = None;
            return;
        }
        let deadline = match self.window_opened_at {
            Some(at) => at + KEEPALIVE_WINDOW,
            None => return,
        };
        if tokio::runtime::Handle::try_current().is_err() {
            // 没有运行时上下文时只能靠后续 poll 推进，不注册定时器。
            return;
        }
        let mut slot = match self.timer.take() {
            Some(slot) => slot,
            None => Box::pin(tokio::time::sleep_until(deadline)),
        };
        slot.as_mut().reset(deadline);
        let _ = slot.as_mut().poll(cx);
        self.timer = Some(slot);
    }

    /// 推动窗口调度与自发帧（PING / GOAWAY）：只能被 poll 驱动（不启后台 task）。
    fn drive(&mut self, cx: &mut Context<'_>) -> Result<(), io::Error> {
        if self.stopped {
            return Ok(());
        }
        // 控制面同步必须最先做：每次 poll（含 Pending 退场）都注册 driver waker，
        // 句柄的 begin_drain / request_client_goaway 才能唤醒挂在 poll_read/write
        // /flush 上的 driver——空闲连接否则永远排空不了。
        let (draining, goaway_queued) = self.sync_control(cx);
        if draining {
            // 排空：从未上线的排队探测直接丢弃（不补发）；在途半帧不受影响，
            // 仍由 push_probe 原样写完。
            self.pending = None;
        }
        if draining {
            // 排空冻结窗口调度：不新开窗口、不计 ACK 失败、撤定时器——排空
            // ≥5s 不得被保活假阳性判死（“不为等心跳无限延长排空”的镜像要求）。
            self.timer = None;
        } else {
            let now = Instant::now();
            match self.window_opened_at {
                None => self.open_window(now)?,
                Some(_) => self.close_due_windows(now)?,
            }
            self.arm_timer(cx);
        }
        // 探测先于 GOAWAY 推进：当前窗口的探测不许被 GOAWAY 饿死；反过来在途
        // 半帧探测（inflight）写完之前，GOAWAY 一个字节都不许多。
        match self.push_probe(cx) {
            Pushed::Done => {}
            Pushed::NotYet(probe) => self.inflight = Some(probe),
            Pushed::Failed(e) => return Err(e),
        }
        self.push_goaway(cx, goaway_queued)?;
        self.confirm_goaway_flush(cx)
    }

    /// 与控制面同步并注册 driver waker；返回 `(排空中, GOAWAY 已排队未确认)`。
    /// 锁只在这条短临界区里持有，绝不跨任何底层 poll。
    fn sync_control(&mut self, cx: &mut Context<'_>) -> (bool, bool) {
        let mut inner = self.shared.lock();
        inner.driver_waker = Some(cx.waker().clone());
        (inner.draining, inner.goaway_queued && !inner.goaway_sent)
    }

    /// 把已凑满的 8B 结构化字段发布到 watch。唯一入口是 [`GoawayObserve`]，
    /// 因此 peer 的 debug 变长尾从类型上就进不了通知，也从不进日志。
    fn notify_peer_goaway(&mut self, payload: &[u8; GOAWAY_PAYLOAD_LEN]) {
        // last-stream-id 是 31 位语义（去掉保留位）；error code 原样 u32。
        let last_stream_id = ((payload[0] as u32 & 0x7f) << 24)
            | ((payload[1] as u32) << 16)
            | ((payload[2] as u32) << 8)
            | payload[3] as u32;
        let error_code = u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]);
        self.shared.note_peer_goaway(last_stream_id, error_code);
    }

    /// 尽力推进自发 GOAWAY 的写出（仅 client 角色，单槽）。
    ///
    /// 顺序硬约束：在途半帧探测（`inflight`）未写完、或注入边界不满足（含首条
    /// SETTINGS 未写出）时**不发起**；一旦发起，半帧必须原样写完，占用字节流
    /// 期间 h2 的帧等待（`poll_write` 返回 Pending）。整帧交给底层还不算写出，
    /// 由 [`Self::confirm_goaway_flush`] 等 flush 确认。
    fn push_goaway(&mut self, cx: &mut Context<'_>, queued: bool) -> Result<(), io::Error> {
        if self.role != H2Role::Client {
            // 双保险：request_client_goaway 已按角色拒绝，这里绝不自发。
            return Ok(());
        }
        if self.out_goaway.is_none() {
            let blocked = !queued
                || self.goaway_flush_pending
                || self.inflight.is_some()
                || !self.injection_allowed();
            if blocked {
                return Ok(());
            }
            self.out_goaway = Some(GoawayFrame::new());
        }
        let frame = match self.out_goaway.take() {
            Some(frame) => frame,
            None => return Ok(()),
        };
        let mut frame = frame;
        while frame.written < GOAWAY_FRAME_LEN {
            let buf = &frame.bytes[frame.written..];
            let io = self.io_mut()?;
            let r = Pin::new(io).poll_write(cx, buf);
            match r {
                Poll::Ready(Ok(0)) => {
                    let kind = io::ErrorKind::BrokenPipe;
                    let msg = "底层写入已关闭".to_string();
                    return Err(self.fail(Failure::Broken { kind, msg }));
                }
                Poll::Ready(Ok(n)) => frame.written += n,
                Poll::Ready(Err(e)) => {
                    let kind = e.kind();
                    let msg = format!("GOAWAY 写出失败: {e}");
                    return Err(self.fail(Failure::Broken { kind, msg }));
                }
                Poll::Pending => {
                    // 排队请求不是可随窗口顶掉的探测：无论是否已有字节上线，槽位
                    // 都保留，下次 poll 继续。
                    self.out_goaway = Some(frame);
                    return Ok(());
                }
            }
        }
        self.goaway_flush_pending = true;
        Ok(())
    }

    /// GOAWAY 的写出确认：整帧通过底层 `poll_write` **且** `poll_flush` 返回
    /// `Ready(Ok)` 才算已写（`mark_sent`）；只入队（flush Pending）不算。
    /// 出站 GOAWAY 的 flush 确认（角色无关的纯观察）：只有底层 `poll_flush`
    /// 真的返回 `Ready(Ok)` 才通知，Pending 不算「已上线」。
    fn confirm_outbound_goaway_flush(&mut self, flushed: bool) {
        if flushed && self.out_goaway_flush_pending {
            self.out_goaway_flush_pending = false;
            self.shared.note_outbound_goaway();
        }
    }

    fn confirm_goaway_flush(&mut self, cx: &mut Context<'_>) -> Result<(), io::Error> {
        if !self.goaway_flush_pending || self.out_goaway.is_some() {
            return Ok(());
        }
        let io = self.io_mut()?;
        let r = Pin::new(io).poll_flush(cx);
        match r {
            Poll::Ready(Ok(())) => {
                self.goaway_flush_pending = false;
                self.shared.mark_sent();
                Ok(())
            }
            Poll::Ready(Err(e)) => {
                let kind = e.kind();
                let msg = format!("GOAWAY flush 失败: {e}");
                Err(self.fail(Failure::Broken { kind, msg }))
            }
            Poll::Pending => Ok(()),
        }
    }

    /// 尽力推进自发探测帧的写出。
    ///
    /// 顺序是硬性的：
    /// 1. `inflight`（已上线的半帧）必须先完整写完，期间不写任何其他字节，也绝不
    ///    因为“窗口换了”就改写成新帧——否则旧帧尾部丢失、新旧帧字节互相污染。
    /// 2. 旧帧写完后，最多再发起**当前窗口**的探测（`pending`），且只能在可注入
    ///    帧边界发起；历史窗口的探测不补发。
    fn push_probe(&mut self, cx: &mut Context<'_>) -> Pushed {
        if let Some(probe) = self.inflight.take() {
            match self.write_probe(probe, cx) {
                Pushed::Done => {}
                other => return other,
            }
        }
        if let Some(probe) = self.pending.take() {
            if !self.injection_allowed() {
                // 不在边界（或首条 SETTINGS 尚未写出，§3.4）：未上线的探测留在
                // 原地，等帧边界或随窗口过期被顶掉。
                self.pending = Some(probe);
                return Pushed::Done;
            }
            return self.write_probe(probe, cx);
        }
        Pushed::Done
    }

    /// 把一条探测帧写到底层。
    ///
    /// “未上线”（`written == 0`）时底层立刻 Pending：不产生任何字节，帧放回
    /// `pending`，允许随窗口过期被顶掉。已上线（`written > 0`）的帧交回
    /// `Pushed::NotYet`，由调用方放回 `inflight` 继续写完。
    fn write_probe(&mut self, mut probe: Probe, cx: &mut Context<'_>) -> Pushed {
        while probe.written < PROBE_FRAME_LEN {
            let buf = &probe.bytes[probe.written..];
            let r = match self.io_mut() {
                Ok(io) => Pin::new(io).poll_write(cx, buf),
                Err(e) => return Pushed::Failed(e),
            };
            match r {
                Poll::Ready(Ok(0)) => {
                    let kind = io::ErrorKind::BrokenPipe;
                    let msg = "底层写入已关闭".to_string();
                    return Pushed::Failed(self.fail(Failure::Broken { kind, msg }));
                }
                Poll::Ready(Ok(n)) => probe.written += n,
                Poll::Ready(Err(e)) => {
                    let kind = e.kind();
                    let msg = format!("保活探测写出失败: {e}");
                    return Pushed::Failed(self.fail(Failure::Broken { kind, msg }));
                }
                Poll::Pending => {
                    if probe.written == 0 {
                        self.pending = Some(probe);
                        return Pushed::Done;
                    }
                    return Pushed::NotYet(probe);
                }
            }
        }
        self.stats.probes_sent += 1;
        Pushed::Done
    }

    /// 当前窗口 token 的 ACK：每窗口只成功一次，其余一律不算刷新健康。
    fn credit_ack(&mut self, token: &[u8; TOKEN_LEN]) -> bool {
        if self.failure.is_some() || self.stopped {
            return false;
        }
        if !self.window_token_set || self.window_acked {
            return false;
        }
        if &self.window_token != token {
            return false;
        }
        self.window_acked = true;
        self.stats.consecutive_failures = 0;
        self.stats.acked_windows += 1;
        true
    }

    // ===== 入站帧解析 =====

    /// 帧头收满后分类：连接级 PING（len=8、stream=0）先收满 token 再判定；
    /// 其余帧把 9B 帧头逐字排队交付，payload 走剩余计数的流式转发。
    ///
    /// 关键：帧头在分类前只留在内部 9B 缓冲，绝不先交给上层，否则被拦截的
    /// PING 会泄漏半个帧头（表现为上层看到重复/错位字节）。
    fn classify_inbound(&mut self) {
        let hdr = self.in_hdr;
        self.in_hdr_filled = 0;
        let len = frame_len(&hdr);
        let is_conn_ping =
            hdr[3] == FRAME_TYPE_PING && len == TOKEN_LEN as u64 && frame_stream(&hdr) == 0;
        // 入站 GOAWAY 结构观察 arm：只认 type=7、stream=0、payload ≥8B；观察位
        // 固定 8B，debug 变长尾不进观察。结构性非法不 arm（也绝不吞帧）。
        self.in_goaway = if hdr[3] == FRAME_TYPE_GOAWAY
            && frame_stream(&hdr) == 0
            && len >= GOAWAY_PAYLOAD_LEN as u64
        {
            Some(GoawayObserve::default())
        } else {
            None
        };
        let mut bytes = [0u8; PROBE_FRAME_LEN];
        bytes[..FRAME_HEADER_LEN].copy_from_slice(&hdr);
        self.in_payload_left = if is_conn_ping || len == 0 {
            None
        } else {
            Some(len)
        };
        let total = if is_conn_ping {
            PROBE_FRAME_LEN
        } else {
            FRAME_HEADER_LEN
        };
        self.staged = Some(Staged {
            bytes,
            total,
            filled: FRAME_HEADER_LEN,
            delivered: 0,
            is_ack: hdr[4] & FLAG_ACK != 0,
            decided: !is_conn_ping,
            swallow: false,
            truncated: false,
        });
    }

    /// 底层 EOF 时把内部帧头缓冲里的 1..8B 残头转成截断暂存，先交给上层。
    ///
    /// 否则这些字节会被永久吞掉并向上传递一个“干净的 EOF”，h2 侧表现为一帧凭空
    /// 消失；截断透传则由 h2 明确报协议/连接错误。
    fn flush_partial_inbound_header(&mut self) {
        let n = self.in_hdr_filled;
        if n == 0 {
            return;
        }
        let hdr = self.in_hdr;
        self.in_hdr_filled = 0;
        let mut bytes = [0u8; PROBE_FRAME_LEN];
        bytes[..n].copy_from_slice(&hdr[..n]);
        self.staged = Some(Staged {
            bytes,
            total: n,
            filled: n,
            delivered: 0,
            is_ack: false,
            decided: true,
            swallow: false,
            truncated: true,
        });
    }

    /// 收满、判定并交付（或吞掉）一个定长暂存片段。
    ///
    /// 结束时清空 `self.staged`；调用方若发现没有向上传付任何字节且
    /// `staged` 已空，说明整帧被拦截，应继续读后续字节（绝不谎报 EOF）。
    fn drain_staged(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut st = match self.staged {
            Some(st) => st,
            None => return Poll::Ready(Ok(())),
        };
        // 只有连接级 PING 需要继续收满 token；上限固定 17B
        if st.filled < st.total && !st.truncated {
            let mut sub = ReadBuf::new(&mut st.bytes[st.filled..st.total]);
            let r = match self.io_mut() {
                Ok(io) => Pin::new(io).poll_read(cx, &mut sub),
                Err(e) => {
                    self.staged = Some(st);
                    return Poll::Ready(Err(e));
                }
            };
            match r {
                Poll::Ready(Ok(())) => {
                    let got = sub.filled().len();
                    st.filled += got;
                    if got == 0 {
                        // 底层 EOF：帧残缺，只能原样交付给 h2 处理
                        st.truncated = true;
                    }
                }
                Poll::Ready(Err(e)) => {
                    // 暂存检视中的 read Err：与写 Err 同口径，当次 fail、释放
                    // 底层、sticky（fail 会清掉暂存位，不再交回 st）。
                    return Poll::Ready(Err(self.fail_read(e)));
                }
                Poll::Pending => {
                    self.staged = Some(st);
                    return Poll::Pending;
                }
            }
        }
        if !st.decided && st.filled == st.total {
            // 只有“当前窗口 token 的首个 ACK”才拦截；迟到/重复/陌生/残缺一律原样交 h2
            let swallow = st.is_ack && self.credit_ack(&staged_token(&st.bytes));
            st.decided = true;
            st.swallow = swallow;
            if swallow {
                self.stats.intercepted_acks += 1;
            }
        }
        if st.swallow {
            self.staged = None;
            return Poll::Ready(Ok(()));
        }
        let avail = st.filled - st.delivered;
        let n = buf.remaining().min(avail);
        if n == 0 {
            self.staged = Some(st);
            return Poll::Ready(Ok(()));
        }
        let all = st.bytes;
        let from = st.delivered;
        st.delivered += n;
        self.staged = if st.delivered == st.filled {
            None
        } else {
            Some(st)
        };
        buf.put_slice(&all[from..from + n]);
        Poll::Ready(Ok(()))
    }

    /// 从底层最多读 `limit` 字节进 `buf` 的未填充区，返回实际交付上层的字节数。
    /// 只按当前帧段长度截断，不改写任何字节，因此逐字透明。
    fn read_limited(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
        limit: usize,
    ) -> Poll<io::Result<usize>> {
        let n = buf.remaining().min(limit);
        if n == 0 {
            return Poll::Ready(Ok(0));
        }
        let region = buf.initialize_unfilled_to(n);
        let mut sub = ReadBuf::new(region);
        let r = match self.io_mut() {
            Ok(io) => Pin::new(io).poll_read(cx, &mut sub),
            Err(e) => return Poll::Ready(Err(e)),
        };
        let got = sub.filled().len();
        match r {
            Poll::Ready(Ok(())) => {
                buf.advance(got);
                Poll::Ready(Ok(got))
            }
            // 前导 / payload 流式段的 read Err：同口径 fail（P3）。
            Poll::Ready(Err(e)) => Poll::Ready(Err(self.fail_read(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

fn frame_len(hdr: &[u8; FRAME_HEADER_LEN]) -> u64 {
    ((hdr[0] as u64) << 16) | ((hdr[1] as u64) << 8) | hdr[2] as u64
}

fn frame_stream(hdr: &[u8; FRAME_HEADER_LEN]) -> u32 {
    ((hdr[5] as u32 & 0x7f) << 24)
        | ((hdr[6] as u32) << 16)
        | ((hdr[7] as u32) << 8)
        | hdr[8] as u32
}

fn staged_token(bytes: &[u8; PROBE_FRAME_LEN]) -> [u8; TOKEN_LEN] {
    let mut out = [0u8; TOKEN_LEN];
    out.copy_from_slice(&bytes[FRAME_HEADER_LEN..]);
    out
}

fn token_is_empty(token: &[u8; TOKEN_LEN]) -> bool {
    token.iter().all(|b| *b == 0)
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for H2KeepaliveIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if let Some(err) = me.failure_err() {
            return Poll::Ready(Err(err));
        }
        if let Err(e) = me.drive(cx) {
            return Poll::Ready(Err(e));
        }
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            // 1) 入站定长暂存（1..9B 帧头或整条 17B PING）
            if me.staged.is_some() {
                let before = buf.filled().len();
                match me.drain_staged(cx, buf) {
                    Poll::Ready(Ok(())) => {}
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                }
                if me.staged.is_some() || buf.filled().len() != before {
                    // 已交付（可能不完整）：本轮结束，保持简单可预期
                    return Poll::Ready(Ok(()));
                }
                // 整帧被拦截消费：继续读后续字节
                continue;
            }
            // 2) 入站连接前导（服务端角色）：原样交付，绝不当帧头
            if me.in_preface_left > 0 {
                let got = ready!(me.read_limited(cx, buf, me.in_preface_left))?;
                me.in_preface_left -= got;
                return Poll::Ready(Ok(()));
            }
            // 3) 帧体：按剩余计数字节流式转发，绝不按 peer 声明长度整帧囤积
            if let Some(left) = me.in_payload_left {
                let before = buf.filled().len();
                let got = ready!(me.read_limited(cx, buf, left as usize))?;
                if got > 0 && me.in_goaway.is_some() {
                    // 只观察 payload 首 8B（固定缓冲），字节仍已逐字交付上层；
                    // debug 变长尾不落任何缓冲、不进日志。
                    let done = {
                        let obs = me.in_goaway.as_mut().expect("just checked");
                        obs.feed(&buf.filled()[before..before + got])
                    };
                    if done {
                        let captured = me.in_goaway.take().expect("just checked").captured;
                        me.notify_peer_goaway(&captured);
                    }
                }
                me.in_payload_left = match left.checked_sub(got as u64) {
                    Some(0) | None => None,
                    Some(rem) => Some(rem),
                };
                return Poll::Ready(Ok(()));
            }
            // 4) 帧头：先收进内部 9B 缓冲，分类后才决定透传还是检视。
            //    先读进栈上临时区：借底层 io 的同时不能再可变借用内部帧头缓冲。
            let filled = me.in_hdr_filled;
            let mut tmp = [0u8; FRAME_HEADER_LEN];
            let got = {
                let mut sub = ReadBuf::new(&mut tmp[filled..FRAME_HEADER_LEN]);
                let r = match me.io_mut() {
                    Ok(io) => Pin::new(io).poll_read(cx, &mut sub),
                    Err(e) => return Poll::Ready(Err(e)),
                };
                match r {
                    Poll::Ready(Ok(())) => sub.filled().len(),
                    // 帧头读取 Err：与写 Err 同口径 fail+当次释放+sticky（P3）。
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(me.fail_read(e))),
                    Poll::Pending => return Poll::Pending,
                }
            };
            if got == 0 {
                // 底层 EOF：若内部帧头缓冲里还有 1..8B 残头，先按截断语义原样
                // 透传，下一次 poll 才报干净 EOF；绝不吞字节后假装 EOF。
                if me.in_hdr_filled > 0 {
                    me.flush_partial_inbound_header();
                    continue;
                }
                // 帧边界干净时向上层报 EOF
                return Poll::Ready(Ok(()));
            }
            me.in_hdr[filled..filled + got].copy_from_slice(&tmp[filled..filled + got]);
            me.in_hdr_filled = filled + got;
            if me.in_hdr_filled == FRAME_HEADER_LEN {
                me.classify_inbound();
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for H2KeepaliveIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if let Some(err) = me.failure_err() {
            return Poll::Ready(Err(err));
        }
        if me.stopped {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "保活适配器已关闭",
            )));
        }
        if let Err(e) = me.drive(cx) {
            return Poll::Ready(Err(e));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // 自发 GOAWAY 占用字节流（半帧已上线）：h2 的帧必须让位。waker 已在
        // GOAWAY 写出时注册，底层可写即回来。
        if me.out_goaway.is_some() {
            return Poll::Pending;
        }
        // 自发探测优先：在途半帧必须先写完，边界上的探测也必须先于本帧
        if let Pushed::NotYet(probe) = me.push_probe(cx) {
            me.inflight = Some(probe);
            return Poll::Pending;
        }
        // 探测推进期间若 GOAWAY 恰好写到了半帧，同样让位
        if me.out_goaway.is_some() {
            return Poll::Pending;
        }
        if let Some(err) = me.failure_err() {
            return Poll::Ready(Err(err));
        }
        let n = buf.len().min(MAX_WRITE_CHUNK);
        let r = match me.io_mut() {
            Ok(io) => Pin::new(io).poll_write(cx, &buf[..n]),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match r {
            Poll::Ready(Ok(0)) => {
                let kind = io::ErrorKind::BrokenPipe;
                let msg = "底层写入已关闭".to_string();
                Poll::Ready(Err(me.fail(Failure::Broken { kind, msg })))
            }
            Poll::Ready(Ok(got)) => {
                me.out_account(&buf[..got]);
                Poll::Ready(Ok(got))
            }
            Poll::Ready(Err(e)) => {
                let kind = e.kind();
                let msg = format!("底层写出失败: {e}");
                Poll::Ready(Err(me.fail(Failure::Broken { kind, msg })))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if let Some(err) = me.failure_err() {
            return Poll::Ready(Err(err));
        }
        // drive 内部已尽力推进在途/当前窗口的探测。探测帧还差字节时不能说“已
        // flush 完”：本组件仍持有必须写出的帧字节，保持 Pending（底层写就绪时
        // 已在上面的 poll_write 里注册 waker），否则调用方会误判全部字节已上线。
        if let Err(e) = me.drive(cx) {
            return Poll::Ready(Err(e));
        }
        if me.inflight.is_some() || me.out_goaway.is_some() {
            // 本组件仍持有必须写出的帧字节（探测或 GOAWAY 半帧）：不能说“已
            // flush 完”，保持 Pending，waker 已在帧写出尝试里注册。
            return Poll::Pending;
        }
        let r = match me.io_mut() {
            Ok(io) => Pin::new(io).poll_flush(cx),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match r {
            Poll::Ready(Ok(())) => {
                me.confirm_outbound_goaway_flush(true);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => {
                let kind = e.kind();
                let msg = format!("底层 flush 失败: {e}");
                Poll::Ready(Err(me.fail(Failure::Broken { kind, msg })))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if let Some(err) = me.failure_err() {
            return Poll::Ready(Err(err));
        }
        // 停止一切新探测与窗口结算：不补发历史窗口，也不再把窗口算作失败
        me.stopped = true;
        me.stats.stopped = true;
        me.timer = None;
        // 从未上线的探测直接丢弃；已经写到一半的帧必须写完，否则污染字节流
        me.pending = None;
        if me.inflight.is_some() {
            match me.push_probe(cx) {
                Pushed::NotYet(probe) => {
                    me.inflight = Some(probe);
                    return Poll::Pending;
                }
                Pushed::Failed(e) => return Poll::Ready(Err(e)),
                Pushed::Done => {}
            }
        }
        if me.io.is_none() {
            // 写探测期间进入终态：底层已释放，只报 sticky 错误
            return Poll::Ready(Err(me.failure_err().unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "保活适配器已释放底层 io")
            })));
        }
        // 自发 GOAWAY：半帧必须原样写完；已排队的请求在本次 shutdown 里尽力
        // 推进到“整帧 + flush 确认”，不把 waiter 吊死（无后台 task，靠调用方
        // 持续 poll shutdown 完成推进）。
        let goaway_queued = {
            let inner = me.shared.lock();
            inner.goaway_queued && !inner.goaway_sent
        };
        if let Err(e) = me.push_goaway(cx, goaway_queued) {
            return Poll::Ready(Err(e));
        }
        if me.out_goaway.is_some() {
            return Poll::Pending;
        }
        if let Err(e) = me.confirm_goaway_flush(cx) {
            return Poll::Ready(Err(e));
        }
        if me.goaway_flush_pending {
            // 整帧只入队：既不能宣告 shutdown 完成，也不能报 Sent。
            return Poll::Pending;
        }
        // 终态：尚未确认的 GOAWAY 不会再有机会写出，Pending waiter 唤醒为 Closed。
        me.shared.mark_closed();
        let flushed = match me.io_mut() {
            Ok(io) => Pin::new(io).poll_flush(cx),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match flushed {
            Poll::Ready(Ok(())) => {
                me.confirm_outbound_goaway_flush(true);
            }
            Poll::Ready(Err(e)) => {
                let kind = e.kind();
                let msg = format!("底层 flush 失败: {e}");
                return Poll::Ready(Err(me.fail(Failure::Broken { kind, msg })));
            }
            Poll::Pending => return Poll::Pending,
        }
        let r = match me.io_mut() {
            Ok(io) => Pin::new(io).poll_shutdown(cx),
            Err(e) => return Poll::Ready(Err(e)),
        };
        match r {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => {
                let kind = e.kind();
                let msg = format!("底层 shutdown 失败: {e}");
                Poll::Ready(Err(me.fail(Failure::Broken { kind, msg })))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> fmt::Debug for H2KeepaliveIo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 有意不打印 token 与帧内容：观察输出仅用于诊断
        f.debug_struct("H2KeepaliveIo")
            .field("role", &self.role)
            .field("windows_opened", &self.stats.windows_opened)
            .field("probes_sent", &self.stats.probes_sent)
            .field("acked_windows", &self.stats.acked_windows)
            .field("intercepted_acks", &self.stats.intercepted_acks)
            .field("consecutive_failures", &self.stats.consecutive_failures)
            .field("failed", &self.stats.failed)
            .field("stopped", &(self.stopped || self.failure.is_some()))
            .field("inner_alive", &self.io.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 永不 ready 的底层：只用于单测 token 源分支与终态所有权释放。
    /// `shutdown_calls` 用来证明“关闭整条连接”不依赖把 `poll_shutdown` 轮一次。
    struct DeadIo {
        alive: Arc<AtomicUsize>,
    }

    impl DeadIo {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let alive = Arc::new(AtomicUsize::new(1));
            (
                Self {
                    alive: alive.clone(),
                },
                alive,
            )
        }
    }

    impl Drop for DeadIo {
        fn drop(&mut self) {
            // 用 0 表示已释放（而不是布尔翻转），便于断言“只被 drop 一次”
            self.alive.store(0, Ordering::SeqCst);
        }
    }

    impl AsyncRead for DeadIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for DeadIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
    }

    fn waker() -> Context<'static> {
        Context::from_waker(std::task::Waker::noop())
    }

    const A: [u8; TOKEN_LEN] = [0xa1; TOKEN_LEN];
    const B: [u8; TOKEN_LEN] = [0xb2; TOKEN_LEN];
    const C: [u8; TOKEN_LEN] = [0xc3; TOKEN_LEN];
    const D: [u8; TOKEN_LEN] = [0xd4; TOKEN_LEN];
    const E: [u8; TOKEN_LEN] = [0xe5; TOKEN_LEN];

    fn scripted(rng_tokens: Vec<[u8; TOKEN_LEN]>) -> TokenRng {
        TokenRng::TestScript {
            tokens: rng_tokens,
            idx: 0,
        }
    }

    /// 近期 token 环必须真的用满 `TOKEN_HISTORY` 个槽位：`recent_len` 饱和增长到
    /// 4，与环形写入位置无关；环满后写入位置回绕，长度不得缩水。
    #[test]
    fn token_history_holds_four_slots() {
        let mut io = H2KeepaliveIo::with_token_rng(
            DeadIo::new().0,
            H2Role::Server,
            scripted(vec![A, B, C, D, E]),
        );
        let mut got = Vec::new();
        for _ in 0..TOKEN_HISTORY {
            got.push(io.next_token().expect("前 4 个 token 应能取到"));
        }
        assert_eq!(got, vec![A, B, C, D]);
        assert_eq!(io.recent_len, TOKEN_HISTORY, "recent_len 必须饱和增长到 4");
        assert!(io.recent_contains(&A), "最旧槽位也必须参与查重");

        // 第 5 个写入位置回绕到 0：长度仍是 4，环里保留最近 4 个
        assert_eq!(io.next_token(), Some(E));
        assert_eq!(io.recent_len, TOKEN_HISTORY, "写入位置回绕后长度不得缩水");
        assert!(io.recent_contains(&E));
        assert!(io.recent_contains(&B));
        assert!(!io.recent_contains(&A), "环里最旧的 A 应已被 E 覆盖");
    }

    /// 历史环满且随机源连续给出旧 token：重采样上限用尽即判定 token 源不可用，
    /// 并由开窗路径转成失败关闭（释放底层 + 去敏错误）。
    ///
    /// 第 4 槽必须参与查重——否则第 5 个候选 `A` 会被当成“新 token”直接放过；
    /// 早期实现只查 3 槽，正好落在这一点上。
    #[tokio::test(flavor = "current_thread")]
    async fn token_resample_exhaustion_fails_closed() {
        tokio::time::pause();
        let (lower, alive) = DeadIo::new();
        let mut io = H2KeepaliveIo::with_token_rng(
            lower,
            H2Role::Server,
            // 脚本耗尽后恒返回最后一个 A：历史环满后必然连续冲突
            scripted(vec![A, B, C, D, A]),
        );
        for _ in 0..TOKEN_HISTORY {
            assert!(io.next_token().is_some());
        }
        assert_eq!(
            io.next_token(),
            None,
            "连续冲突用尽重采样上限必须判定 token 源不可用"
        );
        // 该判定必须转成失败关闭，而不是继续用旧/可预测 token 开窗
        let err = io
            .open_window(Instant::now())
            .expect_err("token 源不可用时不得开窗");
        assert!(is_keepalive_token_error(&err), "{err:?}");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(io.stats().windows_opened, 0, "失败关闭不得记为已开窗");
        assert_eq!(
            alive.load(Ordering::SeqCst),
            0,
            "token 源不可用必须当场释放底层 io"
        );
        assert!(io.get_ref().is_none() && io.get_mut().is_none());
        // Debug 也不得含 token
        assert!(!format!("{io:?}").contains(&format!("{:02x?}", A)));
        assert!(io.into_inner().is_none(), "终态不得交还底层");
    }

    /// CSPRNG 失败：立即失败关闭 + 释放底层，绝不退化成确定性递增 token。
    #[tokio::test]
    async fn csprng_failure_closes_connection_without_deterministic_token() {
        let (lower, alive) = DeadIo::new();
        let mut io = H2KeepaliveIo::with_token_rng(
            lower,
            H2Role::Server,
            TokenRng::TestFail { remaining_ok: 0 },
        );
        let mut cx = waker();
        let err = match Pin::new(&mut io).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) => e,
            other => panic!("CSPRNG 失败必须立刻报错，实际 {other:?}"),
        };
        assert!(is_keepalive_token_error(&err), "{err:?}");
        assert!(!is_keepalive_timeout(&err), "不是死链错误");
        let st = io.stats();
        assert_eq!(st.windows_opened, 0, "取不到 token 不得开窗口");
        assert_eq!(st.probes_sent, 0, "不得写出任何探测帧");
        assert!(st.failed && st.stopped);
        assert_eq!(
            alive.load(Ordering::SeqCst),
            0,
            "CSPRNG 失败必须释放底层 io，而不是继续跑"
        );
        // 去敏：固定文案，不含 token 或底层细节
        let text = format!("{err}");
        assert!(text.contains("CSPRNG"), "{text}");
        assert!(!text.contains("0x"), "{text}");
        assert!(!text.contains(&format!("{:02x?}", E)), "{text}");
    }

    /// 会话中途 CSPRNG 失败：开窗失败即终态，不开新窗、不产生确定性 token、
    /// 不保留定时器。
    #[tokio::test(flavor = "current_thread")]
    async fn csprng_failure_mid_window_fails_closed() {
        tokio::time::pause();
        let (lower, alive) = DeadIo::new();
        let mut io = H2KeepaliveIo::with_token_rng(
            lower,
            H2Role::Server,
            TokenRng::TestFail { remaining_ok: 1 },
        );
        let mut cx = waker();
        // 第 1 窗：token 可用，窗口正常打开（底层永不 ready，探测只能 Pending）
        assert!(
            Pin::new(&mut io).poll_flush(&mut cx).is_pending(),
            "底层不 ready，flush 应 Pending"
        );
        assert_eq!(io.stats().windows_opened, 1);
        assert_eq!(io.stats().consecutive_failures, 0);

        // 第 2 窗开窗时 CSPRNG 已不可用 → 立即终态
        tokio::time::advance(KEEPALIVE_WINDOW).await;
        let err = match Pin::new(&mut io).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) => e,
            other => panic!("第二窗 CSPRNG 失败须报错，实际 {other:?}"),
        };
        assert!(is_keepalive_token_error(&err), "{err:?}");
        assert_eq!(io.stats().windows_opened, 1, "取不到 token 不得算开新窗");
        assert_eq!(
            io.stats().consecutive_failures,
            1,
            "旧窗口仍按未 ACK 计失败"
        );
        assert_eq!(alive.load(Ordering::SeqCst), 0);
        assert!(io.timer.is_none(), "终态不得保留定时器");
    }

    /// 终态四类 poll 一律 sticky，不 panic、不复活底层、不调 `poll_shutdown`。
    #[tokio::test(flavor = "current_thread")]
    async fn terminal_state_is_sticky_and_accessors_are_safe() {
        let (lower, alive) = DeadIo::new();
        let mut io = H2KeepaliveIo::with_token_rng(
            lower,
            H2Role::Server,
            TokenRng::TestFail { remaining_ok: 0 },
        );
        let mut cx = waker();
        for _ in 0..4 {
            let mut buf = [0u8; 4];
            assert!(is_keepalive_token_error(&flush_err(&mut io, &mut cx)));
            assert!(is_keepalive_token_error(&write_err(&mut io, &mut cx, b"x")));
            assert!(is_keepalive_token_error(&read_err(
                &mut io, &mut cx, &mut buf
            )));
            assert!(is_keepalive_token_error(&shutdown_err(&mut io, &mut cx)));
        }
        assert_eq!(alive.load(Ordering::SeqCst), 0, "底层必须已被释放");
        assert!(io.get_ref().is_none() && io.get_mut().is_none());
        // Debug 也不得含 token
        assert!(!format!("{io:?}").contains(&format!("{:02x?}", A)));
        assert!(
            io.into_inner().is_none(),
            "终态 into_inner 返回 None 而非 panic"
        );
    }

    fn flush_err<S: AsyncRead + AsyncWrite + Unpin>(
        io: &mut H2KeepaliveIo<S>,
        cx: &mut Context<'_>,
    ) -> io::Error {
        match Pin::new(io).poll_flush(cx) {
            Poll::Ready(Err(e)) => e,
            other => panic!("期望 Ready(Err)，实际 {other:?}"),
        }
    }

    fn write_err<S: AsyncRead + AsyncWrite + Unpin>(
        io: &mut H2KeepaliveIo<S>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> io::Error {
        match Pin::new(io).poll_write(cx, b) {
            Poll::Ready(Err(e)) => e,
            other => panic!("期望 Ready(Err)，实际 {other:?}"),
        }
    }

    fn read_err<S: AsyncRead + AsyncWrite + Unpin>(
        io: &mut H2KeepaliveIo<S>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> io::Error {
        match Pin::new(io).poll_read(cx, &mut ReadBuf::new(buf)) {
            Poll::Ready(Err(e)) => e,
            other => panic!("期望 Ready(Err)，实际 {other:?}"),
        }
    }

    fn shutdown_err<S: AsyncRead + AsyncWrite + Unpin>(
        io: &mut H2KeepaliveIo<S>,
        cx: &mut Context<'_>,
    ) -> io::Error {
        match Pin::new(io).poll_shutdown(cx) {
            Poll::Ready(Err(e)) => e,
            other => panic!("期望 Ready(Err)，实际 {other:?}"),
        }
    }
}
