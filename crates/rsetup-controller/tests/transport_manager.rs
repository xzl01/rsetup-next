//! `TransportManager` 的连接语义集成测试（Task 5）。
//!
//! # 本文件证明什么、不证明什么（诚实边界）
//!
//! * **证明**：登记表与代际语义（同 key 顶替、旧代际迟到不可见）、`TYPE_SYSTEM` 信令
//!   与延迟切断的先后与只关旧连接、`after_commit` 撤销活跃会话与挂起握手、挂起凭证已取消
//!   时拒绝登记、半开预算有界且 RAII 归还、`DeviceRpc` 恰好一次不重发、准入复查按
//!   **SQL 真相 seam** fail-closed、轮询驱动写入当前代际样本、后台任务随 shutdown 终止。
//! * **不证明**：真实 `TcpListener`、真实记录层/h2 会话、真实 `devices` 表与真实板端进程。
//!   全部用注入替身（`FakeConnection`/`FakeTruth`）；`AdmissionTruth` 是**只读 SQL 入口的
//!   seam**，替身通过不等于真实库撤回行为已实测（真实库验收见
//!   `docs/testing/tunnel-controller-manager-report.md` 的「未验」清单）。
//! * 不触达任何数据库：`AdmissionTruth` 由本文件注入替身，无需 `connect_lazy`。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use chrono::Utc;
use tokio::sync::watch;
use uuid::Uuid;

use rsetup_controller::api::approvals::ManagementNotificationSink;
use rsetup_controller::api::devices::{DeviceConnectionState, DeviceLiveProbe, DeviceStreamHealth};
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::model::{AdmissionSnapshot, AdmissionState, ReviewDecision};
use rsetup_controller::polling::{SnapshotGeneration, SnapshotStore};
use rsetup_controller::tasks::adapter::{TaskTimeSource, TimeEvidenceFuture};
use rsetup_controller::tasks::board_client::BoardClientError;
use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
use rsetup_controller::transport::board_client::RpcReply;
use rsetup_controller::transport::manager::{
    AdmissionDecisionFuture, AdmissionTruth, CallError, CancelSignal, ConnectionFuture,
    ConnectionState, DeviceConnection, HalfOpenPermit, ManagerConfig, NewSession, PendingTicket,
    RegisterError, SampleOutcome, SystemNotice, TransportManager,
};
use rsetup_controller::{ControllerError, model};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::sig::{REASON_APPROVAL_DENIED, REASON_REVOKED};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, DeviceStatus, approved_codec_witness,
    encode_business,
};
use rsetup_tunnel::handshake::AdmissionDecision;

const K1: [u8; 32] = [1u8; 32];
const K2: [u8; 32] = [2u8; 32];

// ---------------------------------------------------------------------------
// 替身
// ---------------------------------------------------------------------------

/// 一条 fake 连接的可观测面（`Arc` 共享，管理器丢弃连接后仍可断言）。
#[derive(Default)]
struct Obs {
    calls: Mutex<Vec<&'static str>>,
    systems: Mutex<Vec<(SystemNotice, Instant)>>,
    aborted_at: Mutex<Option<Instant>>,
    aborts: AtomicUsize,
    drains: AtomicUsize,
}

impl Obs {
    fn aborts(&self) -> usize {
        self.aborts.load(Ordering::Acquire)
    }
    fn drains(&self) -> usize {
        self.drains.load(Ordering::Acquire)
    }
    fn systems(&self) -> Vec<(SystemNotice, Instant)> {
        self.systems.lock().expect("systems lock").clone()
    }
    fn aborted_at(&self) -> Option<Instant> {
        *self.aborted_at.lock().expect("aborted lock")
    }
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls lock").clone()
    }
}

/// [`DeviceConnection`] 的测试替身：按 action 返回固定回复，并记录信令/abort/排空。
///
/// 契约要点逐项落实（与 `SessionHandle` 适配一致）：`request` 恰好一次不重试；
/// `send_system` 的 `Ok(())` 只表示入队；`abort` 幂等。
/// 按 action 返回的固定回复（`(status_code, payload)` 或 typed 失败）。
type Replies = Mutex<HashMap<&'static str, Result<(i32, Vec<u8>), CallError>>>;

struct FakeConnection {
    state: AtomicUsize,
    replies: Replies,
    delay: Duration,
    send_fails: bool,
    obs: Arc<Obs>,
}

impl FakeConnection {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicUsize::new(1),
            replies: Mutex::new(HashMap::new()),
            delay: Duration::ZERO,
            send_fails: false,
            obs: Arc::new(Obs::default()),
        })
    }
    /// `send_fails = true`：信令发不出去（连接已坏）的路径。
    fn with_send_failure(send_fails: bool) -> Arc<Self> {
        let conn = Self::new();
        Arc::new(Self {
            state: AtomicUsize::new(1),
            replies: Mutex::new(HashMap::new()),
            delay: Duration::ZERO,
            send_fails,
            obs: Arc::clone(&conn.obs),
        })
    }
    fn with_state(state: ConnectionState, obs: Arc<Obs>) -> Arc<Self> {
        Arc::new(Self {
            state: AtomicUsize::new(state_code(state)),
            replies: Mutex::new(HashMap::new()),
            delay: Duration::ZERO,
            send_fails: false,
            obs,
        })
    }
    fn set_reply(&self, action: &'static str, reply: Result<(i32, Vec<u8>), CallError>) {
        self.replies
            .lock()
            .expect("replies lock")
            .insert(action, reply);
    }
    fn set_status(&self, status: &DeviceStatus) {
        let wire = encode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            &BusinessPayload::DeviceStatus(status.clone()),
        )
        .expect("status 编码");
        self.set_reply("device.status.get", Ok((0, wire)));
    }
    fn set_capabilities(&self, capabilities: &Capabilities) {
        let wire = encode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            &BusinessPayload::Capabilities(capabilities.clone()),
        )
        .expect("capabilities 编码");
        self.set_reply("device.capabilities.get", Ok((0, wire)));
    }
}

