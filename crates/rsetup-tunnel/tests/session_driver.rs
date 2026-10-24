//! Task4 会话驱动验收：真实 h2c over 真实 `SecureIo`（测试密钥经 `SessionKeys::derive`），
//! 字节传输用 `tokio::io::duplex` 承担，不依赖本机监听端口。
//!
//! 时间口径：全部用例运行在**单线程 + 暂停时钟**运行时。期限类断言只用
//! `sleep(期限 ± 余量)` 显式推进虚拟时钟，因此 5s 业务期限到点时 15s 探测窗口
//! 不会顺带触发；不使用 sleep(0) 竞态，也不依赖真实调度顺序。
use bytes::Bytes;
use rsetup_protocol::crypto::{RecordLayerPair, SessionKeys};
use rsetup_tunnel::envelope::{MAX_ENVELOPE_SIZE, PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::grpc::encode_message;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::driver::quarantined_callbacks;
use rsetup_tunnel::session::{
    HandlerFuture, Reply, RequestContext, RequestError, SessionCloseReason, SessionConfig,
    SessionError, SessionHandle, SessionHandler, SessionOutcome, SessionState, StreamEpoch,
    SystemSignal, start_client_session, start_server_session,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const SHARED: [u8; 32] = [7u8; 32];
const CLIENT_RANDOM: [u8; 32] = [1u8; 32];
const SERVER_RANDOM: [u8; 32] = [2u8; 32];
const CONTROL_PATH: &str = "/custom_tunnel.TunnelService/OpenControl";
const DATA_PATH: &str = "/custom_tunnel.TunnelService/OpenData";
/// duplex 缓冲需大于「单个 h2 流控窗口 + 一批 16KiB DATA 帧」，
/// 否则测的是 duplex 的反压而不是 h2 流控。
const PIPE_BUF: usize = 8 * 1024 * 1024;
const FIVE: Duration = Duration::from_secs(5);
/// 接收端并发名额耗尽时的业务状态码（与 handler 侧约定一致）。
const BUSY_STATUS: i32 = 1002;
const LONG: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- 传输夹具

/// 可控「写阻塞」开关：置位后这条 socket 的 `poll_write` 恒 Pending（等价于对端
/// 收窗为 0 / 写路径卡在内核之外），读方向照常。
///
/// 这是对 duplex 的**受控背压**注入，用来证明排空计时锚定在「真的写上线」之后；
/// 它不代表真实 TCP 的容量或 RTT，报告与断言都不得据此外推生产网络。
#[derive(Clone, Default)]
struct Pause(Arc<PauseInner>);

#[derive(Default)]
struct PauseInner {
    paused: std::sync::atomic::AtomicBool,
    /// 被按住的写方向登记的 waker，`resume` 时叫醒。
    /// 不用 `Notify::notified()` 的临时 future：它在每次 poll 结束后注销，
    /// `notify_waiters` 落在两次 poll 之间就会永久丢唤醒。
    waker: Mutex<Option<std::task::Waker>>,
}

impl Pause {
    fn blocked(&self) -> bool {
        self.0.paused.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 注册唤醒并返回能否继续写；false 表示仍被按住（下次 poll 再看）。
    fn poll_resume(&self, cx: &mut std::task::Context<'_>) -> bool {
        if !self.blocked() {
            return true;
        }
        *self.0.waker.lock().unwrap() = Some(cx.waker().clone());
        // 注册之后再查一次标志：与 `resume` 交错时不会丢唤醒。
        !self.blocked()
    }

    fn pause(&self) {
        self.0
            .paused
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn resume(&self) {
        // 先解除标志再叫醒等待者：任何一次 poll 重看都会看到 paused=false。
        self.0
            .paused
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(waker) = self.0.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

/// 记录底层 socket 是否真正释放：会话退出后 `guard` 的强引用必须只剩测试自己那一份。
struct Measured {
    inner: DuplexStream,
    /// 只作为「socket 是否还有人持有」的计数器一份，从不被读取。
    #[allow(dead_code)]
    guard: Arc<()>,
    /// 置位后这条 socket 立即表现为一根断线：读侧 EOF、写侧 BrokenPipe。
    broken: Arc<std::sync::atomic::AtomicBool>,
    /// 置位后写方向恒 Pending（读不受影响）：受控写阻塞。
    pause: Pause,
}

// DuplexStream 与 Arc 都 Unpin，包装同样 Unpin，可安全取回 &mut。
impl std::marker::Unpin for Measured {}

impl AsyncRead for Measured {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.broken.load(std::sync::atomic::Ordering::Acquire) {
            // 不填入任何字节 = 干净的 EOF，对端不会收到任何 h2 帧。
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Measured {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if me.broken.load(std::sync::atomic::Ordering::Acquire) {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "测试断线")));
        }
        if !me.pause.poll_resume(cx) {
            // 写方向被按住所持有的字节不会交给底层：这就是「GOAWAY 写不出去」。
            return Poll::Pending;
        }
        Pin::new(&mut me.inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn client_layers() -> RecordLayerPair {
    SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("测试密钥可派生")
        .into_client_record_layers()
}

fn server_layers() -> RecordLayerPair {
    SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("测试密钥可派生")
        .into_server_record_layers()
}

/// 真实「加密 + h2」会话对：中控 = h2 client，设备 = h2 server。
struct Harness {
    controller: SessionHandle,
    board: SessionHandle,
    controller_task: JoinHandle<SessionOutcome>,
    board_task: JoinHandle<SessionOutcome>,
    socket: Arc<()>,
    broken: Arc<std::sync::atomic::AtomicBool>,
    /// 两个方向各自的写阻塞开关（受控背压，不代表真实 TCP 容量）。
    controller_write: Pause,
    board_write: Pause,
}

impl Harness {
    fn build(
        controller: Arc<dyn SessionHandler>,
        board: Arc<dyn SessionHandler>,
        controller_config: SessionConfig,
        board_config: SessionConfig,
    ) -> Self {
        let (a, b) = tokio::io::duplex(PIPE_BUF);
        let guard = Arc::new(());
        let broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let controller_write = Pause::default();
        let board_write = Pause::default();
        let controller_io = SecureIo::new(
            Measured {
                inner: a,
                guard: Arc::clone(&guard),
                broken: Arc::clone(&broken),
                pause: controller_write.clone(),
            },
            client_layers(),
        );
        let board_io = SecureIo::new(
            Measured {
                inner: b,
                guard: Arc::clone(&guard),
                broken: Arc::clone(&broken),
                pause: board_write.clone(),
            },
            server_layers(),
        );
        let (controller, controller_task) =
            start_client_session(controller_io, controller, controller_config)
                .expect("中控会话启动");
        let (board, board_task) =
            start_server_session(board_io, board, board_config).expect("设备会话启动");
        Self {
            controller,
            board,
            controller_task,
            board_task,
            socket: guard,
            broken,
            controller_write,
            board_write,
        }
    }

    /// 拔掉底层 socket：不产生任何 h2 帧，等价于对端进程消失。
    fn break_socket(&self) {
        self.broken
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// 按住中控方向的写：本端任何字节都到不了对端（读不受影响）。
    fn pause_controller_writes(&self) {
        self.controller_write.pause();
    }

    fn resume_controller_writes(&self) {
        self.controller_write.resume();
    }

    /// 按住设备方向的写。
    fn pause_board_writes(&self) {
        self.board_write.pause();
    }

    fn resume_board_writes(&self) {
        self.board_write.resume();
    }

    /// 中控 owner 是否仍在运行（未退出）。
    fn controller_running(&self) -> bool {
        !self.controller_task.is_finished()
    }

    /// 设备 owner 是否仍在运行（未退出）。
    fn board_running(&self) -> bool {
        !self.board_task.is_finished()
    }

    fn build_default(controller: &Arc<TestHandler>, board: &Arc<TestHandler>) -> Self {
        Self::build(
            Arc::clone(controller).into_dyn(),
            Arc::clone(board).into_dyn(),
            SessionConfig::default(),
            SessionConfig::default(),
        )
    }

    async fn ready(&mut self) {
        let (cr, br) = tokio::join!(self.controller.wait_ready(), self.board.wait_ready());
        cr.expect("中控两条流首次健康");
        br.expect("设备两条流首次健康");
        assert_eq!(
            *self.controller.subscribe_state().borrow(),
            SessionState::Ready
        );
        assert_eq!(*self.board.subscribe_state().borrow(), SessionState::Ready);
    }

    /// 两端都以同一原因结束，且 owner 确认清理了全部任务与流两半。
    async fn finish(&mut self, why: SessionCloseReason) -> (SessionOutcome, SessionOutcome) {
        let controller = tokio::time::timeout(LONG, &mut self.controller_task)
            .await
            .expect("中控 owner 退出")
            .expect("中控 owner 不 panic");
        let board = tokio::time::timeout(LONG, &mut self.board_task)
            .await
            .expect("设备 owner 退出")
            .expect("设备 owner 不 panic");
        assert_eq!(controller.reason, why, "中控结束原因");
        assert_eq!(board.reason, why, "设备结束原因");
        assert!(controller.cleaned, "中控退出残留任务或流两半");
        assert!(board.cleaned, "设备退出残留任务或流两半");
        (controller, board)
    }
}

/// 越界配置必须直接拒绝启动，不得内部 clamp 后继续跑。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rejects_configs_that_widen_the_spec_budgets() {
    let bad_configs = [
        SessionConfig {
            ordinary_timeout: Duration::from_secs(16),
            ..SessionConfig::default()
        },
        SessionConfig {
            max_pending: 1025,
            ..SessionConfig::default()
        },
        SessionConfig {
            business_queue: 33,
            ..SessionConfig::default()
        },
        SessionConfig {
            handler_concurrency: 17,
            ..SessionConfig::default()
        },
        SessionConfig {
            drain_timeout: Duration::from_secs(4),
            ..SessionConfig::default()
        },
    ];
    for bad in bad_configs {
        let (a, _b) = tokio::io::duplex(64);
        let io = SecureIo::new(
            Measured {
                inner: a,
                guard: Arc::new(()),
                broken: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                pause: Pause::default(),
            },
            client_layers(),
        );
        let error = start_client_session(io, idle_handler(), bad).expect_err("越界配置必须被拒绝");
        assert!(
            matches!(error, SessionError::InvalidConfig(_)),
            "越界配置的结论必须是配置非法: {bad:?} -> {error:?}"
        );
    }
    let (a, _b) = tokio::io::duplex(64);
    let io = SecureIo::new(
        Measured {
            inner: a,
            guard: Arc::new(()),
            broken: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pause: Pause::default(),
        },
        client_layers(),
    );
    let (handle, mut task) =
        start_client_session(io, idle_handler(), SessionConfig::default()).expect("默认配置可用");
    handle.abort();
    drop(handle);
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Abort);
}

// ---------------------------------------------------------------- handler 夹具

/// 可释放的**同步**阻塞闸门：`wait_until_open` 用 Condvar 真实占住调用线程，
/// 不是可取消的 await——这正是「handler 的同步回调不可抢占」的真实形状。
#[derive(Default)]
struct Gate {
    changed: std::sync::Condvar,
    open: Mutex<bool>,
}

/// 兜底守卫：用例无论在哪一步失败，测试自己的守卫析构时都打开闸门。
/// 被卡住的回调持有的是同一个 `Gate`，所以这一步不做的话，失败路径会留下
/// **永久阻塞的 worker**（实测会把测试进程挂死）。
struct GateGuard(Arc<Gate>);

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.0.open();
    }
}

impl Gate {
    fn wait_until_open(&self) {
        let mut guard = self.open.lock().unwrap();
        while !*guard {
            guard = self.changed.wait(guard).unwrap();
        }
    }

    fn open(&self) {
        let mut guard = self.open.lock().unwrap();
        *guard = true;
        self.changed.notify_all();
    }
}

/// 可编程 handler：按 action 决定 echo / 未知动作 / panic / 挂起 / 固定回包。
#[derive(Clone)]
struct TestHandler {
    calls: Arc<AtomicUsize>,
    events: Arc<AtomicUsize>,
    ready: Arc<AtomicUsize>,
    epochs: Arc<Mutex<Vec<StreamEpoch>>>,
    systems: Arc<Mutex<Vec<SystemSignal>>>,
    closed: Arc<Mutex<Vec<SessionCloseReason>>>,
    gates: Arc<Mutex<Vec<oneshot::Receiver<()>>>>,
    /// 事件回调用的**同步**闸门（与 `on_closed` 同一形状）：回调由 driver 在
    /// `tokio::spawn` 的任务里同步调用，只能真实占住线程，不能用 tokio 的
    /// blocking API（见 [`TestHandler::arm_event_gate`]）。
    event_gates: Arc<Mutex<Vec<Arc<Gate>>>>,
    /// 事件回调每**进入**一次补一个许可：测试等「回调真的卡住了」时既不轮询
    /// 也不 sleep，许可累计在信号量里，先产生后等待也不会丢。
    event_entered: Arc<tokio::sync::Semaphore>,
    /// 预置一次「事件回调内真实阻塞」：模拟 handler 把回调任务钉住。
    slow_event: Arc<Mutex<Option<std::time::Duration>>>,
    /// 预置一次「on_closed 内的不可取消阻塞」：直到测试打开闸门才返回。
    closed_gate: Arc<Mutex<Option<Arc<Gate>>>>,
    /// 事件回调**进入** handler 的次数（在阻塞/panic 之前自增）：前置条件要用它
    /// 确认「回调真的开始了」，否则 sleep 只是碰运气。
    events_in: Arc<AtomicUsize>,
    /// 预置一次「on_event 内 panic」：把回调任务打死。
    panic_event: Arc<AtomicBool>,
    payload: Arc<Mutex<Option<Vec<u8>>>>,
}

impl Default for TestHandler {
    /// 逐字段初始化：`event_entered` 需要「零个许可」的起始状态，而
    /// `tokio::sync::Semaphore` 没有 `Default`（`Default` 与 `new(0)` 不同义），
    /// 所以这里不再 derive。
    fn default() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            events: Arc::new(AtomicUsize::new(0)),
            ready: Arc::new(AtomicUsize::new(0)),
            epochs: Arc::new(Mutex::new(Vec::new())),
            systems: Arc::new(Mutex::new(Vec::new())),
            closed: Arc::new(Mutex::new(Vec::new())),
            gates: Arc::new(Mutex::new(Vec::new())),
            event_gates: Arc::new(Mutex::new(Vec::new())),
            event_entered: Arc::new(tokio::sync::Semaphore::new(0)),
            slow_event: Arc::new(Mutex::new(None)),
            closed_gate: Arc::new(Mutex::new(None)),
            events_in: Arc::new(AtomicUsize::new(0)),
            panic_event: Arc::new(AtomicBool::new(false)),
            payload: Arc::new(Mutex::new(None)),
        }
    }
}

impl TestHandler {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn into_dyn(self: Arc<Self>) -> Arc<dyn SessionHandler> {
        self
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn events(&self) -> usize {
        self.events.load(Ordering::SeqCst)
    }

    fn ready_calls(&self) -> usize {
        self.ready.load(Ordering::SeqCst)
    }

    fn systems(&self) -> Vec<SystemSignal> {
        self.systems.lock().unwrap().clone()
    }

    fn closed(&self) -> Vec<SessionCloseReason> {
        self.closed.lock().unwrap().clone()
    }

    /// 固定回包载荷（覆盖 echo）：制造「小请求 -> 大回包」的写阻塞。
    fn arm_reply_payload(self: &Arc<Self>, body: Vec<u8>) {
        *self.payload.lock().unwrap() = Some(body);
    }

    /// 让下一个 `on_event` 在回调任务里真实阻塞一段时间（占住回调任务本身，
    /// 而不是可取消的 async 等待）——用来验证慢回调不能把 owner 钉死。
    fn arm_slow_events(self: &Arc<Self>, how_long: std::time::Duration) {
        *self.slow_event.lock().unwrap() = Some(how_long);
    }

    /// 预置一次「`on_closed` 内的不可取消阻塞」：返回闸门，测试打开后回调才返回。
    ///
    /// 与 [`Self::arm_slow_events`] 的区别是**不固定时长**：本用例要在回调仍卡住时
    /// 观察「原句柄有没有被丢」，再主动放行观察回收，因此不能靠 sleep 的秒数去猜。
    fn arm_closed_gate(self: &Arc<Self>) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        *self.closed_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// 预置一次「`on_event` 内 panic」：回调任务随之消亡，通知通道被关闭。
    fn arm_event_panic(self: &Arc<Self>) {
        self.panic_event.store(true, Ordering::SeqCst);
    }

    /// 事件回调是否已经**进入** handler（在阻塞/panic 之前自增）。
    fn events_entered(&self) -> usize {
        self.events_in.load(Ordering::SeqCst)
    }

    /// 预置一次「`on_event` 内真实阻塞、时长由测试决定」的闸门：回调进入后用 std
    /// 条件变量占住当前 worker，测试打开闸门后回调才返回。
    ///
    /// 为什么不能用 `tokio::sync::oneshot::Receiver::blocking_recv`（本夹具以前就这么写）：
    /// 回调是 driver 在 `tokio::spawn` 的任务里**同步**调用的，而 tokio 文档明确
    /// `blocking_recv` 在异步执行上下文中必定 panic
    /// （`Cannot block the current thread from within a runtime`），与运行时是单线程还是
    /// 多线程无关；这条 panic 会当场把回调任务打死。
    fn arm_event_gate(self: &Arc<Self>) -> Arc<Gate> {
        let gate = Arc::new(Gate::default());
        self.event_gates.lock().unwrap().push(Arc::clone(&gate));
        gate
    }

    /// 等到事件回调**累计**进入 `want` 次为止。许可由回调在进入时补发、在等待返回
    /// 时原样归还，所以信号量里的许可数就是「已进入的总次数」：本方法是门槛，
    /// 不是一次性事件，先产生后等待也不会漏掉。
    ///
    /// 本方法自身**不设期限**：在实时运行时的用例里调用必须套 `timeout(LONG, ..)`
    /// （取消安全，不会吞许可），否则链路一旦回归断掉，测试就是无限挂死而不是失败。
    async fn wait_event_entered(&self, want: usize) {
        let permits = self
            .event_entered
            .acquire_many(want.try_into().expect("等待进入的次数必须在 u32 范围内"))
            .await
            .expect("本测试从不关闭这个信号量");
        drop(permits);
    }

    fn arm_gate(self: &Arc<Self>) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        self.gates.lock().unwrap().push(rx);
        tx
    }

    async fn wait_calls(&self, want: usize) {
        for _ in 0..200 {
            if self.calls() >= want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("handler 调用数未达到 {want}，实际 {}", self.calls());
    }
}

impl SessionHandler for TestHandler {
    fn on_request(&self, ctx: RequestContext) -> HandlerFuture {
        let calls = Arc::clone(&self.calls);
        let gates = Arc::clone(&self.gates);
        let payload = Arc::clone(&self.payload);
        let epochs = Arc::clone(&self.epochs);
        epochs.lock().unwrap().push(ctx.epoch);
        let action = ctx.packet.action.clone();
        let echo = ctx.packet.payload.clone();
        let known = matches!(action.as_str(), "biz.echo" | "biz.panic" | "biz.hang");
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if !known {
                // 未注册动作：与 manager 的 1004 口径一致。
                return Reply {
                    status_code: 1004,
                    error_message: format!("unknown action {action}"),
                    payload: Bytes::new(),
                };
            }
            if action == "biz.panic" {
                panic!("handler panic");
            }
            if action == "biz.hang" {
                let gate = gates.lock().unwrap().pop();
                if let Some(gate) = gate {
                    let _ = gate.await;
                }
            }
            let body = payload.lock().unwrap().clone();
            Reply {
                status_code: 0,
                error_message: String::new(),
                payload: Bytes::from(body.unwrap_or_else(|| echo.clone())),
            }
        })
    }

    fn on_event(&self, _stream: StreamKind, _packet: TunnelPacket) {
        // 先进入计数，再决定慢/panic/放行：前置条件断言的是「回调真的开始了」。
        self.events_in.fetch_add(1, Ordering::SeqCst);
        self.event_entered.add_permits(1);
        if self.panic_event.swap(false, Ordering::SeqCst) {
            panic!("handler callback panic");
        }
        if let Some(how_long) = self.slow_event.lock().unwrap().take() {
            std::thread::sleep(how_long);
            self.events.fetch_add(1, Ordering::SeqCst);
            return;
        }
        let gate = self.event_gates.lock().unwrap().pop();
        if let Some(gate) = gate {
            // 回调由 driver 同步调用，运行在 `tokio::spawn` 的任务里：这里只能像
            // `on_closed` 那样用 std 条件变量真实占住当前 worker，模拟慢回调。
            gate.wait_until_open();
        }
        self.events.fetch_add(1, Ordering::SeqCst);
    }

    fn on_ready(&self) {
        self.ready.fetch_add(1, Ordering::SeqCst);
    }

    fn on_system(&self, signal: SystemSignal) {
        self.systems.lock().unwrap().push(signal);
    }

    fn on_closed(&self, reason: SessionCloseReason) {
        // 先记录再阻塞：测试要能区分「压根没送达」与「送达后被卡在 handler 里」。
        self.closed.lock().unwrap().push(reason);
        let gate = self.closed_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            // std 条件变量：不是 future，取消信号对它无效——同步回调不可抢占的
            // 真实形状。测试打开闸门后本回调才返回。
            gate.wait_until_open();
        }
    }
}

