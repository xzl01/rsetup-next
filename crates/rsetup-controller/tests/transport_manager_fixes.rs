//! 控制器连接管理第一轮修复（Task5）的**回归**集成测试。
//!
//! 与 `transport_manager.rs`（既有 27 项语义测试）互补：本文件逐项钉死审查
//! `controller-manager-review-findings.md` 的 13 项 + 2 补项里**可在 seam 层确定复现**的缺陷。
//!
//! # 诚实边界（与验收报告一致）
//!
//! * 全部使用注入替身（`FakeConnection`/`FakeTruth`）与临时 `tokio` 运行时；
//! * **不**触达真实 `devices` 表、真实板端进程、真实 h2/GOAWAY 帧：那些留在报告的
//!   「未验」清单里，本文件不为它们出具证据；
//! * 并发交错一律用 `Barrier`/`join!`/固定延迟制造**确定性**调度，不用「跑十万次碰概率」。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::{Barrier, watch};
use uuid::Uuid;

use rsetup_controller::ControllerError;
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::model::{AdmissionSnapshot, AdmissionState, ReviewDecision};
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::tasks::adapter::{TaskTimeSource, TimeEvidenceFuture};
use rsetup_controller::tasks::board_client::BoardClientError;
use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
use rsetup_controller::transport::board_client::RpcReply;
use rsetup_controller::transport::manager::{
    AdmissionDecisionFuture, AdmissionTruth, CURRENT_ADMISSION_SQL, CallError, ConnectionFuture,
    ConnectionState, DeviceConnection, HalfOpenPermit, MAX_HALF_OPEN_LIMIT, MAX_MANAGER_DURATION,
    MAX_SESSIONS_LIMIT, ManagerConfig, NewSession, PendingTicket, RegisterError,
    SNAPSHOT_ERROR_CODE, SampleOutcome, SystemNotice, TransportManager, decision_admits,
};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::sig::{REASON_APPROVAL_DENIED, REASON_REVOKED};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, Capabilities, DeviceStatus,
    Query, approved_codec_witness, decode_business, encode_business,
};
use rsetup_tunnel::handshake::AdmissionDecision;

const K1: [u8; 32] = [11u8; 32];
const K2: [u8; 32] = [22u8; 32];

// ---------------------------------------------------------------------------
// 替身（本文件自带，聚焦本组断言需要的面）
// ---------------------------------------------------------------------------

/// 需要真实 `Query` 请求体的动作清单（与生产轮询路径一致）。
const QUERY_ACTIONS: [&str; 2] = ["device.status.get", "device.capabilities.get"];

#[derive(Default)]
struct Obs {
    calls: Mutex<Vec<&'static str>>,
    systems: Mutex<Vec<SystemNotice>>,
    aborts: AtomicUsize,
    queries: AtomicUsize,
    query_violations: AtomicUsize,
    inflight: Arc<AtomicUsize>,
}

impl Obs {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls lock").clone()
    }
    fn aborts(&self) -> usize {
        self.aborts.load(Ordering::Acquire)
    }
    fn queries(&self) -> usize {
        self.queries.load(Ordering::Acquire)
    }
    fn query_violations(&self) -> usize {
        self.query_violations.load(Ordering::Acquire)
    }
    fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
}

struct InflightGuard(Arc<AtomicUsize>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct FakeConnection {
    ready: bool,
    replies: Mutex<HashMap<&'static str, (i32, Vec<u8>)>>,
    delay: Duration,
    obs: Arc<Obs>,
}

impl FakeConnection {
    fn new() -> Arc<Self> {
        Self::with_delay(Duration::ZERO)
    }

    fn with_delay(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            ready: true,
            replies: Mutex::new(HashMap::new()),
            delay,
            obs: Arc::new(Obs::default()),
        })
    }

    fn not_ready() -> Arc<Self> {
        Arc::new(Self {
            ready: false,
            replies: Mutex::new(HashMap::new()),
            delay: Duration::ZERO,
            obs: Arc::new(Obs::default()),
        })
    }

    fn set_status(&self, seq: u64) {
        let wire = encode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            &BusinessPayload::DeviceStatus(status_payload(seq)),
        )
        .expect("status 编码");
        self.replies
            .lock()
            .expect("replies lock")
            .insert("device.status.get", (0, wire));
    }

    /// 清空回复表：之后所有请求都回「状态码 0 + 空 payload」，即业务层解码必失败。
    fn clear_replies(&self) {
        self.replies.lock().expect("replies lock").clear();
    }

    fn set_capabilities(&self) {
        let wire = encode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            &BusinessPayload::Capabilities(capabilities_payload()),
        )
        .expect("capabilities 编码");
        self.replies
            .lock()
            .expect("replies lock")
            .insert("device.capabilities.get", (0, wire));
    }
}