fn state_code(state: ConnectionState) -> usize {
    match state {
        ConnectionState::Connecting => 0,
        ConnectionState::Ready => 1,
        ConnectionState::Draining => 2,
        ConnectionState::Closed => 3,
    }
}

impl DeviceConnection for FakeConnection {
    fn request(
        &self,
        action: &'static str,
        _payload: Vec<u8>,
    ) -> ConnectionFuture<'_, Result<RpcReply, CallError>> {
        let obs = Arc::clone(&self.obs);
        obs.calls.lock().expect("calls lock").push(action);
        let reply = self
            .replies
            .lock()
            .expect("replies lock")
            .get(action)
            .cloned()
            .unwrap_or(Err(CallError::Closed));
        let delay = self.delay;
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let (status_code, payload) = reply?;
            Ok(RpcReply {
                status_code,
                payload,
            })
        })
    }

    fn state(&self) -> ConnectionState {
        match self.state.load(Ordering::Acquire) {
            0 => ConnectionState::Connecting,
            1 => ConnectionState::Ready,
            2 => ConnectionState::Draining,
            _ => ConnectionState::Closed,
        }
    }

    fn send_system(&self, notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>> {
        let obs = Arc::clone(&self.obs);
        obs.systems
            .lock()
            .expect("systems lock")
            .push((notice, Instant::now()));
        let fails = self.send_fails;
        Box::pin(async move { if fails { Err(()) } else { Ok(()) } })
    }

    fn graceful_shutdown(&self) {
        self.obs.drains.fetch_add(1, Ordering::AcqRel);
    }

    fn abort(&self) {
        let obs = Arc::clone(&self.obs);
        obs.aborts.fetch_add(1, Ordering::AcqRel);
        let mut guard = obs.aborted_at.lock().expect("aborted lock");
        if guard.is_none() {
            *guard = Some(Instant::now());
        }
    }
}

/// 只读准入真相替身（**seam**，不是真实库）。
#[derive(Default)]
struct FakeTruth {
    /// 只读「行」：`AdmissionSnapshot` 是 `Copy`，替身无需复制 `ControllerError`。
    rows: Mutex<HashMap<[u8; 32], AdmissionSnapshot>>,
    /// 读失败集合（模拟行不存在/库异常）：优先级高于 `rows`。
    failing: Mutex<HashSet<[u8; 32]>>,
    loads: AtomicUsize,
}

impl FakeTruth {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn put(&self, key: [u8; 32], snapshot: AdmissionSnapshot) {
        self.rows.lock().expect("rows lock").insert(key, snapshot);
        self.failing.lock().expect("failing lock").remove(&key);
    }
    fn put_error(&self, key: [u8; 32]) {
        self.rows.lock().expect("rows lock").remove(&key);
        self.failing.lock().expect("failing lock").insert(key);
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::Acquire)
    }
}

impl AdmissionTruth for FakeTruth {
    /// 返回 `AdmissionDecision`：与生产 `DbPool` 实现里 `classify_row` 的判定逐项一致
    /// （批准的**且仅批准的**组合 → `Approved`；已吊销/已拒绝 → 带原因的 `Rejected`；
    /// 其余（重开审批等）→ `Pending`；读不到 → `Err`）。替身不引入第二套判定。
    fn load_current(&self, public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        let row = if self
            .failing
            .lock()
            .expect("failing lock")
            .contains(&public_key)
        {
            Err(ControllerError::NotFound)
        } else {
            self.rows
                .lock()
                .expect("rows lock")
                .get(&public_key)
                .copied()
                .ok_or(ControllerError::NotFound)
        };
        self.loads.fetch_add(1, Ordering::AcqRel);
        let decision =
            row.map(
                |snapshot| match (snapshot.admission_state, snapshot.review_decision) {
                    (AdmissionState::Approved, ReviewDecision::Approved) => {
                        AdmissionDecision::Approved
                    }
                    (AdmissionState::Revoked, ReviewDecision::Revoked) => {
                        AdmissionDecision::Rejected(REASON_REVOKED)
                    }
                    (AdmissionState::Pending, ReviewDecision::Denied) => {
                        AdmissionDecision::Rejected(REASON_APPROVAL_DENIED)
                    }
                    _ => AdmissionDecision::Pending,
                },
            );
        Box::pin(async move { decision })
    }
}

/// 固定时间证据替身（生产为唯一 `TimeGateHandle`；本测试不测时钟质量语义）。
struct FixedTime;

impl TaskTimeSource for FixedTime {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
        Box::pin(async {
            let now = Utc::now();
            TimeEvidence {
                system_wall_utc: now,
                reference_utc: now,
                quality: TimeQuality::SystemFallback,
                clock_epoch: Uuid::new_v4(),
                source: None,
                sample_age_ms: None,
                offset_ms: None,
                uncertainty_ms: None,
            }
        })
    }
}

/// 恢复模式探针替身：与生产装配一致地 **fail-closed**（管理器只委托，不造第二真源）。
struct FailingRecovery;