fn idle_handler() -> Arc<dyn SessionHandler> {
    TestHandler::new().into_dyn()
}

fn envelope(kind: PacketType, action: &str, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id: 0,
        packet_type: kind,
        action: action.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}

/// 对端 probe 的正确应答：同 trace、同 action、原样 nonce。
fn pong_of(probe: &TunnelPacket) -> TunnelPacket {
    let mut reply = envelope(PacketType::Response, &probe.action, probe.payload.clone());
    reply.trace_id = probe.trace_id;
    reply
}

// ---------------------------------------------------------------- 原始 h2 对端

/// 读写共用同一条 `SecureIo`（h2 需要单一 AsyncRead + AsyncWrite 对象）。
#[derive(Clone)]
struct SharedIo(Arc<tokio::sync::Mutex<SecureIo<DuplexStream>>>);

impl std::marker::Unpin for SharedIo {}

impl AsyncRead for SharedIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut guard = match this.0.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };
        Pin::new(&mut *guard).poll_read(cx, buf)
    }
}

impl AsyncWrite for SharedIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let mut guard = match this.0.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };
        Pin::new(&mut *guard).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut guard = match this.0.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };
        Pin::new(&mut *guard).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut guard = match this.0.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };
        Pin::new(&mut *guard).poll_shutdown(cx)
    }
}

/// 设备侧的原始 h2 客户端：用来构造会话自己绝不会发出的畸形输入。
struct RawClient {
    requests: h2::client::SendRequest<Bytes>,
    _driver: JoinHandle<()>,
}

/// 一条原始长流：写请求体、读响应体。
struct RawStream {
    sender: h2::SendStream<Bytes>,
    receiver: h2::RecvStream,
    buffer: Vec<u8>,
}

/// 建立原始通道。握手挂成任务返回，由调用方在**启动会话 server 之后**再 await，
/// 否则对端还没开始驱动，握手会僵住。
fn raw_transport(
    window: u32,
) -> (
    Pin<Box<dyn Future<Output = RawClient> + Send>>,
    SecureIo<DuplexStream>,
) {
    let (a, b) = tokio::io::duplex(PIPE_BUF);
    let shared = Arc::new(tokio::sync::Mutex::new(SecureIo::new(a, client_layers())));
    let board_io = SecureIo::new(b, server_layers());
    let handshake = Box::pin(async move {
        let (requests, connection) = h2::client::Builder::new()
            .max_frame_size(16 * 1024)
            .initial_window_size(window)
            .initial_connection_window_size(window)
            .handshake(SharedIo(shared))
            .await
            .expect("原始 h2 握手");
        let _driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        RawClient { requests, _driver }
    });
    (handshake, board_io)
}

