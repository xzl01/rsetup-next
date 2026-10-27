//! 已认证设备连接管理器与 `DeviceRpc` 生产实现（`protocol_spec.md` §4.6/§6/§7、Task 5）。
//!
//! 本模块把「已通过 [`server_handshake`] 完成 wire 审计与 Ed25519 验签、并被 SQL 准入放行」
//! 的板端连接登记为**按已验签公钥键控的当前会话**，并据此为 [`TransportBoardClient`] 提供
//! [`DeviceRpc`] 路由。注册只接受握手产出的 `peer_public_key`，绝不从 payload 里的
//! deviceId/serial 绑定身份；设备不在线/未 ready 一律上抛 typed 错误，不返回常量「在线」。
//!
//! [`server_handshake`]: rsetup_tunnel::handshake::server_handshake
//! [`TransportBoardClient`]: crate::transport::board_client::TransportBoardClient
//! [`DeviceRpc`]: crate::transport::board_client::DeviceRpc
//!
//! # 依赖倒置边界（与 session driver 并行实现）
//!
//! 网络会话由 `rsetup_tunnel::session` 的 driver 提供（`start_server_session` /
//! `SessionHandle`）。本模块只依赖自有的 [`DeviceConnection`] trait —— 它逐项对应
//! `SessionHandle` 的公开表面（`request`/`state`/`send_system`/`graceful_shutdown`/`abort`），
//! 真实适配器在 [`listener`](super::listener) 里实现。本文件因此可在 driver 落地前独立编译
//! 与被测试（注入 fake 连接覆盖路由/替换/撤销语义），**不代表** h2 网络已经接线。
//!
//! # 关键不变量
//!
//! * **当前代际唯一**：每个公钥至多一份当前会话；代际 `generation` 由本管理器本地产生
//!   （`Uuid::new_v4`），不由对端自报。同 key 新连接顶替旧会话：先在旧控制流发
//!   `TYPE_SYSTEM tunnel.kick`，**单调 500ms** 后切断旧 TCP（§4.7），定时器只持有被顶替的
//!   那份连接句柄，不受后续新代际影响。旧代际对 `DeviceRpc` 立即不可见。
//! * **撤销即时终止**：管理面 `after_commit` 经有界通道立即撤销活跃会话与挂起握手，不缓存
//!   批准。吊销走 `TYPE_SYSTEM tunnel.revoke` 后强制切断；发信令失败（连接已坏）立即 abort，
//!   「已发/未发」边界由 [`TransportManager::system_signals_sent`] /
//!   [`TransportManager::system_signals_unsent`] 观测。
//! * **登记与撤销同锁**：挂起握手在 ready 前被撤销时，登记直接被拒绝
//!   （[`RegisterError::Revoked`]），不会把已撤销身份装成当前会话。
//! * **通知丢失 fail-closed**：`after_commit` 投递失败不算成功（返回 `Err` 让
//!   `notification_queued=false`）并立刻唤醒一次全量复查；复查读的是 [`AdmissionTruth`]
//!   （生产 = `devices` 表只读 `load`），**不是**本管理器的内存 getter——内存「仍当前」
//!   永远不能证明 SQL 未撤销。
//! * **半开有界**：`accept` 后、握手前须取得 [`HalfOpenPermit`]（RAII 全局上限），超限即
//!   [`RegisterError::HalfOpenFull`]；审批本身无总期限（§4.6 第 3 条），每 peer 的协议 probe
//!   由握手层负责，本模块不写任何永久决定。
//! * **last_seen 只由当前认证代际的成功活动更新**（[`TransportManager::note_activity`]）。
//! * **不重试**：[`DeviceRpc::request`] 恰好发一次；超时/断线结果未知，绝不重放变更。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use sqlx::Row as _;

use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, Capabilities, DeviceStatus,
    Query, approved_codec_witness, decode_business, encode_business,
};
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, HandshakeError, VerifiedPeer,
};

use crate::ControllerError;
use crate::api::ManagementFuture;
use crate::api::approvals::ManagementNotificationSink;
use crate::api::devices::{
    DeviceConnectionState, DeviceLiveProbe, DeviceLiveView, DeviceStreamHealth,
};
use crate::api::system::{RecoveryModeProbe, RecoveryView};
use crate::db::DbPool;
use crate::devices::service::AdmissionChangeEvent;
use crate::model::{AdmissionSnapshot, AdmissionState, ReviewDecision};
use crate::polling::{PollScheduler, SnapshotGeneration, SnapshotStore, UpdateAccepted};
use crate::tasks::adapter::{TaskTimeSource, TimeEvidenceFuture};
use crate::tasks::board_client::{BoardClientError, BoardFuture};
use crate::time::evidence::TimeEvidence;
use crate::transport::board_client::{DeviceRpc, RpcReply};

/// 撤销/拒绝通知的有界通道容量：管理面 `after_commit` 是低频人工事件，256 足够吸收突发；
/// 通道满时的语义见 [`TransportManager::after_commit`]（绝不静默丢弃撤销真相）。
const REVOKE_QUEUE_CAPACITY: usize = 256;

/// §4.7 单连接排他：旧控制流可用时先发 `tunnel.kick`，**延迟 500ms** 后关闭旧 TCP。
/// 单调计时（`tokio::time::sleep`），不受宿主墙钟影响。
pub const SYSTEM_SIGNAL_CLOSE_DELAY: Duration = Duration::from_millis(500);

/// 全量准入复查的默认周期（有界通知丢失时的 fallback 上界）。
pub const DEFAULT_ADMISSION_RECHECK_INTERVAL: Duration = Duration::from_secs(30);
/// 单次准入真相读取的默认墙钟预算（P1-9：绝不无期限 await 一条 SQL）。
pub const DEFAULT_ADMISSION_READ_TIMEOUT: Duration = Duration::from_secs(2);
/// 采集失败投影到 [`SnapshotStore`] 的**去敏固定**错误码（不含 peer 文本、不含 SQL）。
pub const SNAPSHOT_ERROR_CODE: &str = "collection-failed";
/// 轮询默认周期（05 §6：status 30s 过期阈值 → 10s 周期留 3 个采样余量）。
pub const DEFAULT_POLL_PERIOD: Duration = Duration::from_secs(10);
/// 轮询默认 tick 节奏（[`PollScheduler`] 的 `tick_cadence`）。
pub const DEFAULT_POLL_TICK_CADENCE: Duration = Duration::from_millis(100);
/// 同时在途的轮询请求上限；超出即本轮丢弃并把调度器标为 overloaded（不积压）。
pub const DEFAULT_MAX_CONCURRENT_POLLS: usize = 64;

// ---------------------------------------------------------------------------
// 连接抽象
// ---------------------------------------------------------------------------

/// 装箱 future 别名（对象安全，供 `Arc<dyn DeviceConnection>` 持有）。
pub type ConnectionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 一次设备 RPC 的失败分类。driver 的 `RequestError` 逐类映射到 typed
/// [`BoardClientError`]，不向上传播对端文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallError {
    /// 普通请求在 5s（由 `SessionConfig::ordinary_timeout` 保证、从入队起算）内未完成，
    /// 结果未知：**绝不**据此判失败或重发。
    Timeout,
    /// 会话已关闭或正在排空：等价于控制流不在线。
    Closed,
    /// 连接侧 pending/业务队列预算已满（背压）。
    Busy,
    /// 回包违反协议（trace/action/流/代际不符）。
    Protocol,
}

impl From<CallError> for BoardClientError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Timeout => BoardClientError::Timeout,
            CallError::Closed => BoardClientError::DeviceOffline,
            CallError::Busy => BoardClientError::DeviceBusy,
            CallError::Protocol => BoardClientError::ProtocolMismatch,
        }
    }
}

/// 会话连接的可观测状态，逐项对应 driver 的 `SessionState`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Ready,
    Draining,
    Closed,
}

/// `TYPE_SYSTEM` 信令（§4.7/§6.3：只允许在控制流上）。
///
/// 本管理器**不**依赖 driver 类型，适配器负责映射到 `SystemSignal::{Kick, Revoke}`；
/// 绝不用 `request("tunnel.kick")` 之类的业务请求冒充系统信令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemNotice {
    /// 临时踢出：板端按退避重连。
    Kick,
    /// 永久吊销：板端终止自动重连。
    Revoke,
}

/// 设备会话连接抽象：`SessionHandle` 公开表面的镜像。
///
/// 生产适配器见 [`listener::SessionConnection`](super::listener::SessionConnection)；
/// lib/集成测试注入 fake 覆盖路由、顶替与撤销语义。实现必须满足：`request` 恰好发送一次、
/// 不重试；`send_system` 成功只表示**已入队**；`abort` 结清本连接在途恰好一次。
pub trait DeviceConnection: Send + Sync + 'static {
    /// 发送一次业务 RPC（固定 Control 流）。回包已由 session 校验 trace/action/流/代际。
    fn request(
        &self,
        action: &'static str,
        payload: Vec<u8>,
    ) -> ConnectionFuture<'_, Result<RpcReply, CallError>>;
    /// 非阻塞状态快照。
    fn state(&self) -> ConnectionState;
    /// 发送一条 `TYPE_SYSTEM` 信令；`Ok(())` 仅表示已入队，不代表已写出。
    fn send_system(&self, notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>>;
    /// 幂等请求本连接排空（h2 GOAWAY 语义，用于进程关停）。
    fn graceful_shutdown(&self);
    /// 立即中断本连接与其在途任务（不做排空、不重放任何变更）。
    fn abort(&self);
}

// ---------------------------------------------------------------------------
// 取消信号与半开预算
// ---------------------------------------------------------------------------