impl RecoveryModeProbe for FailingRecovery {
    fn current_mode<'a>(&'a self) -> rsetup_controller::api::ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "recovery controller not wired".into(),
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn approved(revision: u64) -> AdmissionSnapshot {
    AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision,
    }
}

fn revoked(revision: u64) -> AdmissionSnapshot {
    AdmissionSnapshot {
        admission_state: AdmissionState::Revoked,
        review_decision: ReviewDecision::Revoked,
        revision,
    }
}

fn denied(revision: u64) -> AdmissionSnapshot {
    AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::Denied,
        revision,
    }
}

struct Fixture {
    manager: Arc<TransportManager>,
    truth: Arc<FakeTruth>,
    snapshots: Arc<SnapshotStore>,
}

fn fixture(config: ManagerConfig) -> Fixture {
    let truth = FakeTruth::new();
    truth.put(K1, approved(1));
    truth.put(K2, approved(1));
    let snapshots = Arc::new(SnapshotStore::new());
    let manager = TransportManager::new(
        Arc::clone(&truth) as Arc<dyn AdmissionTruth>,
        Arc::clone(&snapshots),
        Arc::new(FailingRecovery),
        Arc::new(FixedTime),
        config,
    );
    Fixture {
        manager,
        truth,
        snapshots,
    }
}

/// 登记一条 ready 会话（挂起凭证由 `track_pending` 产生，与握手路径同源）。
fn register(
    manager: &Arc<TransportManager>,
    key: [u8; 32],
    connection: Arc<FakeConnection>,
) -> Result<uuid::Uuid, RegisterError> {
    let pending = manager.track_pending(key);
    register_with_pending(manager, key, connection, pending)
}

/// 用**外部产生的**挂起凭证登记（测顶替顺序、撤销竞争与半开名额转移）。
fn register_with_pending(
    manager: &Arc<TransportManager>,
    key: [u8; 32],
    connection: Arc<FakeConnection>,
    pending: PendingTicket,
) -> Result<uuid::Uuid, RegisterError> {
    let half_open = permit(manager);
    manager
        .register_ready(NewSession {
            public_key: key,
            connection,
            pending,
            half_open,
        })
        .map(|outcome| outcome.generation)
}

/// 取一个半开名额（与握手路径同源：accept 后、握手前取得，登记成功才转移释放）。
fn permit(manager: &Arc<TransportManager>) -> HalfOpenPermit {
    manager.half_open_permit().expect("半开名额")
}

async fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cond()
}

fn status_payload(seq: u64) -> DeviceStatus {
    DeviceStatus {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: Uuid::new_v4().to_string(),
        agent_epoch: Uuid::new_v4().to_string(),
        sample_seq: seq,
        sampled_unix_ms: 1_700_000_000_000,
        sampled_monotonic_ns: seq,
        clock_epoch: Uuid::new_v4().to_string(),
        metrics: Vec::new(),
        clock_quality: "observed".to_string(),
    }
}

fn capabilities_payload() -> Capabilities {
    Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: Uuid::new_v4().to_string(),
        agent_version: "1.0.0".to_string(),
        supported_actions: vec!["device.status.get".to_string()],
        durable_task_journal: true,
        max_status_bytes: 65_536,
        journal_epoch: Uuid::new_v4().to_string(),
    }
}

// ---------------------------------------------------------------------------
// 1. 同 key 顶替：先 kick，再按 §4.7 单调延迟切断**旧**连接
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_key_replacement_kicks_old_then_closes_only_old_after_delay() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let old = FakeConnection::new();
    let old_obs = Arc::clone(&old.obs);
    let old_generation = register(&manager, K1, old).expect("首次登记");

    let new = FakeConnection::new();
    let new_obs = Arc::clone(&new.obs);
    let outcome = manager
        .register_ready(NewSession {
            public_key: K1,
            connection: new,
            pending: manager.track_pending(K1),
            half_open: permit(&manager),
        })
        .expect("同 key 顶替");
    assert_eq!(outcome.replaced, Some(old_generation));

    // 代际切换是同步的：登记返回即旧代际不可见。
    let (generation, connection) = manager.current_connection(&K1).expect("当前会话");
    assert_eq!(generation, outcome.generation);
    assert_eq!(connection.state(), ConnectionState::Ready);

    // 旧控制流上必须先发 TYPE_SYSTEM kick（不是 request 冒充）。
    let kicked = wait_until(Duration::from_millis(200), || !old_obs.systems().is_empty()).await;
    assert!(kicked, "旧连接必须收到 TYPE_SYSTEM kick");
    let (notice, sent_at) = old_obs.systems()[0];
    assert_eq!(notice, SystemNotice::Kick);
    assert_eq!(old_obs.aborts(), 0, "信令刚入队即切断会让信令丢失");

    // 单调延迟（500ms）后只切断被顶替那份句柄。
    assert!(
        wait_until(Duration::from_millis(2_000), || old_obs.aborts() > 0).await,
        "被顶替的旧连接必须在信令后按延迟被切断"
    );
    let aborted_at = old_obs.aborted_at().expect("abort 时刻");
    let elapsed = aborted_at.saturating_duration_since(sent_at);
    assert!(
        elapsed >= Duration::from_millis(450),
        "延迟不得短于 §4.7 的 500ms（实测 {elapsed:?}）"
    );
    assert!(
        elapsed < Duration::from_millis(2_000),
        "延迟必须是有限且接近 500ms（实测 {elapsed:?}）"
    );
    assert_eq!(
        new_obs.aborts(),
        0,
        "定时器只持有被顶替那份句柄，不得误关新代际"
    );
    assert_eq!(new_obs.systems().len(), 0, "新连接不应收到 kick");
}