impl DeviceConnection for FakeConnection {
    fn request(
        &self,
        action: &'static str,
        payload: Vec<u8>,
    ) -> ConnectionFuture<'_, Result<RpcReply, CallError>> {
        let obs = Arc::clone(&self.obs);
        obs.calls.lock().expect("calls lock").push(action);
        // **替身校验真实 Query 字节**（P1-1）：请求体必须能被已审 codec 解码为
        // `Query{schema_version}`；解码失败即记违规（旧实现发空 payload 便永远不被发现）。
        if QUERY_ACTIONS.contains(&action) {
            let decoded = decode_business(
                &BusinessCodec,
                Some(&approved_codec_witness()),
                BusinessPayloadKind::Query,
                &payload,
            );
            match decoded {
                Ok(BusinessPayload::Query(query))
                    if query.schema_version == BUSINESS_SCHEMA_VERSION =>
                {
                    obs.queries.fetch_add(1, Ordering::AcqRel);
                }
                _ => {
                    obs.query_violations.fetch_add(1, Ordering::AcqRel);
                }
            }
        }
        let reply = self
            .replies
            .lock()
            .expect("replies lock")
            .get(action)
            .cloned()
            .unwrap_or((0, Vec::new()));
        let delay = self.delay;
        obs.inflight.fetch_add(1, Ordering::AcqRel);
        let guard = InflightGuard(Arc::clone(&obs.inflight));
        Box::pin(async move {
            let _guard = guard;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let (status_code, payload) = reply;
            Ok(RpcReply {
                status_code,
                payload,
            })
        })
    }

    fn state(&self) -> ConnectionState {
        if self.ready {
            ConnectionState::Ready
        } else {
            ConnectionState::Connecting
        }
    }

    fn send_system(&self, notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>> {
        let obs = Arc::clone(&self.obs);
        obs.systems.lock().expect("systems lock").push(notice);
        Box::pin(async move { Ok(()) })
    }

    fn graceful_shutdown(&self) {}

    fn abort(&self) {
        self.obs.aborts.fetch_add(1, Ordering::AcqRel);
    }
}

/// 只读准入真相替身（**seam**）：判定与生产 `classify_row` 逐项一致，另可模拟挂起的 SQL。
#[derive(Default)]
struct FakeTruth {
    rows: Mutex<HashMap<[u8; 32], AdmissionSnapshot>>,
    hanging: Mutex<HashSet<[u8; 32]>>,
    loads: AtomicUsize,
}

impl FakeTruth {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn put(&self, key: [u8; 32], snapshot: AdmissionSnapshot) {
        self.rows.lock().expect("rows lock").insert(key, snapshot);
    }
    /// 该公钥的真相读取**永不返回**（模拟挂起的 SQL，P1-9）。
    fn put_hanging(&self, key: [u8; 32]) {
        self.hanging.lock().expect("hanging lock").insert(key);
    }
    fn loads(&self) -> usize {
        self.loads.load(Ordering::Acquire)
    }
}

impl AdmissionTruth for FakeTruth {
    fn load_current(&self, public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        if self
            .hanging
            .lock()
            .expect("hanging lock")
            .contains(&public_key)
        {
            self.loads.fetch_add(1, Ordering::AcqRel);
            return Box::pin(std::future::pending::<
                Result<AdmissionDecision, ControllerError>,
            >());
        }
        self.loads.fetch_add(1, Ordering::AcqRel);
        let row = self
            .rows
            .lock()
            .expect("rows lock")
            .get(&public_key)
            .copied()
            .ok_or(ControllerError::NotFound);
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

/// 慢真相替身（审查 N3）：每键有独立读取延迟与决定，并记录**真实被调用次数**（含被 abort 的
/// 调用）。用来确定性地复现「头部慢、deadline 每轮在相同位置截断、排序尾部身份永不被复查」。
#[derive(Default)]
struct SlowTruth {
    entries: Mutex<HashMap<[u8; 32], (Duration, bool)>>,
    calls: Mutex<HashMap<[u8; 32], usize>>,
}

impl SlowTruth {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn entry(&self, key: [u8; 32], delay: Duration, approved: bool) {
        self.entries
            .lock()
            .expect("entries lock")
            .insert(key, (delay, approved));
    }
    fn calls_of(&self, key: [u8; 32]) -> usize {
        self.calls
            .lock()
            .expect("calls lock")
            .get(&key)
            .copied()
            .unwrap_or(0)
    }
}

impl AdmissionTruth for SlowTruth {
    fn load_current(&self, public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        // 记账与查表在创建 future 之前完成（不在跨 await 处持有 Mutex 守卫）。
        let (delay, approved) = self
            .entries
            .lock()
            .expect("entries lock")
            .get(&public_key)
            .copied()
            .unwrap_or((Duration::from_millis(1), false));
        *self
            .calls
            .lock()
            .expect("calls lock")
            .entry(public_key)
            .or_insert(0) += 1;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            if approved {
                Ok(AdmissionDecision::Approved)
            } else {
                // 读到明确不批准与读不到同走 fail-closed（调用方撤销）。
                Err(ControllerError::NotFound)
            }
        })
    }
}

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

struct FailingRecovery;

impl RecoveryModeProbe for FailingRecovery {
    fn current_mode<'a>(&'a self) -> rsetup_controller::api::ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "recovery controller not wired".to_string(),
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

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
    let manager = TransportManager::try_new(
        Arc::clone(&truth) as Arc<dyn AdmissionTruth>,
        Arc::clone(&snapshots),
        Arc::new(FailingRecovery),
        Arc::new(FixedTime),
        config,
    )
    .expect("合法预算");
    Fixture {
        manager,
        truth,
        snapshots,
    }
}

fn approved(revision: u64) -> AdmissionSnapshot {
    AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision,
    }
}

fn revoked_row(revision: u64) -> AdmissionSnapshot {
    AdmissionSnapshot {
        admission_state: AdmissionState::Revoked,
        review_decision: ReviewDecision::Revoked,
        revision,
    }
}

fn permit(manager: &Arc<TransportManager>) -> HalfOpenPermit {
    manager.half_open_permit().expect("半开名额")
}