/// 挂起握手/会话的取消信号：`cancel()` 先置位再唤醒；等待侧用 `enable()` 先注册再复查
/// 标志，关闭「先唤醒后注册」的丢事件竞态。
#[derive(Debug, Default)]
pub struct CancelSignal {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelSignal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 置位并唤醒所有等待者；返回是否由本次调用完成翻转。
    pub fn cancel(&self) -> bool {
        let flipped = !self.cancelled.swap(true, Ordering::AcqRel);
        if flipped {
            self.notify.notify_waiters();
        }
        flipped
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// 等到被取消（已取消则立即返回）。
    pub async fn wait(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut notified = std::pin::pin!(self.notify.notified());
        // `enable()` 先注册唤醒兴趣再复查标志：否则 `notify_waiters()` 恰好落在
        // 「查标志」与「注册」之间时，本次取消就永远不会唤醒本等待者。
        if notified.as_mut().enable() || self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// 全局半开（已 accept 未 ready）计数；`Arc` 语义让 permit 独立于管理器生命周期。
#[derive(Debug)]
struct HalfOpenCounter {
    live: AtomicUsize,
    limit: usize,
}

/// 半开名额（RAII）：`Drop` 归还。握手成功登记、失败、超时、取消都走 `Drop`，
/// 因此不存在计数泄漏路径。
#[derive(Debug)]
pub struct HalfOpenPermit {
    counter: Arc<HalfOpenCounter>,
}

impl Drop for HalfOpenPermit {
    fn drop(&mut self) {
        let previous = self.counter.live.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "半开计数不得下穿 0");
    }
}

/// 挂起握手的登记凭证：把「已绑定已验签公钥、尚未 ready」的握手与它的取消信号绑在一起。
///
/// `Drop` 即摘除登记（握手失败/超时/取消都会走到这里）；`register_ready` 消费它，使
/// 「挂起 → 当前会话」的切换与撤销落在同一把写锁内。
pub struct PendingTicket {
    /// `register_ready` 需要在**同一把写锁内**自行摘除登记时，先取走本句柄抑制 `Drop`
    /// 的自动摘除（`Drop` 会再来取一次写锁，std `RwLock` 不可重入 → 死锁）。
    manager: Option<Arc<TransportManager>>,
    key: [u8; 32],
    id: u64,
    signal: Arc<CancelSignal>,
}

impl Drop for PendingTicket {
    fn drop(&mut self) {
        if let Some(manager) = self.manager.as_ref() {
            manager.forget_pending(self.key, self.id);
        }
    }
}

impl PendingTicket {
    /// 该挂起握手是否已被撤销/拒绝。
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }

    /// 等到撤销/拒绝。
    pub async fn cancelled(&self) {
        self.signal.wait().await;
    }

    /// 本凭证绑定的已验签公钥。
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.key
    }

    /// 立即取消本挂起握手（连接任务被取消时的显式路径）。
    pub fn cancel(&self) -> bool {
        self.signal.cancel()
    }
}

// ---------------------------------------------------------------------------
// 准入真相 seam
// ---------------------------------------------------------------------------

/// 装箱后的「当前准入决定」读取 future。
pub type AdmissionDecisionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AdmissionDecision, ControllerError>> + Send + 'a>>;

/// 只读准入真相的注入点（撤销 fallback 与握手取消复查的**唯一**判据）。
///
/// 返回的是已审 [`classify_row`](crate::transport::admission::classify_row) 的产物 [`AdmissionDecision`]，因此本入口的判定与握手
/// 路径**同一真源**：归档行、`archived` 非 0/1、合法域之外的组合、读取失败全部是
/// `Err`（调用方 fail-closed 撤销既有会话），不存在「`APPROVED`+`approved` 但已归档仍被
/// 当作批准」的第二套口径（审查 P1-8）。
///
/// 生产实现 [`DbPool`] 走本模块自有的**只读** SQL（[`CURRENT_ADMISSION_SQL`]）：
/// * 不 `FOR UPDATE`、不 `INSERT`（区别于 [`admission::SqlxAdmissionPolicy`](crate::transport::admission::SqlxAdmissionPolicy)：那条路径会在身份缺失时
///   登记新的 `PENDING` 行，复查路径绝不能写库）；
/// * 不改动也不复用共享的 `impl AdmissionStore for DbPool::load`（它不投影 `archived`，
///   是管理面既有的读合同，本任务无权修改）。
pub trait AdmissionTruth: Send + Sync {
    fn load_current(&self, public_key: [u8; 32]) -> AdmissionDecisionFuture<'_>;
}

/// 会话复查用的**只读**当前准入投影：与握手用的
/// [`admission::LOCK_ADMISSION_ROW_SQL`](crate::transport::admission::LOCK_ADMISSION_ROW_SQL)
/// 同列，但**没有** `FOR UPDATE`（复查绝不持行锁）。
///
/// 列集合是判定的必要充分集：`admission_state`、`review_decision`、`revision` 与
/// `archived`（归档行必须失败关闭，P1-8）。
pub const CURRENT_ADMISSION_SQL: &str = "SELECT admission_state,review_decision,revision,CAST(archived AS SIGNED) AS archived FROM devices WHERE public_key = ?";

impl AdmissionTruth for DbPool {
    fn load_current(&self, public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        Box::pin(async move {
            let row = sqlx::query(CURRENT_ADMISSION_SQL)
                .bind(public_key.as_slice())
                .fetch_optional(&self.0)
                .await
                .map_err(|_| ControllerError::Config("admission truth unavailable".into()))?
                .ok_or(ControllerError::NotFound)?;
            let stored = crate::transport::admission::AdmissionRow {
                admission_state: row
                    .try_get("admission_state")
                    .map_err(|_| ControllerError::Config("admission truth unavailable".into()))?,
                review_decision: row
                    .try_get("review_decision")
                    .map_err(|_| ControllerError::Config("admission truth unavailable".into()))?,
                revision: row
                    .try_get::<u64, _>("revision")
                    .map_err(|_| ControllerError::Config("admission truth unavailable".into()))?,
                archived: row
                    .try_get::<i64, _>("archived")
                    .map_err(|_| ControllerError::Config("admission truth unavailable".into()))?,
            };
            crate::transport::admission::classify_row(&stored)
                .map_err(|_| ControllerError::Config("admission truth unavailable".into()))
        })
    }
}

/// 当前 SQL 是否允许既有会话继续存在：**只有** `APPROVED` + `approved`。
///
/// `Pending`（含审批被重开）、`Denied`、`Revoked`、归档与非法组合都不是「继续存在」的
/// 许可：批准只允许新建连接，不豁免在途会话。
#[must_use]
pub fn snapshot_admits(snapshot: &AdmissionSnapshot) -> bool {
    snapshot.admission_state == AdmissionState::Approved
        && snapshot.review_decision == ReviewDecision::Approved
}

/// [`AdmissionTruth`] 产物到「是否仍允许既有会话」的判定（与 [`snapshot_admits`] 同义）。
#[must_use]
pub fn decision_admits(decision: &AdmissionDecision) -> bool {
    matches!(decision, AdmissionDecision::Approved)
}

// ---------------------------------------------------------------------------
// 登记表
// ---------------------------------------------------------------------------

/// 单个公钥的一份当前会话。
struct ActiveSession {
    /// 本管理器为这条连接分配的当前代际（本地 Uuid，仅作进程内连接标识）。
    generation: Uuid,
    /// 本会话的流代际（本地产生，与 `generation` 一起构成 [`SnapshotStore`] 的代际坐标）。
    stream_epoch: Uuid,
    connection: Arc<dyn DeviceConnection>,
    /// 最近一次「当前认证代际上的成功活动」墙钟（`last_seen` 的唯一来源）。
    last_activity: SystemTime,
    /// 当前代际上取回的能力快照（`DeviceLiveProbe` 的真实 typed 缓存；未知为 `None`）。
    capabilities: RwLock<Option<Capabilities>>,
    /// 是否已就本代际向 [`SnapshotStore`] 确认过代际（唯一入口见 `ingest_status_sample`）。
    snapshot_confirmed: AtomicBool,
    /// 已向 [`SnapshotStore`] 确认的代际坐标（首条可信样本落地时写入）。
    /// 连接断开只按这份坐标复位连接标记，绝不凭 `public_key` 猜测代际。
    confirmed_coordinate: RwLock<Option<SnapshotGeneration>>,
    /// 事件闸门（P2-12）：撤销/顶替/摘除在**管理器写锁内**关闭；事件投递方在 sink 调用前
    /// `claim`、调用后 `commit` 都必须看到它。因此「本代际已不是当前会话」之后到达的事件
    /// 一律 `commit` 失败并计数，绝不静默算作已消费。
    event_gate: Arc<EventGate>,
}

impl ActiveSession {
    /// 已向快照 store 确认的代际坐标（未确认过即 `None`）。
    fn snapshot_coordinate(&self) -> Option<SnapshotGeneration> {
        self.confirmed_coordinate
            .read()
            .ok()
            .and_then(|stored| stored.clone())
    }
}

/// 单个公钥的连接状态机：至多一份当前会话 + 若干挂起握手。
#[derive(Default)]
struct KeyEntry {
    current: Option<ActiveSession>,
    pending: HashMap<u64, Arc<CancelSignal>>,
    /// 曾登记为当前会话的挂起握手中最大的本地序号（0 = 从未登记过会话）。
    ///
    /// 这是「新连接替换旧挂起连接后，旧连接的异步回调无权更改新连接状态」（§4.6 第 3 条）
    /// 的判据：**本地单调序号**，绝不比较随机 Uuid 的字节序。它**不随会话摘除而回退**，
    /// 因此「更新的会话已经上线又断开」之后，更旧的握手同样无权把自己装成当前会话。
    ready_sequence: u64,
}

impl KeyEntry {
    /// 在**当前写锁内**关闭当前会话的事件闸门（撤销/顶替/摘除三条路径共用）。
    fn close_event_gate(&mut self) {
        if let Some(active) = self.current.as_ref() {
            active.event_gate.close();
        }
    }
}

/// 登记一个 ready 会话的输入。
pub struct NewSession {
    /// 握手返回的 `peer_public_key`（已验签；禁止从 payload 派生）。
    pub public_key: [u8; 32],
    /// 已 `wait_ready` 成功的设备连接。
    pub connection: Arc<dyn DeviceConnection>,
    /// 该连接的挂起凭证（来自 [`CancellableAdmissionPolicy::into_pending`]）。
    pub pending: PendingTicket,
    /// 本连接的半开名额（§4.6 第 1 条）。
    ///
    /// **转移边界在此**：名额从「accept 后、握手前」一直持有到「已 ready 且被登记为当前会话」
    /// 才由本方法释放（成功/失败/取消都靠 `Drop` 归还）。未 ready 的连接因此始终占用半开
    /// 名额，ready 之后改由 `max_sessions` 计量（审查 P1-4：既不会活到 session 末把名额耗尽，
    /// 也不会因握手前就释放而无界积压未 ready 会话）。
    pub half_open: HalfOpenPermit,
}

/// 登记结果错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// 半开预算已满（§4.6 第 1 条：可重试资源拒绝，不写永久决定）。
    HalfOpenFull,
    /// 活跃会话预算已满，且这是新建当前会话（顶替不受影响）。
    SessionsFull,
    /// 挂起握手在登记前已被撤销/拒绝：失败关闭，不安装已撤销身份的会话。
    Revoked,
    /// 凭证与本公钥不匹配（装配错误，对端无法触发）。
    TicketMismatch,
    /// 更旧的挂起握手试图顶替更新的当前会话（§4.6 第 3 条：旧连接的异步回调无权更改
    /// 新连接状态）。可重试语义不适用：这条连接已被更新的握手取代，调用方必须终止它。
    StaleHandshake,
}

/// 登记结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterOutcome {
    /// 被顶替的旧代际（若有）。
    pub replaced: Option<Uuid>,
    /// 本次登记的当代际。
    pub generation: Uuid,
}

/// 撤销动作的结果（观测与测试断言）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RevokeOutcome {
    /// 是否终止了一份活跃会话。
    pub terminated_session: bool,
    /// 取消了多少个挂起握手。
    pub cancelled_pending: usize,
}