#[tokio::test]
async fn signal_send_failure_closes_immediately_without_waiting() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    // 旧连接已坏：send_system 失败 → 不等 500ms，立即 abort（安全边界更靠前）。
    let old = FakeConnection::with_send_failure(true);
    let old_obs = Arc::clone(&old.obs);
    register(&manager, K1, old).expect("登记");
    manager
        .register_ready(NewSession {
            public_key: K1,
            connection: FakeConnection::new(),
            pending: manager.track_pending(K1),
            half_open: permit(&manager),
        })
        .expect("顶替");

    assert!(
        wait_until(Duration::from_millis(120), || old_obs.aborts() > 0).await,
        "信令发不出去时必须立即切断"
    );
    assert_eq!(manager.system_signals_sent(), 0);
    assert_eq!(manager.system_signals_unsent(), 1, "未发边界必须可观测");
}

// ---------------------------------------------------------------------------
// 2. 旧代际迟到：不可见、不活动、不写样本
// ---------------------------------------------------------------------------

#[tokio::test]
async fn old_generation_late_activity_call_and_sample_are_all_rejected() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig::default());
    let first = FakeConnection::new();
    let old_generation = register(&manager, K1, first).expect("登记");
    let second = FakeConnection::new();
    let second_obs = Arc::clone(&second.obs);
    let new_generation = register(&manager, K1, second).expect("顶替");
    assert_ne!(old_generation, new_generation);

    // last_seen 只能由当前代际更新。
    assert!(!manager.note_activity(&K1, old_generation));
    assert!(manager.note_activity(&K1, new_generation));
    assert!(!manager.session_is_current(&K1, old_generation));

    // 旧代际连接结束不得抹掉新代际，也不得推进快照断开。
    assert!(!manager.deregister(&K1, old_generation));
    assert!(manager.session_is_current(&K1, new_generation));

    // 旧代际迟到样本：StaleGeneration，且绝不为它建立代际。
    let status = status_payload(7);
    assert_eq!(
        manager.ingest_status_sample(
            &K1,
            old_generation,
            &status,
            fixed_time().await,
            Instant::now()
        ),
        SampleOutcome::StaleGeneration
    );
    assert!(snapshots.read(&K1).is_none(), "旧代际不得建立快照条目");

    // 旧代际的 RPC 也不应再走旧连接：DeviceRpc 只按当前代际路由。
    let rpc = manager.as_device_rpc();
    rpc.request(&K1, "device.status.get", Vec::new()).await.ok();
    assert_eq!(second_obs.calls(), vec!["device.status.get"]);
}

async fn fixed_time() -> TimeEvidence {
    FixedTime.current_evidence().await
}

#[tokio::test]
async fn current_generation_sample_is_confirmed_once_and_written() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let generation = register(&manager, K1, conn).expect("登记");
    let status = status_payload(3);
    let coordinate = SnapshotGeneration {
        session_epoch: generation,
        boot_id: status.boot_id.clone(),
        ..clone_stream(&status, generation)
    };
    let outcome =
        manager.ingest_status_sample(&K1, generation, &status, fixed_time().await, Instant::now());
    assert!(matches!(outcome, SampleOutcome::Accepted(_)), "{outcome:?}");
    let view = snapshots.read(&K1).expect("首样本已写入");
    assert_eq!(view.status.sample_seq, 3);
    // 代际坐标只由管理器产生（session_epoch=本地 generation），boot/agent 取自当前代际样本。
    assert_eq!(view.generation.session_epoch, generation);
    assert_eq!(view.generation.boot_id, coordinate.boot_id);
    // 同序号重复样本被拒但保留好样本（序号下界不因重复而丢失）。
    let repeat =
        manager.ingest_status_sample(&K1, generation, &status, fixed_time().await, Instant::now());
    assert_eq!(repeat, SampleOutcome::Rejected("stale-sample-seq"));
    assert_eq!(
        snapshots.read(&K1).expect("好样本仍在").status.sample_seq,
        3
    );
}

fn clone_stream(status: &DeviceStatus, generation: Uuid) -> SnapshotGeneration {
    let _ = generation;
    SnapshotGeneration {
        session_epoch: Uuid::nil(),
        stream_epoch: Uuid::nil(),
        boot_id: status.boot_id.clone(),
        agent_epoch: status.agent_epoch.clone(),
    }
}

// ---------------------------------------------------------------------------
// 3. after_commit 撤销：活跃会话 + 挂起握手，即时且按类型选原因
// ---------------------------------------------------------------------------