/// 登记一条 ready 会话（半开名额与握手路径同源）。
fn register(
    manager: &Arc<TransportManager>,
    key: [u8; 32],
    connection: Arc<FakeConnection>,
) -> Uuid {
    let pending = manager.track_pending(key);
    let half_open = permit(manager);
    manager
        .register_ready(NewSession {
            public_key: key,
            connection,
            pending,
            half_open,
        })
        .expect("登记")
        .generation
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

// ---------------------------------------------------------------------------
// P1-1 轮询请求体必须是真实编码的 Query
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p1_1_polling_sends_real_query_payload() {
    let config = ManagerConfig {
        poll_period: Duration::from_millis(30),
        poll_tick_cadence: Duration::from_millis(5),
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    let connection = FakeConnection::new();
    let obs = Arc::clone(&connection.obs);
    connection.set_status(1);
    connection.set_capabilities();
    register(&manager, K1, connection);

    let (tx, rx) = watch::channel(false);
    let mut shutdown_rx = rx;
    let driver = manager.spawn_poll_driver(shutdown_rx.clone());
    assert!(
        wait_until(Duration::from_secs(3), || obs.queries() >= 2).await,
        "轮询必须以真实 Query 请求体调用 status/capabilities（实测 queries={})",
        obs.queries()
    );
    let _ = tx.send(true);
    shutdown_rx.changed().await.ok();
    tokio::time::timeout(Duration::from_secs(3), driver)
        .await
        .expect("轮询 owner 必须退出")
        .expect("driver 不 panic");

    assert_eq!(
        obs.query_violations(),
        0,
        "轮询请求体必须全部可被已审 codec 解码为 Query（P1-1 回归）"
    );
    let calls = obs.calls();
    assert!(calls.contains(&"device.status.get"));
    assert!(calls.contains(&"device.capabilities.get"));
}

/// 反向确认：空请求体（旧实现）一定会被替身判为违规——否则本组测试是自证清白。
#[tokio::test]
async fn p1_1_empty_payload_is_caught_by_the_fake() {
    let connection = FakeConnection::new();
    let obs = Arc::clone(&connection.obs);
    let _ = connection
        .request("device.status.get", Vec::new())
        .await
        .expect("回复仍按配置返回");
    assert_eq!(obs.query_violations(), 1, "空请求体必须判违规");
    let valid = encode_business(
        &BusinessCodec,
        Some(&approved_codec_witness()),
        &BusinessPayload::Query(Query {
            schema_version: BUSINESS_SCHEMA_VERSION,
        }),
    )
    .expect("Query 编码");
    let _ = connection
        .request("device.status.get", valid)
        .await
        .expect("回复仍按配置返回");
    assert_eq!(obs.query_violations(), 1, "合法 Query 不得判违规");
    assert_eq!(obs.queries(), 1);
}

// ---------------------------------------------------------------------------
// P1-2 首样本 confirm/update 与顶替必须互斥
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p1_2_first_sample_cannot_lease_generation_for_old_session() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig::default());
    let old = FakeConnection::new();
    let old_generation = register(&manager, K1, old);
    let new = FakeConnection::new();
    let new_generation = register(&manager, K1, new);

    // 8 路旧代际采样与 1 路新代际首样本在同一道栅栏后同时进入：
    // 修复前「读当前会话 → confirm_generation → update」三步跨锁，旧代际可以把已失效的
    // 坐标 confirm 进 store，或把新代际的首样本洗成 StaleGeneration。
    const OLD_RACERS: usize = 8;
    let barrier = Arc::new(Barrier::new(OLD_RACERS + 1));
    let mut handles = Vec::new();
    for index in 0..OLD_RACERS {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.ingest_status_sample(
                &K1,
                old_generation,
                &status_payload(index as u64 + 1),
                FixedTime.current_evidence().await,
                Instant::now(),
            )
        }));
    }
    let manager_new = Arc::clone(&manager);
    let barrier_new = Arc::clone(&barrier);
    let new_sample = tokio::spawn(async move {
        barrier_new.wait().await;
        manager_new.ingest_status_sample(
            &K1,
            new_generation,
            &status_payload(9_000),
            FixedTime.current_evidence().await,
            Instant::now(),
        )
    });
    for handle in handles {
        let outcome = handle.await.expect("旧代际采样任务");
        assert_eq!(
            outcome,
            SampleOutcome::StaleGeneration,
            "旧代际样本必须整批被拒且对 store 零副作用（P1-2）"
        );
    }
    let outcome = new_sample.await.expect("新代际采样任务");
    assert!(
        matches!(outcome, SampleOutcome::Accepted(_)),
        "新代际首样本不得被旧代际竞态洗掉，实测 {outcome:?}"
    );
    let view = snapshots.read(&K1).expect("当代际样本必须已写入");
    assert_eq!(view.status.sample_seq, 9_000, "store 里只能是当代际的样本");
}

// ---------------------------------------------------------------------------
// P1-4 半开名额在 ready 时释放（且拒绝路径也归还）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p1_4_half_open_permit_is_released_at_ready_not_at_session_end() {
    let config = ManagerConfig {
        max_half_open: 1,
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    let held = permit(&manager);
    assert!(
        manager.half_open_permit().is_err(),
        "半开预算必须严格有界（不 `max(1)` 静默放宽）"
    );

    let pending = manager.track_pending(K1);
    manager
        .register_ready(NewSession {
            public_key: K1,
            connection: FakeConnection::new(),
            pending,
            half_open: held,
        })
        .expect("登记");
    assert_eq!(
        manager.half_open_count(),
        0,
        "ready 登记成功那一刻名额必须已释放（不是活到会话结束，P1-4）"
    );
    assert!(
        manager.half_open_permit().is_ok(),
        "名额立即可复用：未 ready 的握手才不会挤占 ready 会话的预算"
    );
}

#[tokio::test]
async fn p1_4_unready_backlog_cannot_grow_past_half_open_budget() {
    let config = ManagerConfig {
        max_half_open: 2,
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(permit(&manager));
    }
    // 第三条握手的 socket 必须在**昂贵握手之前**被拒（资源不足＝拒新，不排队、不写决定）。
    assert!(manager.half_open_permit().is_err());
    // 释放（模拟失败/取消的 Drop 路径）后立刻可复用。
    held.clear();
    assert_eq!(manager.half_open_count(), 0);
    assert!(manager.half_open_permit().is_ok());
}