/// 可信采样写入的结果分类（去敏，不含 payload 文本）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SampleOutcome {
    Accepted(UpdateAccepted),
    /// 该 `(public_key, generation)` 已不是当前认证代际（旧连接迟到样本）。
    StaleGeneration,
    /// 样本自身或代际不自洽（原因去敏）。
    Rejected(&'static str),
}

/// 管理器构造配置（全部为硬上界/正值，越界由 [`ManagerConfig::validate`] 拒绝，
/// 不静默放宽）。
///
/// # 单一真源
///
/// 生产装配**只解析一次**环境（`TransportConfig::from_env`），再经
/// [`TransportConfig::manager_config`](crate::transport::listener::TransportConfig::manager_config)
/// 把同一份预算同时交给管理器与监听线；本结构的字段不得被另一处默认值覆盖（审查 P1-5）。
#[derive(Clone, Copy, Debug)]
pub struct ManagerConfig {
    /// 全局半开（已 accept 未 ready）上限。
    pub max_half_open: usize,
    /// 活跃（ready）会话上限。
    pub max_sessions: usize,
    /// 全量准入复查周期。
    pub admission_recheck_interval: Duration,
    /// 单次准入真相读取的墙钟预算（超时即 `Err` → fail-closed 撤销，绝不无限等）。
    pub admission_read_timeout: Duration,
    /// 一轮全量复查的总墙钟上界（超期即中止本轮剩余项，可被 shutdown 取消）。
    pub admission_sweep_deadline: Duration,
    /// 一轮复查内并发读取真相的上限（受控并发：不逐条串到无穷，也不 1024 路齐发）。
    pub admission_recheck_concurrency: usize,
    /// 轮询周期。
    pub poll_period: Duration,
    /// 轮询 tick 节奏。
    pub poll_tick_cadence: Duration,
    /// 同时在途轮询请求上限。
    pub max_concurrent_polls: usize,
}

/// 半开上限的硬上界：再大就等于没有上界（§4.6 第 1 条要求「有界资源」）。
pub const MAX_HALF_OPEN_LIMIT: usize = 65_536;
/// 活跃会话上限的硬上界（§8 第 5 条的容量口径是约 1024 台）。
pub const MAX_SESSIONS_LIMIT: usize = 65_536;
/// 在途轮询请求上限的硬上界。
pub const MAX_CONCURRENT_POLLS_LIMIT: usize = 4_096;
/// 一次复查读取真相的并发上限硬上界（真相是 SQL，不允许把 DB 连接池打满）。
pub const MAX_RECHECK_CONCURRENCY_LIMIT: usize = 256;
/// 任何可配置期限的硬上界（配成「一天」这类明显错误必须失败关闭，而不是静默接受）。
pub const MAX_MANAGER_DURATION: Duration = Duration::from_secs(3_600);

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            max_half_open: 256,
            max_sessions: 1_024,
            admission_recheck_interval: DEFAULT_ADMISSION_RECHECK_INTERVAL,
            admission_read_timeout: DEFAULT_ADMISSION_READ_TIMEOUT,
            admission_sweep_deadline: Duration::from_secs(10),
            admission_recheck_concurrency: 16,
            poll_period: DEFAULT_POLL_PERIOD,
            poll_tick_cadence: DEFAULT_POLL_TICK_CADENCE,
            max_concurrent_polls: DEFAULT_MAX_CONCURRENT_POLLS,
        }
    }
}

impl ManagerConfig {
    /// 配置合法性：任一预算为 0、期限为零或超过硬上界即失败关闭（宁可不启动，不静默放宽）。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_half_open == 0 || self.max_sessions == 0 {
            return Err("max_half_open/max_sessions must be non-zero");
        }
        if self.max_half_open > MAX_HALF_OPEN_LIMIT {
            return Err("max_half_open exceeds the hard upper bound");
        }
        if self.max_sessions > MAX_SESSIONS_LIMIT {
            return Err("max_sessions exceeds the hard upper bound");
        }
        if self.max_concurrent_polls == 0 {
            return Err("max_concurrent_polls must be non-zero");
        }
        if self.max_concurrent_polls > MAX_CONCURRENT_POLLS_LIMIT {
            return Err("max_concurrent_polls exceeds the hard upper bound");
        }
        if self.admission_recheck_concurrency == 0 {
            return Err("admission_recheck_concurrency must be non-zero");
        }
        if self.admission_recheck_concurrency > MAX_RECHECK_CONCURRENCY_LIMIT {
            return Err("admission_recheck_concurrency exceeds the hard upper bound");
        }
        if self.admission_recheck_interval.is_zero()
            || self.admission_read_timeout.is_zero()
            || self.admission_sweep_deadline.is_zero()
            || self.poll_period.is_zero()
            || self.poll_tick_cadence.is_zero()
        {
            return Err("manager durations must be non-zero");
        }
        if self.admission_recheck_interval > MAX_MANAGER_DURATION
            || self.admission_read_timeout > MAX_MANAGER_DURATION
            || self.admission_sweep_deadline > MAX_MANAGER_DURATION
            || self.poll_period > MAX_MANAGER_DURATION
            || self.poll_tick_cadence > MAX_MANAGER_DURATION
        {
            return Err("manager duration exceeds the hard upper bound");
        }
        if self.poll_tick_cadence > self.poll_period {
            return Err("poll_tick_cadence must not exceed poll_period");
        }
        if self.admission_read_timeout > self.admission_sweep_deadline {
            return Err("admission_read_timeout must not exceed admission_sweep_deadline");
        }
        Ok(())
    }

    /// 连接任务总量的硬上界（`max_half_open + max_sessions` 的**受检**和）：溢出即 `None`，
    /// 调用方必须失败关闭，绝不用 `saturating_add` 把溢出洗成「无界」。
    #[must_use]
    pub fn connection_task_budget(&self) -> Option<usize> {
        self.max_half_open.checked_add(self.max_sessions)
    }
}

// ---------------------------------------------------------------------------
// TransportManager
// ---------------------------------------------------------------------------

/// 已认证设备连接管理器：构造一次、`Arc` 共享；所有后台任务由显式 `spawn_*` 启动，
/// 并由同一个 `shutdown` watch 取消。
pub struct TransportManager {
    entries: RwLock<HashMap<[u8; 32], KeyEntry>>,
    next_pending_id: AtomicU64,
    half_open: Arc<HalfOpenCounter>,
    /// 挂起握手续约（= `entries` 内所有 `pending` 条目之和）。
    ///
    /// **只在持有 `entries` 写锁时增减**（P2-10）：因此「按实际移除条目归还」与「插入时记账」
    /// 落在同一条线性化边界上，不存在锁外窗口，也不可能被 `cancel()` 的翻转次数冒充。
    pending_slots: AtomicUsize,
    revoke_tx: mpsc::Sender<[u8; 32]>,
    revoke_rx: AsyncMutex<mpsc::Receiver<[u8; 32]>>,
    /// 通知投递失败次数（fallback 补扫的可观测证据）。
    revoke_dropped: AtomicU64,
    /// 整轮准入复查因 `admission_sweep_deadline` 超期被中止的次数（P1-9 可观测证据）。
    sweep_dropped: AtomicU64,
    /// 准入复查的**轮转游标**（审查 N3）：本轮从上一轮**实际完成**的位置之后继续扫，避免
    /// 每轮都从排序头部重扫、deadline 每轮在相同位置到期导致排序尾部身份被永久饿死。
    recheck_cursor: AtomicUsize,
    /// 立即复查唤醒（撤销通知丢失，或外部要求立即复核）。
    sweep: Notify,
    /// 系统信令已发出（入队成功）/未能发出的次数：撤销与顶替的「已发/未发」边界。
    signals_sent: Arc<AtomicU64>,
    signals_unsent: Arc<AtomicU64>,
    /// 在途的「信令 + 延迟切断」收尾任务数（有界，每个至多 `SYSTEM_SIGNAL_CLOSE_DELAY`）。
    close_out_tasks: Arc<AtomicUsize>,
    /// 在途收尾任务的**句柄**（P2-11 同源要求：收尾必须有 owner，不能只有原子计数）。
    ///
    /// 只追加 + 顺带回收已完成项；关停时由 [`Self::drain_close_outs`] 逐个 await（超期才
    /// abort），因此「计数已归零」不再是「没人 join」的伪证明。
    close_out_handles: Mutex<Vec<JoinHandle<()>>>,
    admission: Arc<dyn AdmissionTruth>,
    snapshots: Arc<SnapshotStore>,
    recovery: Arc<dyn RecoveryModeProbe>,
    time_source: Arc<dyn TaskTimeSource>,
    config: ManagerConfig,
}