#[tokio::test]
async fn after_commit_revocation_terminates_active_session_and_pending_handshake() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig::default());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let drain = manager.spawn_revoke_drain(shutdown_rx);

    let conn = FakeConnection::new();
    let conn_obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");
    let pending = manager.track_pending(K1);
    assert!(!pending.is_cancelled(), "登记后新挂起握手仍是 PENDING 候选");
    assert_eq!(manager.pending_count(), 1);

    // SQL 已撤回批准 → after_commit 只投递，真正撤销在 drain 任务里发生。
    truth.put(K1, revoked(9));
    manager
        .after_commit(&rsetup_controller::devices::service::AdmissionChangeEvent {
            public_key: K1,
            previous_state: AdmissionState::Approved,
            snapshot: revoked(9),
        })
        .expect("通知已入队");

    assert!(
        wait_until(Duration::from_millis(500), || manager
            .current_connection(&K1)
            .is_none())
        .await,
        "撤销必须终止活跃会话"
    );
    assert!(
        wait_until(Duration::from_millis(500), || manager.pending_count() == 0).await,
        "撤销必须取消挂起握手"
    );
    assert!(pending.is_cancelled(), "挂起握手必须被取消");
    assert!(
        wait_until(Duration::from_millis(200), || !conn_obs
            .systems()
            .is_empty())
        .await,
        "撤销必须发 TYPE_SYSTEM revoke"
    );
    assert_eq!(conn_obs.systems()[0].0, SystemNotice::Revoke);
    // 切断发生在「revoke 信令入队」之后的有界 flush 窗口内（路由层已在撤销瞬间摘除会话）。
    assert!(
        wait_until(Duration::from_millis(1_500), || conn_obs.aborts() > 0).await,
        "撤销必须在信令后被强制切断，不允许旧会话继续"
    );
    let rpc = manager.as_device_rpc();
    assert!(matches!(
        rpc.request(&K1, "device.status.get", Vec::new()).await,
        Err(BoardClientError::DeviceOffline)
    ));

    let _ = shutdown_tx.send(true);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), drain)
            .await
            .is_ok(),
        "撤销消费任务必须随 shutdown 终止"
    );
}

#[tokio::test]
async fn approved_notification_never_terminates_existing_session() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig::default());
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let _drain = manager.spawn_revoke_drain(shutdown_rx);
    let conn = FakeConnection::new();
    let conn_obs = Arc::clone(&conn.obs);
    let generation = register(&manager, K1, conn).expect("登记");
    truth.put(K1, approved(2));
    manager
        .after_commit(&rsetup_controller::devices::service::AdmissionChangeEvent {
            public_key: K1,
            previous_state: AdmissionState::Pending,
            snapshot: approved(2),
        })
        .expect("批准通知无需任何动作");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(manager.session_is_current(&K1, generation));
    assert_eq!(conn_obs.systems().len(), 0, "批准不得动既有会话");
    assert_eq!(conn_obs.aborts(), 0);
}

#[tokio::test]
async fn cancelled_pending_refuses_registration_of_revoked_identity() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let conn_obs = Arc::clone(&conn.obs);
    let pending = manager.track_pending(K1);
    // 撤销通知在 ready 之前到达（握手与撤销的竞态窗口）。
    pending.cancel();
    let error = manager
        .register_ready(NewSession {
            public_key: K1,
            connection: conn,
            pending,
            half_open: permit(&manager),
        })
        .expect_err("已撤销身份不得装成当前会话");
    assert_eq!(error, RegisterError::Revoked);
    assert!(manager.current_connection(&K1).is_none());
    assert_eq!(manager.pending_count(), 0, "拒绝后挂起计数必须归还");
    // 审查 P1-4：登记被拒同样必须**归还半开名额**（名额不会因拒绝路径泄漏）。
    assert_eq!(manager.half_open_count(), 0, "登记被拒后半开名额必须归还");
    assert_eq!(conn_obs.systems().len(), 0, "未登记的连接不该收到信令");
}

// ---------------------------------------------------------------------------
// 4. 通知通道满：不假装成功，唤醒补扫
// ---------------------------------------------------------------------------