impl RawClient {
    async fn open(&mut self, path: &str) -> RawStream {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .version(http::Version::HTTP_2)
            .uri(format!("http://rsetup{path}"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .body(())
            .expect("构造请求");
        let (response, sender) = self
            .requests
            .clone()
            .send_request(request, false)
            .expect("打开原始流");
        let receiver = response.await.expect("响应 headers").into_body();
        RawStream {
            sender,
            receiver,
            buffer: Vec::new(),
        }
    }
}

impl RawClient {
    /// 打开一条流，但对「对端立刻 RST」返回 None（违规拒绝的观测用）。
    async fn open_allow_reset(&mut self, path: &str) -> Option<RawStream> {
        let request = http::Request::builder()
            .method(http::Method::POST)
            .version(http::Version::HTTP_2)
            .uri(format!("http://rsetup{path}"))
            .header("content-type", "application/grpc+proto")
            .header("te", "trailers")
            .body(())
            .expect("构造请求");
        let (response, sender) = self
            .requests
            .clone()
            .send_request(request, false)
            .expect("打开原始流");
        match response.await {
            Ok(response) => Some(RawStream {
                sender,
                receiver: response.into_body(),
                buffer: Vec::new(),
            }),
            Err(_) => None,
        }
    }
}

impl RawStream {
    fn send(&mut self, packet: &TunnelPacket) {
        let frame = encode_message(packet).expect("编码信封");
        self.sender
            .send_data(Bytes::from(frame), false)
            .expect("写入原始包");
    }

    /// 写入**不经公共编码器**的原始 gRPC 帧：只用来构造本端 `encode_message`
    /// 永远不会产出的畸形输入（生产校验路径完全不变）。
    fn send_raw_frame(&mut self, fields: &[u8]) {
        let mut frame = Vec::with_capacity(5 + fields.len());
        frame.push(0); // gRPC 长度前缀首字节：压缩标志，必须为 0
        frame.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        frame.extend_from_slice(fields);
        self.sender
            .send_data(Bytes::from(frame), false)
            .expect("写入原始帧");
    }

    fn reset(&mut self, reason: h2::Reason) {
        self.sender.send_reset(reason);
    }

    /// 读一包 gRPC 帧；`None` 表示流已 EOF 或被重置。
    async fn recv(&mut self) -> Option<TunnelPacket> {
        loop {
            if let Some(packet) = decode_one(&mut self.buffer) {
                return Some(packet);
            }
            let data = match self.receiver.data().await {
                Some(Ok(data)) => data,
                _ => return None,
            };
            let len = data.len();
            self.buffer.extend_from_slice(&data);
            let _ = self.receiver.flow_control().release_capacity(len);
        }
    }

    /// 读到流结束（EOF 或 reset）。
    async fn wait_closed(&mut self) -> bool {
        loop {
            match self.receiver.data().await {
                Some(Ok(data)) => {
                    let len = data.len();
                    self.buffer.extend_from_slice(&data);
                    let _ = self.receiver.flow_control().release_capacity(len);
                }
                None | Some(Err(_)) => return true,
            }
        }
    }
}

/// 从 gRPC 长度前缀帧流里取出一包（消费已用字节）。
fn decode_one(buffer: &mut Vec<u8>) -> Option<TunnelPacket> {
    if buffer.len() < 5 {
        return None;
    }
    let len = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
    if buffer.len() < 5 + len {
        return None;
    }
    let bytes: Vec<u8> = buffer.drain(..5 + len).collect();
    TunnelPacket::decode(&bytes[5..]).ok()
}

/// 原始对端 + 已 Ready 的设备会话：Control/Data 都建好并回了合法 pong。
async fn raw_ready_session(
    window: u32,
) -> (
    RawClient,
    RawStream,
    RawStream,
    SessionHandle,
    JoinHandle<SessionOutcome>,
) {
    let (raw, control, data, handle, task, _board) =
        raw_ready_session_with(window, TestHandler::new(), SessionConfig::default()).await;
    (raw, control, data, handle, task)
}

/// 同上，但允许注入 handler 与会话配置（背压/门禁类用例需要改小预算）。
async fn raw_ready_session_with(
    window: u32,
    board: Arc<TestHandler>,
    config: SessionConfig,
) -> (
    RawClient,
    RawStream,
    RawStream,
    SessionHandle,
    JoinHandle<SessionOutcome>,
    Arc<TestHandler>,
) {
    let (handshake, board_io) = raw_transport(window);
    let (handle, task) = start_server_session(board_io, Arc::clone(&board).into_dyn(), config)
        .expect("设备会话启动");
    let mut raw = handshake.await;
    let mut control = raw.open(CONTROL_PATH).await;
    let control_probe = control.recv().await.expect("控制流探测");
    assert_eq!(control_probe.packet_type, PacketType::Request);
    assert_eq!(control_probe.action, "tunnel.ping");
    assert_eq!(control_probe.payload.len(), 16, "probe 必须携带 16B nonce");
    control.send(&pong_of(&control_probe));
    let mut data = raw.open(DATA_PATH).await;
    let data_probe = data.recv().await.expect("数据流探测");
    assert_eq!(data_probe.action, "tunnel.ping");
    data.send(&pong_of(&data_probe));
    tokio::time::timeout(FIVE, handle.wait_ready())
        .await
        .expect("原始对端两条流首探测成功")
        .expect("wait_ready");
    (raw, control, data, handle, task, board)
}

/// 原始 h2 server 对端：接受 client 打开的每一条流、记录 path，然后立刻 RST。
/// 用于观测「同一条流反复被重置」时的升级行为。
fn raw_reset_server() -> (Arc<Mutex<Vec<String>>>, SecureIo<DuplexStream>) {
    let (a, b) = tokio::io::duplex(PIPE_BUF);
    let session_io = SecureIo::new(a, client_layers());
    let peer_io = SecureIo::new(b, server_layers());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&paths);
    tokio::spawn(async move {
        let mut builder = h2::server::Builder::new();
        builder
            .max_frame_size(16 * 1024)
            .initial_window_size(1024 * 1024)
            .initial_connection_window_size(1024 * 1024);
        let Ok(mut conn) = builder.handshake::<_, Bytes>(peer_io).await else {
            return;
        };
        while let Some(Ok((request, response))) = conn.accept().await {
            let path = request.uri().path().to_string();
            observed.lock().unwrap().push(path);
            // 不回任何包：请求与响应发送端一起丢弃，h2 随即发出 RST_STREAM。
            drop(request);
            drop(response);
        }
    });
    (paths, session_io)
}

// ---------------------------------------------------------------- 用例

/// 双首探测 Ready + 关闭只结清一次 + 任务与 socket 全释放。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dual_first_probe_ready_and_close_exactly_once() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;
    assert_eq!(controller.ready_calls(), 1, "on_ready 只通知一次");
    assert_eq!(board.ready_calls(), 1);
    assert!(
        controller.epochs.lock().unwrap().is_empty(),
        "探测不进 handler"
    );

    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let handle = harness.controller.clone();
            tokio::spawn(async move {
                handle
                    .request(StreamKind::Data, "biz.echo", vec![i as u8])
                    .await
            })
        })
        .collect();
    for task in tasks {
        let reply = tokio::time::timeout(LONG, task)
            .await
            .expect("请求按时完成")
            .expect("调用任务不 panic")
            .expect("请求成功");
        assert_eq!(reply.payload.len(), 1);
    }

    harness.controller.abort();
    harness.board.abort();
    let (controller_out, board_out) = harness.finish(SessionCloseReason::Abort).await;
    assert!(board_out.probes_served > 0, "对端探测应在本端计数");
    assert!(controller_out.probes_served > 0, "反方向同样在本端计数");
    assert_eq!(
        controller_out.probe_missions, 0,
        "对端每次都回了合法 pong，不应有失守的探测窗口"
    );
    assert_eq!(board_out.probe_missions, 0);
    assert_eq!(controller.closed().len(), 1, "on_closed 恰好一次");
    assert_eq!(board.closed().len(), 1);
    assert_eq!(
        Arc::strong_count(&harness.socket),
        1,
        "底层 io 必须随会话释放"
    );
}

/// 双向 RPC（含设备反向请求中控）+ 32KiB 大包含流控 + 未知动作 1004。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn bidirectional_rpc_and_32k_flow_control() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.echo", b"ping".to_vec())
        .await
        .expect("中控请求");
    assert_eq!(reply.status_code, 0);
    assert_eq!(reply.payload.as_ref(), b"ping");

    // 设备 → 中控：同一条流反向。
    let reply = harness
        .board
        .request(StreamKind::Data, "biz.echo", b"pong".to_vec())
        .await
        .expect("设备请求");
    assert_eq!(reply.payload.as_ref(), b"pong");

    // 控制流同样可跑业务。
    let reply = harness
        .controller
        .request(StreamKind::Control, "biz.echo", b"ctl".to_vec())
        .await
        .expect("控制流业务");
    assert_eq!(reply.payload.as_ref(), b"ctl");

    // 32KiB 大包：超过 16KiB 帧上限，必须靠 h2 流控分片往返。
    let big = vec![0x5a_u8; 32 * 1024];
    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.echo", big.clone())
        .await
        .expect("32KiB 请求");
    assert_eq!(reply.payload.len(), big.len());
    assert!(reply.payload.starts_with(&big[..2048]));

    // 未注册动作 → 固定未知码，payload 为空。
    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.nope", Vec::new())
        .await
        .expect("未知动作仍有响应");
    assert_eq!(reply.status_code, 1004);
    assert!(reply.payload.is_empty());

    harness.controller.abort();
    harness.board.abort();
    harness.finish(SessionCloseReason::Abort).await;
}

/// 超过信封上限的请求：直接 ProtocolMismatch，会话不受影响。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn oversized_request_is_rejected_without_harming_session() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;
    let big = vec![0u8; 1024 * 1024];
    let error = harness
        .controller
        .request(StreamKind::Data, "biz.echo", big)
        .await
        .expect_err("超过 512KiB 必须拒绝");
    assert_eq!(error, RequestError::ProtocolMismatch);
    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.echo", b"still ok".to_vec())
        .await
        .expect("会话仍然可用");
    assert_eq!(reply.payload.as_ref(), b"still ok");
    harness.controller.abort();
    harness.board.abort();
    harness.finish(SessionCloseReason::Abort).await;
}

/// 未决预算：max_pending=2 时业务名额为 0（1022+2 口径的极值）→ 立刻 Busy；
/// 两个预留名额仍被两条流的 probe 使用，会话保持 Ready。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pending_budget_reserves_probe_slots() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let config = SessionConfig {
        max_pending: 2,
        ..SessionConfig::default()
    };
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        config,
        config,
    );
    harness.ready().await;
    let error = harness
        .controller
        .request(StreamKind::Data, "biz.echo", Vec::new())
        .await
        .expect_err("业务名额应为 probe 让出");
    assert_eq!(error, RequestError::Busy);
    assert_eq!(
        *harness.controller.subscribe_state().borrow(),
        SessionState::Ready
    );
    harness.controller.abort();
    harness.board.abort();
    let (controller_out, _) = harness.finish(SessionCloseReason::Abort).await;
    assert!(
        controller_out.probes_served > 0,
        "预留名额确实被 probe 使用"
    );
}

/// 5s 期限从入队起算：handler 只 1 个并发名额，第二条排在业务队列里，
/// 必须在距离**自己入队** 5s 时 Timeout，且不得重发。
/// 接收端处理队列必须有界：并发名额占满时立即回 busy，绝不无限排队。
/// 而「业务超时从入队起算」由 `stalled_writer_still_times_out` 覆盖
/// （那条用例让包真的滞留在本地业务队列里）。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn handler_budget_exhaustion_answers_busy() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    // 应答侧（中控）只留 1 个 handler 名额。
    let answering_config = SessionConfig {
        handler_concurrency: 1,
        ..SessionConfig::default()
    };
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        answering_config,
        SessionConfig::default(),
    );
    harness.ready().await;

    // 扣住第一个 handler：唯一并发名额被占住。
    let gate = controller.arm_gate();
    let hanging = {
        let handle = harness.board.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.hang", Vec::new())
                .await
        })
    };
    controller.wait_calls(1).await;

    let reply = harness
        .board
        .request(StreamKind::Data, "biz.echo", Vec::new())
        .await
        .expect("忙响应仍是一次合法的 RPC 往返");
    assert_eq!(
        reply.status_code, BUSY_STATUS,
        "名额耗尽必须回 busy，不能静默排队: {reply:?}"
    );
    assert!(!reply.error_message.is_empty(), "busy 必须带可诊断原因");

    // 挂起的请求由调用方自己的 5s 期限结清，且不得被重放进 handler。
    let first = tokio::time::timeout(LONG, hanging)
        .await
        .expect("挂起的请求由期限结清")
        .expect("调用任务不 panic");
    assert_eq!(first.err(), Some(RequestError::Timeout));
    assert_eq!(
        controller.calls(),
        1,
        "超时的请求不得被重发或再次进入 handler"
    );

    drop(gate);
    harness.board.abort();
    harness.controller.abort();
    harness.finish(SessionCloseReason::Abort).await;
}