impl TransportManager {
    /// 装配管理器共享句柄（**构造期校验**）。
    ///
    /// 生产装配必须走本入口：任一预算越界即 `Err`，进程失败关闭，绝不带着被静默放宽的
    /// 限额启动（审查 P2-13「`ManagerConfig::validate` 未调用」）。
    pub fn try_new(
        admission: Arc<dyn AdmissionTruth>,
        snapshots: Arc<SnapshotStore>,
        recovery: Arc<dyn RecoveryModeProbe>,
        time_source: Arc<dyn TaskTimeSource>,
        config: ManagerConfig,
    ) -> Result<Arc<Self>, &'static str> {
        config.validate()?;
        Ok(Self::with_config(
            admission,
            snapshots,
            recovery,
            time_source,
            config,
        ))
    }

    /// 装配管理器共享句柄（测试与内部装配便捷入口）。
    ///
    /// 配置非法时**不放宽、不 panic**：预算原样生效（0 即「一律拒绝」），debug 构建下直接
    /// 断言失败，生产装配请走 [`Self::try_new`]。
    #[must_use]
    pub fn new(
        admission: Arc<dyn AdmissionTruth>,
        snapshots: Arc<SnapshotStore>,
        recovery: Arc<dyn RecoveryModeProbe>,
        time_source: Arc<dyn TaskTimeSource>,
        config: ManagerConfig,
    ) -> Arc<Self> {
        debug_assert!(
            config.validate().is_ok(),
            "非法 ManagerConfig 必须经 TransportManager::try_new 失败关闭"
        );
        Self::with_config(admission, snapshots, recovery, time_source, config)
    }

    fn with_config(
        admission: Arc<dyn AdmissionTruth>,
        snapshots: Arc<SnapshotStore>,
        recovery: Arc<dyn RecoveryModeProbe>,
        time_source: Arc<dyn TaskTimeSource>,
        config: ManagerConfig,
    ) -> Arc<Self> {
        let (revoke_tx, revoke_rx) = mpsc::channel::<[u8; 32]>(REVOKE_QUEUE_CAPACITY);
        Arc::new(Self {
            entries: RwLock::new(HashMap::new()),
            next_pending_id: AtomicU64::new(1),
            // 限额原样生效：0 名额就是「一个都不给」，不 `max(1)` 静默放宽。
            half_open: Arc::new(HalfOpenCounter {
                live: AtomicUsize::new(0),
                limit: config.max_half_open,
            }),
            pending_slots: AtomicUsize::new(0),
            revoke_tx,
            revoke_rx: AsyncMutex::new(revoke_rx),
            revoke_dropped: AtomicU64::new(0),
            sweep_dropped: AtomicU64::new(0),
            recheck_cursor: AtomicUsize::new(0),
            sweep: Notify::new(),
            signals_sent: Arc::new(AtomicU64::new(0)),
            signals_unsent: Arc::new(AtomicU64::new(0)),
            close_out_tasks: Arc::new(AtomicUsize::new(0)),
            close_out_handles: Mutex::new(Vec::new()),
            admission,
            snapshots,
            recovery,
            time_source,
            config,
        })
    }

    #[must_use]
    pub fn config(&self) -> ManagerConfig {
        self.config
    }

    /// 供 `TransportBoardClient::new` 使用的共享 RPC 句柄。
    #[must_use]
    pub fn as_device_rpc(self: &Arc<Self>) -> Arc<dyn DeviceRpc> {
        Arc::clone(self) as Arc<dyn DeviceRpc>
    }

    // --- 半开预算 ---------------------------------------------------------

    /// 预留一个半开名额（listener 在 `accept` 之后、开始握手之前调用）。
    pub fn half_open_permit(self: &Arc<Self>) -> Result<HalfOpenPermit, RegisterError> {
        let counter = Arc::clone(&self.half_open);
        // 先加后判：避免「判未满 → 别人也判未满 → 双双加」的超卖。
        if counter.live.fetch_add(1, Ordering::AcqRel) >= counter.limit {
            counter.live.fetch_sub(1, Ordering::AcqRel);
            return Err(RegisterError::HalfOpenFull);
        }
        Ok(HalfOpenPermit { counter })
    }

    #[must_use]
    pub fn half_open_count(&self) -> usize {
        self.half_open.live.load(Ordering::Acquire)
    }

    // --- 挂起握手登记 -----------------------------------------------------

    /// 登记一次挂起握手（由 [`CancellableAdmissionPolicy`] 在首次 `evaluate` 时按已验签
    /// 公钥调用），返回绑定该公钥的凭证与取消信号。
    ///
    /// 记账与插入在**同一次写锁**内完成（P2-10）；同一公钥上更旧的挂起握手在此被取消并摘除
    /// （§4.6 第 3 条：新连接替换旧挂起连接），其名额也在同一把锁内按实际移除条目归还。
    pub fn track_pending(self: &Arc<Self>, public_key: [u8; 32]) -> PendingTicket {
        let id = self.next_pending_id.fetch_add(1, Ordering::AcqRel).max(1);
        let signal = Arc::new(CancelSignal::new());
        {
            let mut entries = self.write_entries();
            let mut released = 0usize;
            {
                let entry = entries.entry(public_key).or_default();
                let superseded: Vec<u64> = entry
                    .pending
                    .keys()
                    .filter(|older| **older < id)
                    .copied()
                    .collect();
                for older in &superseded {
                    // 先取消（唤醒等待方），再从表里摘除：两步之间不放锁，因此不存在
                    // 「已取消但仍计数」的窗口。
                    if let Some(old_signal) = entry.pending.remove(older) {
                        old_signal.cancel();
                        released += 1;
                    }
                }
                entry.pending.insert(id, Arc::clone(&signal));
            }
            if released > 0 {
                let previous = self.pending_slots.fetch_sub(released, Ordering::AcqRel);
                debug_assert!(previous >= released, "挂起握手续约不得下穿 0");
            }
            self.pending_slots.fetch_add(1, Ordering::AcqRel);
            Self::prune_entry(&mut entries, &public_key);
        }
        PendingTicket {
            manager: Some(Arc::clone(self)),
            key: public_key,
            id,
            signal,
        }
    }

    /// 摘除一个挂起登记（幂等：id 不存在即零副作用）。
    fn forget_pending(&self, public_key: [u8; 32], id: u64) {
        let removed = {
            let mut entries = self.write_entries();
            let removed = entries
                .get_mut(&public_key)
                .and_then(|entry| entry.pending.remove(&id))
                .is_some();
            if removed {
                let previous = self.pending_slots.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "挂起握手续约不得下穿 0");
                Self::prune_entry(&mut entries, &public_key);
            }
            removed
        };
        let _ = removed;
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending_slots.load(Ordering::Acquire)
    }

    // --- 当前会话登记 -----------------------------------------------------

    /// 登记一个已 ready 的会话为当前会话；同 key 的旧会话按 §4.7 顶替。
    ///
    /// 同一把写锁内线性化完成四件事（审查 P1-7/P2-10）：
    /// 1. 挂起凭证已被取消 → [`RegisterError::Revoked`]，绝不安装已撤销身份的会话；
    /// 2. 握序序号严格旧于已登记的当前会话 → [`RegisterError::StaleHandshake`]，更旧的握手
    ///    不能逆向顶替更新的会话（判据是**本地单调序号**，绝不比较随机 Uuid）；
    /// 3. 按实际移除的 pending 条目归还名额（不是 `cancel()` 的翻转次数）；
    /// 4. 安装新会话并把半开名额随 `pending` 一起释放（转移边界见 [`NewSession::half_open`]）。
    pub fn register_ready(&self, session: NewSession) -> Result<RegisterOutcome, RegisterError> {
        let NewSession {
            public_key,
            connection,
            mut pending,
            half_open,
        } = session;
        if pending.key != public_key {
            return Err(RegisterError::TicketMismatch);
        }
        // 抑制 `Drop` 的自动摘除（它要再取一次同一把写锁，std RwLock 不可重入），
        // 改为本函数在锁内自行摘除；管理器句柄在释放锁之后才归还。
        let ticket_manager = pending.manager.take();
        let pending_id = pending.id;
        let signal = Arc::clone(&pending.signal);
        drop(pending);

        let (previous, outcome) = {
            let mut entries = self.write_entries();
            // 借用顺序：`entry` 一旦借出就不能再整体读 `entries`，故先算活跃数量。
            let active_count = Self::active_count_locked(&entries);
            let entry = entries.entry(public_key).or_default();
            let removed = entry.pending.remove(&pending_id).is_some();
            // 与撤销共用这把写锁：撤销要么先取消（此处直接拒装），要么后见到新会话。
            let cancelled = signal.is_cancelled();
            // 更旧的握手不得逆向顶替：判据是本地单调握序号（0 = 从未登记会话）。
            let stale = pending_id < entry.ready_sequence;
            let replaced = entry.current.as_ref().map(|old| ActiveSnapshot {
                generation: old.generation,
                connection: Arc::clone(&old.connection),
                gate: Arc::clone(&old.event_gate),
            });
            let full = replaced.is_none() && active_count >= self.config.max_sessions;
            if cancelled || stale || full {
                entry.close_event_gate();
                if removed {
                    let slots = self.pending_slots.fetch_sub(1, Ordering::AcqRel);
                    debug_assert!(slots > 0, "挂起握手续约不得下穿 0");
                }
                Self::prune_entry(&mut entries, &public_key);
                // 写锁在 `return` 展开局部时释放；`ticket_manager` 的归还不取锁，安全。
                // 半开名额同样随 `half_open` 在本函数返回时 `Drop` 归还。
                return Err(if cancelled {
                    RegisterError::Revoked
                } else if stale {
                    RegisterError::StaleHandshake
                } else {
                    RegisterError::SessionsFull
                });
            }
            if removed {
                let slots = self.pending_slots.fetch_sub(1, Ordering::AcqRel);
                debug_assert!(slots > 0, "挂起握手续约不得下穿 0");
            }
            let generation = Uuid::new_v4();
            let stream_epoch = Uuid::new_v4();
            let event_gate = Arc::new(EventGate::open());
            entry.ready_sequence = entry.ready_sequence.max(pending_id);
            entry.current = Some(ActiveSession {
                generation,
                stream_epoch,
                connection: Arc::clone(&connection),
                last_activity: SystemTime::now(),
                capabilities: RwLock::new(None),
                snapshot_confirmed: AtomicBool::new(false),
                confirmed_coordinate: RwLock::new(None),
                event_gate: Arc::clone(&event_gate),
            });
            let outcome = RegisterOutcome {
                replaced: replaced.as_ref().map(|old| old.generation),
                generation,
            };
            // 顶替：旧会话的事件闸门在**同一把写锁内**关闭，之后的事件一律 commit 失败。
            if let Some(old) = replaced.as_ref() {
                old.gate.close();
            }
            (replaced, Ok(outcome))
        };
        drop(ticket_manager);
        // 半开名额在「已登记为当前会话」这一刻归还（P1-4 的转移边界）。
        drop(half_open);

        // 顶替旧连接：代际切换已在上面那把写锁内完成；这里只对**被顶替的那份句柄**发
        // TYPE_SYSTEM 并按 §4.7 单调延迟切断，不受后续新代际影响。
        if let Some(old) = previous.as_ref() {
            self.close_connection(&old.connection, SystemNotice::Kick);
        }
        outcome
    }

    /// 连接任务退出时摘除自己：只有仍是当前代际才生效（旧连接不得抹掉新连接）。
    pub fn deregister(&self, public_key: &[u8; 32], generation: Uuid) -> bool {
        let (removed, coordinate) = {
            let mut entries = self.write_entries();
            let matched = entries
                .get(public_key)
                .and_then(|entry| entry.current.as_ref())
                .is_some_and(|active| active.generation == generation);
            let coordinate = if matched {
                entries
                    .get_mut(public_key)
                    .and_then(|entry| {
                        // 闸门与摘除同锁关闭：会话不存在之后，事件不得再被接受（P2-12）。
                        entry.close_event_gate();
                        entry.current.take()
                    })
                    .and_then(|active| active.snapshot_coordinate())
            } else {
                None
            };
            if matched {
                Self::prune_entry(&mut entries, public_key);
            }
            (matched, coordinate)
        };
        if let Some(coordinate) = coordinate.as_ref() {
            // 本代际连接结束：只复位连接标记，作用域与序号下界由 store 保留（绝不清除）。
            self.snapshots.disconnect_generation(public_key, coordinate);
        }
        removed
    }

    /// 立即撤销一个公钥：终止活跃会话（`TYPE_SYSTEM tunnel.revoke` 后强制切断）+ 取消全部
    /// 挂起握手。
    ///
    /// **记账口径（P2-10）**：名额按 `entry.pending` 里**实际被移除的条目数**归还，与是否
    /// 已经被人 `cancel()` 过无关（已取消的凭证只是标志为真，仍占着条目与名额）。
    /// `RevokeOutcome::cancelled_pending` 只统计「本次调用首次翻转」的数量，是纯观测量，
    /// 绝不参与归还计算。取消、清表、闸门关闭与记账全部在同一次写锁内完成。
    pub fn apply_revoke(&self, public_key: [u8; 32]) -> RevokeOutcome {
        let mut outcome = RevokeOutcome::default();
        let (connection, coordinate) = {
            let mut entries = self.write_entries();
            let Some(entry) = entries.get_mut(&public_key) else {
                return outcome;
            };
            let (connection, coordinate) = if let Some(active) = entry.current.as_ref() {
                outcome.terminated_session = true;
                (
                    Some(Arc::clone(&active.connection)),
                    active.snapshot_coordinate(),
                )
            } else {
                (None, None)
            };
            let removed: Vec<Arc<CancelSignal>> = entry.pending.values().map(Arc::clone).collect();
            outcome.cancelled_pending = removed.iter().filter(|signal| signal.cancel()).count();
            let released = removed.len();
            entry.pending.clear();
            // 事件闸门与摘除同锁关闭：之后的 `claim` 一律失败，已在途的 `commit` 也判失效。
            if let Some(active) = entry.current.as_ref() {
                active.event_gate.close();
            }
            entry.current.take();
            Self::prune_entry(&mut entries, &public_key);
            if released > 0 {
                let previous = self.pending_slots.fetch_sub(released, Ordering::AcqRel);
                debug_assert!(previous >= released, "挂起握手续约不得下穿 0");
            }
            (connection, coordinate)
        };
        if let Some(coordinate) = coordinate.as_ref() {
            // 已确认过代际才需要复位；从未采过样的会话在 store 里没有条目。
            self.snapshots
                .disconnect_generation(&public_key, coordinate);
        }
        if let Some(connection) = connection.as_ref() {
            self.close_connection(connection, SystemNotice::Revoke);
        }
        outcome
    }

    // --- 观测与活动 -------------------------------------------------------

    /// 只在 `(public_key, generation)` 仍是当前代际时更新 `last_seen` 证据。
    ///
    /// 返回 `false` 即旧代际/离线连接：调用方**不得**据此刷新 last_seen。
    pub fn note_activity(&self, public_key: &[u8; 32], generation: Uuid) -> bool {
        let mut entries = self.write_entries();
        let Some(entry) = entries.get_mut(public_key) else {
            return false;
        };
        match entry.current.as_mut() {
            Some(active) if active.generation == generation => {
                active.last_activity = SystemTime::now();
                true
            }
            _ => false,
        }
    }

    /// 该 `(public_key, generation)` 是否仍是当前会话（异步回调的 fail-closed 复查）。
    ///
    /// **注意**：这只证明「本进程内存仍把它当当前会话」，**不能**证明 SQL 未撤销；
    /// 撤销的最终判据是 [`AdmissionTruth`] 与 [`Self::apply_revoke`]。
    #[must_use]
    pub fn session_is_current(&self, public_key: &[u8; 32], generation: Uuid) -> bool {
        let entries = self.read_entries();
        entries
            .get(public_key)
            .and_then(|entry| entry.current.as_ref())
            .is_some_and(|active| active.generation == generation)
    }

    /// 当前会话的 `(代际, 连接句柄)`。
    #[must_use]
    pub fn current_connection(
        &self,
        public_key: &[u8; 32],
    ) -> Option<(Uuid, Arc<dyn DeviceConnection>)> {
        let entries = self.read_entries();
        entries
            .get(public_key)
            .and_then(|entry| entry.current.as_ref())
            .map(|active| (active.generation, Arc::clone(&active.connection)))
    }

    /// 当前 ready 且未排空的公钥集合（轮询驱动遍历用，稳定排序）。
    #[must_use]
    pub fn active_devices(&self) -> Vec<[u8; 32]> {
        let mut ready: Vec<[u8; 32]> = self
            .read_entries()
            .iter()
            .filter(|(_, entry)| {
                entry
                    .current
                    .as_ref()
                    .is_some_and(|active| active.connection.state() == ConnectionState::Ready)
            })
            .map(|(key, _)| *key)
            .collect();
        ready.sort_unstable();
        ready
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        Self::active_count_locked(&self.read_entries())
    }

    /// 当前认证代际上成功取回的能力快照（`DeviceLiveProbe` 的唯一 typed 来源）。
    #[must_use]
    pub fn cached_capabilities(&self, public_key: &[u8; 32]) -> Option<Capabilities> {
        self.read_entries()
            .get(public_key)
            .and_then(|entry| entry.current.as_ref())
            .and_then(|active| {
                active
                    .capabilities
                    .read()
                    .ok()
                    .and_then(|cached| cached.clone())
            })
    }

    /// 撤销通知投递失败次数（>0 即必须依赖补扫，见 [`Self::after_commit`]）。
    #[must_use]
    pub fn dropped_revocations(&self) -> u64 {
        self.revoke_dropped.load(Ordering::Acquire)
    }

    /// 系统信令入队成功的次数（已发边界）。
    #[must_use]
    pub fn system_signals_sent(&self) -> u64 {
        self.signals_sent.load(Ordering::Acquire)
    }

    /// 系统信令未能发出的次数（未发边界：连接已坏或无运行时）。
    #[must_use]
    pub fn system_signals_unsent(&self) -> u64 {
        self.signals_unsent.load(Ordering::Acquire)
    }

    /// 在途「信令 + 延迟切断」收尾任务数（应回落到 0，是有界性的可观测证据）。
    #[must_use]
    pub fn pending_close_outs(&self) -> usize {
        self.close_out_tasks.load(Ordering::Acquire)
    }

    /// 整轮准入复查被总上界中止的次数（每轮剩余项留给下一轮）。
    #[must_use]
    pub fn admission_sweeps_dropped(&self) -> u64 {
        self.sweep_dropped.load(Ordering::Acquire)
    }

    /// 立即唤醒一次全量准入复查（不等待结果）。
    pub fn request_admission_sweep(&self) {
        self.sweep.notify_one();
    }

    // --- 可信采样入口 -----------------------------------------------------

    /// 可信状态样本入口：**只有**当前认证代际的样本才被接受。
    ///
    /// 代际坐标的 `session_epoch`/`stream_epoch` 由本管理器在 `register_ready` 时本地产生，
    /// `boot_id`/`agent_epoch` 取自**该已认证代际的 Control 流上取回**的样本；任意 packet
    /// 自报、其它 peer 或其它代际都无法在此推进代际（旧代际一律 `StaleGeneration`）。
    pub fn ingest_status_sample(
        &self,
        public_key: &[u8; 32],
        generation: Uuid,
        status: &DeviceStatus,
        evidence: TimeEvidence,
        received_at: Instant,
    ) -> SampleOutcome {
        // 审查 P1-2：当前代际校验、首样本 `confirm_generation`、`snapshot_confirmed` 标志与
        // `update` **全部在同一次管理器写锁内**完成。
        //
        // * 锁序固定为「管理器 `entries` 写锁 → [`SnapshotStore`] 内部锁」，两个方向都不存在
        //   反向路径（store 从不调用本管理器）；
        // * 临界区内**无 await、无外部回调**（store 只做一次 HashMap 条目替换），因此顶替/
        //   撤销要么整体早于本次采样、要么整体晚于本次采样，不会把旧坐标 confirm 到新代际上；
        // * 旧代际样本在第一步就被拒（`StaleGeneration`），既不 confirm 也不 update，对
        //   store 零副作用，更不可能把新代际的首样本洗成 `StaleGeneration`。
        let mut entries = self.write_entries();
        let Some(active) = entries
            .get_mut(public_key)
            .and_then(|entry| entry.current.as_mut())
            .filter(|active| active.generation == generation)
        else {
            return SampleOutcome::StaleGeneration;
        };
        let coordinate = SnapshotGeneration {
            session_epoch: generation,
            stream_epoch: active.stream_epoch,
            boot_id: status.boot_id.clone(),
            agent_epoch: status.agent_epoch.clone(),
        };
        if !active.snapshot_confirmed.load(Ordering::Acquire) {
            // 首条样本才推进代际：认证连接管理器确认设备当前代际的唯一入口。
            let _outcome = self
                .snapshots
                .confirm_generation(*public_key, coordinate.clone());
            if let Ok(mut stored) = active.confirmed_coordinate.write() {
                *stored = Some(coordinate.clone());
            }
            active.snapshot_confirmed.store(true, Ordering::Release);
        }
        match self
            .snapshots
            .update(public_key, &coordinate, status, evidence, received_at)
        {
            Ok(accepted) => SampleOutcome::Accepted(accepted),
            Err(crate::polling::UpdateRejected::NoActiveGeneration) => {
                SampleOutcome::StaleGeneration
            }
            Err(crate::polling::UpdateRejected::StaleGeneration) => SampleOutcome::StaleGeneration,
            Err(crate::polling::UpdateRejected::StaleSampleSeq { .. }) => {
                SampleOutcome::Rejected("stale-sample-seq")
            }
            Err(crate::polling::UpdateRejected::PayloadGenerationMismatch) => {
                SampleOutcome::Rejected("payload-generation-mismatch")
            }
            Err(crate::polling::UpdateRejected::InvalidPayload { .. }) => {
                SampleOutcome::Rejected("invalid-payload")
            }
        }
    }

    /// 采集失败投影（审查「补项 · 采集失败漏接 `record_error`」）。
    ///
    /// 只按**本次采样所属的当前代际**写入既有 [`SnapshotStore::record_error`]：保留最后好样本
    /// 与其单调年龄，只附加去敏固定错误码（[`SNAPSHOT_ERROR_CODE`]），绝不回填 0、绝不新增
    /// 第二套新鲜度真源。旧代际的失败对 store 零副作用（由 store 自己的代际校验兜底，本方法
    /// 先在管理器锁内拒掉旧代际，因此也不会误清新代际的错误位）。
    ///
    /// 返回 `false`：该 `(public_key, generation)` 已不是当前代际，或本代际**从未**确认过
    /// 代际坐标（store 里没有条目可投影）——后者如实返回失败，绝不为「有地方写错误」而提前
    /// 伪造一个代际条目。
    pub fn record_sample_error(&self, public_key: &[u8; 32], generation: Uuid) -> bool {
        let entries = self.read_entries();
        let Some(active) = entries
            .get(public_key)
            .and_then(|entry| entry.current.as_ref())
            .filter(|active| active.generation == generation)
        else {
            return false;
        };
        let Some(coordinate) = active.snapshot_coordinate() else {
            return false;
        };
        // 锁序与 `ingest_status_sample` 一致：管理器读锁 → store 写锁，临界区无 await/回调。
        self.snapshots
            .record_error(public_key, &coordinate, SNAPSHOT_ERROR_CODE)
            .is_ok()
    }

    /// 板端事件入口（审查 P2-12）：**当前已 ready 的 Control 会话**才发得出投递上下文。
    ///
    /// 判据全部来自管理器锁内的当前会话（本地代际 + 连接状态 + 事件闸门），不取 payload 身份。
    /// 返回的 [`TaskResultContext`] 携带本地代际上下文，消费方必须在自己的写入边界再核
    /// [`TaskResultContext::is_current`]；本方法**不同步持有**管理器锁跨越 sink 调用，因此
    /// sink 内部可安全回调管理器而不会自锁。
    pub fn claim_task_result(
        self: &Arc<Self>,
        public_key: &[u8; 32],
        generation: Uuid,
    ) -> Option<TaskResultContext> {
        let entries = self.read_entries();
        let active = entries
            .get(public_key)
            .and_then(|entry| entry.current.as_ref())
            .filter(|active| active.generation == generation)?;
        if active.connection.state() != ConnectionState::Ready || !active.event_gate.claim() {
            return None;
        }
        Some(TaskResultContext {
            manager: Arc::clone(self),
            public_key: *public_key,
            generation,
            gate: Arc::clone(&active.event_gate),
        })
    }

    /// 当前代际上成功取回的能力快照写入（同样只认当前代际；也是 last_seen 的活动证据）。
    pub fn ingest_capabilities(
        &self,
        public_key: &[u8; 32],
        generation: Uuid,
        capabilities: Capabilities,
    ) -> bool {
        let mut entries = self.write_entries();
        let Some(active) = entries
            .get_mut(public_key)
            .and_then(|entry| entry.current.as_mut())
        else {
            return false;
        };
        if active.generation != generation {
            return false;
        }
        if let Ok(mut cached) = active.capabilities.write() {
            *cached = Some(capabilities);
        }
        active.last_activity = SystemTime::now();
        true
    }

    /// 取一次当前时间证据（生产即唯一 `TimeGateHandle` 的权威快照；不可信时如实
    /// `SystemFallback`，不冒 NTP）。
    pub fn sample_evidence(&self) -> TimeEvidenceFuture<'_> {
        self.time_source.current_evidence()
    }

    // --- 后台任务 ---------------------------------------------------------

    /// 消费撤销通道：`after_commit` 投递的撤销/拒绝在这里真正执行。
    pub fn spawn_revoke_drain(
        self: &Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            // 独占接收端：`apply_revoke` 不碰通道，因此整轮持锁不会自锁。
            let mut rx = manager.revoke_rx.lock().await;
            loop {
                if *shutdown.borrow_and_update() {
                    break;
                }
                tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        if changed.is_err() {
                            break;
                        }
                    }
                    item = rx.recv() => {
                        match item {
                            Some(public_key) => {
                                manager.apply_revoke(public_key);
                            }
                            // 所有发送端关闭：本任务无事可做。
                            None => break,
                        }
                    }
                }
            }
            // 退出前把通道里已排队的通知消费掉：撤销不允许「随进程一起丢」。
            while let Ok(public_key) = rx.try_recv() {
                manager.apply_revoke(public_key);
            }
        })
    }

    /// 定期（以及被唤醒时）全量复查活跃会话的**当前 SQL 准入**，不再批准即撤销。
    ///
    /// 这是「有界通知丢失」的 fallback：判据来自 [`AdmissionTruth`]（真实库读取），读失败
    /// 同样 fail-closed（撤销本会话）而不是保留。
    pub fn spawn_admission_recheck(
        self: &Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let manager = Arc::clone(self);
        let interval = self.config.admission_recheck_interval;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        if *shutdown.borrow_and_update() {
                            break;
                        }
                    }
                    () = manager.sweep.notified() => {}
                    _ = ticker.tick() => {}
                }
                if *shutdown.borrow_and_update() {
                    break;
                }
                // 审查 P1-9：一轮复查必须是**可取消**的。`recheck_all_sessions` 自身有
                // 单条期限与总上界，这里再把它整体放进 `select!`：关停到达即丢弃本轮剩余的
                // 真相读取（连同子任务一起 abort），绝不因为一条挂起的 SQL 而退不出去。
                tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow_and_update() {
                            break;
                        }
                    }
                    _ = manager.recheck_all_sessions() => {}
                }
            }
        })
    }

    /// 一次全量复查：**受控并发** + **单条期限** + **整轮上界** + **轮转续扫**（审查 P1-9 / N3）。
    ///
    /// * 每条读取套 `admission_read_timeout`：超时/读不到（行不存在、归档、库异常、非法组合）
    ///   一律 fail-closed 撤销，绝不「读不到就保留」；
    /// * 并发上限 `admission_recheck_concurrency`（默认 16）：1024 台既不会串成一轮跑不完，
    ///   也不会 1024 路齐发打爆连接池；
    /// * 整轮套 `admission_sweep_deadline`：超期即 abort 剩余子任务并返回（调用方 `select!` 掉
    ///   本 future 时，子 `JoinSet` 随 drop 一并 abort）；
    /// * **轮转续扫（N3）**：本轮从 `recheck_cursor` 之后开始，而非每轮都从排序头部重扫。游标
    ///   只是**稳定排序枚举顺序**上的偏移（`active_keys` 的 `sort_unstable` 结果），绝不拿
    ///   uuid/排序当「时间新旧」。推进规则（诚实版，不得再按「被取消者一定留在游标之后」引用）：
    ///   整轮自然结束 ⇒ 游标复位 0（本轮全部当前 key 都已实际复查）；整轮被
    ///   `admission_sweep_deadline` 截断 ⇒ 游标按本轮**已 join 完成**的 key 数前进，在途子任务随
    ///   `JoinSet` drop 被 abort，其 key **不**计入「已复查」。
    ///   进展性来自 `ManagerConfig::validate` 强制的 `admission_read_timeout <=
    ///   admission_sweep_deadline`：轮首一波读取必定在期限内返回（读到结果或单条超时 fail-closed），
    ///   因此**被截断的轮次游标至少前进 1**，游标 ≤ `len` 轮必然绕回一次。于是每个当前 key——包括
    ///   本轮已启动却被 deadline 取消者——最迟在一整轮遍历后重新排到轮首被**实际读取**，复查不通过
    ///   即 fail-closed 摘除；排序尾部不再有「永不被读」的饥饿。最坏收敛界 ≈
    ///   `ceil(len / 每轮前进数) × admission_recheck_interval`，默认 1024 条会话下**远大于** 5s
    ///   复查节拍，报告与运维都不得把节拍本身当作撤销上界。
    /// * 临界区不持任何管理器锁跨 await。
    pub async fn recheck_all_sessions(self: &Arc<Self>) -> usize {
        let keys = self.active_keys();
        let len = keys.len();
        if len == 0 {
            // 无会话：游标复位，避免残留偏移在下一次有会话时从错误位置起扫。
            self.recheck_cursor.store(0, Ordering::Release);
            return 0;
        }
        // 轮转起点：把 [offset..] 排到 [..offset] 之前，本轮优先覆盖上轮没扫到的尾部。
        let offset = self.recheck_cursor.load(Ordering::Acquire) % len;
        let mut ordered: Vec<[u8; 32]> = Vec::with_capacity(len);
        ordered.extend_from_slice(&keys[offset..]);
        ordered.extend_from_slice(&keys[..offset]);

        let per_read = self.config.admission_read_timeout;
        let concurrency = self.config.admission_recheck_concurrency.max(1);
        let deadline = self.config.admission_sweep_deadline;
        let mut children: JoinSet<usize> = JoinSet::new();
        let mut pending = std::collections::VecDeque::from(ordered);
        let mut revoked = 0usize;
        // 本轮**实际完成 join** 的 key 数：只按它推进游标（被取消/panic 的不计入，留待下轮）。
        let mut completed = 0usize;
        let work = async {
            while !pending.is_empty() || !children.is_empty() {
                while children.len() < concurrency {
                    let Some(public_key) = pending.pop_front() else {
                        break;
                    };
                    let manager = Arc::clone(self);
                    children.spawn(async move {
                        let admits = match tokio::time::timeout(
                            per_read,
                            manager.admission.load_current(public_key),
                        )
                        .await
                        {
                            Ok(Ok(decision)) => decision_admits(&decision),
                            // 超时或读不到真相：无法证明仍批准 → fail-closed。
                            _ => false,
                        };
                        usize::from(!admits && manager.apply_revoke(public_key).terminated_session)
                    });
                }
                match children.join_next().await {
                    Some(Ok(count)) => {
                        revoked += count;
                        completed += 1;
                    }
                    // 子任务 panic/被取消：该项**不计入 completed**（绝不把「没确认」当「已复查」），
                    // 游标因此不会越过它，下一轮必然重做。
                    Some(Err(_)) => continue,
                    None => continue,
                }
            }
            (revoked, completed)
        };
        match tokio::time::timeout(deadline, work).await {
            Ok((revoked, completed)) => {
                // 整轮自然完成：全部当前 key 都被复查过 → 游标复位，下一轮从头部起新一轮全扫。
                self.recheck_cursor.store(0, Ordering::Release);
                let _ = completed;
                revoked
            }
            Err(_) => {
                // 整轮超期：`JoinSet` 随 future 一起 drop，其中在途子任务被 abort（不等 SQL）。
                // 游标按本轮已完成数前进；乱序完成时可越过被取消的位置，
                // 成员稳定后仍会绕回并重新读取，不能据游标位置认定某 key 已完成。
                self.sweep_dropped.fetch_add(1, Ordering::AcqRel);
                let advance = completed.min(len);
                self.recheck_cursor
                    .store((offset + advance) % len, Ordering::Release);
                revoked
            }
        }
    }

    /// 轮询基础循环：按 [`PollScheduler`] 的相位均匀驱动当前会话的 `device.status.get`
    /// （写入 [`SnapshotStore`]），并在本代际尚无能力快照时补一次 `device.capabilities.get`。
    ///
    /// 复用既有调度器与快照 store，不新建第二套在线/新鲜度真源；在途请求有界，超出即把
    /// 调度器标为 overloaded 并本轮丢弃，不积压。
    pub fn spawn_poll_driver(
        self: &Arc<Self>,
        mut shutdown: watch::Receiver<bool>,
    ) -> JoinHandle<()> {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let period = manager.config.poll_period;
            let cadence = manager.config.poll_tick_cadence;
            let permits = Arc::new(tokio::sync::Semaphore::new(
                manager.config.max_concurrent_polls.max(1),
            ));
            let mut children: JoinSet<()> = JoinSet::new();
            let mut membership = manager.active_devices();
            let mut scheduler =
                PollScheduler::new(Instant::now(), period, cadence, membership.clone());
            // 两次 `due()` 之间的实际间隔必须**严格小于** `tick_cadence`：`PollScheduler`
            // 把超过 tick_cadence 的间隔判为「暂停」，只推进相位而不派发（防暂停后突发）。
            let nap = (cadence / 3).max(Duration::from_millis(1));
            loop {
                if *shutdown.borrow_and_update() {
                    break;
                }
                let now = Instant::now();
                let current = manager.active_devices();
                if current != membership {
                    // 只在成员变化时重建（重新均摊相位）；按固定周期无条件重建会让首台设备
                    // 的 `next_due = origin + period` 永远追不上重建，等于永不派发。
                    membership = current;
                    scheduler = PollScheduler::new(now, period, cadence, membership.clone());
                }
                scheduler.set_overloaded(permits.available_permits() == 0);
                for public_key in scheduler.due(now) {
                    let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                        // 在途已满：本轮丢弃，不排队积压（§6.4 中控侧全局预算）。
                        break;
                    };
                    let manager = Arc::clone(&manager);
                    children.spawn(async move {
                        manager.poll_device(public_key).await;
                        drop(permit);
                    });
                }
                tokio::select! {
                    _ = tokio::time::sleep(nap) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow_and_update() {
                            break;
                        }
                    }
                }
                // 收割已完成的子任务（同时把 panic 如实丢弃，绝不 spawn-and-forget）。
                while children.try_join_next().is_some() {}
            }
            // 退出：先关闭预算（不再派发），再**abort 并 join** 每一个在途 child。
            // `JoinSet::shutdown` 会 abort 全部任务并等待它们真正结束，因此本 owner 任务
            // 返回时保证「没有 child 还在发 capabilities/status 请求」（审查 P2-11）。
            scheduler.set_overloaded(true);
            permits.close();
            children.shutdown().await;
        })
    }

    /// 单台设备一次采样：状态（必做）+ 本代际首次能力补齐。
    async fn poll_device(&self, public_key: [u8; 32]) {
        let Some((generation, connection)) = self.current_connection(&public_key) else {
            return;
        };
        if connection.state() != ConnectionState::Ready {
            return;
        }
        let needs_capabilities = self.cached_capabilities(&public_key).is_none();
        let evidence = self.sample_evidence().await;
        let sample_at = Instant::now();
        // 审查 P1-6：**从采样开始到回包消费都绑定这一条连接**（`connection` 是本代际在
        // `register_ready` 时登记的句柄），绝不「capture 旧代际却按 key 动态路由到新连接、
        // 再把结果记到旧代际名下」。回包后 `ingest_*` 与 `request_on` 各自再核当代际。
        match self
            .request_on(
                &connection,
                "device.status.get",
                BusinessPayloadKind::DeviceStatus,
            )
            .await
        {
            Ok(BusinessPayload::DeviceStatus(status)) => {
                self.ingest_status_sample(&public_key, generation, &status, evidence, sample_at);
            }
            // 采集失败（RPC 失败/状态码非 0/payload 非本类型）：把失败投影进快照（补项），
            // 保留最后好样本与其年龄并附去敏错误码，而不是让 fresh 状态一直挂到 30s 过期。
            _ => {
                self.record_sample_error(&public_key, generation);
            }
        }
        // let-chain：只有本代际还没有能力快照时才补一次查询（不每轮重复问）。
        if needs_capabilities {
            match self
                .request_on(
                    &connection,
                    "device.capabilities.get",
                    BusinessPayloadKind::Capabilities,
                )
                .await
            {
                Ok(BusinessPayload::Capabilities(capabilities)) => {
                    self.ingest_capabilities(&public_key, generation, capabilities);
                }
                _ => {
                    self.record_sample_error(&public_key, generation);
                }
            }
        }
    }

    /// 关停：对所有活跃会话请求排空（h2 GOAWAY，板端重连到新进程），取消全部挂起握手。
    /// 不重试任何已发出的变更。
    pub fn shutdown_all(&self) -> usize {
        let (connections, released) = {
            let mut entries = self.write_entries();
            let mut connections: Vec<Arc<dyn DeviceConnection>> = Vec::new();
            let mut released = 0usize;
            for entry in entries.values_mut() {
                if let Some(active) = entry.current.as_ref() {
                    // 闸门与摘除同锁关闭（P2-12）。
                    active.event_gate.close();
                }
                if let Some(active) = entry.current.take() {
                    connections.push(active.connection);
                }
                // 名额按**实际移除的条目数**归还：已被 `cancel()` 过的凭证同样占着条目，
                // 只按 `cancel()` 翻转次数减必漏（审查 P2-10）。
                released += entry.pending.len();
                for signal in entry.pending.values() {
                    signal.cancel();
                }
                entry.pending.clear();
            }
            let keys: Vec<[u8; 32]> = entries.keys().copied().collect();
            for key in keys {
                entries.remove(&key);
            }
            if released > 0 {
                let previous = self.pending_slots.fetch_sub(released, Ordering::AcqRel);
                debug_assert!(previous >= released, "挂起握手续约不得下穿 0");
            }
            (connections, released)
        };
        // 释放写锁后再触碰连接：`SessionHandle` 的关停路径不得在管理器锁内执行。
        for connection in &connections {
            connection.graceful_shutdown();
        }
        let _ = released;
        connections.len()
    }

    // --- 内部辅助 ---------------------------------------------------------

    /// 一次 typed 业务调用 + 真实 codec 解码（恰好一次，不重试）。
    ///
    /// **请求体是真实编码的 [`Query`]**（审查 P1-1）：与已审
    /// [`TransportBoardClient`](crate::transport::board_client::TransportBoardClient)
    /// 的 `query()` 完全同源（`schema_version = BUSINESS_SCHEMA_VERSION`，经
    /// `encode_business` + codec witness），板端 `decode_business(Query)` 才会接受。
    /// 旧实现传 `Vec::new()` 会被真板端以缺字段拒绝（1003），seam 测试因忽略 payload 而没发现。
    async fn request_on(
        &self,
        connection: &Arc<dyn DeviceConnection>,
        action: &'static str,
        expected: BusinessPayloadKind,
    ) -> Result<BusinessPayload, BoardClientError> {
        let reply = connection
            .request(action, encode_query_payload()?)
            .await
            .map_err(BoardClientError::from)?;
        if reply.status_code != 0 {
            return Err(BoardClientError::Rejected {
                status: reply.status_code,
                reason_code: None,
            });
        }
        let witness = approved_codec_witness();
        decode_business(&BusinessCodec, Some(&witness), expected, &reply.payload)
            .map_err(|_| BoardClientError::ProtocolMismatch)
    }

    /// 发信令并按 §4.7 收尾：信令入队成功 → 单调 500ms 后切断；入队失败（连接已坏）→
    /// 立即切断。定时器只持有**这一份**句柄，因此不会误关后来的新代际。
    fn close_connection(&self, connection: &Arc<dyn DeviceConnection>, notice: SystemNotice) {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle,
            // 没有运行时（纯同步上下文）：不假装「稍后关闭」，立即切断，安全边界更靠前。
            Err(_) => {
                let _ = notice;
                self.signals_unsent.fetch_add(1, Ordering::AcqRel);
                connection.abort();
                return;
            }
        };
        let sent = Arc::clone(&self.signals_sent);
        let unsent = Arc::clone(&self.signals_unsent);
        let inflight = Arc::clone(&self.close_out_tasks);
        let connection = Arc::clone(connection);
        inflight.fetch_add(1, Ordering::AcqRel);
        let task = handle.spawn(async move {
            let delivered = connection.send_system(notice).await.is_ok();
            if delivered {
                sent.fetch_add(1, Ordering::AcqRel);
            } else {
                unsent.fetch_add(1, Ordering::AcqRel);
            }
            if delivered {
                // 单调延迟：给 owner 任务把已入队信令写出的窗口，然后强制切断。
                tokio::time::sleep(SYSTEM_SIGNAL_CLOSE_DELAY).await;
            }
            connection.abort();
            inflight.fetch_sub(1, Ordering::AcqRel);
        });
        // 收尾任务必须有 owner（审查 P2-11 同类要求）：句柄登记在管理器里，关停时由
        // `drain_close_outs` 逐个 await（超期才 abort），不是「原子计数当 join」。
        let mut handles = self
            .close_out_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        handles.retain(|handle| !handle.is_finished());
        handles.push(task);
    }

    /// 关停收尾：有界地 await 全部「信令 + 延迟切断」收尾任务，超期才 abort 并确认退出。
    ///
    /// 返回仍在途（被 abort）的数量；正常关停应为 0。
    pub async fn drain_close_outs(&self, grace: Duration) -> usize {
        let handles = std::mem::take(
            &mut *self
                .close_out_handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let deadline = tokio::time::Instant::now() + grace;
        let mut unfinished = 0usize;
        for mut handle in handles {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if tokio::time::timeout(remaining, &mut handle).await.is_err() {
                // 超期：显式 abort 后再 await 一次确认任务真的退出（不是「随进程一起丢」）。
                handle.abort();
                let _ = handle.await;
                unfinished += 1;
            }
        }
        unfinished
    }

    fn write_entries(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<[u8; 32], KeyEntry>> {
        self.entries
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read_entries(&self) -> std::sync::RwLockReadGuard<'_, HashMap<[u8; 32], KeyEntry>> {
        self.entries
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 既无当前会话也无挂起握手的条目直接删除，避免 map 随历史公钥无界增长。
    fn prune_entry(entries: &mut HashMap<[u8; 32], KeyEntry>, public_key: &[u8; 32]) {
        if entries
            .get(public_key)
            .is_some_and(|entry| entry.current.is_none() && entry.pending.is_empty())
        {
            entries.remove(public_key);
        }
    }

    fn active_keys(&self) -> Vec<[u8; 32]> {
        let mut keys: Vec<[u8; 32]> = self
            .read_entries()
            .iter()
            .filter(|(_, entry)| entry.current.is_some())
            .map(|(key, _)| *key)
            .collect();
        keys.sort_unstable();
        keys
    }

    fn active_count_locked(entries: &HashMap<[u8; 32], KeyEntry>) -> usize {
        entries
            .values()
            .filter(|entry| entry.current.is_some())
            .count()
    }
}

/// `register_ready` 内对被顶替旧会话的轻量引用。
struct ActiveSnapshot {
    generation: Uuid,
    connection: Arc<dyn DeviceConnection>,
    gate: Arc<EventGate>,
}

/// 板端事件（`device.task.result`）的投递闸门（审查 P2-12）。
///
/// * [`EventGate::close`] 只由管理器在**持有 `entries` 写锁**时调用（撤销、顶替、摘除），
///   因此「闸门已关闭」与「本代际不再是当前会话」不会互相越过；
/// * [`EventGate::claim`] 成功即占一个在途投递名额（`TaskResultContext` 的 `Drop` 归还）；
///   闸门关闭后不再发放新名额；
/// * 闸门**不是**第二套准入或健康真源：它只记录「这一份本地会话代际是否仍被管理器承认」，
///   判据始终来自 [`TransportManager`] 的当前代际。
struct EventGate {
    closed: AtomicBool,
    inflight: AtomicUsize,
}

impl EventGate {
    fn open() -> Self {
        Self {
            closed: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    #[must_use]
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// 闸门未关闭则占用一个在途名额。
    #[must_use]
    fn claim(&self) -> bool {
        if self.is_closed() {
            return false;
        }
        self.inflight.fetch_add(1, Ordering::AcqRel);
        if self.is_closed() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn release(&self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 交给 [`TaskResultSink`](crate::transport::listener::TaskResultSink) 的**本地会话上下文**。
///
/// 只携带中控本地产生、可核验的信息：已验签公钥 + 本地代际 + 事件闸门。**绝不**从 payload
/// 里的 deviceId/serial 取身份（审查 P2-12）。真正写任务的那一层必须在自己的写入边界调用
/// [`TaskResultContext::is_current`] 复核，才不会被旧会话或已撤销会话注入。
#[derive(Clone)]
pub struct TaskResultContext {
    manager: Arc<TransportManager>,
    public_key: [u8; 32],
    generation: Uuid,
    gate: Arc<EventGate>,
}

impl std::fmt::Debug for TaskResultContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskResultContext")
            .field("public_key", &"<verified 32B>")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl TaskResultContext {
    /// 已验签公钥（登记键）。
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// 中控本地产生的当前代际。
    #[must_use]
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    /// 本上下文是否**仍然**有效：代际仍是当前会话 **且** 事件闸门未关闭。
    ///
    /// 消费方必须在自己的写入边界调用它（一次 `claim` 只保证「取到上下文那一刻」有效）。
    #[must_use]
    pub fn is_current(&self) -> bool {
        if self.gate.is_closed() {
            return false;
        }
        self.manager
            .session_is_current(&self.public_key, self.generation)
    }
}

impl Drop for TaskResultContext {
    fn drop(&mut self) {
        // 归还在途名额（不取管理器锁：闸门内部只有原子计数，因此不会与管理器锁交叉）。
        self.gate.release();
    }
}

// ---------------------------------------------------------------------------
// DeviceRpc：只按当前已认证会话路由
// ---------------------------------------------------------------------------

impl DeviceRpc for TransportManager {
    fn request<'a>(
        &'a self,
        device: &'a [u8; 32],
        action: &'static str,
        payload: Vec<u8>,
    ) -> BoardFuture<'a, RpcReply> {
        Box::pin(async move {
            let Some((generation, connection)) = self.current_connection(device) else {
                // 无当前会话＝控制流不在线：如实上抛，不给常量 ready/在线。
                return Err(BoardClientError::DeviceOffline);
            };
            if connection.state() != ConnectionState::Ready {
                return Err(BoardClientError::DeviceOffline);
            }
            // 恰好一次：错误/超时都不重发（变更类结果未知时由上层意图链判定）。
            match connection.request(action, payload).await {
                Ok(reply) => {
                    // 审查 P1-6：跨 await 之后必须重新核验「这一份代际仍是当前会话、且这条
                    // 连接仍处于 ready」。等待期间被顶替/撤销/排空时，本次回包属于**已经不
                    // 存在的那条会话**，一律丢弃并回 typed 离线错误，绝不带着
                    // `note_activity == false` 照样把 reply 当成功返回给业务层，也不重试。
                    if !self.session_is_current(device, generation)
                        || connection.state() != ConnectionState::Ready
                    {
                        return Err(BoardClientError::DeviceOffline);
                    }
                    // 成功即当前认证代际的一次成功活动证据；旧代际回调会被拒。
                    self.note_activity(device, generation);
                    Ok(reply)
                }
                Err(error) => Err(error.into()),
            }
        })
    }
}

/// 轮询请求体：与已审 `board_client::query()` 同源的真实 `Query` 编码（P1-1）。
fn encode_query_payload() -> Result<Vec<u8>, BoardClientError> {
    let witness = approved_codec_witness();
    encode_business(
        &BusinessCodec,
        Some(&witness),
        &BusinessPayload::Query(Query {
            schema_version: BUSINESS_SCHEMA_VERSION,
        }),
    )
    .map_err(|_| BoardClientError::ProtocolMismatch)
}

// ---------------------------------------------------------------------------
// 管理面通知 sink：after_commit 只投递，撤销真相在 SQL
// ---------------------------------------------------------------------------

impl ManagementNotificationSink for TransportManager {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
        if snapshot_admits(&event.snapshot) {
            // 批准：既不缓存，也不据此终止既有会话（下一次握手由 SQL 复查放行）。
            return Ok(());
        }
        // 其余任何迁移（吊销、拒绝、重开、归档…）都不再允许既有会话继续存在。
        match self.revoke_tx.try_send(event.public_key) {
            Ok(()) => Ok(()),
            Err(_) => {
                // 通道满/已关闭：**不**假装通知已入队（调用方据此得到
                // `notification_queued=false`），并立刻唤醒一次全量复查兜底。
                self.revoke_dropped.fetch_add(1, Ordering::AcqRel);
                self.sweep.notify_one();
                Err(ControllerError::Config(
                    "admission notification queue unavailable; fail-closed sweep scheduled"
                        .to_string(),
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 实时视图 / 恢复模式
// ---------------------------------------------------------------------------

impl DeviceLiveProbe for TransportManager {
    fn snapshot<'a>(&'a self, id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
        Box::pin(async move {
            let Some((_generation, connection)) = self.current_connection(id) else {
                // 读取本身成功、设备确实不在线：如实 Offline，不伪造能力。
                return Ok(DeviceLiveView {
                    connection_state: DeviceConnectionState::Offline,
                    control_health: DeviceStreamHealth::Offline,
                    data_health: DeviceStreamHealth::Offline,
                    capabilities: None,
                });
            };
            let health = match connection.state() {
                ConnectionState::Ready => DeviceStreamHealth::Healthy,
                ConnectionState::Connecting => DeviceStreamHealth::Starting,
                ConnectionState::Draining => DeviceStreamHealth::Degraded,
                ConnectionState::Closed => DeviceStreamHealth::Offline,
            };
            Ok(DeviceLiveView {
                connection_state: DeviceConnectionState::Online,
                control_health: health,
                data_health: health,
                // typed 投影：只把已验签会话上取回的能力字段搬到 02 视图，不搬原始 wire。
                capabilities: self.cached_capabilities(id).and_then(project_capabilities),
            })
        })
    }
}

impl RecoveryModeProbe for TransportManager {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        // 恢复模式真源只有一个（启动门禁/恢复控制器）：本管理器只委托，不另造。
        self.recovery.current_mode()
    }
}

/// `Capabilities` 未派生 `Serialize`（wire 类型），因此显式 typed 投影成 02 视图 JSON。
fn project_capabilities(value: Capabilities) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "schema_version": value.schema_version,
        "boot_id": value.boot_id,
        "agent_version": value.agent_version,
        "supported_actions": value.supported_actions,
        "durable_task_journal": value.durable_task_journal,
        "max_status_bytes": value.max_status_bytes,
        "journal_epoch": value.journal_epoch,
    }))
}

// ---------------------------------------------------------------------------
// 握手策略包装：可取消的 SQL 准入
// ---------------------------------------------------------------------------

/// 把已审的 [`admission::SqlxAdmissionPolicy`](crate::transport::admission::SqlxAdmissionPolicy)（真实 SQL，零缓存）包成**可取消**的握手策略。
///
/// * 首次 `evaluate` 时按已验签公钥登记挂起握手（凭证 + 取消信号）；
/// * 每次 `evaluate` 都与「撤销/拒绝通知」竞争：取消后立即重读**当前 SQL 真相**，不再批准
///   就当场终止本连接（挂起握手因此不必等下一次 probe 才结束）；
/// * 批准前 SQL 已在临界区复查（撤销优先），本包装不引入任何缓存。
pub struct CancellableAdmissionPolicy {
    manager: Arc<TransportManager>,
    /// 被包装的**已审**准入策略。类型就是 `rsetup-tunnel` 的 trait 对象：生产由
    /// [`TransportConfig::admission_policy`](crate::transport::listener::TransportConfig::admission_policy)
    /// 注入 [`admission::SqlxAdmissionPolicy`](crate::transport::admission::SqlxAdmissionPolicy)（真实 SQL，零缓存），本模块不因此产生第二套准入判断。
    inner: Arc<dyn AdmissionPolicy>,
    bound: std::sync::Mutex<Option<PendingTicket>>,
}

impl CancellableAdmissionPolicy {
    #[must_use]
    pub fn new(manager: &Arc<TransportManager>, inner: Arc<dyn AdmissionPolicy>) -> Self {
        Self {
            manager: Arc::clone(manager),
            inner,
            bound: std::sync::Mutex::new(None),
        }
    }

    /// 握手结束后交出挂起凭证（`register_ready` 在同一把写锁内消费它）。
    pub fn into_pending(&self) -> Option<PendingTicket> {
        let mut guard = self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take()
    }

    /// 立即取消当前绑定的挂起握手（连接任务被取消时的显式路径）。
    pub fn cancel_current(&self) {
        let guard = self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ticket) = guard.as_ref() {
            ticket.cancel();
        }
    }

    fn bound_signal(&self, peer: &VerifiedPeer) -> Arc<CancelSignal> {
        let mut guard = self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_none() {
            let ticket = self.manager.track_pending(peer.public_key);
            let signal = Arc::clone(&ticket.signal);
            *guard = Some(ticket);
            return signal;
        }
        Arc::clone(&guard.as_ref().expect("刚绑定").signal)
    }
}

impl AdmissionPolicy for CancellableAdmissionPolicy {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        Box::pin(async move {
            let signal = self.bound_signal(peer);
            if signal.is_cancelled() {
                return recheck(&self.manager, peer).await;
            }
            let inner = self.inner.evaluate(peer);
            tokio::pin!(inner);
            tokio::select! {
                biased;
                () = signal.wait() => recheck(&self.manager, peer).await,
                decision = &mut inner => decision,
            }
        })
    }
}

/// 取消后的当场复核：读**当前** SQL 真相决定，绝不读内存 getter。
///
/// 真相入口就是 [`AdmissionTruth`]（生产 = 本模块自有的只读 SQL，含 `archived`），与握手
/// 首轮用的 [`classify_row`](crate::transport::admission::classify_row) 同一判定：归档行、
/// 非法组合、读失败一律 [`HandshakeError::Policy`] 失败关闭（审查 P1-8）。
/// 单次读取另有 `admission_read_timeout` 上界（P1-9），挂起的 SQL 不能把这条握手钉死。
async fn recheck(
    manager: &Arc<TransportManager>,
    peer: &VerifiedPeer,
) -> Result<AdmissionDecision, HandshakeError> {
    let budget = manager.config.admission_read_timeout;
    let decision = tokio::time::timeout(budget, manager.admission.load_current(peer.public_key))
        .await
        .map_err(|_| HandshakeError::Policy)?
        .map_err(|_| HandshakeError::Policy)?;
    match decision {
        AdmissionDecision::Approved => Ok(AdmissionDecision::Approved),
        AdmissionDecision::Rejected(reason) => Ok(AdmissionDecision::Rejected(reason)),
        // PENDING/none（例如审批被重开）：不能签任何终局原因，失败关闭让本连接结束、
        // 板端按 §7 退避后重新走完整握手与 probe。
        AdmissionDecision::Pending => Err(HandshakeError::Policy),
    }
}