// ---------------------------------------------------------------------------
// P1-6 回包后必须重核当代际：旧连接的回包一律丢弃
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p1_6_reply_from_superseded_connection_is_dropped() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    // 该连接的请求故意慢 200ms：在途期间被顶替。
    let old = FakeConnection::with_delay(Duration::from_millis(200));
    old.set_status(1);
    let old_obs = Arc::clone(&old.obs);
    register(&manager, K1, old);

    let rpc = manager.as_device_rpc();
    // `join!` 让两个分支在同一任务上确定性交错：请求先挂起，顶替在其睡眠窗口内完成。
    let replacement = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        register(&manager, K1, FakeConnection::new())
    };
    let (result, _new_generation) = tokio::join!(
        rpc.request(&K1, "device.status.get", Vec::new()),
        replacement
    );

    assert!(
        matches!(result, Err(BoardClientError::DeviceOffline)),
        "回包到达时会话已被顶替 → 必须丢弃并回 typed 离线错误，实得 {result:?}"
    );
    assert!(
        old_obs.calls().contains(&"device.status.get"),
        "请求确实发出过（不是没发就判失败）"
    );
    assert!(
        wait_until(Duration::from_millis(1_500), || old_obs.aborts() > 0).await,
        "被顶替的旧连接仍按 §4.7 收尾"
    );
}

#[tokio::test]
async fn p1_6_older_handshake_cannot_replace_newer_session() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    // 同一公钥上的新握手会取代并取消更旧的挂起握手（§4.6 第 3 条）。
    let older = manager.track_pending(K1);
    let newer = manager.track_pending(K1);
    let current = register_with_pending(&manager, K1, FakeConnection::new(), newer);

    let error = try_register_with_pending(&manager, K1, FakeConnection::new(), older)
        .expect_err("更旧的握手不得装成当前会话");
    assert!(
        matches!(
            error,
            RegisterError::StaleHandshake | RegisterError::Revoked
        ),
        "旧握手的登记必须被拒，实得 {error:?}"
    );
    assert_eq!(
        manager.current_connection(&K1).expect("当前会话").0,
        current,
        "旧连接的异步回调不得把当前会话改回自己（P1-6）"
    );
}

fn register_with_pending(
    manager: &Arc<TransportManager>,
    key: [u8; 32],
    connection: Arc<FakeConnection>,
    pending: PendingTicket,
) -> Uuid {
    try_register_with_pending(manager, key, connection, pending).expect("登记")
}