/// 业务写阻塞不暂停期限也不拖住 owner：原始对端把窗口压到 16KiB 且不再读，
/// 设备侧 32KiB 响应卡在 h2 流控里；5s 到点调用方必须拿到 Timeout，会话不被误杀。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stalled_writer_still_times_out() {
    let (raw, control, data, board, mut task) = raw_ready_session(16 * 1024).await;
    let big = vec![0x33_u8; 32 * 1024];
    let stalled = {
        let handle = board.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let result = handle.request(StreamKind::Data, "biz.echo", big).await;
            (result, started.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(5_100)).await;
    let (result, elapsed) = tokio::time::timeout(LONG, stalled)
        .await
        .expect("写阻塞期间期限照样到点")
        .expect("调用任务不 panic");
    // 期限已在入队后 5s 到点：这里必须几乎立刻拿到结果，
    // 不能等「写阻塞解除」再判定（本例中它永远不会解除）。
    assert!(
        elapsed >= FIVE && elapsed < FIVE + Duration::from_millis(200),
        "期限必须从入队起算，实测 {:?}",
        elapsed
    );
    assert_eq!(
        result.err(),
        Some(RequestError::Timeout),
        "写阻塞不得返回成功或悬空"
    );
    assert_ne!(
        *board.subscribe_state().borrow(),
        SessionState::Closed,
        "业务写阻塞不是连接故障"
    );
    board.abort();
    drop(raw);
    drop(control);
    drop(data);
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

/// midstream GOAWAY：响应被 handler 扣住时 graceful_shutdown，GOAWAY 立即发出，
/// 排空期内响应照样送达（不等全部完成）；排空 >=5s，两端结论明确。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn midstream_goaway_keeps_inflight_response_and_drains_five_seconds() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let gate = board.arm_gate();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    let inflight = {
        let handle = harness.controller.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.hang", b"in flight".to_vec())
                .await
        })
    };
    board.wait_calls(1).await;

    let started = tokio::time::Instant::now();
    harness.controller.graceful_shutdown();
    assert_eq!(
        *harness.controller.subscribe_state().borrow_and_update(),
        SessionState::Draining,
        "graceful_shutdown 必须同步把状态推到 Draining"
    );
    let rejected = harness
        .controller
        .request(StreamKind::Data, "biz.echo", Vec::new())
        .await
        .expect_err("排空期不得接受新业务");
    assert!(matches!(
        rejected,
        RequestError::Closed | RequestError::StreamClosed
    ));

    // GOAWAY 后、排空期限内才放行已受理请求的响应。last_stream_id 针对对端
    // 发起的流，不能据此把正常存量响应丢失当作预期；此处必须拿到精确载荷。
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(
        *harness.board.subscribe_state().borrow_and_update(),
        SessionState::Draining,
        "对端看到 GOAWAY 必须进入 Draining（此时仍在排空窗口内）"
    );
    gate.send(()).expect("放行 handler");
    let inflight = tokio::time::timeout(LONG, inflight)
        .await
        .expect("在途请求必须被结清")
        .expect("调用任务不 panic");
    let reply = inflight.expect("已受理请求必须在健康连接的 GOAWAY 排空窗口内成功响应");
    assert_eq!(reply.payload.as_ref(), b"in flight");

    let controller_out = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("中控 owner 在排空期限内退出")
        .expect("中控 owner 不 panic");
    let elapsed = started.elapsed();
    assert!(elapsed >= FIVE, "GOAWAY 后排空至少 5s，实测 {elapsed:?}");
    assert_eq!(controller_out.reason, SessionCloseReason::Local);
    assert!(controller_out.cleaned);

    let board_out = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备 owner 在排空期限内退出")
        .expect("设备 owner 不 panic");
    assert_eq!(board_out.reason, SessionCloseReason::PeerGoaway);
    assert!(board_out.cleaned);
}

/// 一侧 socket 结束：本端结论是传输故障，on_closed 恰好一次。
/// 对端的 h2 连接优雅结束时会先写 GOAWAY：本端据此给出 PeerGoaway，
/// 而不是谎报成传输故障（这与「EOF 之前没有任何帧」是两种结论）。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn peer_h2_close_reports_peer_goaway() {
    let (handshake, board_io) = raw_transport(1024 * 1024);
    let board = TestHandler::new();
    let (_handle, board_task) = start_server_session(
        board_io,
        Arc::clone(&board).into_dyn(),
        SessionConfig::default(),
    )
    .expect("设备会话启动");
    let mut raw = handshake.await;
    let mut control = raw.open(CONTROL_PATH).await;
    let probe = control.recv().await.expect("控制流探测");
    assert_eq!(probe.action, "tunnel.ping");
    drop(raw);
    drop(control);
    let outcome = tokio::time::timeout(LONG, board_task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert_eq!(outcome.reason, SessionCloseReason::PeerGoaway);
    assert!(outcome.cleaned);
    assert_eq!(board.closed().len(), 1, "on_closed 恰好一次");
}

/// 底层 socket 直接断掉：字节流里没有任何 GOAWAY，结论只能是 Transport。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn broken_socket_reports_transport() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    let pending = {
        let handle = harness.board.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", b"eof".to_vec())
                .await
        })
    };
    harness.break_socket();

    let first = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("断线后中控必须退出")
        .expect("中控 owner 不 panic");
    assert_eq!(first.reason, SessionCloseReason::Transport);
    assert!(first.cleaned);
    let second = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("断线后设备必须退出")
        .expect("设备 owner 不 panic");
    assert_eq!(second.reason, SessionCloseReason::Transport);
    assert!(second.cleaned);

    // 断线期间的在途请求被明确结清一次，不靠期限再等 5s。
    let result = tokio::time::timeout(LONG, pending)
        .await
        .expect("在途请求必须被结清")
        .expect("调用任务不 panic");
    // 连接级故障：两条流的未决一次性结清为 Closed（不是逐流 StreamClosed）。
    assert_eq!(result.err(), Some(RequestError::Closed));
    assert_eq!(controller.closed().len(), 1, "on_closed 恰好一次");
    assert_eq!(board.closed().len(), 1);
    assert_eq!(
        Arc::strong_count(&harness.socket),
        1,
        "断线退出后仍有人持有底层 socket"
    );
}

/// handler panic 只回固定 1009、清任务，会话继续服务。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn handler_panic_answers_1009_and_keeps_session() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;
    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.panic", Vec::new())
        .await
        .expect("panic 也必须有响应");
    assert_eq!(reply.status_code, 1009);
    let reply = harness
        .controller
        .request(StreamKind::Data, "biz.echo", b"after panic".to_vec())
        .await
        .expect("panic 后会话仍可用");
    assert_eq!(reply.payload.as_ref(), b"after panic");
    harness.controller.abort();
    harness.board.abort();
    let (controller_out, board_out) = harness.finish(SessionCloseReason::Abort).await;
    assert_eq!(board_out.handler_panics, 1, "panic 计数在设备侧");
    assert!(controller_out.cleaned && board_out.cleaned);
}

/// 关停意图不得搭业务队列的车：`graceful_shutdown` 与 `abort` 走独立的
/// 单调 watch，容量 1 的命令通道被打满时依然生效，且 owner 必须真的继续驱动
/// 排空（发出 GOAWAY、走满 >=5s），而不是只把状态改成 Draining。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn shutdown_and_abort_survive_saturated_command_queue() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    // 如实记录饱和来源：cmd 通道容量是 `business_queue.max(4)`（本例 4，不是 1），
    // 而每条流的业务队列 biz_tx 容量才是 business_queue = 1。因此观察到的 Busy
    // 可能来自 cmd 通道满，也可能来自 biz_tx 满——两者都是有界背压真的打满；
    // max_pending 取默认值，用来排除「未决表配额」这一来源。
    let saturated = SessionConfig {
        business_queue: 1,
        ..SessionConfig::default()
    };
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        SessionConfig::default(),
        saturated,
    );
    harness.ready().await;

    // 64 个并发调用挤满容量 4 的 cmd 通道；观察到 Busy 的那次调用里，
    // graceful_shutdown/abort 与 Busy 之间没有 await，所以 abort 落在队列仍然打满
    // 的时刻（旧实现会把它静默丢掉）。注意本用例断言的是「打满时关停意图仍然生效、
    // owner 仍能退出」，并不声称已经证明「不修必然失败」——具体调度顺序不由测试独占。
    let busy = Arc::new(AtomicUsize::new(0));
    let mut hammer = Vec::new();
    for _ in 0..64 {
        let handle = harness.board.clone();
        let busy = Arc::clone(&busy);
        hammer.push(tokio::spawn(async move {
            let result = handle
                .request(StreamKind::Data, "biz.echo", b"hammer".to_vec())
                .await;
            if result.as_ref().err() == Some(&RequestError::Busy) {
                busy.fetch_add(1, Ordering::SeqCst);
                handle.graceful_shutdown();
                handle.abort();
            }
            result
        }));
    }
    let mut ok_count = 0usize;
    let mut results = Vec::new();
    for task in hammer {
        let result = tokio::time::timeout(LONG, task)
            .await
            .expect("调用方不得悬空")
            .expect("调用任务不 panic");
        if result.is_ok() {
            ok_count += 1;
        }
        results.push(result);
    }
    assert!(
        busy.load(Ordering::SeqCst) > 0,
        "容量 4 的 cmd 通道 + 容量 1 的业务队列，64 并发必须观察到打满"
    );

    let owner = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("队列打满时 abort 仍必须让 owner 退出")
        .expect("owner 不 panic");
    assert_eq!(owner.reason, SessionCloseReason::Abort);
    assert!(owner.cleaned);

    // 没有伪造应答：每个成功回执都要能对上一次真实的 handler 调用
    // （必要条件检查，不是「已证明未伪造」的充分证据）。
    assert!(
        controller.calls() >= ok_count,
        "成功回执 {} 次，真实 handler 调用 {} 次",
        ok_count,
        controller.calls()
    );
    for result in &results {
        if let Err(error) = result {
            assert!(
                matches!(
                    error,
                    RequestError::Busy
                        | RequestError::Closed
                        | RequestError::StreamClosed
                        | RequestError::Timeout
                ),
                "结清只能是明确错误: {error:?}"
            );
        }
    }

    // 本端已经真的关闭：对端会话必须看到连接消失，而不是等到自己的期限。
    let peer = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("对端必须看到连接消失")
        .expect("owner 不 panic");
    assert!(
        matches!(
            peer.reason,
            SessionCloseReason::Transport | SessionCloseReason::PeerGoaway
        ),
        "对端结论必须来自真实连接消失: {:?}",
        peer.reason
    );
    assert_eq!(
        Arc::strong_count(&harness.socket),
        1,
        "退出后仍有人持有底层 socket"
    );
}

/// 最后一个句柄消失 = 终止意图：命令通道打满也要生效，并且必须真的
/// 关掉同一条 socket（原始对端看到 EOF），且不留下任何伪造回执。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn last_handle_drop_really_closes_the_socket() {
    let (handshake, board_io) = raw_transport(1024 * 1024);
    let board = TestHandler::new();
    let saturated = SessionConfig {
        business_queue: 1,
        ..SessionConfig::default()
    };
    let (handle, task) = start_server_session(board_io, Arc::clone(&board).into_dyn(), saturated)
        .expect("设备会话启动");
    let mut raw = handshake.await;
    let mut control = raw.open(CONTROL_PATH).await;
    let control_probe = control.recv().await.expect("控制流探测");
    control.send(&pong_of(&control_probe));
    let mut data = raw.open(DATA_PATH).await;
    let data_probe = data.recv().await.expect("数据流探测");
    data.send(&pong_of(&data_probe));
    tokio::time::timeout(FIVE, handle.wait_ready())
        .await
        .expect("首探测成功")
        .expect("wait_ready");

    let busy = Arc::new(AtomicUsize::new(0));
    let mut hammer = Vec::new();
    for _ in 0..64 {
        let handle = handle.clone();
        let busy = Arc::clone(&busy);
        hammer.push(tokio::spawn(async move {
            let result = handle
                .request(StreamKind::Data, "biz.echo", b"hammer".to_vec())
                .await;
            if result.as_ref().err() == Some(&RequestError::Busy) {
                busy.fetch_add(1, Ordering::SeqCst);
            }
            drop(handle);
            result
        }));
    }
    for task in hammer {
        let _ = tokio::time::timeout(LONG, task)
            .await
            .expect("调用方不得悬空")
            .expect("调用任务不 panic");
    }
    assert!(busy.load(Ordering::SeqCst) > 0, "必须打满命令通道");

    // 最后一个句柄消失：owner 必须自己收场。
    drop(handle);
    let outcome = tokio::time::timeout(LONG, task)
        .await
        .expect("最后一个句柄消失后 owner 必须退出")
        .expect("owner 不 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Abort);
    assert!(outcome.cleaned);

    // 真实 socket 证据：原始对端在两条流上都读到结束，且没收到任何伪造回执。
    assert!(
        tokio::time::timeout(FIVE, data.wait_closed())
            .await
            .expect("对端必须看到 Data 流随连接结束"),
        "Data 流必须结束"
    );
    assert!(
        tokio::time::timeout(FIVE, control.wait_closed())
            .await
            .expect("对端必须看到 Control 流随连接结束"),
        "Control 流必须结束"
    );
    assert_eq!(board.calls(), 0, "被挤掉的请求不得在退出后被补发或伪造应答");
}

// ---------------------------------------------------------------------------- 补充矩阵

/// 构造一个原始信封（对端主动发包用）。
fn raw_packet(kind: PacketType, action: &str, payload: &[u8], trace_id: u64) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: kind,
        action: action.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: payload.to_vec(),
        metadata: BTreeMap::new(),
    }
}