#[tokio::test]
async fn notification_channel_full_reports_error_and_counts_drop() {
    // 不启动 drain 任务：256 容量必然填满，第 257 条必须报错（不假装已入队）。
    let Fixture { manager, truth, .. } = fixture(ManagerConfig::default());
    truth.put(K1, revoked(1));
    let event = rsetup_controller::devices::service::AdmissionChangeEvent {
        public_key: K1,
        previous_state: AdmissionState::Approved,
        snapshot: revoked(1),
    };
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for _ in 0..300 {
        if manager.after_commit(&event).is_ok() {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    assert!(accepted > 0);
    assert!(
        rejected > 0,
        "通道满必须返回 Err，让 notification_queued=false"
    );
    assert_eq!(manager.dropped_revocations() as usize, rejected);
}

// ---------------------------------------------------------------------------
// 5. 半开预算
// ---------------------------------------------------------------------------

#[tokio::test]
async fn half_open_budget_is_bounded_and_returned_by_raii() {
    let config = ManagerConfig {
        max_half_open: 3,
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(manager.half_open_permit().expect("预算内可预留"));
    }
    assert!(
        manager
            .half_open_permit()
            .is_err_and(|error| error == RegisterError::HalfOpenFull),
        "半开必须有全局上界（§4.6 第 1 条：资源拒绝可重试，不写永久决定）"
    );
    assert_eq!(manager.half_open_count(), 3);
    held.pop();
    assert_eq!(
        manager.half_open_count(),
        2,
        "permit Drop 即归还名额（RAII，无计数泄漏）"
    );
    let extra = manager.half_open_permit().expect("释放后可再预留");
    assert_eq!(manager.half_open_count(), 3);
    drop(extra);
    held.clear();
    assert_eq!(manager.half_open_count(), 0, "全部释放后计数必须归零");
}

#[tokio::test]
async fn sessions_budget_rejects_only_new_sessions() {
    let config = ManagerConfig {
        max_sessions: 1,
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    register(&manager, K1, FakeConnection::new()).expect("第一份");
    let error = register(&manager, K2, FakeConnection::new()).expect_err("活跃会话预算必须生效");
    assert_eq!(error, RegisterError::SessionsFull);
    assert_eq!(manager.pending_count(), 0, "拒绝后挂起计数必须归还");
    // 同 key 顶替不受预算影响。
    register(&manager, K1, FakeConnection::new()).expect("顶替不占新名额");
}

// ---------------------------------------------------------------------------
// 6. DeviceRpc 恰好一次、不重试、typed 错误映射
// ---------------------------------------------------------------------------

#[tokio::test]
async fn request_is_issued_exactly_once_and_maps_errors_without_retry() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let obs = Arc::clone(&conn.obs);
    conn.set_reply("device.status.get", Err(CallError::Timeout));
    register(&manager, K1, conn).expect("登记");
    let rpc = manager.as_device_rpc();

    let error = rpc
        .request(&K1, "device.status.get", Vec::new())
        .await
        .expect_err("超时必须上抛");
    assert!(
        matches!(error, BoardClientError::Timeout),
        "超时不得被误报为失败或离线：{error:?}"
    );
    assert_eq!(
        obs.calls().len(),
        1,
        "变更类请求结果未知，绝不重发（§6.4 不重试）"
    );

    for (call_error, expected) in [
        (CallError::Closed, BoardClientError::DeviceOffline),
        (CallError::Busy, BoardClientError::DeviceBusy),
        (CallError::Protocol, BoardClientError::ProtocolMismatch),
    ] {
        let conn = FakeConnection::new();
        conn.set_reply("device.status.get", Err(call_error));
        register(&manager, K2, conn).expect("登记");
        let error = rpc
            .request(&K2, "device.status.get", Vec::new())
            .await
            .expect_err("必须上抛");
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
        manager.apply_revoke(K2);
    }
}

#[tokio::test]
async fn request_waits_for_ready_and_rejects_non_ready_session() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::with_state(ConnectionState::Connecting, Arc::new(Obs::default()));
    register(&manager, K1, conn).expect("登记");
    let rpc = manager.as_device_rpc();
    assert!(
        matches!(
            rpc.request(&K1, "device.status.get", Vec::new()).await,
            Err(BoardClientError::DeviceOffline)
        ),
        "未 ready 的连接不得当作可路由"
    );
}

#[tokio::test]
async fn unknown_device_reads_are_honest_offline_not_error_or_fake_online() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let view = manager
        .snapshot(&[9u8; 32])
        .await
        .expect("读取本身不得失败");
    assert_eq!(view.connection_state, DeviceConnectionState::Offline);
    assert_eq!(view.control_health, DeviceStreamHealth::Offline);
    assert_eq!(view.data_health, DeviceStreamHealth::Offline);
    assert!(view.capabilities.is_none(), "离线设备不得伪造能力");
    // 恢复模式只委托：真源不可读即上抛（不降级成 ready=true）。
    let mode = manager
        .current_mode()
        .await
        .expect_err("恢复模式真源不可读必须上抛");
    assert!(matches!(mode, ControllerError::Config(_)));
}

#[tokio::test]
async fn live_view_projects_current_session_health_and_cached_capabilities() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let capabilities = capabilities_payload();
    conn.set_capabilities(&capabilities);
    let generation = register(&manager, K1, conn.clone()).expect("登记");
    let rpc = manager.as_device_rpc();
    // 能力只在当前代际的成功调用后进入 typed 缓存。
    rpc.request(&K1, "device.capabilities.get", Vec::new())
        .await
        .expect("能力回包");
    assert_eq!(
        manager.cached_capabilities(&K1),
        None,
        "缓存写入必须经 ingest_capabilities（带代际校验），不是任意回包"
    );
    assert!(manager.ingest_capabilities(&K1, generation, capabilities.clone()));
    assert!(!manager.ingest_capabilities(&K1, Uuid::nil(), capabilities.clone()));
    let view = manager.snapshot(&K1).await.expect("在线视图");
    assert_eq!(view.connection_state, DeviceConnectionState::Online);
    assert_eq!(view.control_health, DeviceStreamHealth::Healthy);
    let json = view.capabilities.expect("typed 能力投影");
    assert_eq!(json["boot_id"], serde_json::json!(capabilities.boot_id));
    assert_eq!(json["durable_task_journal"], serde_json::json!(true));
    assert!(
        json.get("journal_epoch").is_some(),
        "投影必须搬 typed 字段，不搬原始 wire"
    );
}

// ---------------------------------------------------------------------------
// 7. 准入复查：判据是 SQL 真相 seam，读失败同样 fail-closed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recheck_uses_sql_truth_and_revokes_on_change_or_read_failure() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let conn_obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");
    let before = truth.loads();

    // 仍然批准：什么都不做（不缓存批准，也不因复查断开）。
    assert_eq!(manager.recheck_all_sessions().await, 0);
    assert!(truth.loads() > before, "复查必须真的读一次真相入口");
    assert_eq!(conn_obs.aborts(), 0);
    assert!(manager.active_devices().contains(&K1));

    // 撤回：复查即撤销（有界通知丢失的 fallback）。
    truth.put(K1, revoked(5));
    assert_eq!(manager.recheck_all_sessions().await, 1);
    assert!(manager.current_connection(&K1).is_none());

    // 读不到（行不存在/库异常）：同样 fail-closed，内存「仍当前」不算证据。
    register(&manager, K2, FakeConnection::new()).expect("登记");
    truth.put_error(K2);
    assert_eq!(manager.recheck_all_sessions().await, 1);
    assert!(manager.current_connection(&K2).is_none());

    // Pending/Denied 也一律不再允许既有会话继续。
    register(&manager, K1, FakeConnection::new()).expect("登记");
    truth.put(K1, denied(6));
    assert_eq!(manager.recheck_all_sessions().await, 1);
}