fn try_register_with_pending(
    manager: &Arc<TransportManager>,
    key: [u8; 32],
    connection: Arc<FakeConnection>,
    pending: PendingTicket,
) -> Result<Uuid, RegisterError> {
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

// ---------------------------------------------------------------------------
// P1-7 / P2-10 已取消的挂起凭证仍占名额：按实际移除条目归还
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p2_10_revoke_releases_slots_of_already_cancelled_pendings() {
    // 这正是审查报告里被复现的缺陷：只按 `cancel()` 的翻转次数减账 → 名额永久泄漏。
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    register(&manager, K1, FakeConnection::new());
    let cancelled = manager.track_pending(K1);
    assert!(cancelled.cancel(), "首次取消完成翻转");
    assert_eq!(
        manager.pending_count(),
        1,
        "已取消的凭证仍占着条目与名额（标志位不等于归还）"
    );

    let outcome = manager.apply_revoke(K1);
    assert!(outcome.terminated_session);
    assert_eq!(
        outcome.cancelled_pending, 0,
        "观测量只统计本次首次翻转（不参与归还）"
    );
    assert_eq!(
        manager.pending_count(),
        0,
        "撤销必须按**实际移除条目**归还名额（P1-7/P2-10）"
    );
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn p2_10_shutdown_all_releases_every_pending_slot() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    register(&manager, K1, FakeConnection::new());
    register(&manager, K2, FakeConnection::new());
    let first = manager.track_pending(K1);
    let second = manager.track_pending(K2);
    first.cancel();
    let _ = second.cancel();
    assert_eq!(manager.pending_count(), 2);

    let terminated = manager.shutdown_all();
    assert_eq!(terminated, 2);
    assert_eq!(manager.pending_count(), 0, "关停后不得残留任何挂起名额");
    assert_eq!(manager.active_count(), 0);
}

// ---------------------------------------------------------------------------
// P1-8 归档行必须进不了既有会话（真相查询完整性 + 判定映射）
// ---------------------------------------------------------------------------

#[test]
fn p1_8_current_truth_query_projects_archived_and_stays_read_only() {
    let sql = CURRENT_ADMISSION_SQL.to_lowercase();
    assert!(sql.trim_start().starts_with("select"));
    assert!(
        sql.contains("archived"),
        "只读真相必须投影 `archived`，否则归档设备会被当成仍然批准（P1-8）"
    );
    for column in [
        "admission_state",
        "review_decision",
        "revision",
        "public_key",
    ] {
        assert!(sql.contains(column), "缺列 {column}");
    }
    assert!(
        !sql.contains("for update"),
        "只读真相不得加行锁（并发真相由判定层负责）"
    );
    assert!(!sql.contains(';'), "单条语句");
    assert!(
        !sql.contains("insert") && !sql.contains("update "),
        "只读：不写库"
    );
}

#[test]
fn p1_8_decision_mapping_admits_only_approved() {
    assert!(decision_admits(&AdmissionDecision::Approved));
    assert!(!decision_admits(&AdmissionDecision::Rejected(
        REASON_REVOKED
    )));
    assert!(!decision_admits(&AdmissionDecision::Rejected(
        REASON_APPROVAL_DENIED
    )));
    // PENDING 与「读不到」都不足以保留既有会话（fail-closed）。
    assert!(!decision_admits(&AdmissionDecision::Pending));
}

#[tokio::test]
async fn p1_8_revoke_on_truth_change_sweeps_existing_session() {
    let Fixture { manager, truth, .. } = fixture(ManagerConfig::default());
    register(&manager, K1, FakeConnection::new());
    // 真相被改成归档/吊销（替身以 revoked 表达同一判定入口）→ 复查必须摘除既有会话。
    truth.put(K1, revoked_row(9));
    let revoked = {
        let manager = Arc::clone(&manager);
        manager.recheck_all_sessions().await
    };
    assert_eq!(revoked, 1, "真相不再批准 → 既有会话必须被撤销");
    assert_eq!(manager.active_count(), 0);
    assert_eq!(truth.loads(), 1, "一次复查一条真相读取");
}

// ---------------------------------------------------------------------------
// P1-9 复查必须有单条期限与整轮上界，且可被关停取消
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p1_9_recheck_bounds_each_truth_read_and_fails_closed() {
    let config = ManagerConfig {
        admission_read_timeout: Duration::from_millis(120),
        admission_sweep_deadline: Duration::from_secs(3),
        admission_recheck_concurrency: 2,
        ..ManagerConfig::default()
    };
    let Fixture { manager, truth, .. } = fixture(config);
    truth.put_hanging(K1);
    register(&manager, K1, FakeConnection::new());

    let started = Instant::now();
    let revoked = {
        let manager = Arc::clone(&manager);
        manager.recheck_all_sessions().await
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "挂起的 SQL 不能把复查钉死：单条期限必须生效（实测 {elapsed:?}）"
    );
    assert_eq!(
        revoked, 1,
        "读不到真相＝无法证明仍批准 → fail-closed 撤销既有会话"
    );
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test]
async fn p1_9_sweep_deadline_stops_round_and_is_observable() {
    // 形状：单条期限 120ms（合法地 ≤ 整轮上界 200ms）、并发 1、8 个挂起项 →
    // 一轮至少要 8×120ms，必然被整轮上界中止。
    let config = ManagerConfig {
        admission_read_timeout: Duration::from_millis(120),
        admission_sweep_deadline: Duration::from_millis(200),
        admission_recheck_concurrency: 1,
        ..ManagerConfig::default()
    };
    let Fixture { manager, truth, .. } = fixture(config);
    let mut keys = Vec::new();
    for index in 0..8u8 {
        let key = [20u8 + index; 32];
        truth.put_hanging(key);
        register(&manager, key, FakeConnection::new());
        keys.push(key);
    }

    let started = Instant::now();
    let _ = {
        let manager = Arc::clone(&manager);
        manager.recheck_all_sessions().await
    };
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "整轮必须被 admission_sweep_deadline 中止（实测 {elapsed:?}）"
    );
    assert_eq!(
        manager.admission_sweeps_dropped(),
        1,
        "整轮超期必须可观测，且剩余项留给下一轮"
    );
    // 中止不是「悄悄跳过」：单条期限到点的那几项已 fail-closed 撤销，**剩余项仍留着**
    // 由下一轮按真相继续判定（整轮上界不是「全部撤光」，也不是「一个都不动」）。
    let remaining = manager.active_count();
    assert!(
        (1..keys.len()).contains(&remaining),
        "整轮必须在超期时停手：剩余 {remaining}/{} 项留给下一轮",
        keys.len()
    );
}

// ---------------------------------------------------------------------------
// N3 整轮复查的轮转续扫：排序尾部身份不再被永久饿死
// ---------------------------------------------------------------------------

fn slow_manager(truth: Arc<SlowTruth>, config: ManagerConfig) -> Arc<TransportManager> {
    TransportManager::try_new(
        truth as Arc<dyn AdmissionTruth>,
        Arc::new(SnapshotStore::new()),
        Arc::new(FailingRecovery),
        Arc::new(FixedTime),
        config,
    )
    .expect("合法预算")
}