/// 针对某个 Request 生成 RESPONSE 信封（同 trace、同 action）。
fn response_of(request: &TunnelPacket, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id: request.trace_id,
        packet_type: PacketType::Response,
        action: request.action.clone(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}

/// Event 是单向的：不建未决项、不等响应，因此未决预算耗尽时仍然可发。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn event_is_one_way_and_needs_no_pending_slot() {
    let board = TestHandler::new();
    // 未决上限压到 2：两条流的首探测已占满，业务必须 Busy。
    let tight = SessionConfig {
        max_pending: 2,
        ..SessionConfig::default()
    };
    let (_raw, mut control, mut data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), tight).await;

    let busy = handle
        .request(StreamKind::Data, "biz.echo", b"x".to_vec())
        .await;
    assert_eq!(
        busy.err(),
        Some(RequestError::Busy),
        "未决预算耗尽时业务必须 Busy"
    );

    handle
        .send_event(StreamKind::Data, "board.metric", b"42".to_vec())
        .await
        .expect("Event 不占未决名额");
    let packet = data.recv().await.expect("Event 必须上线");
    assert_eq!(packet.packet_type, PacketType::Event);
    assert_eq!(packet.action, "board.metric");
    assert_eq!(packet.payload, b"42");
    assert_ne!(packet.trace_id, 0, "Event 也必须带非零 trace");

    handle
        .send_event(StreamKind::Control, "board.state", b"a".to_vec())
        .await
        .expect("控制流同样可发 Event");
    let on_control = control.recv().await.expect("控制流 Event 上线");
    assert_eq!(on_control.packet_type, PacketType::Event);

    // 排空之后不再接受新的 Event。
    handle.graceful_shutdown();
    let late = handle
        .send_event(StreamKind::Data, "board.metric", b"1".to_vec())
        .await;
    assert_eq!(
        late.err(),
        Some(RequestError::Closed),
        "排空中不得再注入新 Event"
    );
    handle.abort();
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

/// 事件队列有界：回调被钉住时的入站洪峰按计数丢弃，不撑爆内存、不拖垮会话。
///
/// 三处同步都不靠 sleep 猜，也不放宽 `Busy`：
/// * 第一条事件先走完整链路，测试等到「回调已进入并卡在闸门里」才发洪峰；
/// * 洪峰只有 9 条，整体塞得进发送侧业务队列（默认容量 32）：以前一次灌 64 条，
///   「发送侧 Event 入队」实际上是在赌 writer 有没有被调度到，机器一紧就被回 `Busy`
///   （CI 上正是这样），而本用例要观察的只有接收侧的丢弃；
/// * 收尾在**同一条 Data 流**上做一次 echo 往返作 happens-before 屏障，链条是：
///   洪峰的每条 Event 帧都在设备侧 Data 流唯一的业务发送队列里入队（`send_event`
///   的 ack 即入队完成）；echo 的 Request 在洪峰全部 ack 之后才由中控发出，所以
///   设备 echo 的回包帧必然排在同一条 Data 流队列的 9 条事件帧**之后**，经同一条
///   半流 FIFO 上线；中控只有一个 reader、一个 owner 循环，按上线顺序逐帧处理，
///   回包被结清时 9 条事件的入站分支（入队或丢弃计数）已全部是既成事实。
///   方向核验：屏障的因果锚在**回包帧与事件帧共享设备侧同一发送队列与同一条
///   Data 半流**上，与 Request 命令方向相反是设计使然（实测：按住设备写方向后
///   回包确定到不了，放开后计数立即精确；去掉屏障 50 次单核跑 42 次计数不足）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_event_burst_is_dropped_with_counter() {
    // 接收侧回调通道总容量 = event_queue + notify_queue；钉住的回调另外占着 1 条。
    const EVENT_QUEUE: u8 = 1;
    const NOTIFY_QUEUE: u8 = 1;
    const IN_CALLBACK: u64 = 1;
    const BURST: u8 = 9;

    let controller = TestHandler::new();
    let board = TestHandler::new();
    let tight = SessionConfig {
        event_queue: EVENT_QUEUE as usize,
        notify_queue: NOTIFY_QUEUE as usize,
        ..SessionConfig::default()
    };
    // 发送侧（设备）用的是默认配置：洪峰必须能整体塞进它的业务队列，否则
    // 「发送侧 Event 入队」这条期望就变成在赌 writer 有没有被调度到。
    assert!(
        usize::from(BURST) <= SessionConfig::default().business_queue,
        "洪峰 {} 不得超过发送侧业务队列容量 {}",
        BURST,
        SessionConfig::default().business_queue
    );
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        tight,
        SessionConfig::default(),
    );
    harness.ready().await;
    // 前置条件：`on_ready` 这条生命周期通知已被回调消费掉，回调通道是空的——
    // 下面那个精确的丢弃期望依赖这一点。
    wait_until("on_ready 必须已送达 handler", || {
        controller.ready_calls() >= 1
    })
    .await;

    // 第一个事件把回调任务钉住：之后的事件只能在有界队列门口被丢弃。
    let gate = controller.arm_event_gate();
    // 兜底：用例在任何一步失败都要开闸，否则钉住的 worker 永不归还（实测会挂死进程）。
    let gate_guard = GateGuard(Arc::clone(&gate));

    harness
        .board
        .send_event(StreamKind::Data, "board.metric", vec![1u8; 8])
        .await
        .expect("首个事件入队");
    // 有界等待：本用例是实时多线程运行时，没有暂停时钟兜底。信号量门槛一旦等不到
    // （回归丢事件），必须按 LONG 期限失败快退，而不是把整个测试进程挂死。
    // `acquire_many` 取消安全：超时丢弃等待不会吞掉已发放的许可。
    tokio::time::timeout(LONG, controller.wait_event_entered(1))
        .await
        .expect("事件回调必须在期限内进入闸门");
    assert_eq!(controller.events(), 0, "前置条件：回调必须还卡在闸门里");

    for i in 2..=BURST {
        harness
            .board
            .send_event(StreamKind::Data, "board.metric", vec![i; 8])
            .await
            .expect("发送侧 Event 入队");
    }

    // 同流往返 = 接收侧 owner 的屏障：回包一到，9 条事件都已过完入站分支。
    let echo = harness
        .controller
        .request(StreamKind::Data, "biz.echo", b"flush".to_vec())
        .await
        .expect("同流 echo 往返必须成功");
    assert_eq!(echo.status_code, 0, "echo 回包状态码");
    assert_eq!(echo.payload, Bytes::from_static(b"flush"), "echo 原样回显");

    assert_eq!(
        *harness.controller.subscribe_state().borrow_and_update(),
        SessionState::Ready,
        "事件丢弃不是会话故障"
    );
    assert_eq!(
        controller.events_entered(),
        1,
        "只有第一条事件进得了被钉住的回调"
    );
    assert_eq!(
        controller.events(),
        0,
        "被钉住的回调不应继续消费事件: {}",
        controller.events()
    );

    gate.open();
    drop(gate_guard);
    wait_until("闸门打开后被钉住的回调必须返回", || {
        controller.events() >= 1
    })
    .await;

    harness.board.abort();
    harness.controller.abort();
    let owner = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("中控 owner 退出")
        .expect("owner 不 panic");
    // 9 条入站事件：1 条被钉住的回调持有 + 通道里压着 2 条（容量 = event_queue +
    // notify_queue），其余必须在队列门口被真实丢弃并计数。
    let expected_dropped = u64::from(BURST) - IN_CALLBACK - u64::from(EVENT_QUEUE + NOTIFY_QUEUE);
    assert_eq!(
        owner.dropped_events,
        expected_dropped,
        "丢弃计数必须是接收侧真实计数：{} 发 - {} 在回调里 - {} 在队",
        BURST,
        IN_CALLBACK,
        EVENT_QUEUE + NOTIFY_QUEUE
    );
    assert!(owner.cleaned);
}

/// System 只允许出现在控制流上；发出后整条连接收场，不走 5s 排空。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn system_signal_uses_control_stream_only() {
    let board = TestHandler::new();
    let (_raw, mut control, _data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    handle
        .send_system(SystemSignal::Kick)
        .await
        .expect("kick 入队（控制流优先通道）");
    let packet = control.recv().await.expect("System 必须出现在控制流");
    assert_eq!(packet.packet_type, PacketType::System);
    assert_eq!(packet.action, "tunnel.kick");
    assert!(packet.payload.is_empty(), "System 载荷必须为空");
    assert_ne!(packet.trace_id, 0);

    // 发出方不因此自杀：kick/revoke 的收场义务在**收到**信令的一侧。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        *handle.subscribe_state().borrow_and_update(),
        SessionState::Ready,
        "发出 System 的会话必须继续服务"
    );
    handle.abort();
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

/// 入站 System：回调拿到信令，随后两侧会话都收场。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn inbound_system_signal_reaches_handler_and_ends_session() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    harness
        .controller
        .send_system(SystemSignal::Revoke)
        .await
        .expect("revoke 入队");

    let peer = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备 owner 必须随 System 收场")
        .expect("owner 不 panic");
    assert_eq!(peer.reason, SessionCloseReason::Revoked);
    assert_eq!(board.systems(), vec![SystemSignal::Revoke]);
    assert_eq!(board.closed().len(), 1, "on_closed 恰好一次");
    // 发起侧不因为「自己发出了 revoke」而自杀：它的结论来自对端真的消失。
    let sender = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("发起侧 owner 也必须收场")
        .expect("owner 不 panic");
    assert_eq!(sender.reason, SessionCloseReason::Transport);
    assert!(sender.cleaned);
    assert_eq!(
        Arc::strong_count(&harness.socket),
        1,
        "System 收场后不得留着 socket"
    );
}

/// 业务流上出现 TYPE_SYSTEM 属于协议违规：结论是 Protocol，不是静默忽略。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn system_packet_on_data_stream_is_protocol_violation() {
    let board = TestHandler::new();
    let (_raw, _control, mut data, _handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    data.send(&raw_packet(
        PacketType::System,
        "tunnel.kick",
        b"",
        0x0000_0000_0000_ABCD,
    ));

    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("违规 System 必须让会话收场")
        .expect("owner 不 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Protocol);
    assert!(outcome.cleaned);
    assert!(board.systems().is_empty(), "违规信令不得交给业务回调");
}

/// Data 必须先于 Control 之后建立：先来 Data 一律 RST，会话保持等待。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn data_stream_before_control_is_rejected() {
    let (handshake, board_io) = raw_transport(1024 * 1024);
    let board = TestHandler::new();
    let (handle, mut task) = start_server_session(
        board_io,
        Arc::clone(&board).into_dyn(),
        SessionConfig::default(),
    )
    .expect("设备会话启动");
    let mut raw = handshake.await;
    assert!(
        raw.open_allow_reset(DATA_PATH).await.is_none(),
        "对端未先建 Control 时必须拒绝这条 Data 流"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), handle.wait_ready())
            .await
            .is_err(),
        "没有 Control 不得判 Ready"
    );
    assert_ne!(
        *handle.subscribe_state().borrow(),
        SessionState::Closed,
        "违规的流不该把整条会话判死"
    );

    drop(raw);
    drop(handle);
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
    assert_eq!(board.closed().len(), 1);
}

/// 单流 RST 只结清那条流的未决：另一条流的请求仍要能拿到响应。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn single_stream_reset_settles_only_that_stream() {
    let board = TestHandler::new();
    let (_raw, mut control, mut data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    let on_data = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", b"d".to_vec())
                .await
        })
    };
    let on_control = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Control, "biz.echo", b"c".to_vec())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    data.reset(h2::Reason::NO_ERROR);
    let data_result = tokio::time::timeout(LONG, on_data)
        .await
        .expect("被重置流的未决必须结清")
        .expect("调用任务不 panic");
    assert_eq!(
        data_result.err(),
        Some(RequestError::StreamClosed),
        "只有被重置那条流的未决允许被结清"
    );

    let packet = control.recv().await.expect("控制流业务请求上线");
    assert_eq!(packet.packet_type, PacketType::Request);
    control.send(&response_of(&packet, b"c".to_vec()));
    let control_result = tokio::time::timeout(LONG, on_control)
        .await
        .expect("Control 流的请求不受影响")
        .expect("调用任务不 panic")
        .expect("Control 流请求必须成功");
    assert_eq!(&control_result.payload[..], &b"c"[..]);
    assert_eq!(board.calls(), 0, "原始对端没有 handler，回调计数必须为零");

    handle.abort();
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

/// 同一条流反复被重置：三次重开后升级，绝不无限重开。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn repeated_resets_escalate_to_stream_flapping() {
    let (paths, session_io) = raw_reset_server();
    let (handle, task) = start_client_session(session_io, idle_handler(), SessionConfig::default())
        .expect("中控会话启动");

    // 每次开流失败要等满 15s 开流窗口才计一次重开失败，三次才升级。
    let outcome = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("反复重置必须升级收场")
        .expect("owner 不 panic");
    assert_eq!(outcome.reason, SessionCloseReason::StreamFlapping);
    assert!(outcome.cleaned);
    let seen = paths.lock().unwrap().clone();
    assert_eq!(seen[0], CONTROL_PATH, "必须先建 Control 再建 Data");
    assert!(seen.len() >= 3, "至少要看到三次重开: {seen:?}");
    drop(handle);
}