#[tokio::test]
async fn recheck_task_runs_periodically_and_terminates_on_shutdown() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig {
        admission_recheck_interval: Duration::from_millis(40),
        ..ManagerConfig::default()
    });
    register(&manager, K1, FakeConnection::new()).expect("登记");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = manager.spawn_admission_recheck(shutdown_rx);
    assert!(
        wait_until(Duration::from_millis(500), || truth.loads() >= 2).await,
        "周期复查必须持续读真相入口"
    );
    let _ = shutdown_tx.send(true);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn sweep_wakeup_triggers_immediate_recheck() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig {
        admission_recheck_interval: Duration::from_secs(3_600),
        ..ManagerConfig::default()
    });
    register(&manager, K1, FakeConnection::new()).expect("登记");
    truth.put(K1, revoked(3));
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = manager.spawn_admission_recheck(shutdown_rx);
    manager.request_admission_sweep();
    assert!(
        wait_until(Duration::from_millis(500), || manager
            .current_connection(&K1)
            .is_none())
        .await,
        "唤醒必须立即复查并撤销（不等周期）"
    );
    task.abort();
}

// ---------------------------------------------------------------------------
// 8. 轮询驱动：复用既有调度器/快照，随 shutdown 终止，不积压
// ---------------------------------------------------------------------------

#[tokio::test]
async fn poll_driver_writes_current_generation_then_terminates() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig {
        poll_period: Duration::from_millis(120),
        poll_tick_cadence: Duration::from_millis(40),
        max_concurrent_polls: 2,
        ..ManagerConfig::default()
    });
    let status = status_payload(11);
    let boot_id = status.boot_id.clone();
    let agent_epoch = status.agent_epoch.clone();
    let conn = FakeConnection::new();
    conn.set_status(&status);
    let capabilities = capabilities_payload();
    conn.set_capabilities(&capabilities);
    let obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = manager.spawn_poll_driver(shutdown_rx);
    assert!(
        wait_until(Duration::from_millis(1_500), || snapshots
            .read(&K1)
            .is_some())
        .await,
        "轮询必须把当前代际样本写入共享快照 store"
    );
    let view = snapshots.read(&K1).expect("样本已写入");
    assert_eq!(view.status.sample_seq, 11);
    assert_eq!(view.status.boot_id, boot_id);
    assert_eq!(view.generation.agent_epoch, agent_epoch);
    assert!(
        wait_until(Duration::from_millis(500), || manager
            .cached_capabilities(&K1)
            .is_some())
        .await,
        "本代际首次应补一次能力查询（typed 缓存）"
    );
    assert!(
        obs.calls()
            .iter()
            .all(|action| *action == "device.status.get" || *action == "device.capabilities.get"),
        "轮询只允许读类动作"
    );
    let _ = shutdown_tx.send(true);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .is_ok(),
        "轮询驱动必须随 shutdown 终止"
    );
    assert!(snapshots.is_connected(&K1) == Some(true));
}

#[tokio::test]
async fn poll_driver_stops_polling_after_revoke() {
    let Fixture { manager, .. } = fixture(ManagerConfig {
        poll_period: Duration::from_millis(100),
        poll_tick_cadence: Duration::from_millis(40),
        ..ManagerConfig::default()
    });
    let conn = FakeConnection::new();
    conn.set_status(&status_payload(1));
    let obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = manager.spawn_poll_driver(shutdown_rx);
    assert!(wait_until(Duration::from_millis(1_000), || !obs.calls().is_empty()).await);
    manager.apply_revoke(K1);
    tokio::time::sleep(Duration::from_millis(80)).await;
    let after = obs.calls().len();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        obs.calls().len(),
        after,
        "撤销后的连接不得继续被轮询（登记表已无该设备）"
    );
    task.abort();
}

// ---------------------------------------------------------------------------
// 9. 后台任务与关停
// ---------------------------------------------------------------------------

#[tokio::test]
async fn all_background_tasks_terminate_on_shutdown_and_close_outs_drain() {
    let Fixture { manager, .. } = fixture(ManagerConfig {
        poll_period: Duration::from_millis(40),
        poll_tick_cadence: Duration::from_millis(10),
        ..ManagerConfig::default()
    });
    let conn = FakeConnection::new();
    let obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");
    let _pending = manager.track_pending(K2);

    let (_shutdown_tx, shutdown_rx) = watch::channel(true);
    let revoke = manager.spawn_revoke_drain(shutdown_rx.clone());
    let recheck = manager.spawn_admission_recheck(shutdown_rx.clone());
    let poll = manager.spawn_poll_driver(shutdown_rx);
    for task in [revoke, recheck, poll] {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .is_ok(),
            "已置真的 shutdown 必须让后台任务立即退出"
        );
    }
    drop(_pending);
    assert_eq!(
        manager.pending_count(),
        0,
        "后台任务不得留下挂起握手续约（自持凭证已释放）"
    );

    // 顶替产生的收尾任务有界：至多一次延迟后归零。
    register(&manager, K1, FakeConnection::new()).expect("顶替→产生一个收尾任务");
    assert!(obs.aborts() <= 1);
    assert!(
        wait_until(Duration::from_millis(2_000), || manager
            .pending_close_outs()
            == 0)
        .await,
        "「信令+延迟切断」收尾任务必须归零（有界，不泄漏）"
    );
    // shutdown 前已置真的 watch 也让 pending_close_outs 归零（收尾任务不依赖 watch）。
    assert!(manager.system_signals_sent() >= 1);
}

