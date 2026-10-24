//! 生产连接循环验收：退避、Ready 才清零、取消全阶段、永久停止与退出码、本地未授权
//! 状态，以及**真实 loopback 双端互签 pin** 的端到端业务往返。
//!
//! 控制流断言用注入的 connector/session 替身（可穷尽、可复现）；`real_*` /
//! `signed_*` / `bad_pin_*` 用真实 TCP + 完整 v3 握手 + 加密记录层 + h2 双流会话。
//! 不连真实数据库、不读 secret、绝不触发真实主机重启。
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rsetup_board_agent::connect::{
    BACKOFF_CAP, BoardConnector, ConnectEvent, ConnectEventKind, ConnectionId, DRAIN_TIMEOUT,
    EnvConfigError, FaultReason, LiveSession, PermanentStop, ReconnectPolicy, RetryReason,
    RunOutcome, SessionEnd, SessionEndReason, SessionFuture, SharedAgentFactory, Shutdown,
    TunnelConnector, UnauthorizedState, backoff_delay, board_connect_config_from_env,
    csprng_jitter, descriptor_from_env, run_board_connection, state_dir_of,
};
use rsetup_board_agent::connect_config::{BoardConnectConfig, ConnectConfigError};
use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessStatus, JournalEpochSource, TaskJournalReader,
};
use rsetup_board_agent::host_source::SystemReadOnlySource;
use rsetup_board_agent::journal::{JournalStore, RealFs};
use rsetup_board_agent::read_only::ReadOnlySource;
use rsetup_board_agent::session_handler::BusinessDispatch;
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, Capabilities, ClockSample,
    DeviceStatus, Query, RebootPrepare, TaskQuery, TaskRecord as WireTaskRecord,
    approved_codec_witness, decode_business, encode_business,
};
use rsetup_tunnel::envelope::StreamKind;
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, HandshakeError, ServerHandshakeConfig,
    VerifiedPeer, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::{
    HandlerFuture, Reply, RequestContext, SessionCloseReason, SessionConfig, SessionHandle,
    SessionHandler, SessionState, SystemSignal, start_server_session,
};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rsetup-board-connect-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&dir).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&dir, permissions).unwrap();
    dir
}

/// 测试用退避：抖动固定 0（抖动另有专项断言），基数/上限压到毫秒级。
fn policy() -> ReconnectPolicy {
    ReconnectPolicy::with_jitter(Duration::from_millis(5), Duration::from_millis(40), || {
        Duration::from_secs(0)
    })
}

// ================================================================ 替身

/// 替身会话的两条 watch 信号通道（Ready 与 End）。
type ReadySignal = Option<Result<(), SessionEnd>>;
type EndSignal = Option<SessionEnd>;

#[derive(Clone)]
struct FakeSessionControl {
    drains: Arc<AtomicUsize>,
    aborts: Arc<AtomicUsize>,
    joins: Arc<AtomicUsize>,
    ready_tx: watch::Sender<ReadySignal>,
    end_tx: watch::Sender<EndSignal>,
    // 必须常驻一个接收端：`watch::Sender::send` 在无接收端时会静默丢弃。
    _resident_receivers: (watch::Receiver<ReadySignal>, watch::Receiver<EndSignal>),
    live: Arc<AtomicBool>,
    ignore_drain: bool,
    never_join: bool,
}

impl FakeSessionControl {
    fn new(ready: Option<Result<(), SessionEnd>>) -> Self {
        Self::build(ready, false, false)
    }
    fn build(ready: Option<Result<(), SessionEnd>>, ignore_drain: bool, never_join: bool) -> Self {
        // 常驻接收者：`watch::Sender::send` 在没有接收者时会丢弃取值，
        // 替身必须保证「先发后订阅」也看得见（真实 `TunnelSession` 自带接收者）。
        let (ready_tx, _ready_keep) = watch::channel(ready);
        let (end_tx, _end_keep) = watch::channel(None);
        Self {
            drains: Arc::new(AtomicUsize::new(0)),
            aborts: Arc::new(AtomicUsize::new(0)),
            joins: Arc::new(AtomicUsize::new(0)),
            ready_tx,
            end_tx,
            _resident_receivers: (_ready_keep, _end_keep),
            live: Arc::new(AtomicBool::new(false)),
            ignore_drain,
            never_join,
        }
    }
    fn mark_ready(&self) {
        let _ = self.ready_tx.send(Some(Ok(())));
    }
    fn end_with(&self, reason: SessionEndReason) {
        let _ = self.end_tx.send(Some(SessionEnd { reason }));
    }
    fn drains(&self) -> usize {
        self.drains.load(Ordering::SeqCst)
    }
    fn aborts(&self) -> usize {
        self.aborts.load(Ordering::SeqCst)
    }
}

struct FakeSession {
    control: FakeSessionControl,
}

impl LiveSession for FakeSession {
    fn wait_ready(&mut self) -> SessionFuture<'_, Result<(), SessionEnd>> {
        let mut receiver = self.control.ready_tx.subscribe();
        Box::pin(async move {
            loop {
                if let Some(value) = *receiver.borrow_and_update() {
                    return value;
                }
                if receiver.changed().await.is_err() {
                    return Err(SessionEnd {
                        reason: SessionEndReason::Aborted,
                    });
                }
            }
        })
    }
    fn wait_end(&mut self) -> SessionFuture<'_, SessionEnd> {
        let mut receiver = self.control.end_tx.subscribe();
        Box::pin(async move {
            loop {
                if let Some(end) = *receiver.borrow_and_update() {
                    return end;
                }
                if receiver.changed().await.is_err() {
                    return SessionEnd {
                        reason: SessionEndReason::Aborted,
                    };
                }
            }
        })
    }
    fn request_drain(&self) {
        self.control.drains.fetch_add(1, Ordering::SeqCst);
        if !self.control.ignore_drain {
            let _ = self.control.end_tx.send(Some(SessionEnd {
                reason: SessionEndReason::Drained,
            }));
        }
    }
    fn abort(&self) {
        self.control.aborts.fetch_add(1, Ordering::SeqCst);
        let _ = self.control.end_tx.send(Some(SessionEnd {
            reason: SessionEndReason::Aborted,
        }));
    }
    fn join(&mut self, _deadline: Duration) -> SessionFuture<'_, bool> {
        self.control.joins.fetch_add(1, Ordering::SeqCst);
        let joined = !self.control.never_join;
        Box::pin(async move { joined })
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        // 会话实例被回收的证据（驱动任务不残留 → 不会双循环）。
        self.control.live.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