/// 没人等的响应（过期或伪造 trace）必须被静默丢弃，会话继续可用。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unknown_response_trace_is_dropped_without_harming_session() {
    let board = TestHandler::new();
    let (_raw, mut control, mut data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    control.send(&raw_packet(
        PacketType::Response,
        "biz.echo",
        b"stale",
        0xFFFF_FFFF_FFFF_0001,
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        *handle.subscribe_state().borrow_and_update(),
        SessionState::Ready,
        "无人认领的响应不得影响会话"
    );

    let call = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", b"live".to_vec())
                .await
        })
    };
    let packet = data.recv().await.expect("真实请求上线");
    data.send(&response_of(&packet, b"live".to_vec()));
    let reply = tokio::time::timeout(LONG, call)
        .await
        .expect("真实往返完成")
        .expect("调用任务不 panic")
        .expect("往返成功");
    assert_eq!(&reply.payload[..], &b"live"[..]);

    handle.abort();
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

// ================================================================ 首轮修复补测
//
// 以下用例逐条对应独立审查 finding（F1..F8，见
// docs/testing/tunnel-session-driver-report.md）。它们都是**先 RED 后修**的用例：
// 记录的是「规范说了什么、当时的实现差在哪」，不是把实现行为重新定义成期望。

/// 按 protobuf wire 手工拼一条 **非法探测**：
/// `08 <trace>`（字段1 trace_id，varint）+ `10 01`（字段2 type=TYPE_REQUEST）+
/// `1a 0b tunnel.ping`（字段3 action）+ `32 <len> <bytes>`（字段6 payload）。
/// 唯一「非法」之处是 payload 不是 §6.7.2 规定的恰好 16B：公共
/// `TunnelPacket::encode`/`encode_message` 会先在 `validate_fields` 里拒掉它，
/// 所以这里只能手工拼可解释的 wire 字节。生产侧的 decode/预算/校验一律不变。
fn illegal_ping_wire(trace_id: u64, nonce: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    fields.push(0x08);
    let mut value = trace_id;
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            fields.push(byte);
            break;
        }
        fields.push(byte | 0x80);
    }
    fields.extend_from_slice(&[0x10, 0x01]);
    fields.push(0x1a);
    fields.push(11);
    fields.extend_from_slice(b"tunnel.ping");
    fields.push(0x32);
    fields.push(nonce.len() as u8);
    fields.extend_from_slice(nonce);
    fields
}

/// 有界等待某个观测点成立（每次让出 1ms 虚拟时间）。
async fn wait_until(evidence: &str, mut observed: impl FnMut() -> bool) {
    for _ in 0..5_000 {
        if observed() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("观测点 {evidence} 在 5s 虚拟期限内未成立");
}

/// F2：非法探测请求（payload 不是 16B）按 §6.7.2 **只终止它所在的那条流**：
/// 本流未决结清、代际推进后可重开；另一条流继续完成真实往返；会话既不
/// Closed 也不 Draining，非法包更不会被当成「已服务的探测」代答。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn illegal_probe_resets_only_that_stream() {
    let board = TestHandler::new();
    let (mut raw, mut control, mut data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    // 非法包所在流上先挂一个真实请求：它的未决必须随本流结清。
    let inflight = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Control, "biz.echo", b"c".to_vec())
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    control.send_raw_frame(&illegal_ping_wire(0x1234, b"short"));

    assert!(
        tokio::time::timeout(FIVE, control.wait_closed())
            .await
            .is_ok(),
        "非法探测必须终止它所在的那条流"
    );
    let settled = tokio::time::timeout(LONG, inflight)
        .await
        .expect("本流未决必须结清，不得悬空")
        .expect("调用任务不 panic");
    let settled_with = settled.err();
    assert!(
        matches!(
            settled_with,
            Some(RequestError::StreamClosed) | Some(RequestError::Closed)
        ),
        "本流未决只能按流关闭结清，实际 {settled_with:?}"
    );

    let state = *handle.subscribe_state().borrow_and_update();
    assert_ne!(
        state,
        SessionState::Closed,
        "非法探测不是连接级故障，不得把整条会话判死"
    );
    assert_ne!(state, SessionState::Draining, "非法探测也不得触发排空");

    // 另一条流完全不受影响：一次真实往返必须拿到精确载荷。
    let other = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", b"live".to_vec())
                .await
        })
    };
    let packet = data.recv().await.expect("Data 流业务请求必须继续上线");
    assert_eq!(packet.action, "biz.echo");
    data.send(&response_of(&packet, b"live".to_vec()));
    let reply = tokio::time::timeout(LONG, other)
        .await
        .expect("另一条流不受本流违规影响")
        .expect("调用任务不 panic")
        .expect("Data 流往返必须成功");
    assert_eq!(&reply.payload[..], b"live");

    // 代际已推进：原始对端重开 Control 后必须看到新代际的首个 16B 探测。
    let mut control2 = raw.open(CONTROL_PATH).await;
    let probe = tokio::time::timeout(FIVE, control2.recv())
        .await
        .expect("重开的 Control 流必须收到探测")
        .expect("探测包");
    assert_eq!(probe.action, "tunnel.ping");
    assert_eq!(probe.payload.len(), 16, "重开后的探测仍是恰好 16B nonce");
    control2.send(&pong_of(&probe));
    tokio::time::timeout(FIVE, handle.wait_ready())
        .await
        .expect("重开并 pong 后必须重新 Ready")
        .expect("wait_ready");

    handle.abort();
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
    assert_eq!(
        outcome.probes_served, 0,
        "本端只发探测不收探测：非法探测更不得被代答或计入已服务探测"
    );
}

/// F1（client 主动）：本地优雅关闭的 ≥5s 排空必须锚定在 **GOAWAY 真的写出并
/// flush 之后**。写方向被按住期间即使早已过满 5s 也不得退出；解除后从真正上线
/// 一刻起再满 5s；已受理请求在排空窗内必须拿到精确载荷。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn local_graceful_drain_is_anchored_at_actual_goaway_flush() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let gate = board.arm_gate();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    let inflight = {
        let handle = harness.controller.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.hang", b"in flight".to_vec())
                .await
        })
    };
    board.wait_calls(1).await;

    // 写方向按住：GOAWAY 只能停在本地，一个字节都没上线。
    harness.pause_controller_writes();
    let requested = tokio::time::Instant::now();
    harness.controller.graceful_shutdown();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        harness.controller_running(),
        "GOAWAY 从未上线就退出，等于把「请求发 GOAWAY」当成「已排空」"
    );
    assert_eq!(
        *harness.board.subscribe_state().borrow_and_update(),
        SessionState::Ready,
        "写阻塞期间对端绝不能看到 GOAWAY"
    );

    // 恢复写：GOAWAY 这才真的上线。以**对端观察到**为计时观察点。
    harness.resume_controller_writes();
    wait_until("对端已进入 Draining", || {
        *harness.board.subscribe_state().borrow_and_update() == SessionState::Draining
    })
    .await;
    let flushed = tokio::time::Instant::now();

    // 排空窗内已受理请求必须完成，且载荷精确。
    gate.send(()).expect("放行 handler");
    let reply = tokio::time::timeout(LONG, inflight)
        .await
        .expect("在途请求必须结清")
        .expect("调用任务不 panic")
        .expect("已受理请求必须在排空窗内成功");
    assert_eq!(reply.payload.as_ref(), b"in flight");

    tokio::time::sleep(Duration::from_millis(4_500)).await;
    assert!(
        harness.controller_running(),
        "GOAWAY 上线后必须再维持至少 drain_timeout（5s）；旧实现按「请求发 GOAWAY」\
         起算，此时早已退出"
    );
    let owner = tokio::time::timeout(FIVE, &mut harness.controller_task)
        .await
        .expect("上线后 5s 期限内必须退出")
        .expect("owner 不 panic");
    let since_flush = flushed.elapsed();
    // 观察点（对端读到 GOAWAY）最多比真正的 flush 晚一个轮询间隔，因此留出带宽；
    // 关键是它绝不在 5s 之前，也绝不拖成第二个 5s。
    assert!(
        since_flush >= Duration::from_millis(4_950) && since_flush <= Duration::from_millis(5_400),
        "排空必须从实际 flush 起算 5s，实测上线后 {since_flush:?}"
    );
    assert!(
        requested.elapsed() < Duration::from_secs(12),
        "排空总时长必须仍是有限值，实测 {:?}",
        requested.elapsed()
    );
    assert_eq!(owner.reason, SessionCloseReason::Local);
    assert!(owner.cleaned);

    let peer = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("对端必须随连接消失收场")
        .expect("owner 不 panic");
    assert_eq!(peer.reason, SessionCloseReason::PeerGoaway);
    assert_eq!(controller.closed().len(), 1, "on_closed 恰好一次");
}

/// F1（client 主动、写永久阻塞）：发不出 GOAWAY 不能无限等。必须在有界期限内
/// 收场，并且**不得谎报 Local**（本端从未把 GOAWAY 写上线）。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unwritable_goaway_fails_bounded_without_claiming_local() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    harness.pause_controller_writes();
    let requested = tokio::time::Instant::now();
    harness.controller.graceful_shutdown();

    let owner = tokio::time::timeout(Duration::from_secs(30), &mut harness.controller_task)
        .await
        .expect("GOAWAY 永久写不出去时必须有界收场")
        .expect("owner 不 panic");
    let elapsed = requested.elapsed();
    assert_ne!(
        owner.reason,
        SessionCloseReason::Local,
        "GOAWAY 一个字节都没上线，结论绝不能是本地优雅排空"
    );
    assert!(
        elapsed <= Duration::from_secs(15),
        "有界失败不得拖成第二个 5s 以上量级，实测 {elapsed:?}"
    );
    assert!(owner.cleaned, "有界失败同样必须确认无残留");

    // 解除写阻塞，让设备侧也收场，不给测试留悬挂任务。
    harness.resume_controller_writes();
    let peer = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备侧必须收场")
        .expect("owner 不 panic");
    assert!(
        matches!(
            peer.reason,
            SessionCloseReason::Transport | SessionCloseReason::PeerGoaway
        ),
        "对端结论来自连接真的消失：{:?}",
        peer.reason
    );
}

/// F8 + F1（server 主动）：设备侧 `graceful_shutdown` 走 h2 官方 graceful，同样
/// 必须以「GOAWAY 真的写上线」为排空计时起点；在途请求在排空窗内必须拿到精确
/// 响应载荷，绝不能靠 Closed/Timeout 放行。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn board_graceful_shutdown_drains_after_actual_goaway_flush() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let gate = board.arm_gate();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    // 在途请求由设备侧 handler 处理，被 gate 挂住。
    let inflight = {
        let handle = harness.controller.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.hang", b"in flight".to_vec())
                .await
        })
    };
    board.wait_calls(1).await;

    harness.board.graceful_shutdown();
    // 中控观察到 GOAWAY == 设备侧 GOAWAY 已经写上线并被对端读到。
    wait_until("中控已进入 Draining", || {
        *harness.controller.subscribe_state().borrow_and_update() == SessionState::Draining
    })
    .await;
    let flushed = tokio::time::Instant::now();

    gate.send(()).expect("放行 handler");
    let reply = tokio::time::timeout(LONG, inflight)
        .await
        .expect("在途请求必须结清")
        .expect("调用任务不 panic")
        .expect("server 主动排空期间在途请求必须成功");
    assert_eq!(reply.payload.as_ref(), b"in flight");

    tokio::time::sleep(Duration::from_millis(4_500)).await;
    assert!(
        harness.board_running(),
        "server 侧同样必须从 GOAWAY 上线起维持至少 5s"
    );
    let owner = tokio::time::timeout(FIVE, &mut harness.board_task)
        .await
        .expect("server 侧必须在上线后 5s 期限内退出")
        .expect("owner 不 panic");
    let since_flush = flushed.elapsed();
    assert!(
        since_flush >= Duration::from_millis(4_950) && since_flush <= Duration::from_millis(5_400),
        "server 侧排空必须从实际 flush 起算 5s，实测上线后 {since_flush:?}"
    );
    assert_eq!(owner.reason, SessionCloseReason::Local);
    assert!(owner.cleaned);

    let peer = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("中控必须收场")
        .expect("owner 不 panic");
    assert_eq!(peer.reason, SessionCloseReason::PeerGoaway);
    assert_eq!(board.closed().len(), 1, "on_closed 恰好一次");
}