/// 审查 N3：旧实现每轮从排序头部重扫 + 整轮 deadline 每轮在**相同位置**截断 → 排序尾部的已撤销
/// 身份永不被复查（永久饥饿，`calls==0`、会话恒存活）。轮转续扫让游标跨轮推进，尾部必然被实际
/// 复查并 fail-closed 摘除。
///
/// 用 `start_paused` 把 `tokio::time` 换成**自动推进的虚拟时钟**：多轮同构慢读 + 尾部 Revoke
/// 因此是**确定性**用例（无真实墙钟竞态，不靠跑概率）。断言的是「尾部被读/摘除」而非「dropped>0」。
#[tokio::test(start_paused = true)]
async fn n3_sweep_rotation_terminates_tail_starvation() {
    let truth = SlowTruth::new();
    let mut approved_keys = Vec::new();
    let mut revoked_keys = Vec::new();
    // 头部：3 个慢但未超时的 Approved（80ms < 单条 100ms），排序在前（前缀 0x01）。
    for index in 0u8..3 {
        let mut key = [1u8; 32];
        key[31] = index;
        truth.entry(key, Duration::from_millis(80), true);
        approved_keys.push(key);
    }
    // 尾部：3 个慢的已撤销身份（90ms < 单条 100ms，但每轮逼近整轮 deadline），排序在后（前缀 0x09）。
    for index in 0u8..3 {
        let mut key = [9u8; 32];
        key[31] = index;
        truth.entry(key, Duration::from_millis(90), false);
        revoked_keys.push(key);
    }
    let config = ManagerConfig {
        admission_read_timeout: Duration::from_millis(100),
        admission_sweep_deadline: Duration::from_millis(150),
        admission_recheck_concurrency: 2,
        ..ManagerConfig::default()
    };
    let manager = slow_manager(Arc::clone(&truth), config);
    for key in approved_keys.iter().chain(revoked_keys.iter()) {
        register(&manager, *key, FakeConnection::new());
    }
    assert_eq!(manager.active_count(), 6);

    // 多轮同构慢读：轮转续扫必须在**有界轮数**内把全部尾部 Revoke 读出并摘除（每轮游标至少前进 1，
    // 单波 2 条慢读 90ms < 150ms 必完成 → completed≥1 → 游标严格前进 → ≤len 轮覆盖全部 key）。
    let mut rounds_used = 0usize;
    for round in 1..=8usize {
        let _ = manager.recheck_all_sessions().await;
        rounds_used = round;
        if manager.active_count() == approved_keys.len() {
            break;
        }
    }

    // 1) 每个已撤销身份的真相**至少被读过一次**（旧实现尾部 calls==0）。
    for key in &revoked_keys {
        assert!(
            truth.calls_of(*key) > 0,
            "尾部已撤销身份 0x{:02x} 必须最终被复查（旧实现每轮从头重扫 → 永不被读）",
            key[31]
        );
    }
    // 2) 全部尾部已撤销身份被 fail-closed 摘除，只剩 3 个 Approved（收敛，非仅 dropped>0）。
    assert_eq!(
        manager.active_count(),
        approved_keys.len(),
        "{rounds_used} 轮后尾部 Revoke 必须全部摘除（有界收敛），不得残留已撤销会话"
    );
    // 3) 前提成立：整轮确实被 deadline 截断过——轮转是「带着截断仍能收敛」，不是「取消截断」。
    assert!(
        manager.admission_sweeps_dropped() >= 1,
        "该场景必须以整轮超期截断为前提（否则并未测到 deadline 截断下的续扫）"
    );
}

/// 审查 N3 的第二半：一轮内**已启动却被整轮 deadline 取消**的 key，不得因为游标更新而被永久跳过。
///
/// 装配（虚拟时钟，确定性）：并发 1、单条期限 150ms、整轮上界 150ms、每条读取 140ms。
/// 于是每轮「第一条完成（140ms）→ 第二条刚启动就在 150ms 被 abort」，游标每轮恰好前进 1：
/// * 第 1 轮：K1 被复查（并摘除），K2 **已启动但被取消**（读取从未完成 ⇒ 会话仍在）；
/// * 第 2 轮起：游标让 K2 重新排到轮首，它必须被**实际读到**并摘除。
/// 断言的是「K2 的读取次数先变成 1 而会话仍存活（=确实被取消过），随后被真正复查并摘除」，
/// 不是「dropped>0」。最坏收敛界：每轮至少前进 1 ⇒ ≤ `len` 轮内全部 key 被实际复查。
#[tokio::test(start_paused = true)]
async fn n3_key_cancelled_mid_round_is_rechecked_next_round() {
    let truth = SlowTruth::new();
    let mut keys = Vec::new();
    for index in 1u8..=4 {
        let mut key = [index; 32];
        key[31] = index;
        // 全部是「应被撤销」的身份：读到了就会摘除（读不到＝同一路径 fail-closed）。
        truth.entry(key, Duration::from_millis(140), false);
        keys.push(key);
    }
    let config = ManagerConfig {
        admission_read_timeout: Duration::from_millis(150),
        admission_sweep_deadline: Duration::from_millis(150),
        admission_recheck_concurrency: 1,
        ..ManagerConfig::default()
    };
    let manager = slow_manager(Arc::clone(&truth), config);
    for key in &keys {
        register(&manager, *key, FakeConnection::new());
    }
    assert_eq!(manager.active_count(), 4);

    // 第 1 轮：K1 完成并摘除；K2 被启动却在整轮超期时 abort（读取未完成 ⇒ 不摘除）。
    let _ = manager.recheck_all_sessions().await;
    assert_eq!(truth.calls_of(keys[0]), 1, "轮首 K1 必须被实际读取");
    assert_eq!(manager.active_count(), 3, "K1 复查不通过应已摘除");
    assert!(
        truth.calls_of(keys[1]) >= 1,
        "K2 本轮必须已启动（被取消的前提是它已经开始读）"
    );
    assert_eq!(
        manager.active_count(),
        3,
        "K2 的读取未真正完成 ⇒ 本轮既不得摘除它、也不得把它算作已复查"
    );
    assert!(
        manager.admission_sweeps_dropped() >= 1,
        "本轮必须是被整轮 deadline 截断的一轮"
    );

    // 后续轮：被取消的 K2 必须重新排到轮首被**实际读到并摘除**，最终 4 条全部收敛。
    let mut rounds_used = 1usize;
    for round in 2..=8usize {
        let _ = manager.recheck_all_sessions().await;
        rounds_used = round;
        if manager.active_count() == 0 {
            break;
        }
    }
    assert_eq!(
        manager.active_count(),
        0,
        "{rounds_used} 轮内全部身份必须被实际复查并摘除（游标每轮至少前进 1 ⇒ ≤len 轮收敛）"
    );
    for (index, key) in keys.iter().enumerate() {
        assert!(
            truth.calls_of(*key) >= 1,
            "K{} 必须至少被实际读取一次（被取消不得变成永久跳过）",
            index + 1
        );
    }
    // K2 被读取的次数 ≥ 2：一次被取消、一次真正完成——正是「已启动但被取消 ⇒ 之后重做」的直接证据。
    assert!(
        truth.calls_of(keys[1]) >= 2,
        "K2 必须被重做（实得 {} 次）",
        truth.calls_of(keys[1])
    );
}