enum Scripted {
    Fail(AttemptFailureAlias),
    Session(FakeSessionControl),
    /// open 永不返回：断言 shutdown 能取消建连阶段且不泄漏任务。
    Hang,
}

type AttemptFailureAlias = rsetup_board_agent::connect::AttemptFailure;

struct FakeConnector {
    script: Mutex<VecDeque<Scripted>>,
    opens: Arc<AtomicUsize>,
    ids: Arc<Mutex<Vec<String>>>,
    stops: Arc<Mutex<Vec<PermanentStop>>>,
    preflight_failure: Mutex<Option<AttemptFailureAlias>>,
    hang_cancelled: Arc<AtomicBool>,
    record_ok: bool,
}

impl FakeConnector {
    fn new(script: Vec<Scripted>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            opens: Arc::new(AtomicUsize::new(0)),
            ids: Arc::new(Mutex::new(Vec::new())),
            stops: Arc::new(Mutex::new(Vec::new())),
            preflight_failure: Mutex::new(None),
            hang_cancelled: Arc::new(AtomicBool::new(false)),
            record_ok: true,
        }
    }
    fn with_record_ok(mut self, record_ok: bool) -> Self {
        self.record_ok = record_ok;
        self
    }
    /// 移动 connector 进 `Arc` 之前取观察者句柄。
    fn opens_witness(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.opens)
    }
    fn ids_witness(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.ids)
    }
    fn fail_preflight(&self, failure: AttemptFailureAlias) {
        *self.preflight_failure.lock().unwrap() = Some(failure);
    }
    /// 脚本耗尽后重复最后一步，便于「持续失败」类断言。
    fn next(&self) -> Option<Scripted> {
        let mut script = self.script.lock().unwrap();
        if script.len() > 1 {
            script.pop_front()
        } else {
            script.back().cloned()
        }
    }
}

impl BoardConnector for FakeConnector {
    fn preflight(&self) -> Result<(), AttemptFailureAlias> {
        match *self.preflight_failure.lock().unwrap() {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
    fn open(
        &self,
        connection_id: ConnectionId,
    ) -> rsetup_board_agent::connect::OpenSessionFuture<'_> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.ids
            .lock()
            .unwrap()
            .push(connection_id.as_str().to_string());
        match self.next() {
            Some(Scripted::Fail(failure)) => Box::pin(async move { Err(failure) }),
            Some(Scripted::Session(control)) => Box::pin(async move {
                control.live.store(true, Ordering::SeqCst);
                Ok(Box::new(FakeSession { control }) as Box<dyn LiveSession>)
            }),
            Some(Scripted::Hang) => {
                let cancelled = Arc::clone(&self.hang_cancelled);
                Box::pin(async move {
                    struct CancelWitness(Arc<AtomicBool>);
                    impl Drop for CancelWitness {
                        fn drop(&mut self) {
                            self.0.store(true, Ordering::SeqCst);
                        }
                    }
                    let _witness = CancelWitness(cancelled);
                    std::future::pending::<()>().await;
                    unreachable!()
                })
            }
            None => Box::pin(async move {
                Err(AttemptFailureAlias::Fault(
                    rsetup_board_agent::connect::FaultReason::Internal,
                ))
            }),
        }
    }
    fn note_permanent_stop(&self, stop: PermanentStop) -> bool {
        self.stops.lock().unwrap().push(stop);
        self.record_ok
    }
}

fn events_collector() -> (
    Arc<Mutex<Vec<ConnectEvent>>>,
    rsetup_board_agent::connect::Observer,
) {
    let events: Arc<Mutex<Vec<ConnectEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let observer: rsetup_board_agent::connect::Observer =
        Arc::new(move |event| sink.lock().unwrap().push(event));
    (events, observer)
}

/// 等板端第 `expected` 次 Ready（本端两条流各自首 probe 成功，§6.6）。
///
/// 中控侧不能一建好会话就发业务：板端两条流尚未开出时会拿到 `StreamClosed`。
fn ready_count(events: &Arc<Mutex<Vec<ConnectEvent>>>) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.kind == ConnectEventKind::Ready)
        .count()
}

async fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !condition() {
        if Instant::now() > deadline {
            panic!("条件在 25s 内未成立：{what}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ================================================================ 退避与抖动

#[test]
fn backoff_is_exponential_and_capped_at_sixty_seconds() {
    let base = Duration::from_secs(1);
    let zero = Duration::from_secs(0);
    let expected = [1u64, 2, 4, 8, 16, 32, 60, 60, 60, 60];
    for (index, seconds) in expected.iter().enumerate() {
        let failures = index as u32 + 1;
        assert_eq!(
            backoff_delay(failures, base, BACKOFF_CAP, zero),
            Duration::from_secs(*seconds),
            "第 {failures} 次失败"
        );
    }
    // 任意大的失败计数都不得溢出或超过上限。
    assert_eq!(
        backoff_delay(u32::MAX, base, BACKOFF_CAP, zero),
        Duration::from_secs(60)
    );
}

#[test]
fn jitter_stays_inside_one_second_and_is_not_constant() {
    let mut seen = Vec::new();
    for _ in 0..200 {
        let jitter = csprng_jitter();
        assert!(jitter < Duration::from_secs(1), "{jitter:?}");
        seen.push(jitter);
    }
    let first = seen[0];
    assert!(
        seen.iter().skip(1).any(|value| *value != first),
        "jitter 恒为常量说明退化成固定延时"
    );
    let base = Duration::from_secs(1);
    let with_jitter = backoff_delay(1, base, BACKOFF_CAP, Duration::from_millis(999));
    assert!(with_jitter > base && with_jitter <= base + Duration::from_secs(1));
    // 抖动再大也被夹到 1s 上界，不会被配成近乎无限的等待。
    assert_eq!(
        backoff_delay(1, base, BACKOFF_CAP, Duration::from_secs(30)),
        base + Duration::from_secs(1)
    );
}

#[test]
fn drain_deadline_cannot_be_shorter_than_the_spec_minimum() {
    let short =
        ReconnectPolicy::with_jitter(Duration::from_millis(1), Duration::from_millis(2), || {
            Duration::from_secs(0)
        })
        .with_drain(Duration::from_millis(1));
    assert!(short.drain >= DRAIN_TIMEOUT);
    assert!(DRAIN_TIMEOUT.as_secs() >= 5);
}

#[test]
fn close_reason_mapping_never_calls_a_kick_a_revocation() {
    assert_eq!(
        SessionEnd::from_close(SessionCloseReason::Kicked).reason,
        SessionEndReason::Kicked
    );
    assert_eq!(
        SessionEnd::from_close(SessionCloseReason::Revoked).reason,
        SessionEndReason::Revoked
    );
    assert_eq!(
        SessionEnd::from_close(SessionCloseReason::StreamFlapping).reason,
        SessionEndReason::Transport
    );
    assert_eq!(
        SessionEnd::from_close(SessionCloseReason::Local).reason,
        SessionEndReason::Drained
    );
}

// ================================================================ 循环控制流

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backoff_grows_until_ready_then_resets() {
    let session = FakeSessionControl::new(None);
    let connector = FakeConnector::new(vec![
        Scripted::Fail(rsetup_board_agent::connect::AttemptFailure::Retryable(
            RetryReason::Tcp,
        )),
        Scripted::Fail(rsetup_board_agent::connect::AttemptFailure::Retryable(
            RetryReason::Handshake,
        )),
        Scripted::Session(session.clone()),
    ]);
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("两次失败后退避", || backoffs(&events) == vec![1, 2]).await;
    // 握手后未 Ready 不得清零：Ready 之前一直不算成功。
    session.mark_ready();
    wait_for("Ready 事件", || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == ConnectEventKind::Ready)
    })
    .await;
    // Ready 之后被踢：退避必须从第 1 档重新开始（Kick 不是吊销）。
    session.end_with(SessionEndReason::Kicked);
    wait_for("Ready 后重新从第 1 档退避", || {
        backoffs(&events).last().copied() == Some(1)
    })
    .await;
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failures_before_ready_keep_growing_and_are_not_reset() {
    // 连上了、但 ready 判据未达成就断：每次都必须算失败，计数不得清零。
    let control = FakeSessionControl::new(Some(Err(SessionEnd {
        reason: SessionEndReason::Transport,
    })));
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("连续失败计数递增", || {
        let values = backoffs(&events);
        values.len() >= 4 && values.windows(2).all(|pair| pair[1] > pair[0])
    })
    .await;
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .all(|event| event.kind != ConnectEventKind::Ready)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_revoke_and_approval_denied_stop_reconnecting_with_zero_exit() {
    for (stop, failure) in [
        (
            PermanentStop::Revoked,
            rsetup_board_agent::connect::AttemptFailure::Permanent(PermanentStop::Revoked),
        ),
        (
            PermanentStop::ApprovalDenied,
            rsetup_board_agent::connect::AttemptFailure::Permanent(PermanentStop::ApprovalDenied),
        ),
    ] {
        let connector = FakeConnector::new(vec![Scripted::Fail(failure)]);
        let (events, observer) = events_collector();
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            run_board_connection(Arc::new(connector), policy(), Shutdown::new(), observer),
        )
        .await
        .expect("永久停止必须立即返回，不能退避循环");
        assert_eq!(
            outcome,
            RunOutcome::Stopped {
                stop,
                state_written: true
            }
        );
        assert_eq!(outcome.exit_code(), 0, "永久停止必须以 0 退出");
        assert!(
            events.lock().unwrap().iter().any(|event| {
                event.kind == ConnectEventKind::Stopped && event.stop == Some(stop)
            })
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_rejections_keep_retrying_and_never_claim_revocation() {
    // 未认证信息（坏签名/不受信公钥）与超时、传输错误一样可重试，
    // 绝不能冒充合法吊销（否则一次中间人就能永久停住板端）。
    let connector = FakeConnector::new(vec![
        Scripted::Fail(rsetup_board_agent::connect::AttemptFailure::Retryable(
            RetryReason::Handshake,
        )),
        Scripted::Fail(rsetup_board_agent::connect::AttemptFailure::Retryable(
            RetryReason::Resolve,
        )),
        Scripted::Fail(rsetup_board_agent::connect::AttemptFailure::Retryable(
            RetryReason::Tcp,
        )),
    ]);
    let opens = connector.opens_witness();
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("未认证失败必须持续重试", || {
        opens.load(Ordering::SeqCst) >= 3
    })
    .await;
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_fault_stops_without_hot_loop() {
    for reason in [
        FaultReason::Config,
        FaultReason::Identity,
        FaultReason::Journal,
        FaultReason::UnauthorizedState,
    ] {
        let connector = FakeConnector::new(vec![Scripted::Fail(
            rsetup_board_agent::connect::AttemptFailure::Fault(reason),
        )]);
        let opens = connector.opens_witness();
        let (_events, observer) = events_collector();
        let outcome =
            run_board_connection(Arc::new(connector), policy(), Shutdown::new(), observer).await;
        assert_eq!(outcome, RunOutcome::Fault(reason));
        assert_eq!(opens.load(Ordering::SeqCst), 1, "{reason:?} 不得热 loop");
        assert_eq!(outcome.exit_code(), 78);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preflight_failure_prevents_any_connection_attempt() {
    let connector = FakeConnector::new(vec![Scripted::Hang]);
    connector.fail_preflight(rsetup_board_agent::connect::AttemptFailure::Fault(
        FaultReason::UnauthorizedState,
    ));
    let opens = connector.opens_witness();
    let (events, observer) = events_collector();
    let outcome =
        run_board_connection(Arc::new(connector), policy(), Shutdown::new(), observer).await;
    assert_eq!(outcome, RunOutcome::Fault(FaultReason::UnauthorizedState));
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "未授权状态必须在建连之前就停住"
    );
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == ConnectEventKind::PreflightFailed)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permanent_stop_without_writable_state_is_reported_not_hidden() {
    let connector = FakeConnector::new(vec![Scripted::Fail(
        rsetup_board_agent::connect::AttemptFailure::Permanent(PermanentStop::Revoked),
    )])
    .with_record_ok(false);
    let (events, observer) = events_collector();
    let outcome =
        run_board_connection(Arc::new(connector), policy(), Shutdown::new(), observer).await;
    assert_eq!(
        outcome,
        RunOutcome::Stopped {
            stop: PermanentStop::Revoked,
            state_written: false
        }
    );
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == ConnectEventKind::StopStateUnwritten)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_the_connect_stage_without_leaving_tasks() {
    let connector = FakeConnector::new(vec![Scripted::Hang]);
    let cancelled = Arc::clone(&connector.hang_cancelled);
    let opens = connector.opens_witness();
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("进入建连阶段", || opens.load(Ordering::SeqCst) >= 1).await;
    // 建连 future 仍在挂着（尚未被取消 drop）。
    assert!(!cancelled.load(Ordering::SeqCst));
    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("shutdown 必须能取消建连阶段的等待")
        .unwrap();
    assert_eq!(outcome, RunOutcome::Shutdown);
    // 建连 future 被 drop：不留挂着的连接任务。
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_during_pending_state_drains_then_joins() {
    // PENDING 无总审批期限：ready 永不到来，只能被 shutdown 结清。
    let control = FakeSessionControl::new(None);
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("会话实例存在", || control.live.load(Ordering::SeqCst)).await;
    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("PENDING 阶段必须可被 shutdown 取消")
        .unwrap();
    assert_eq!(outcome, RunOutcome::Shutdown);
    // 先优雅排空，再确认驱动任务回收；排空成功因此不 abort。
    assert_eq!(control.drains(), 1);
    assert_eq!(control.aborts(), 0);
    assert!(!control.live.load(Ordering::SeqCst), "会话实例必须被回收");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drain_timeout_escalates_to_abort_and_still_terminates() {
    // 会话不响应 GOAWAY：排空期限（规范最小 5s）用尽后必须 abort，绝不无限挂资源。
    let control = FakeSessionControl::build(Some(Ok(())), true, false);
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("会话 Ready", || control.live.load(Ordering::SeqCst)).await;
    let started = Instant::now();
    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(25), run)
        .await
        .expect("排空失败也必须收口")
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(outcome, RunOutcome::Shutdown);
    assert!(elapsed >= DRAIN_TIMEOUT, "排空不得少于 5s: {elapsed:?}");
    assert!(
        elapsed < DRAIN_TIMEOUT + Duration::from_secs(5),
        "不得无限等待: {elapsed:?}"
    );
    assert_eq!(control.drains(), 1);
    assert!(control.aborts() >= 1, "排空失败必须升级 abort");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_revoke_stops_and_records_local_unauthorized_state() {
    let control = FakeSessionControl::new(Some(Ok(())));
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let (_events, observer) = events_collector();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        Shutdown::new(),
        observer,
    ));
    wait_for("会话 Ready", || control.live.load(Ordering::SeqCst)).await;
    control.end_with(SessionEndReason::Revoked);
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("revoke 必须立即停止循环")
        .unwrap();
    assert_eq!(
        outcome,
        RunOutcome::Stopped {
            stop: PermanentStop::Revoked,
            state_written: true
        }
    );
    assert_eq!(outcome.exit_code(), 0);
    // revoke 不受 drain 限制（§6.6）：直接 abort 结清。
    assert!(control.aborts() >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_is_not_revoke_and_reconnects_after_backoff() {
    let first = FakeSessionControl::new(Some(Ok(())));
    let second = FakeSessionControl::new(None);
    let connector = FakeConnector::new(vec![
        Scripted::Session(first.clone()),
        Scripted::Session(second.clone()),
    ]);
    let stops = Arc::clone(connector_stops(&connector));
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("第一条会话建立", || {
        first.live.load(Ordering::SeqCst)
    })
    .await;
    first.end_with(SessionEndReason::Kicked);
    wait_for("第二条会话建立", || {
        second.live.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        stops.lock().unwrap().is_empty(),
        "Kick 不是吊销，不得落盘停止状态"
    );
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
    assert_eq!(first.drains(), 0, "Kick 不是本端排空");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_attempt_uses_a_fresh_connection_generation() {
    let control = FakeSessionControl::new(Some(Err(SessionEnd {
        reason: SessionEndReason::Transport,
    })));
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let ids = connector.ids_witness();
    let (_events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("至少三次尝试", || connector_opens(&ids) >= 3).await;
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
    let ids = ids.lock().unwrap().clone();
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "connection_id 不得跨重连复用");
    assert!(ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unrecoverable_join_still_terminates_the_loop() {
    let control = FakeSessionControl::build(Some(Ok(())), false, true);
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let opens = connector.opens_witness();
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("会话 Ready", || control.live.load(Ordering::SeqCst)).await;
    control.end_with(SessionEndReason::Transport);
    // 收尾不可确认（第二次 join 仍是 false）时循环必须**自己**终止：
    // 旧实现忽略第二次 join 的结果，会退避后再 open 一条新会话，带着未回收的驱动
    // 任务形成两条循环。因此这里不得靠观察者在第二轮 signal shutdown 来「收口」。
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("join 不可确认必须自行终止，不得等外部 shutdown")
        .unwrap();
    assert!(
        matches!(outcome, RunOutcome::Fault(FaultReason::DriverUnreclaimed)),
        "收尾不可确认必须以驱动未回收故障结束: {outcome:?}"
    );
    assert_eq!(outcome.exit_code(), 78);
    assert_eq!(
        opens.load(Ordering::SeqCst),
        1,
        "收尾不可确认后不得再开新会话（否则双循环）"
    );
    assert!(
        control.joins_load() >= 2,
        "join 失败必须先 abort 再确认一次，两次都不足才算不可确认"
    );
    let events = events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| event.kind == ConnectEventKind::DriverUnreclaimed),
        "必须显式声明「驱动任务未确认回收」，不能只说会话结束: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == ConnectEventKind::Backoff),
        "不可确认的收尾不得进入退避重连: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == ConnectEventKind::Fault),
        "必须把故障作为事件报出: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_with_unconfirmed_join_still_stops_writes_state_and_exits_zero() {
    // 吊销方向的停止语义优先：join 不可确认既不能让循环重连，也不能把退出码
    // 变成 78（那会让 Restart=on-failure 绕过「不再自动重连」）。
    let control = FakeSessionControl::build(Some(Ok(())), false, true);
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let stops = Arc::clone(connector_stops(&connector));
    let opens = connector.opens_witness();
    let (events, observer) = events_collector();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        Shutdown::new(),
        observer,
    ));
    wait_for("会话 Ready", || control.live.load(Ordering::SeqCst)).await;
    control.end_with(SessionEndReason::Revoked);
    let outcome = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("revoke 必须立即停止循环")
        .unwrap();
    assert_eq!(
        outcome,
        RunOutcome::Stopped {
            stop: PermanentStop::Revoked,
            state_written: true
        },
        "收尾不可确认不得改变吊销的停止结果"
    );
    assert_eq!(outcome.exit_code(), 0, "退出 0 才不会被 systemd 重启绕过");
    assert_eq!(stops.lock().unwrap().len(), 1, "必须落盘永久停止状态");
    assert_eq!(opens.load(Ordering::SeqCst), 1, "吊销后不得再 open");
    assert!(control.aborts() >= 1);
    let events = events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| event.kind == ConnectEventKind::DriverUnreclaimed),
        "revoke 路径同样要如实声明收尾不可确认: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == ConnectEventKind::Stopped),
        "必须报告永久停止事件: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == ConnectEventKind::Fault),
        "吊销不得被改成本端故障（退出码会变）: {events:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_with_unconfirmed_join_is_still_a_zero_exit_shutdown() {
    // SIGTERM 路径：收尾不可确认必须可见，但退出结果仍是 Shutdown（0），
    // 否则一次正常的停止会被 systemd 当成失败而重新拉起。
    let control = FakeSessionControl::build(Some(Ok(())), false, true);
    let connector = FakeConnector::new(vec![Scripted::Session(control.clone())]);
    let opens = connector.opens_witness();
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("会话 Ready", || control.live.load(Ordering::SeqCst)).await;
    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(25), run)
        .await
        .expect("shutdown 必须收口循环")
        .unwrap();
    assert_eq!(outcome, RunOutcome::Shutdown);
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(control.drains(), 1, "仍要先优雅排空");
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    let events = events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .any(|event| event.kind == ConnectEventKind::DriverUnreclaimed),
        "收尾不可确认必须可见: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == ConnectEventKind::Fault),
        "SIGTERM 路径不得被改判为 78 故障: {events:?}"
    );
}

fn prod_source_calls(prod: &Production) -> usize {
    prod.source_calls.load(Ordering::SeqCst)
}

fn connector_opens(ids: &Arc<Mutex<Vec<String>>>) -> usize {
    ids.lock().unwrap().len()
}

fn connector_stops(connector: &FakeConnector) -> &Arc<Mutex<Vec<PermanentStop>>> {
    &connector.stops
}

fn backoffs(events: &Arc<Mutex<Vec<ConnectEvent>>>) -> Vec<u32> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.kind == ConnectEventKind::Backoff)
        .map(|event| event.attempt)
        .collect()
}

// ================================================================ 本地未授权状态

impl FakeSessionControl {
    fn joins_load(&self) -> usize {
        self.joins.load(Ordering::SeqCst)
    }
}

#[test]
fn unauthorized_state_marker_is_fail_closed_and_owner_only() {
    let dir = temp_dir("unauthorized");
    let state = UnauthorizedState::in_state_dir(&dir);
    assert!(state.check().is_ok(), "无标记必须允许启动");
    assert!(state.record(PermanentStop::Revoked));
    assert_eq!(
        state.check(),
        Err(rsetup_board_agent::connect::AttemptFailure::Fault(
            FaultReason::UnauthorizedState
        )),
        "有标记必须停住（恢复需管理员重新授权 + 人工删除标记）"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(state.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "未授权标记必须 0600");
    let body = std::fs::read_to_string(state.path()).unwrap();
    assert!(body.contains("revoked"));
    // 去敏：单行、不含路径分隔符（因此不可能带出真实路径）、不含密钥。
    assert_eq!(body.lines().count(), 1);
    assert!(!body.contains('/'), "标记内容不得带出路径: {body}");
    // 幂等：已存在仍算落盘成功。
    assert!(state.record(PermanentStop::ApprovalDenied));
    std::fs::remove_file(state.path()).unwrap();
    assert!(state.check().is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unauthorized_state_refuses_a_symlinked_marker() {
    let dir = temp_dir("unauthorized-symlink");
    let target = dir.join("elsewhere");
    std::fs::write(&target, "x").unwrap();
    let state = UnauthorizedState::in_state_dir(&dir);
    std::os::unix::fs::symlink(&target, state.path()).unwrap();
    // 符号链接不是可信标记：同样 fail-closed。
    assert_eq!(
        state.check(),
        Err(rsetup_board_agent::connect::AttemptFailure::Fault(
            FaultReason::UnauthorizedState
        ))
    );
    // O_EXCL 落盘不会跟随符号链接改写目标内容。
    let _ = state.record(PermanentStop::Revoked);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn state_dir_is_the_identity_files_parent() {
    assert_eq!(
        state_dir_of(Path::new("/var/lib/rsetup-board-agent/identity.pk8")),
        PathBuf::from("/var/lib/rsetup-board-agent")
    );
}

// ================================================================ 环境读取

#[test]
fn environment_reader_never_treats_non_unicode_as_absent() {
    let _guard = env_lock().lock().unwrap();
    let keys = [
        "RSETUP_CONTROLLER_ADDR",
        "RSETUP_CONTROLLER_PUBLIC_KEYS",
        "RSETUP_BOARD_KEY_FILE",
        "RSETUP_BOARD_JOURNAL_DIR",
        "RSETUP_BOARD_DEVICE_SN",
    ];
    let saved: Vec<(&str, Option<std::ffi::OsString>)> = keys
        .iter()
        .map(|key| (*key, std::env::var_os(key)))
        .collect();
    for key in keys {
        unsafe { std::env::remove_var(key) };
    }
    let pin = "ab".repeat(32);
    // 1) 缺显式 pin → validated 构造直接失败关闭（禁止 TOFU，绝不自动生成信任）。
    unsafe { std::env::set_var("RSETUP_CONTROLLER_ADDR", "controller.example:8443") };
    assert_eq!(
        board_connect_config_from_env().unwrap_err(),
        EnvConfigError::Invalid(ConnectConfigError::MissingPins)
    );
    // 2) 齐备 → validated 构造。
    unsafe { std::env::set_var("RSETUP_CONTROLLER_PUBLIC_KEYS", pin.as_str()) };
    let config = board_connect_config_from_env().expect("显式 pin 必须可启动");
    assert_eq!(config.trusted_servers, vec![[0xabu8; 32]]);
    // 3) 必需路径**非 UTF-8**：必须报 NotUnicode，绝不回落默认值。
    use std::os::unix::ffi::OsStringExt;
    // 真实的非 UTF-8 路径字节（0xff 不是合法 UTF-8 起始字节）。
    let non_unicode = std::ffi::OsString::from_vec(vec![b'/', 0xff, b'x']);
    for key in ["RSETUP_BOARD_KEY_FILE", "RSETUP_BOARD_JOURNAL_DIR"] {
        unsafe { std::env::set_var(key, non_unicode.clone()) };
        assert_eq!(
            board_connect_config_from_env().unwrap_err(),
            EnvConfigError::NotUnicode,
            "{key} 非 UTF-8 不得被当成未设置"
        );
        unsafe { std::env::remove_var(key) };
    }
    // 4) 超长取值（含路径）先在本层预算处收住（4 KiB 足够真实路径）。
    let long_value = format!("/var/lib/{}x", "y".repeat(5000));
    unsafe { std::env::set_var("RSETUP_BOARD_JOURNAL_DIR", long_value.as_str()) };
    assert_eq!(
        board_connect_config_from_env().unwrap_err(),
        EnvConfigError::TooLong
    );
    unsafe { std::env::remove_var("RSETUP_BOARD_JOURNAL_DIR") };
    unsafe { std::env::set_var("RSETUP_BOARD_DEVICE_SN", long_value.as_str()) };
    assert_eq!(descriptor_from_env().unwrap_err(), EnvConfigError::TooLong);
    unsafe { std::env::remove_var("RSETUP_BOARD_DEVICE_SN") };
    // 5) descriptor 缺字段就是空串：绝不伪造设备身份。
    let descriptor = descriptor_from_env().unwrap();
    assert_eq!(descriptor, DeviceDescriptor::new("", "", "", ""));

    for (key, value) in saved {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Mutex::default)
}

// ================================================================ 真实 loopback

/// 只读来源包装：统计真实触达次数（握手失败必须为零）。
struct CountingSource {
    inner: SystemReadOnlySource,
    calls: Arc<AtomicUsize>,
}

impl ReadOnlySource for CountingSource {
    fn capabilities(&self) -> Capabilities {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.capabilities()
    }
    fn status(&self) -> DeviceStatus {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.status()
    }
    fn clock(&self) -> ClockSample {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.clock()
    }
}

/// `Arc<JournalStore<RealFs>>` 的 reader/epoch 适配（与生产 bin 同形）。
struct StoreReader {
    store: Arc<JournalStore<RealFs>>,
}

impl TaskJournalReader for StoreReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<WireTaskRecord>, BusinessStatus> {
        self.store
            .get(controller_key, &query.sub_task_id)
            .map(|record| record.map(|record| record.to_wire()))
            .map_err(|_| BusinessStatus::InternalError)
    }
}

struct StoreEpoch {
    epoch: String,
}

impl JournalEpochSource for StoreEpoch {
    fn journal_epoch(&self) -> String {
        self.epoch.clone()
    }
}

/// 中控侧替身：真实 TCP listener + 完整 v3 服务端握手 + 服务端会话。
struct Controller {
    address: SocketAddr,
    /// 中控长期公钥：板端唯一的显式 pin 目标。
    controller_key: [u8; 32],
    sessions: tokio::sync::Mutex<mpsc::UnboundedReceiver<SessionHandle>>,
    handshakes: Arc<AtomicUsize>,
    rejections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Controller {
    async fn start(board_key: [u8; 32], reject_reason: Option<u32>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let handshakes = Arc::new(AtomicUsize::new(0));
        let rejections = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&handshakes);
        let rejections_seen = Arc::clone(&rejections);
        let identity =
            Arc::new(IdentityKey::load_or_generate(temp_dir("controller").join("id.pk8")).unwrap());
        let controller_key = identity.public_key();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let _ = stream.set_nodelay(true);
                let config = ServerHandshakeConfig {
                    identity: Arc::clone(&identity),
                };
                let admission = TestAdmission {
                    board_key,
                    reason: reject_reason,
                    rejections: Arc::clone(&rejections),
                };
                let Ok(result) = server_handshake(stream, &config, &admission).await else {
                    continue;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let secure =
                    SecureIo::new(result.io, result.session_keys.into_server_record_layers());
                let Ok((handle, driver)) =
                    start_server_session(secure, Arc::new(NullHandler), SessionConfig::default())
                else {
                    continue;
                };
                if sender.send(handle).is_err() {
                    return;
                }
                // 会话结束后继续 accept，模拟真实中控。
                let _ = tokio::time::timeout(Duration::from_secs(60), driver).await;
            }
        });
        Self {
            address,
            controller_key,
            sessions: tokio::sync::Mutex::new(receiver),
            handshakes,
            rejections: rejections_seen,
            task,
        }
    }
    async fn next_session(&self) -> SessionHandle {
        tokio::time::timeout(Duration::from_secs(25), self.sessions.lock().await.recv())
            .await
            .expect("等待板端建连超时")
            .expect("会话通道已关闭")
    }
    async fn take(&mut self) {
        self.task.abort();
    }
}

struct TestAdmission {
    board_key: [u8; 32],
    reason: Option<u32>,
    rejections: Arc<AtomicUsize>,
}

impl AdmissionPolicy for TestAdmission {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        if peer.public_key != self.board_key {
            return Box::pin(async { Err(HandshakeError::Policy) });
        }
        let reason = self.reason;
        let rejections = Arc::clone(&self.rejections);
        Box::pin(async move {
            match reason {
                Some(reason) => {
                    rejections.fetch_add(1, Ordering::SeqCst);
                    Ok(AdmissionDecision::Rejected(reason))
                }
                None => Ok(AdmissionDecision::Approved),
            }
        })
    }
}

/// 中控侧 handler：本文件只由服务端主动发请求，不回应业务。
struct NullHandler;

impl SessionHandler for NullHandler {
    fn on_request(&self, _ctx: RequestContext) -> HandlerFuture {
        Box::pin(async {
            Reply {
                status_code: 1002,
                error_message: "not served here".to_string(),
                payload: Vec::new().into(),
            }
        })
    }
}

/// 一套完整的生产装配（真实身份文件 + 真实 journal 目录 + 真实只读来源）。
struct Production {
    /// 取出后交给连接循环（同一时刻只有一个持有者）。
    connector: Option<TunnelConnector>,
    controller: Controller,
    journal_dir: PathBuf,
    state_dir: PathBuf,
    source_calls: Arc<AtomicUsize>,
    board_id: String,
}

impl Production {
    fn take_connector(&mut self) -> Arc<dyn BoardConnector> {
        Arc::new(self.connector.take().expect("connector 只能取走一次"))
    }
}

impl Production {
    async fn new(reject_reason: Option<u32>) -> Self {
        Self::pinned(reject_reason, false).await
    }

    /// `wrong_pin` 时用与中控无关的公钥做 pin（坏 pin 场景）。
    async fn pinned(reject_reason: Option<u32>, wrong_pin: bool) -> Self {
        let state_dir = temp_dir("state");
        let journal_dir = temp_dir("journal");
        let board_identity =
            Arc::new(IdentityKey::load_or_generate(state_dir.join("identity.pk8")).unwrap());
        let board_key = board_identity.public_key();
        let controller = Controller::start(board_key, reject_reason).await;
        // 板端公钥在装配后立即交给中控准入策略（互签 pin）。
        // 显式 pin：正常场景钉中控真实公钥；坏 pin 场景钉一个无关公钥。
        let pin = if wrong_pin {
            IdentityKey::load_or_generate(temp_dir("foreign").join("id.pk8"))
                .unwrap()
                .public_key()
        } else {
            controller.controller_key
        };
        let config = BoardConnectConfig::from_values(
            &controller.address.to_string(),
            &state_dir.join("identity.pk8"),
            &journal_dir,
            &hex::encode(pin),
        )
        .expect("生产配置必须通过 validated 构造");
        let store = JournalStore::open(&journal_dir, RealFs).expect("journal 必须能真实打开");
        let epoch = store.journal_epoch().to_string();
        let store = Arc::new(store);
        let source_calls = Arc::new(AtomicUsize::new(0));
        let readonly = SystemReadOnlySource::from_system(epoch.clone(), false, true)
            .expect("本机内核 boot_id 必须真实可读");
        let board_id = readonly.boot_id().to_string();
        let source = CountingSource {
            inner: readonly,
            calls: Arc::clone(&source_calls),
        };
        let agent = BoardAgent::new(
            source,
            StoreReader {
                store: Arc::clone(&store),
            },
            StoreEpoch { epoch },
        );
        let dispatch: Arc<dyn BusinessDispatch> = Arc::new(agent);
        let connector = TunnelConnector::new(
            config,
            board_identity,
            DeviceDescriptor::new("sn-loopback", "model-test", "fw-1", "connect-test"),
            Arc::new(SharedAgentFactory::new(dispatch)),
        )
        .with_unauthorized_state(UnauthorizedState::in_state_dir(&state_dir));
        Self {
            connector: Some(connector),
            controller,
            journal_dir,
            state_dir,
            source_calls,
            board_id,
        }
    }

    fn marker(&self) -> UnauthorizedState {
        UnauthorizedState::in_state_dir(&self.state_dir)
    }
    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.journal_dir);
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

fn query_bytes() -> Vec<u8> {
    encode_business(
        &BusinessCodec,
        Some(&approved_codec_witness()),
        &BusinessPayload::Query(Query {
            schema_version: BUSINESS_SCHEMA_VERSION,
        }),
    )
    .unwrap()
}

fn decode(bytes: &[u8], kind: BusinessPayloadKind) -> BusinessPayload {
    decode_business(&BusinessCodec, Some(&approved_codec_witness()), kind, bytes).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_loopback_signed_session_serves_read_only_business_actions() {
    let mut prod = Production::new(None).await;
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let connector = prod.take_connector();
    let journal_dir = prod.journal_dir.clone();
    let state_dir = prod.state_dir.clone();
    let marker = prod.marker();
    let board_id = prod.board_id.clone();
    let run = tokio::spawn(run_board_connection(
        connector,
        policy(),
        shutdown.clone(),
        observer,
    ));

    let session = prod.controller.next_session().await;
    // 必须等板端两条流各自首 probe 成功（Ready）后再发业务，否则拿到 StreamClosed。
    wait_for("板端 Ready", || ready_count(&events) >= 1).await;
    session.wait_ready().await.expect("中控侧两条流首次健康");
    let reply = session
        .request(StreamKind::Control, "device.status.get", query_bytes())
        .await
        .expect("status 请求");
    assert_eq!(reply.status_code, 0);
    match decode(&reply.payload, BusinessPayloadKind::DeviceStatus) {
        BusinessPayload::DeviceStatus(status) => {
            assert!(status.sample_seq >= 1);
            assert_eq!(status.clock_quality, "observed");
        }
        _ => panic!("status 回包形状不符"),
    }

    let reply = session
        .request(
            StreamKind::Control,
            "device.capabilities.get",
            query_bytes(),
        )
        .await
        .expect("capabilities 请求");
    assert_eq!(reply.status_code, 0);
    match decode(&reply.payload, BusinessPayloadKind::Capabilities) {
        BusinessPayload::Capabilities(caps) => {
            // 未注入真实 reboot dispatcher：能力清单不得虚报可执行重启。
            assert!(
                !caps
                    .supported_actions
                    .iter()
                    .any(|action| action.starts_with("device.reboot.")),
                "能力申报不得包含未接线的重启"
            );
            assert_eq!(caps.boot_id, board_id);
            assert!(caps.durable_task_journal);
        }
        _ => panic!("capabilities 回包形状不符"),
    }

    let reply = session
        .request(StreamKind::Control, "device.clock.get", query_bytes())
        .await
        .expect("clock 请求");
    assert_eq!(reply.status_code, 0);

    // journal 无记录：1006 + 空 payload，绝不伪造记录。
    let reply = session
        .request(
            StreamKind::Control,
            "device.task.get",
            encode_business(
                &BusinessCodec,
                Some(&approved_codec_witness()),
                &BusinessPayload::TaskQuery(TaskQuery {
                    schema_version: BUSINESS_SCHEMA_VERSION,
                    sub_task_id: SUB_TASK.to_string(),
                }),
            )
            .unwrap(),
        )
        .await
        .expect("task.get 请求");
    assert_eq!(reply.status_code, 1006);
    assert!(reply.payload.is_empty());

    // 载荷类型不符（Query 冒充 RebootPrepare）：1003 + 空 payload，且不触达任何来源。
    let calls_before = prod_source_calls(&prod);
    let reply = session
        .request(StreamKind::Control, "device.reboot.prepare", query_bytes())
        .await
        .expect("错误载荷也必须有回包");
    assert_eq!(reply.status_code, 1003);
    assert!(reply.payload.is_empty());
    assert_eq!(
        prod_source_calls(&prod),
        calls_before,
        "解码失败前不得触达本机来源"
    );

    // 合法 RebootPrepare + 未注入真实 reboot dispatcher：1004 + 空 payload，
    // 绝不伪造票据，也绝不触发任何真实重启。
    let reply = session
        .request(
            StreamKind::Control,
            "device.reboot.prepare",
            encode_business(
                &BusinessCodec,
                Some(&approved_codec_witness()),
                &BusinessPayload::RebootPrepare(RebootPrepare {
                    schema_version: BUSINESS_SCHEMA_VERSION,
                    sub_task_id: SUB_TASK.to_string(),
                    expected_boot_id: board_id.clone(),
                }),
            )
            .unwrap(),
        )
        .await
        .expect("reboot.prepare 请求");
    assert_eq!(reply.status_code, 1004);
    assert!(reply.payload.is_empty());

    // 白名单外动作：1002。
    let reply = session
        .request(StreamKind::Control, "device.shutdown", query_bytes())
        .await
        .expect("未知动作也必须有回包");
    assert_eq!(reply.status_code, 1002);
    assert!(reply.payload.is_empty());

    // shutdown：真实 GOAWAY 排空，服务端会话状态必须离开 Ready。
    let mut state = session.subscribe_state();
    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .expect("生产循环必须在排空后收口")
        .unwrap();
    assert_eq!(outcome, RunOutcome::Shutdown);
    assert!(matches!(
        *state.borrow_and_update(),
        SessionState::Draining | SessionState::Closed
    ));
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == ConnectEventKind::Ready)
    );
    // journal 真实落盘（RealFs 持久路径，不是内存假 durable）。
    assert!(
        journal_dir
            .join(JournalStore::<RealFs>::META_FILE)
            .is_file()
    );
    assert!(journal_dir.join(JournalStore::<RealFs>::LOG_FILE).is_file());
    // 未收到 revoke：不得留下未授权标记。
    assert!(!marker.path().exists());
    assert!(
        state_dir_of(&state_dir.join("identity.pk8"))
            .join("identity.pk8")
            .is_file()
    );
    drop(board_id);
    prod.controller.take().await;
    prod.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_pin_fails_handshake_with_zero_business_side_effects() {
    let mut prod = Production::pinned(None, true).await;
    let before = prod.source_calls.load(Ordering::SeqCst);
    let log = prod.journal_dir.join(JournalStore::<RealFs>::LOG_FILE);
    let _ = &mut prod;
    let log_len_before = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let connector = prod.take_connector();
    let opened = tokio::time::timeout(
        Duration::from_secs(25),
        connector.open(ConnectionId::new_v4()),
    )
    .await
    .expect("握手必须在有界期限内给出结论");
    let failure = match opened {
        Ok(_) => panic!("未受信公钥必须握手失败，不能建立会话"),
        Err(failure) => failure,
    };
    // 分类是「可重试的握手失败」：不是吊销，也不是本端故障。
    assert_eq!(
        failure,
        rsetup_board_agent::connect::AttemptFailure::Retryable(RetryReason::Handshake),
        "坏 pin 不得冒充合法吊销"
    );
    // 零副作用：业务 handler / 本机来源 / journal 都未被触达。
    assert_eq!(prod.source_calls.load(Ordering::SeqCst), before);
    assert_eq!(
        std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0),
        log_len_before,
        "握手失败前不得写 journal"
    );
    assert!(!prod.marker().path().exists());
    prod.controller.take().await;
    prod.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_reject_reason_two_stops_before_any_session() {
    let mut prod = Production::new(Some(2)).await;
    let (events, observer) = events_collector();
    let connector = prod.take_connector();
    let marker = prod.marker();
    let outcome = tokio::time::timeout(
        Duration::from_secs(25),
        run_board_connection(connector, policy(), Shutdown::new(), observer),
    )
    .await
    .expect("合法吊销必须立即返回");
    assert_eq!(
        outcome,
        RunOutcome::Stopped {
            stop: PermanentStop::Revoked,
            state_written: true
        }
    );
    assert_eq!(outcome.exit_code(), 0, "0 退出才不会被 systemd 重启绕过");
    assert!(marker.path().is_file(), "必须落盘本地未授权状态");
    assert_eq!(prod.controller.rejections.load(Ordering::SeqCst), 1);
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind == ConnectEventKind::Stopped)
    );
    prod.controller.take().await;
    prod.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_reject_reason_five_keeps_backoff_and_never_stops() {
    // reason=5（服务端过载）可重试，绝不能被当成吊销。
    let mut prod = Production::new(Some(5)).await;
    let (_events, observer) = events_collector();
    let connector = prod.take_connector();
    let marker = prod.marker();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        connector,
        policy(),
        shutdown.clone(),
        observer,
    ));
    wait_for("过载拒绝后继续重试", || {
        prod.controller.rejections.load(Ordering::SeqCst) >= 2
    })
    .await;
    shutdown.signal();
    assert_eq!(run.await.unwrap(), RunOutcome::Shutdown);
    assert!(
        !marker.path().exists(),
        "过载拒绝不得写吊销状态（只有 reason 2/3 才停）"
    );
    prod.controller.take().await;
    prod.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_session_kick_reconnects_and_revoke_stops_permanently() {
    let mut prod = Production::new(None).await;
    let (events, observer) = events_collector();
    let connector = prod.take_connector();
    let marker = prod.marker();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        connector,
        policy(),
        shutdown.clone(),
        observer,
    ));

    // Kick：按退避重连，会看到第二条真实会话。
    let first = prod.controller.next_session().await;
    wait_for("第一条会话板端 Ready", || ready_count(&events) >= 1).await;
    first.send_system(SystemSignal::Kick).await.expect("kick");
    let second = prod.controller.next_session().await;
    wait_for("第二条会话板端 Ready", || ready_count(&events) >= 2).await;
    assert!(prod.controller.handshakes.load(Ordering::SeqCst) >= 2);
    assert!(!marker.path().exists(), "Kick 不是吊销");

    // Revoke：停止自动重连 + 落盘未授权状态。
    second
        .send_system(SystemSignal::Revoke)
        .await
        .expect("revoke");
    let outcome = tokio::time::timeout(Duration::from_secs(25), run)
        .await
        .expect("revoke 必须收口循环")
        .unwrap();
    assert_eq!(
        outcome,
        RunOutcome::Stopped {
            stop: PermanentStop::Revoked,
            state_written: true
        }
    );
    assert_eq!(outcome.exit_code(), 0);
    assert!(marker.path().is_file());
    prod.controller.take().await;
    prod.cleanup();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn business_request_on_the_real_data_stream_is_rejected_without_touching_sources() {
    let mut prod = Production::new(None).await;
    let before = prod.source_calls.load(Ordering::SeqCst);
    let (events, observer) = events_collector();
    let connector = prod.take_connector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        connector,
        policy(),
        shutdown.clone(),
        observer,
    ));
    let session = prod.controller.next_session().await;
    wait_for("板端 Ready", || ready_count(&events) >= 1).await;
    session.wait_ready().await.expect("中控侧两条流首次健康");
    // 业务只允许 Control 流：数据流上的业务请求必须类型化拒绝且不触达任何来源。
    let reply = session
        .request(StreamKind::Data, "device.status.get", query_bytes())
        .await
        .expect("数据流业务也必须有回包");
    assert_eq!(reply.status_code, 1003);
    assert!(reply.payload.is_empty());
    assert_eq!(
        prod.source_calls.load(Ordering::SeqCst),
        before,
        "数据流业务必须在触达任何来源之前被拒绝"
    );
    shutdown.signal();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(25), run)
            .await
            .unwrap()
            .unwrap(),
        RunOutcome::Shutdown
    );
    prod.controller.take().await;
    prod.cleanup();
}