/// F3：byte 预算是**整连接一份**，不是每条流一份（config 的注释本来就写着两条流共享）。
///
/// 机制：对端只声明 16KiB 的流窗口，控制流上先压一个 400KB 请求，writer 卡在
/// h2 流控里，那条预留（400000+1024）就一直占着整连接配额（下限 512KiB）；
/// 此时**另一条流**上的同尺寸包必须 Busy。若每条流/每代际各自新建一份配额，
/// 第二个包就会被放行——那等于把承诺的上限偷偷乘以流数与重开次数。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn byte_budget_is_one_per_connection_not_per_stream() {
    let board = TestHandler::new();
    let tight = SessionConfig {
        business_queue_bytes: MAX_ENVELOPE_SIZE,
        ..SessionConfig::default()
    };
    let (raw, control, data, handle, mut task, _board) =
        raw_ready_session_with(16 * 1024, Arc::clone(&board), tight).await;

    let stuck = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", vec![0x33_u8; 400_000])
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    let other = handle
        .send_event(StreamKind::Control, "board.metric", vec![1u8; 200_000])
        .await;
    assert!(
        matches!(other, Err(RequestError::Busy)),
        "两条流必须共用同一份 byte 预算，实测 {other:?}"
    );

    handle.abort();
    drop(raw);
    drop(control);
    drop(data);
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
    let _ = tokio::time::timeout(LONG, stuck).await;
}

/// F4：回包因预算/队列耗尽被丢弃时，必须留下**独立**的 sanitized 计数，
/// 既不混进 `dropped_events`，也不伪造「已送达」——对端的请求只能按自己的期限结清。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropped_replies_are_counted_not_faked() {
    let board = TestHandler::new();
    // 小请求 -> 固定 400KB 回包；对端只给 16KiB 流窗口，第一个回包卡在流控里，
    // 整连接配额（下限 512KiB）就此占满。
    board.arm_reply_payload(vec![0x5a_u8; 400_000]);
    let tight = SessionConfig {
        business_queue_bytes: MAX_ENVELOPE_SIZE,
        ..SessionConfig::default()
    };
    let (raw, control, mut data, handle, mut task, _board) =
        raw_ready_session_with(16 * 1024, Arc::clone(&board), tight).await;

    data.send(&raw_packet(
        PacketType::Request,
        "biz.echo",
        b"first",
        7_001,
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    data.send(&raw_packet(
        PacketType::Request,
        "biz.echo",
        b"second",
        7_002,
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(board.calls(), 2, "两个请求都必须真的进 handler");

    handle.abort();
    drop(raw);
    drop(control);
    drop(data);
    let board_out = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert_eq!(
        board_out.dropped_replies, 1,
        "第二个 echo 回包必须被计入 dropped_replies，不得静默丢弃"
    );
    assert_eq!(board_out.dropped_events, 0, "回包丢弃不得混进事件丢弃计数");
    assert!(board_out.cleaned);
}

/// F1 的 server 侧镜像：**GOAWAY 一个字节都上不了线时，绝不宣称本地优雅排空**。
///
/// 设备的写方向被完全按住（对端再也收不到任何字节）：此时 `graceful_shutdown()`
/// 只能在「GOAWAY 实际 flush」的有界等待到期后转成非 Local 结论；
/// 旧实现按「请求发 GOAWAY」起算 5s，会谎称 Local，甚至根本写不出 GOAWAY。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn board_graceful_without_writable_goaway_is_not_local() {
    let controller = TestHandler::new();
    let board = TestHandler::new();
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        SessionConfig::default(),
        SessionConfig::default(),
    );
    harness.ready().await;

    harness.pause_board_writes();
    let started = tokio::time::Instant::now();
    harness.board.graceful_shutdown();

    let outcome = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("GOAWAY 写不出去也必须有界退出")
        .expect("owner 不 panic");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= FIVE && elapsed < Duration::from_secs(12),
        "必须给 GOAWAY 一整段 drain_timeout，又必须有界，实测 {elapsed:?}"
    );
    assert_ne!(
        outcome.reason,
        SessionCloseReason::Local,
        "GOAWAY 一个字节都没上线，结论绝不能是本地优雅排空"
    );
    assert!(outcome.cleaned, "有界失败路径也要如实清理");

    // 放开写方向，让底层结清走正常关闭路径（而不是被永久按住的写门）。
    harness.resume_board_writes();
    // 中控一侧从未收到 GOAWAY：它只会因底层断裂而结清，不会误判成对端优雅关停。
    drop(harness.board);
    harness.controller.abort();
    let controller_out = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("中控 owner 退出")
        .expect("owner 不 panic");
    assert_ne!(
        controller_out.reason,
        SessionCloseReason::PeerGoaway,
        "对端从未收到 GOAWAY，不得被记成对端优雅关停"
    );
}

/// F2 的健康核验半边：**错误 nonce 的 pong 不得刷新健康**。
///
/// 对照组（数据流）每个窗口都正确代答 => 一直健康；实验组（控制流）只回一个
/// 「trace/action 都对、nonce 被改坏一个字节」的形似合法 pong => 该窗口拿不到
/// 有效证据，按**既有** ProbeState 连续 3 个未答窗口升级重开（不新建第二套状态机），
/// 而另一条流照常完成精确载荷的往返。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn wrong_nonce_pong_does_not_refresh_health() {
    let board = TestHandler::new();
    let (raw, mut control, mut data, handle, mut task, _board) =
        raw_ready_session_with(1024 * 1024, Arc::clone(&board), SessionConfig::default()).await;

    // 第二个探测窗口：Control 回错 nonce，Data 回正确 nonce。
    let mut probes = Vec::new();
    let window_deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while probes.len() < 2 {
        tokio::select! {
            packet = control.recv() => probes.push((StreamKind::Control, packet.expect("控制流探测"))),
            packet = data.recv() => probes.push((StreamKind::Data, packet.expect("数据流探测"))),
            _ = tokio::time::sleep_until(window_deadline) => panic!(
                "20s 内没有凑齐第二个探测窗口（已收 {} 个）",
                probes.len()
            ),
        }
    }
    let mut wrong_answered = 0;
    let mut right_answered = 0;
    for (kind, probe) in probes {
        assert_eq!(probe.action, "tunnel.ping");
        let mut pong = pong_of(&probe);
        if kind == StreamKind::Control {
            // 只把 nonce 改坏一个字节：trace 与 action 都对，形似合法 pong。
            pong.payload[0] ^= 0xff;
            control.send(&pong);
            wrong_answered += 1;
        } else {
            data.send(&pong);
            right_answered += 1;
        }
    }
    assert_eq!((wrong_answered, right_answered), (1, 1));

    // 之后 Data 继续正确代答，Control 一律不回 pong。
    // 连续 3 个未答窗口后控制流必须按既有 ProbeState 重开（对端看到本流被终结）。
    let mut control_dead = false;
    let limit = tokio::time::Instant::now() + Duration::from_secs(120);
    while tokio::time::Instant::now() < limit {
        if let Ok(maybe) = tokio::time::timeout(Duration::from_secs(4), data.recv()).await {
            match maybe {
                Some(probe) => {
                    assert_eq!(probe.action, "tunnel.ping", "数据流只应送来探测");
                    right_answered += 1;
                    data.send(&pong_of(&probe));
                }
                None => panic!("数据流一直正确代答，不该被判死"),
            }
        }
        // Control 上的探测一律不作答（等价于错 nonce 的窗口效果）；
        // recv() 拿到 None 只可能是本流被 EOF/RST 终结。
        let polled = tokio::time::timeout(Duration::from_secs(4), control.recv()).await;
        if matches!(polled, Ok(None)) {
            control_dead = true;
            break;
        }
    }
    assert!(
        control_dead,
        "错误 pong 却刷新了健康：控制流未按既有 ProbeState 升级重开（正确代答 {right_answered} 次）"
    );
    assert!(
        right_answered >= 3,
        "数据流必须一直在正确代答，实测 {right_answered} 次"
    );

    // 另一条流不受影响：仍然完成一次载荷精确的真实往返。
    let state = *handle.subscribe_state().borrow_and_update();
    assert_ne!(state, SessionState::Closed, "单流重开不是连接故障");
    let other = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .request(StreamKind::Data, "biz.echo", b"live".to_vec())
                .await
        })
    };
    let packet = loop {
        let packet = data.recv().await.expect("数据流仍可上线");
        if packet.action != "tunnel.ping" {
            break packet;
        }
        data.send(&pong_of(&packet));
    };
    assert_eq!(packet.action, "biz.echo");
    data.send(&response_of(&packet, b"live".to_vec()));
    let reply = tokio::time::timeout(LONG, other)
        .await
        .expect("数据流不受控制流重开影响")
        .expect("调用任务不 panic")
        .expect("数据流往返必须成功");
    assert_eq!(&reply.payload[..], b"live");

    handle.abort();
    drop(raw);
    drop(control);
    let outcome = tokio::time::timeout(LONG, &mut task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(outcome.cleaned);
}

// ------------------------------------------------ 进程级追踪表用例的串行夹具

// `quarantined_callbacks()` 读的是**进程级**追踪表（`OnceLock<Mutex<CallbackRegistry>>`），
// 而下面 3 个用例对它做精确计数断言（0 → 1 → 0）。`make test` 默认并行跑整个文件：
// `stuck_callback_*` 的 6s 同步 sleep 在 owner 有界退出**之后**仍占着一个名额，另外两例的
// `== 0` 前置/收尾断言撞上的正是这段残留（本轮之前只靠命令行 `--test-threads=1` 规避，
// 等于把正确性寄托在调用参数上）。
//
// 因此这 3 个用例在测试内部显式串行：
// - 只串行这 3 个：其余用例的回调全程只占 `Reserved` 名额（确认了结即归还，不进入
//   `tracked()` 计数，`cleaned == true` 走的就是这一路），彼此不干扰，无需 33 个一起串行；
// - 用 `tokio::sync::Mutex`：各用例跑在各自独立的 runtime 上，跨 runtime 唤醒可靠，且失败时
//   不中毒（`std::sync::Mutex` 中毒会让后续用例级联报错，掩盖真正的失败原因）；
// - **释放互斥前必须确认自己那份阻塞已经结束**（闸门已开 / sleep 已走完、表回到 0）。owner
//   一返回就 drop guard，等于把同一份残留甩给下一个用例，串行就白做了；
// - 互斥只存在于本测试文件，不进生产，也不新增任何生产侧 test setter。
static QUARANTINE_TABLE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 进入用例时等追踪表回到基线的预算。正常情况下上一个持有者已自行清场，第一跳即成立；
/// 预算只兜住「上一个用例**自身失败**后留下的、终将自然结束」的回调。真出现永不结束的回调
/// （=真实泄漏）会到期直接报错，所以这不是把 `== 0` 放宽成「迟早会是 0」。
const QUARANTINE_BASELINE: Duration = Duration::from_secs(10);

/// 用例收尾时等**自己那份**回调结束的预算：必须大于 `stuck_callback_*` 的 6s 慢回调
/// （该时长是旧 F6 敏感性的前提，不得为了省时间缩短）。
const QUARANTINE_DRAIN: Duration = Duration::from_secs(30);

/// 有界等待进程级追踪表**恰好**等于 `expected`（真实时钟，每 20ms 观察一次；
/// `quarantined_callbacks()` 自身会顺带 `prune` 已确认结束的句柄，不需要额外驱动）。
/// 期限与 `wait_until` 的 5s 分开：慢回调用例的回收窗口本来就长于 5s。
async fn wait_quarantined_is(expected: usize, budget: Duration, evidence: &str) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        let current = quarantined_callbacks();
        if current == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{evidence}：期限内追踪表仍为 {current}，期望 {expected}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// F6：回调任务被 handler 卡住、通知队列已满时，owner 仍必须**有界**退出，