#[tokio::test]
async fn shutdown_all_gracefully_drains_and_cancels_pending() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let obs = Arc::clone(&conn.obs);
    register(&manager, K1, conn).expect("登记");
    let pending = manager.track_pending(K2);
    let count = manager.shutdown_all();
    assert_eq!(count, 1);
    assert_eq!(obs.drains(), 1, "关停走排空（GOAWAY），不是硬切断");
    assert_eq!(obs.aborts(), 0);
    assert!(pending.is_cancelled(), "挂起握手随关停取消");
    assert_eq!(manager.pending_count(), 0);
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn deregister_disconnects_snapshot_without_losing_scope_or_floor() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig::default());
    let conn = FakeConnection::new();
    let status = status_payload(21);
    let generation = register(&manager, K1, conn).expect("登记");
    manager.ingest_status_sample(&K1, generation, &status, fixed_time().await, Instant::now());
    assert_eq!(snapshots.is_connected(&K1), Some(true));
    assert!(manager.deregister(&K1, generation));
    assert_eq!(
        snapshots.is_connected(&K1),
        Some(false),
        "连接断开只复位连接标记"
    );
    // 作用域与序号下界保留：断线前那条流的迟到旧样本仍无法覆盖新值。
    assert!(snapshots.read(&K1).is_some(), "最后好样本不得因断开被清除");
    let late = manager.ingest_status_sample(
        &K1,
        generation,
        &status_payload(9),
        fixed_time().await,
        Instant::now(),
    );
    assert_eq!(late, SampleOutcome::StaleGeneration);
}

#[tokio::test]
async fn cancel_signal_wakes_waiters_and_is_idempotent() {
    let signal = Arc::new(CancelSignal::new());
    assert!(!signal.is_cancelled());
    let waiter = {
        let signal = Arc::clone(&signal);
        tokio::spawn(async move {
            signal.wait().await;
        })
    };
    // 先注册后取消：必须被唤醒。
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(signal.cancel(), "首次取消应完成翻转");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), waiter)
            .await
            .is_ok(),
        "取消必须唤醒等待者"
    );
    assert!(!signal.cancel(), "重复取消不改变状态");
}

#[tokio::test]
async fn pending_ticket_drop_returns_slot() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    {
        // 不同公钥的两个挂起握手并存（各占一个名额）。
        let first = manager.track_pending(K1);
        let _second = manager.track_pending(K2);
        assert_eq!(manager.pending_count(), 2);
        // 审查 P1-7 / §4.6 第 3 条：同一公钥上的新握手**取代**更旧的挂起握手——取消、摘除
        // 与名额归还在同一次写锁内按**实际移除条目**完成，绝不出现「同 key 两个挂起并存、
        // 旧的只被翻转标志却仍占名额」。
        let _third = manager.track_pending(K1);
        assert!(first.is_cancelled(), "被取代的旧挂起握手必须已被取消");
        assert_eq!(
            manager.pending_count(),
            2,
            "取代后同 key 只占一个名额（另一个是 K2）"
        );
    }
    assert_eq!(manager.pending_count(), 0, "凭证 Drop 必须归还挂起名额");
    assert!(
        !manager
            .active_devices()
            .iter()
            .collect::<HashSet<_>>()
            .contains(&&K1),
        "既无会话也无挂起的条目应被回收（map 不得无界增长）"
    );
}

#[tokio::test]
async fn pending_ticket_wakes_on_revoke() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let ticket = manager.track_pending(K1);
    let wait = {
        // 与握手路径一致：撤销前挂起任务在等取消。
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            manager.apply_revoke(K1).cancelled_pending
        })
    };
    tokio::time::timeout(Duration::from_millis(300), ticket.cancelled())
        .await
        .expect("撤销必须唤醒挂起握手");
    assert_eq!(wait.await.expect("join"), 1);
}

#[tokio::test]
async fn config_validation_fails_closed() {
    assert!(ManagerConfig::default().validate().is_ok());
    let zero = ManagerConfig {
        max_half_open: 0,
        ..ManagerConfig::default()
    };
    assert!(zero.validate().is_err(), "预算为 0 必须拒绝（不静默放宽）");
    let zero_duration = ManagerConfig {
        poll_period: Duration::ZERO,
        ..ManagerConfig::default()
    };
    assert!(zero_duration.validate().is_err());
    let bad_cadence = ManagerConfig {
        poll_period: Duration::from_millis(10),
        poll_tick_cadence: Duration::from_millis(50),
        ..ManagerConfig::default()
    };
    assert!(bad_cadence.validate().is_err());
}

#[tokio::test]
async fn snapshot_admits_and_reject_reason_are_exact() {
    use rsetup_controller::transport::manager::snapshot_admits;
    assert!(snapshot_admits(&approved(1)));
    assert!(!snapshot_admits(&revoked(1)));
    assert!(!snapshot_admits(&denied(1)));
    assert!(!snapshot_admits(&AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::None,
        revision: 1,
    }));
}

/// 未使用告警的唯一消费者：确认 `VecDeque`/`SystemTime` 在本文件确有语义位置
/// （`VecDeque` 留给后续多轮回包用例，`SystemTime` 由 `note_activity` 的墙钟语义使用）。
#[allow(dead_code)]
fn _type_anchors(
    queue: VecDeque<u8>,
    when: SystemTime,
    signal: Arc<CancelSignal>,
    model_row: model::AdmissionSnapshot,
) -> (
    usize,
    SystemTime,
    Arc<CancelSignal>,
    model::AdmissionSnapshot,
) {
    (queue.len(), when, signal, model_row)
}