#[tokio::test]
async fn p1_9_recheck_task_is_cancellable_on_shutdown() {
    let config = ManagerConfig {
        admission_recheck_interval: Duration::from_millis(20),
        admission_read_timeout: Duration::from_secs(30),
        admission_sweep_deadline: Duration::from_secs(30),
        ..ManagerConfig::default()
    };
    let Fixture { manager, truth, .. } = fixture(config);
    truth.put_hanging(K1);
    register(&manager, K1, FakeConnection::new());

    let (tx, rx) = watch::channel(false);
    let mut shutdown_rx = rx.clone();
    let task = manager.spawn_admission_recheck(rx);
    assert!(
        wait_until(Duration::from_millis(500), || truth.loads() > 0).await,
        "复查任务必须真的按节拍读真相"
    );
    let _ = tx.send(true);
    shutdown_rx.changed().await.ok();
    // 一轮正在挂起的读取不得阻止退出（整轮也被 select 掉）。
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("关停必须能取消整轮复查（P1-9）")
        .expect("复查任务不 panic");
}

// ---------------------------------------------------------------------------
// P2-11 轮询 child 必须有 owner：关停时 abort 并 join
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p2_11_poll_driver_joins_inflight_children_before_returning() {
    let config = ManagerConfig {
        poll_period: Duration::from_millis(20),
        poll_tick_cadence: Duration::from_millis(5),
        ..ManagerConfig::default()
    };
    let Fixture { manager, .. } = fixture(config);
    // 每次请求 400ms：关停时一定有在途 child。
    let connection = FakeConnection::with_delay(Duration::from_millis(400));
    let obs = Arc::clone(&connection.obs);
    connection.set_status(1);
    connection.set_capabilities();
    register(&manager, K1, connection);

    let (tx, mut rx) = watch::channel(false);
    let driver = manager.spawn_poll_driver(rx.clone());
    assert!(
        wait_until(Duration::from_secs(3), || obs.inflight() > 0).await,
        "先制造一个在途 child"
    );
    let _ = tx.send(true);
    rx.changed().await.ok();
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("轮询 owner 必须退出")
        .expect("driver 不 panic");
    assert_eq!(
        obs.inflight(),
        0,
        "owner 返回时不得还有 child 在发请求（P2-11：abort 并 join，不是发个信号就走）"
    );
}

// ---------------------------------------------------------------------------
// P2-12 板端事件只认「当前已 ready 的 Control 会话」
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p2_12_task_result_context_follows_current_generation_only() {
    let Fixture { manager, .. } = fixture(ManagerConfig::default());
    let connection = FakeConnection::new();
    let generation = register(&manager, K1, connection);

    let context = manager
        .claim_task_result(&K1, generation)
        .expect("当代际可领取投递上下文");
    assert_eq!(context.public_key(), K1);
    assert_eq!(context.generation(), generation);
    assert!(context.is_current());

    // 未登记的代际（含旧代际）一律领不到。
    assert!(manager.claim_task_result(&K1, Uuid::new_v4()).is_none());
    // 尚未 ready 的连接即使登记也领不到（状态判据来自连接本身）。
    let pending_only = manager.track_pending(K2);
    let half_open = permit(&manager);
    let connecting_generation = manager
        .register_ready(NewSession {
            public_key: K2,
            connection: FakeConnection::not_ready(),
            pending: pending_only,
            half_open,
        })
        .expect("登记")
        .generation;
    assert!(
        manager
            .claim_task_result(&K2, connecting_generation)
            .is_none(),
        "未 ready 的会话不得投递任务事件"
    );

    // 顶替后：旧上下文立即失效（闸门与摘除同锁关闭）。
    let replacement = register(&manager, K1, FakeConnection::new());
    assert!(
        !context.is_current(),
        "顶替后旧事件上下文必须立即失效（P2-12）"
    );
    assert!(manager.claim_task_result(&K1, generation).is_none());

    // 撤销后：当代际既领不到，也已领出的上下文同样失效。
    let current = manager
        .claim_task_result(&K1, replacement)
        .expect("新代际可领取");
    manager.apply_revoke(K1);
    assert!(
        manager.claim_task_result(&K1, replacement).is_none(),
        "已撤销会话不得再投递事件"
    );
    assert!(!current.is_current(), "撤销必须让在途上下文失效");
}

// ---------------------------------------------------------------------------
// P2-13 非法预算在构造期失败关闭；预算有硬上界且求和受检
// ---------------------------------------------------------------------------