/// 并对「确认不了的残留」如实给出 `cleaned=false`（旧实现会无界等 notify 入队，
/// 等回调跑完才走，等于把会话生命周期交给一次慢回调）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stuck_callback_exits_bounded_and_reports_cleaned_honestly() {
    // 本用例会往进程级追踪表留下一个未确认名额，故与另两例显式串行（见上面的夹具注释）。
    let _serial = QUARANTINE_TABLE.lock().await;
    wait_quarantined_is(0, QUARANTINE_BASELINE, "前置条件：追踪表起始为空").await;
    let controller = TestHandler::new();
    // 第一个事件把回调任务真实阻塞 6s：长于 SYSTEM_GRACE(0.5s)+CLEANUP_GRACE(2s)。
    controller.arm_slow_events(Duration::from_millis(6_000));
    let board = TestHandler::new();
    // 接收侧回调通道总容量 = event_queue + notify_queue = 2：1 条被慢回调持有、2 条压进
    // 通道，第 4 条起**必然**被丢弃。「队列真的被打满」由容量与发送条数钉死，不再依赖
    // 「发送侧挤进去多少条」这种调度偶然（旧形状：`let _ =` 吞掉 Busy 后实际入网的条数
    // 只有 ≤ 发送管线容量，可以一条都不丢）。
    const EVENT_QUEUE: usize = 1;
    const NOTIFY_QUEUE: usize = 1;
    const IN_CALLBACK: u64 = 1;
    const BURST: u8 = 5;
    // 洪峰必须整体塞得进发送侧默认业务队列，每条 send_event 才能可靠拿到入队 ack。
    assert!(
        usize::from(BURST) <= SessionConfig::default().business_queue,
        "洪峰 {} 不得超过发送侧业务队列容量 {}",
        BURST,
        SessionConfig::default().business_queue
    );
    let tight = SessionConfig {
        event_queue: EVENT_QUEUE,
        notify_queue: NOTIFY_QUEUE,
        ..SessionConfig::default()
    };
    let mut harness = Harness::build(
        Arc::clone(&controller).into_dyn(),
        Arc::clone(&board).into_dyn(),
        tight,
        SessionConfig::default(),
    );
    harness.ready().await;

    // 第一个事件走完整链路进入 handler，在那里被真实阻塞 6s，从此不腾出消费者名额。
    harness
        .board
        .send_event(StreamKind::Data, "board.metric", vec![0u8; 8])
        .await
        .expect("第一个事件先入队");
    // 真实屏障而不是 sleep(200ms) 赌「这会儿回调应该开始了」：进入许可由回调在**进入
    // handler 时**补发，等到它即证明回调确实已占住消费者。本用例跑在实时运行时上，
    // 屏障自身必须套 timeout(LONG)（`acquire_many` 取消安全），回归时快退而不是挂死。
    tokio::time::timeout(LONG, controller.wait_event_entered(1))
        .await
        .expect("慢回调必须在期限内进入 handler");
    assert_eq!(controller.events(), 0, "前置条件：回调必须还卡在 6s 阻塞里");

    // 其余事件逐条发送并确认入队（不吞 Busy）：入队条数、接收侧通道容量与被钉住的
    // 回调一起把「必然有丢弃」变成确定结论。
    for i in 2..=BURST {
        harness
            .board
            .send_event(StreamKind::Data, "board.metric", vec![i; 8])
            .await
            .expect("发送侧 Event 入队");
    }

    // 同一条 Data 流上的 echo 往返 = 接收侧 owner 的 happens-before 屏障（与
    // `inbound_event_burst_is_dropped_with_counter` 已核验的链条同一条）：echo 回包帧
    // 在设备侧 Data 流同一个业务发送队列里排在洪峰事件帧**之后**、经同一条半流 FIFO
    // 上线，而中控只有一个 owner 按上线顺序逐帧处理——回包被结清时，每条洪峰事件的
    // 入站分支（入队或丢弃计数）都已是既成事实。丢弃计数从这一刻起才是可读证据，
    // 不再赌「发送循环跑完时 owner 恰好处理够了」。
    let echo = harness
        .controller
        .request(StreamKind::Data, "biz.echo", b"flush".to_vec())
        .await
        .expect("同流 echo 往返必须成功");
    assert_eq!(echo.status_code, 0, "echo 回包状态码");

    // 此刻：通知队列满 + 回调任务被 handler 钉住，且计数已随屏障结清。旧实现在这里
    // `notify_tx.send(Closed).await` 无限等下去，只能等慢回调跑完才走。
    harness.controller.abort();
    let started = std::time::Instant::now();
    let owner = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("慢回调不能把 owner 钉死")
        .expect("owner 不 panic");
    let elapsed = started.elapsed();
    // 5 条洪峰：1 条被慢回调持有 + 2 条压在通道里，其余必然在队列门口被真实计数。
    let expected_dropped = u64::from(BURST) - IN_CALLBACK - (EVENT_QUEUE + NOTIFY_QUEUE) as u64;
    assert!(
        owner.dropped_events > 0,
        "前置条件：通知队列必须真的被打满（否则本用例没测到东西）"
    );
    assert_eq!(
        owner.dropped_events,
        expected_dropped,
        "丢弃计数必须是接收侧真实计数：{} 发 - {} 在回调里 - {} 在队",
        BURST,
        IN_CALLBACK,
        EVENT_QUEUE + NOTIFY_QUEUE
    );
    assert!(
        elapsed < Duration::from_millis(4_000),
        "整个 notify+join 阶段必须有界（旧实现要等 6s 的慢回调跑完），实测 {elapsed:?}"
    );
    assert!(
        !owner.cleaned,
        "回调任务无法确认结束时必须 cleaned=false，绝不谎称已清理"
    );
    assert_eq!(
        controller.closed().len(),
        0,
        "on_closed 没送达就是没送达，不得伪造；正常路径的「恰好一次」由其它用例守住"
    );
    // 此刻那个确认不了的原句柄**确实还挂在追踪表里**（6s 的同步 sleep 还在跑）。
    // 这既是「不谎称已清理」的可核对证据，也是下面收尾必须等它结束才放锁的原因。
    wait_quarantined_is(
        1,
        QUARANTINE_BASELINE,
        "慢回调未确认结束期间必须仍占着追踪名额",
    )
    .await;

    // 另一侧照常收场，测试不留悬挂任务。
    harness.board.abort();
    let board_out = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert!(board_out.cleaned);

    // 放锁前的收口：owner 早在 6s sleep 跑完前就返回了（那正是本用例要证明的有界性），
    // 名额却还占着。必须等回调自己跑完、表回到 0 再释放互斥，否则残留照旧会撞下一个用例
    // 的 `== 0`。代价如实记录：本用例墙钟时长由「与 owner 退出重叠」变为「等满 6s」。
    wait_quarantined_is(
        0,
        QUARANTINE_DRAIN,
        "收口：本用例留下的慢回调必须结束并回收名额",
    )
    .await;
}

/// F6 收口（Low-1）：回调是**不可取消**的同步代码、owner 的确认期限到点时，
/// 原回调句柄必须**转交进程级追踪表**继续追踪（丢弃 `JoinHandle` = detach），
/// owner 仍然有界返回并如实 `cleaned=false`；回调真的跑完后名额回收回 0，
/// 且已完成的回调不再扣着 handler。
///
/// 口径（不得夸大）：本用例证明的是「有界退出 + 不丢句柄 + 可回收」，**不是**强制取消
/// ——被卡住的 handler 代码不会因为 owner 退出而停下来，它只是继续被追踪。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uncancellable_callback_is_tracked_then_reclaimed() {
    // 追踪表是进程级名额，本用例断言精确计数，故与另两例显式串行（见夹具注释）：
    // 上一例（6s 慢回调）在 owner 有界退出后仍占名额，默认并行时那就是本用例的 `== 0`。
    let _serial = QUARANTINE_TABLE.lock().await;
    wait_quarantined_is(0, QUARANTINE_BASELINE, "前置条件：追踪表起始为空").await;
    let controller = TestHandler::new();
    // on_closed 里用 std 条件变量真实阻塞：不是 future，取消信号对它无效。
    let gate = controller.arm_closed_gate();
    let _gate_guard = GateGuard(Arc::clone(&gate));
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    harness.controller.abort();
    let started = std::time::Instant::now();
    let owner = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("同步回调不可抢占，但 owner 必须按期限返回")
        .expect("owner 不 panic");
    let elapsed = started.elapsed();
    // 上界口径：通知 ≤0.5s + 完成信号 ≤0.5s + cleanup ≤2s + 回调确认 ≤2s ≈ 5s，
    // 再加调度余量。旧实现也是「有界」的，但它靠**丢弃未确认的句柄**换到有界。
    assert!(
        elapsed < Duration::from_millis(4_500),
        "owner 退出必须有界，实测 {elapsed:?}"
    );
    assert_eq!(
        controller.closed().len(),
        1,
        "on_closed 已送达，此刻正被卡在 handler 里"
    );
    assert!(
        !owner.cleaned,
        "确认不了结束就必须 cleaned=false，绝不谎称已清理"
    );
    assert_eq!(
        quarantined_callbacks(),
        1,
        "超时的原回调句柄必须仍在追踪表里（丢弃句柄=detach）"
    );
    let held = Arc::strong_count(&controller);
    assert_eq!(
        held, 2,
        "未确认结束期间回调任务仍在跑，必须仍持有它捕获的 handler（实测 {held} 份）"
    );

    // 放行 → 回调真的返回 → 任务被确认结束：名额回收，捕获的 handler 也释放。
    // 这一步同时是**释放互斥的前置条件**：闸门没开就放锁，等于把残留交给下一个用例。
    gate.open();
    wait_quarantined_is(
        0,
        QUARANTINE_DRAIN,
        "回调返回后追踪表必须回收回 0（不后台轮询，靠下一次观察/启动/退出 prune）",
    )
    .await;
    // 任务被标记完成后，runtime 才 drop 它的 future（连同捕获的 handler Arc）：
    // 这一步晚于 `is_finished()`，因此按有界轮询等它落地，而不是 sleep 猜时长。
    let mut handler_released = false;
    for _ in 0..500 {
        handler_released = Arc::strong_count(&controller) == 1;
        if handler_released {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        handler_released,
        "已完成的回调不得继续扣着 handler（实测仍有 {} 份引用）",
        Arc::strong_count(&controller)
    );

    harness.board.abort();
    let board_out = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert!(board_out.cleaned);

    // 放锁前的收口：闸门已开（失败路径由 `GateGuard` 兜底开闸，不留永久卡死的 worker）、
    // 本用例的回调已确认结束、表回到 0，才把这份全局状态交给下一个用例。
    wait_quarantined_is(0, QUARANTINE_BASELINE, "收口：本用例的回调必须已确认结束").await;
}

/// 投递判定按**真实结果**收紧（F6 附带）：`Ok(Err(..))`（接收端已关闭=回调任务早已
/// 消亡）不是「on_closed 已送达」。owner 必须先去**确认**任务结局，再给 `cleaned`；
/// 确认得了就不占追踪名额，确认不了就转交。
///
/// 敏感性如实标注：本用例在新旧实现上都绿（旧实现也在 `Ok(Err(_))` 分支判「已了结」），
/// 它 pin 的是收紧后的语义与「名额不泄漏」，不单独作为本轮修复的敏感性证据。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_callback_task_is_confirmed_not_claimed_delivered() {
    // 同前两例：断言进程级追踪表的精确计数，故在测试内显式串行（见夹具注释）。
    let _serial = QUARANTINE_TABLE.lock().await;
    wait_quarantined_is(0, QUARANTINE_BASELINE, "前置条件：追踪表起始为空").await;
    let controller = TestHandler::new();
    controller.arm_event_panic();
    let board = TestHandler::new();
    let mut harness = Harness::build_default(&controller, &board);
    harness.ready().await;

    // 第一个事件走完整条链路：入队 → 回调任务取走 → 在 handler 里 panic → 任务消亡
    // → 通知通道被关闭。前置条件按**观测**成立（有界轮询「回调有没有进入 handler」），
    // 不是「睡够了就信」。
    harness
        .board
        .send_event(StreamKind::Data, "board.metric", vec![1u8; 8])
        .await
        .expect("第一个事件入队");
    wait_until(
        "回调必须真的进入 handler（否则 panic 没发生，本用例什么也没测）",
        || controller.events_entered() > 0,
    )
    .await;
    // 通道已关闭：后续事件只能被丢弃并计数，不会有人替它「送达」。每条事件都要走一次
    // 完整往返（本任务因此反复让出调度点），panic 与通道关闭都已先落地；最终断言看的
    // 是 outcome.dropped_events 这个真实计数，而不是这里发了几条。
    for i in 1..12_u16 {
        let _ = harness
            .board
            .send_event(StreamKind::Data, "board.metric", vec![i as u8; 8])
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    harness.controller.abort();
    let owner = tokio::time::timeout(LONG, &mut harness.controller_task)
        .await
        .expect("owner 退出")
        .expect("owner 不 panic");
    assert!(
        owner.dropped_events > 0,
        "前置条件：通知通道必须真的已关闭（否则没走到收紧后的分支）"
    );
    assert_eq!(
        controller.closed().len(),
        0,
        "回调任务早已消亡，on_closed 就是没送达，不得伪造"
    );
    assert!(
        owner.cleaned,
        "任务已消亡属「可确认了结」：既不谎称有残留，也不靠丢句柄换有界"
    );
    assert_eq!(
        quarantined_callbacks(),
        0,
        "确认了结的回调不得继续占用追踪名额"
    );

    harness.board.abort();
    let board_out = tokio::time::timeout(LONG, &mut harness.board_task)
        .await
        .expect("设备 owner 退出")
        .expect("owner 不 panic");
    assert!(board_out.cleaned);

    // 放锁前的收口：本用例没有闸门要开（回调是 panic 消亡，可确认了结），但仍要在释放
    // 互斥前**再看一次**表回到 0——两条 owner 都已确认退出，才对下一个用例是干净的。
    wait_quarantined_is(
        0,
        QUARANTINE_BASELINE,
        "收口：确认了结的回调不得占着追踪名额",
    )
    .await;
}