#[test]
fn p2_13_illegal_manager_config_fails_closed() {
    let truth = FakeTruth::new();
    let snapshots = Arc::new(SnapshotStore::new());
    let build = |config: ManagerConfig| {
        TransportManager::try_new(
            Arc::clone(&truth) as Arc<dyn AdmissionTruth>,
            Arc::clone(&snapshots),
            Arc::new(FailingRecovery),
            Arc::new(FixedTime),
            config,
        )
    };
    let illegal: Vec<(&str, ManagerConfig)> = vec![
        (
            "半开预算为 0",
            ManagerConfig {
                max_half_open: 0,
                ..ManagerConfig::default()
            },
        ),
        (
            "会话预算为 0",
            ManagerConfig {
                max_sessions: 0,
                ..ManagerConfig::default()
            },
        ),
        (
            "半开超硬上界",
            ManagerConfig {
                max_half_open: MAX_HALF_OPEN_LIMIT + 1,
                ..ManagerConfig::default()
            },
        ),
        (
            "会话超硬上界",
            ManagerConfig {
                max_sessions: MAX_SESSIONS_LIMIT + 1,
                ..ManagerConfig::default()
            },
        ),
        (
            "真相读取期限为 0",
            ManagerConfig {
                admission_read_timeout: Duration::ZERO,
                ..ManagerConfig::default()
            },
        ),
        (
            "整轮期限超上界",
            ManagerConfig {
                admission_sweep_deadline: MAX_MANAGER_DURATION * 2,
                ..ManagerConfig::default()
            },
        ),
        (
            "复查并发为 0",
            ManagerConfig {
                admission_recheck_concurrency: 0,
                ..ManagerConfig::default()
            },
        ),
        (
            "轮询 tick 不短于周期",
            ManagerConfig {
                poll_period: Duration::from_millis(10),
                poll_tick_cadence: Duration::from_millis(500),
                ..ManagerConfig::default()
            },
        ),
    ];
    for (label, config) in illegal {
        assert!(
            build(config).is_err(),
            "非法预算必须构造期失败关闭：{label}"
        );
    }
    assert!(build(ManagerConfig::default()).is_ok());

    // 连接任务上限是**受检和**：溢出即 None（绝不 saturating_add 出一个假上界）。
    let overflow = ManagerConfig {
        max_half_open: MAX_HALF_OPEN_LIMIT,
        max_sessions: MAX_SESSIONS_LIMIT,
        ..ManagerConfig::default()
    };
    let _ = overflow.connection_task_budget();
    let absurd = ManagerConfig {
        max_half_open: usize::MAX,
        max_sessions: usize::MAX,
        ..ManagerConfig::default()
    };
    assert_eq!(absurd.connection_task_budget(), None);
    assert!(absurd.validate().is_err());
}

// ---------------------------------------------------------------------------
// 补项 采集失败必须投影到快照（保留最后好样本 + 去敏错误码）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn supplement_sampling_failure_is_recorded_on_current_generation_only() {
    let Fixture {
        manager, snapshots, ..
    } = fixture(ManagerConfig::default());
    let connection = FakeConnection::new();
    connection.set_status(7);
    let generation = register(&manager, K1, connection);
    let outcome = manager.ingest_status_sample(
        &K1,
        generation,
        &status_payload(7),
        FixedTime.current_evidence().await,
        Instant::now(),
    );
    assert!(matches!(outcome, SampleOutcome::Accepted(_)), "{outcome:?}");
    let view = snapshots.read(&K1).expect("好样本已写入");
    assert!(
        view.last_error.is_none(),
        "初始状态不得带错误位：{:?}",
        view.last_error
    );

    assert!(
        manager.record_sample_error(&K1, generation),
        "当代际的采集失败必须可投影"
    );
    let view = snapshots.read(&K1).expect("最后好样本必须保留");
    assert_eq!(
        view.status.sample_seq, 7,
        "失败投影不得把 fresh 状态回填成 0/清空"
    );
    assert_eq!(
        view.last_error.as_deref(),
        Some(SNAPSHOT_ERROR_CODE),
        "错误码必须是去敏固定值（单一日志码），不含 peer/SQL 文本"
    );

    // 从未确认过代际坐标的会话：不伪造条目，如实返回失败。
    let fresh = register(&manager, K2, FakeConnection::new());
    assert!(
        !manager.record_sample_error(&K2, fresh),
        "本代际还没确认过代际坐标 → store 里没有条目可投影，不得为此提前造条目"
    );
    // 旧代际的失败对 store 零副作用。
    let before = snapshots.read(&K1).expect("仍在").last_error.clone();
    assert!(!manager.record_sample_error(&K1, Uuid::new_v4()));
    assert_eq!(
        snapshots.read(&K1).expect("仍在").last_error,
        before,
        "旧代际不得清新代际或改新代际的错误位"
    );
}

#[tokio::test]
async fn supplement_poll_failure_projects_error_instead_of_leaving_stale_fresh() {
    let config = ManagerConfig {
        poll_period: Duration::from_millis(30),
        poll_tick_cadence: Duration::from_millis(5),
        ..ManagerConfig::default()
    };
    let Fixture {
        manager, snapshots, ..
    } = fixture(config);
    let connection = FakeConnection::new();
    let obs = Arc::clone(&connection.obs);
    connection.set_status(3);
    connection.set_capabilities();
    register(&manager, K1, Arc::clone(&connection));

    // 先让轮询**成功**一轮：当代际的代际坐标由驱动自己确认（不是测试手工塞样本）。
    let (tx, mut rx) = watch::channel(false);
    let driver = manager.spawn_poll_driver(rx.clone());
    assert!(
        wait_until(Duration::from_secs(3), || snapshots
            .read(&K1)
            .is_some_and(|view| view.status.sample_seq == 3))
        .await,
        "轮询必须先写进一个好样本"
    );

    // 同一条**当代际**连接随后开始回非法 payload：轮询失败（解码不通过）。
    connection.clear_replies();
    assert!(
        wait_until(Duration::from_secs(3), || snapshots
            .read(&K1)
            .is_some_and(|view| view.last_error.is_some()))
        .await,
        "同一当代际的采集失败必须投影到快照（补项）"
    );
    let _ = tx.send(true);
    rx.changed().await.ok();
    tokio::time::timeout(Duration::from_secs(5), driver)
        .await
        .expect("driver 退出")
        .expect("不 panic");
    assert!(
        obs.query_violations() == 0,
        "中控发出的请求体始终是合法 Query"
    );

    let view = snapshots.read(&K1).expect("最后好样本仍在");
    assert_eq!(
        view.status.sample_seq, 3,
        "失败不得把好样本清零（单调年龄由 store 负责）"
    );
    assert_eq!(
        view.last_error.as_deref(),
        Some(SNAPSHOT_ERROR_CODE),
        "采集失败必须已投影（补项：否则 fresh 状态会一直挂到过期）"
    );
}
