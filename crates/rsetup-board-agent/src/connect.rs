//! 生产主动出站连接循环：真实 TCP → v3 客户端握手（显式 pin）→ `SecureIo` →
//! `start_client_session` → ready / 业务 handler / 取消 / 退避重连 / 永久停止。
//!
//! 这里是本 crate 唯一建立网络的地方，装配顺序与失败分类都必须符合规范：
//!
//! * **每条 connection 全新代际**：新 TCP、完整 v3 握手（`client_handshake` 内部
//!   每次 fresh random + fresh X25519 临时密钥）、新 `connection_id`。票据绑定
//!   `connection_id`，因此旧连接遗留的票据/任务在新 connection 上必然失效。
//! * **信任只来自显式 pin**：`trusted_servers` 取自 [`BoardConnectConfig`] 的
//!   validated 构造（人工填入的中控公钥列表），无 TOFU、不读对端 key 扩信任。
//!   握手失败只按 `HandshakeError` 的固定去敏类别分流：**只有验签通过的**
//!   `REVOKED` / `APPROVAL_DENIED` 才停止自动重连（§4.6 第 4 条），
//!   坏签名 / 不受信公钥（未认证信息）绝不冒充「合法吊销」。
//! * **退避**：`1s, 2s, 4s … ≤60s` + `[0,1s)` CSPRNG 均匀抖动（§7 第 3 条）。
//!   只有本端两条流各自首 probe 成功（Ready）才清零失败计数；握手成功但没 Ready
//!   不算清零条件。
//! * **取消覆盖全阶段**：connect / 握手等待 / PENDING（无总审批期限，只能被 shutdown
//!   或库驱动的 probe 结清）/ 会话期。会话期先优雅排空（≥5s），期限用尽才 abort，
//!   最后**确认**驱动任务回收：确认 = 看到 join 返回成功，一次不成先 abort 再确认一次；
//!   仍不可确认就以 `Fault(DriverUnreclaimed)` 结束循环，绝不带着未回收的驱动再 open，
//!   也不把「只是 drop 了句柄」说成已 join（不留「重连双循环」）。
//! * **永久停止**：合法签名的 revoke/approval-denied 与会话内 `tunnel.revoke`
//!   都停止自动重连，并把「本地未授权」状态落盘（§7 第 4 条：恢复需管理员重新授权
//!   **+ 板端人工重置**）。退出策略必须是「不被 systemd 重启」：进程以 0 退出，
//!   `Restart=on-failure` 因此不会再拉起，也就不会绕过停止语义。
//! * **配置/状态故障不热 loop**：本端配置、身份密钥、journal、未授权标记读不到都是
//!   `AttemptFailure::Fault`，直接结束循环交给运维，不退避重试刷日志。

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_tunnel::handshake::{
    AuthenticatedRejection, ClientHandshakeConfig, HandshakeError, client_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::{
    SessionCloseReason, SessionConfig, SessionHandle, SessionHandler, SessionOutcome,
    start_client_session,
};
use tokio::net::TcpStream;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::connect_config::{BoardConnectConfig, ConnectConfigError};
use crate::session_handler::{BoardSessionHandler, BusinessDispatch, BusinessHandler};

/// TCP 建连（含 DNS）期限：与 §4.1 的 10s 握手步超时同级。
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 指数退避基数（§7 第 3 条）。
pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// 指数退避上限（§7 第 3 条）。
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// 退避抖动上界：每次附加 `[0, 1s)` 均匀随机抖动。
pub const BACKOFF_JITTER: Duration = Duration::from_secs(1);
/// 优雅排空期限：§6.6 至少 5s，只能加长不能缩短。
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 排空之外的宽限：用于「排空窗口没结清 → abort」与「确认驱动回收」两步的上界。
pub const JOIN_GRACE: Duration = Duration::from_secs(2);
/// 单个环境变量值长度预算（地址、公钥列表、路径、descriptor 字段）。
///
/// 这是生产入口新增的**入参预算**：`BoardConnectConfig` 已对地址与 pin 列表另有更严
/// 上限，本预算专门把路径类取值收在 4 KiB 内（远超任何真实文件系统限制），避免把
/// 任意长度的环境值继续交给 `IdentityKey` / `JournalStore`。symlink/owner/mode 校验
/// 仍由 `IdentityKey` 负责，本层不放宽也不替代。
pub const MAX_ENV_VALUE_BYTES: usize = 4096;

// ------------------------------------------------------------------ 类型化错误

/// 永久停止自动重连的原因（只可能来自**验签通过**的信息）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermanentStop {
    /// 中控签名 `REVOKED`（reason=2）或会话内 `tunnel.revoke`。
    Revoked,
    /// 中控签名 `APPROVAL_DENIED`（reason=3）。
    ApprovalDenied,
}

impl PermanentStop {
    /// 落盘 / 日志用的固定去敏标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::ApprovalDenied => "approval-denied",
        }
    }
}

/// 可重试失败的类别（去敏，不含地址/密钥/路径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryReason {
    /// 域名解析失败、无可用地址或解析超时。
    Resolve,
    /// TCP 建连失败或超时。
    Tcp,
    /// v3 握手未完成（传输/超时/协议违例/**未认证**的签名问题）。
    Handshake,
    /// 会话驱动启动失败。
    Session,
}

/// 本端故障：不可用退避重试掩盖，必须结束循环。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultReason {
    /// 配置缺失/非法（含环境值非 UTF-8 或过长、会话预算越界）。
    Config,
    /// 身份密钥不可用（0600/owner/symlink/格式）。
    Identity,
    /// journal 打不开或不可持久。
    Journal,
    /// 本地未授权标记存在，需人工重置（§7 第 4 条）。
    UnauthorizedState,
    /// 上一条会话的驱动任务在「排空 → abort → 再确认」后仍**无法确认已回收**。
    /// 带着未回收的驱动再 open 会形成两条循环/两份会话状态，只能停止交给运维。
    DriverUnreclaimed,
    /// 其它内部故障。
    Internal,
}

/// 一次建连尝试的失败分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptFailure {
    /// 退避后重试。
    Retryable(RetryReason),
    /// 合法签名导致的永久停止：不再自动重连。
    Permanent(PermanentStop),
    /// 本端故障：停止循环，交给 systemd/运维。
    Fault(FaultReason),
}

impl AttemptFailure {
    /// 是否属于「应退避重试」。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_))
    }
}

/// 会话结束原因（把驱动的 `SessionCloseReason` 收敛成板端策略需要的几类）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEndReason {
    /// 本端优雅排空（收到 shutdown 后主动 GOAWAY）。
    Drained,
    /// 对端优雅关闭（GOAWAY）。
    PeerGoaway,
    /// `tunnel.kick`：按退避重连（§7 第 4 条）。
    Kicked,
    /// `tunnel.revoke`：永久停止（§7 第 4 条）。
    Revoked,
    /// 传输/流控/探测升级等连接级故障。
    Transport,
    /// 协议违例。
    Protocol,
    /// 本端 abort（含 Indeterminate 主动中止）。
    Aborted,
}

/// 会话结束事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEnd {
    pub reason: SessionEndReason,
}

impl SessionEnd {
    /// 驱动结束原因 → 板端策略；不猜测、不合并成「成功」。
    pub fn from_close(reason: SessionCloseReason) -> Self {
        Self {
            reason: match reason {
                SessionCloseReason::Local => SessionEndReason::Drained,
                SessionCloseReason::PeerGoaway => SessionEndReason::PeerGoaway,
                SessionCloseReason::Kicked => SessionEndReason::Kicked,
                SessionCloseReason::Revoked => SessionEndReason::Revoked,
                SessionCloseReason::Abort => SessionEndReason::Aborted,
                SessionCloseReason::Transport | SessionCloseReason::StreamFlapping => {
                    SessionEndReason::Transport
                }
                SessionCloseReason::Protocol => SessionEndReason::Protocol,
            },
        }
    }
    fn aborted() -> Self {
        Self {
            reason: SessionEndReason::Aborted,
        }
    }
    fn transport() -> Self {
        Self {
            reason: SessionEndReason::Transport,
        }
    }
}

// ------------------------------------------------------------------ 关闭信号

/// 一次性关闭信号：`Clone` 廉价，`wait` 可被多处并发等待且**取消安全**
/// （先注册兴趣再检查标志，不存在「检查后才注册而漏掉 signal」的窗口）。
#[derive(Clone, Default)]
pub struct Shutdown {
    notify: Arc<Notify>,
    signalled: Arc<AtomicBool>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self::default()
    }
    /// 幂等：多次调用只生效一次。
    pub fn signal(&self) {
        if !self.signalled.swap(true, Ordering::AcqRel) {
            self.notify.notify_waiters();
        }
    }
    pub fn signalled(&self) -> bool {
        self.signalled.load(Ordering::Acquire)
    }
    /// 等待关闭；已关闭则立即返回。
    pub async fn wait(&self) {
        let notified = self.notify.notified();
        let mut notified = std::pin::pin!(notified);
        // 先登记兴趣，再读标志：任何时序下都不会漏掉 signal()。
        notified.as_mut().enable();
        if self.signalled.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

// ------------------------------------------------------------------ 退避

/// 纯函数退避：`failures >= 1`，第 n 次失败的基础延迟 `base << (n-1)`，上限 `cap`，
/// 再附加调用方给出的 `[0, BACKOFF_JITTER]` 抖动。
///
/// 单独成函数是为了让「指数、上限、抖动区间」可被穷尽断言，而不必真的等时间。
pub fn backoff_delay(failures: u32, base: Duration, cap: Duration, jitter: Duration) -> Duration {
    let exponent = failures.saturating_sub(1).min(31);
    let factor = 1u32.checked_shl(exponent).unwrap_or(u32::MAX);
    let doubled = base.saturating_mul(factor).min(cap);
    doubled + jitter.min(BACKOFF_JITTER)
}

/// 来自 OS CSPRNG 的 `[0, 1s)` 均匀抖动。
///
/// 熵源诚实声明：本 crate 的直接依赖里没有独立 CSPRNG helper，取
/// `uuid::Uuid::new_v4()`（内部走 `getrandom`，即 OS CSPRNG）的前 4 个**完全随机**字节
/// ——UUIDv4 的 version/variant 固定位落在第 7、9 字节，不影响前 4 字节。用
/// multiply-shift（Lemhain）映射到纳秒，避免取模偏差；亚秒抖动只需 30 bit 量级。
pub fn csprng_jitter() -> Duration {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let uniform = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let nanos = ((u128::from(uniform) * 1_000_000_000u128) >> 32) as u64;
    Duration::from_nanos(nanos)
}

/// 重连策略：期限与抖动源都可注入（生产用 [`ReconnectPolicy::system`]）。
///
/// 建连期限不在这里：它属于连接器的 I/O 行为（[`CONNECT_TIMEOUT`]），
/// 放在两处会出现两个事实来源。
#[derive(Clone)]
pub struct ReconnectPolicy {
    pub base: Duration,
    pub cap: Duration,
    /// 优雅排空期限；只允许加长到 `>= DRAIN_TIMEOUT`（§6.6）。
    pub drain: Duration,
    pub join_grace: Duration,
    jitter: Arc<dyn Fn() -> Duration + Send + Sync + 'static>,
}

impl std::fmt::Debug for ReconnectPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectPolicy")
            .field("base", &self.base)
            .field("cap", &self.cap)
            .field("drain", &self.drain)
            .field("join_grace", &self.join_grace)
            .finish_non_exhaustive()
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::system()
    }
}

impl ReconnectPolicy {
    /// 生产默认：1s…60s + CSPRNG 抖动，排空 5s。
    pub fn system() -> Self {
        Self {
            base: BACKOFF_BASE,
            cap: BACKOFF_CAP,
            drain: DRAIN_TIMEOUT,
            join_grace: JOIN_GRACE,
            jitter: Arc::new(csprng_jitter),
        }
    }
    /// 测试/受限环境：自定义基数与上限，抖动替换为确定性函数（**仍保留抖动加法结构**，
    /// 只是让断言可复现）。生产装配不用它。
    pub fn with_jitter(
        base: Duration,
        cap: Duration,
        jitter: impl Fn() -> Duration + Send + Sync + 'static,
    ) -> Self {
        Self {
            base,
            cap,
            drain: DRAIN_TIMEOUT,
            join_grace: JOIN_GRACE,
            jitter: Arc::new(jitter),
        }
    }
    /// 排空期限只允许加长（传入更短的值会被抬到 5s）。
    pub fn with_drain(mut self, drain: Duration) -> Self {
        self.drain = drain.max(DRAIN_TIMEOUT);
        self
    }
    /// 第 `failures` 次失败后应等待多久。
    pub fn delay_for(&self, failures: u32) -> Duration {
        backoff_delay(failures, self.base, self.cap, (self.jitter)())
    }
}

// ------------------------------------------------------------------ 会话缝

pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 一条**已建立**的会话：ready、结束事件、优雅排空、abort、驱动任务回收确认。
///
/// 抽成本 trait 是为了让「退避 / 停止 / 取消 / join」这套控制流能用替身穷尽断言；
/// 真实实现是 [`TunnelConnector`] 产出的 [`TunnelSession`]。
pub trait LiveSession: Send {
    /// 本端两条流各自首 probe 成功（§6.6 ready 判据）；失败即会话已结束。
    /// **无总审批期限**：PENDING 期间只能被 shutdown 或库驱动结清。
    fn wait_ready(&mut self) -> SessionFuture<'_, Result<(), SessionEnd>>;
    /// 会话结束（可并发、可重复等待，取消安全）。
    fn wait_end(&mut self) -> SessionFuture<'_, SessionEnd>;
    /// 请求优雅排空（幂等；client 恰好一条 GOAWAY）。
    fn request_drain(&self);
    /// 立即中止（不排空、不重放任何已发出的变更）。
    fn abort(&self);
    /// 确认驱动任务已回收（true = 期限内确认）。
    fn join(&mut self, deadline: Duration) -> SessionFuture<'_, bool>;
}

/// 排空 → （必要时）abort → 结清会话，返回最终结束原因。
async fn drain_and_wait(session: &mut dyn LiveSession, policy: &ReconnectPolicy) -> SessionEnd {
    session.request_drain();
    let window = policy.drain + policy.join_grace;
    match tokio::time::timeout(window, session.wait_end()).await {
        Ok(end) => end,
        Err(_) => {
            // 排空窗口用尽：立即 abort，再给一次结清宽限；绝不无限挂资源。
            session.abort();
            match tokio::time::timeout(policy.join_grace, session.wait_end()).await {
                Ok(end) => end,
                Err(_) => SessionEnd::aborted(),
            }
        }
    }
}

// ------------------------------------------------------------------ 连接缝

pub type OpenSessionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn LiveSession>, AttemptFailure>> + Send + 'a>>;

/// 建连注入点：每次调用都必须产出**全新代际**的会话。
pub trait BoardConnector: Send + Sync + 'static {
    /// 循环开始前的一次性检查（例如本地未授权标记）。Err(Fault) → 不进入循环。
    fn preflight(&self) -> Result<(), AttemptFailure> {
        Ok(())
    }
    /// 建立一条全新会话。
    fn open(&self, connection_id: ConnectionId) -> OpenSessionFuture<'_>;
    /// 永久停止时落盘「本地未授权」状态；返回是否已落盘（false 必须如实上报）。
    fn note_permanent_stop(&self, stop: PermanentStop) -> bool {
        let _ = stop;
        true
    }
}

/// 连接代际：每次 connection 新生成的 UUIDv4；票据与 reboot 调度按它绑定。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// 每次调用都新随机一个（绝不复用上一个 connection 的值）。
    pub fn new_v4() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 去敏的循环事件（供 bin 打日志、测试断言）；不含地址、公钥或路径。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectEventKind {
    /// 启动前检查失败（含本地未授权标记）。
    PreflightFailed,
    /// 开始一次建连尝试。
    AttemptStarted,
    /// 会话建立且本端双流首探成功。
    Ready,
    /// 会话结束。
    SessionEnded,
    /// 进入退避等待。
    Backoff,
    /// 永久停止（合法签名 revoke / approval-denied）。
    Stopped,
    /// 永久停止但本地未授权状态**未能落盘**（仍需人工介入）。
    StopStateUnwritten,
    /// 上一条会话的驱动任务未被确认回收（如实声明收尾结果，不声称已 join）。
    /// 会话结束原因本身已在 `SessionEnded` 里给出，这一条只补「任务回收不可确认」。
    DriverUnreclaimed,
    /// 本端故障。
    Fault,
    /// 收到 shutdown 后正常退出。
    Shutdown,
}

/// 循环事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectEvent {
    pub kind: ConnectEventKind,
    pub attempt: u32,
    pub stop: Option<PermanentStop>,
}

/// 事件观察器（日志）。
pub type Observer = Arc<dyn Fn(ConnectEvent) + Send + Sync + 'static>;

/// 静默观察器（测试与不需要日志的调用方）。
pub fn silent_observer() -> Observer {
    Arc::new(|_event| {})
}

/// 循环退出结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// 收到 shutdown：已按 §6.6 排空后退出。
    Shutdown,
    /// 合法签名导致的永久停止（`state_written` = 本地未授权标记是否落盘）。
    Stopped {
        stop: PermanentStop,
        state_written: bool,
    },
    /// 本端故障（不热 loop）。
    Fault(FaultReason),
}

impl RunOutcome {
    /// 进程退出码策略：
    /// * `Shutdown` 与 `Stopped` → **0**：`Restart=on-failure` 不会拉起，
    ///   因此「永久停止」不会被 systemd 的重启策略绕过（§4.6 第 4 条 / §7 第 4 条）；
    /// * `Fault` → 78：本端配置/状态故障，需要人工处理（unit 的 StartLimit 负责
    ///   阻止快速循环重启）。
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Shutdown | Self::Stopped { .. } => 0,
            Self::Fault(_) => 78,
        }
    }
}

/// 生产连接循环。
pub async fn run_board_connection(
    connector: Arc<dyn BoardConnector>,
    policy: ReconnectPolicy,
    shutdown: Shutdown,
    observer: Observer,
) -> RunOutcome {
    if let Err(failure) = connector.preflight() {
        return finish_with_failure(&connector, failure, 0, &observer);
    }
    // failures：连续「未达 Ready」的次数，只在 Ready 时清零（§7 第 3 条）。
    let mut failures: u32 = 0;
    loop {
        if shutdown.signalled() {
            emit(&observer, ConnectEventKind::Shutdown, failures, None);
            return RunOutcome::Shutdown;
        }
        let connection_id = ConnectionId::new_v4();
        emit(&observer, ConnectEventKind::AttemptStarted, failures, None);

        // 1) 建连 + 完整握手 + 会话启动：整体可被 shutdown 取消。
        let opened = tokio::select! {
            _ = shutdown.wait() => {
                emit(&observer, ConnectEventKind::Shutdown, failures, None);
                return RunOutcome::Shutdown;
            }
            result = connector.open(connection_id) => result,
        };
        let mut session = match opened {
            Ok(session) => session,
            Err(failure) if failure.is_retryable() => {
                failures = failures.saturating_add(1);
                let delay = policy.delay_for(failures);
                emit(&observer, ConnectEventKind::Backoff, failures, None);
                tokio::select! {
                    _ = shutdown.wait() => {
                        emit(&observer, ConnectEventKind::Shutdown, failures, None);
                        return RunOutcome::Shutdown;
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
                continue;
            }
            Err(failure) => {
                return finish_with_failure(&connector, failure, failures, &observer);
            }
        };

        // 2) 等 Ready：PENDING 无总审批期限，只能被 shutdown 取消。
        let ready = {
            let waiting = session.wait_ready();
            tokio::select! {
                _ = shutdown.wait() => None,
                result = waiting => Some(result),
            }
        };
        let end = match ready {
            Some(Ok(())) => {
                // 只有真正 Ready 才清零失败计数。
                failures = 0;
                emit(&observer, ConnectEventKind::Ready, 0, None);
                tokio::select! {
                    _ = shutdown.wait() => drain_and_wait(session.as_mut(), &policy).await,
                    end = session.wait_end() => end,
                }
            }
            Some(Err(end)) => end,
            // shutdown 抢在 Ready 之前：仍按 §6.6 优雅排空（GOAWAY）后退出。
            None => drain_and_wait(session.as_mut(), &policy).await,
        };
        emit(&observer, ConnectEventKind::SessionEnded, failures, None);

        // 3) 会话内 revoke：等价于合法签名的永久吊销，停止自动重连（§7 第 4 条）。
        if end.reason == SessionEndReason::Revoked {
            session.abort();
            // 收尾不可确认**不改变**停止方向：吊销下唯一安全的动作就是不再重连、落盘并
            // 以 0 退出（`Restart=on-failure` 因此无法绕过）。但绝不声称任务已回收：
            // 只 join 一次、失败就如实报 `DriverUnreclaimed`。
            if !session.join(policy.join_grace).await {
                emit(
                    &observer,
                    ConnectEventKind::DriverUnreclaimed,
                    failures,
                    None,
                );
            }
            return finish_with_failure(
                &connector,
                AttemptFailure::Permanent(PermanentStop::Revoked),
                failures,
                &observer,
            );
        }

        // 4) 无论走哪条路，都必须确认驱动任务已回收，绝不带着未回收任务开新循环：
        //    一次 join 不够就先 abort 打断，再给一次期限；两次都不确认就是本端故障，
        //    终止循环交给运维（继续 open 会得到两条循环与两份会话状态）。
        if !session.join(policy.join_grace).await {
            session.abort();
            if !session.join(policy.join_grace).await {
                emit(
                    &observer,
                    ConnectEventKind::DriverUnreclaimed,
                    failures,
                    None,
                );
                if shutdown.signalled() {
                    // 退出已由 shutdown 决定：进程本就要结束，退出码保持 0（不能让一次
                    // SIGTERM 变成 78 从而被 systemd 重新拉起），但收尾不可确认必须可见。
                    emit(&observer, ConnectEventKind::Shutdown, failures, None);
                    return RunOutcome::Shutdown;
                }
                return finish_with_failure(
                    &connector,
                    AttemptFailure::Fault(FaultReason::DriverUnreclaimed),
                    failures,
                    &observer,
                );
            }
        }
        if shutdown.signalled() {
            emit(&observer, ConnectEventKind::Shutdown, failures, None);
            return RunOutcome::Shutdown;
        }

        // 5) Kicked 与其它故障同样按 §7 退避重连（Kick 不是吊销）。
        failures = failures.saturating_add(1);
        let delay = policy.delay_for(failures);
        emit(&observer, ConnectEventKind::Backoff, failures, None);
        tokio::select! {
            _ = shutdown.wait() => {
                emit(&observer, ConnectEventKind::Shutdown, failures, None);
                return RunOutcome::Shutdown;
            }
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

fn emit(observer: &Observer, kind: ConnectEventKind, attempt: u32, stop: Option<PermanentStop>) {
    (observer)(ConnectEvent {
        kind,
        attempt,
        stop,
    });
}

/// 把失败分类落成循环退出结果（永久停止会先尝试落盘未授权状态）。
fn finish_with_failure(
    connector: &Arc<dyn BoardConnector>,
    failure: AttemptFailure,
    attempt: u32,
    observer: &Observer,
) -> RunOutcome {
    match failure {
        AttemptFailure::Permanent(stop) => {
            let written = connector.note_permanent_stop(stop);
            emit(
                observer,
                if written {
                    ConnectEventKind::Stopped
                } else {
                    ConnectEventKind::StopStateUnwritten
                },
                attempt,
                Some(stop),
            );
            RunOutcome::Stopped {
                stop,
                state_written: written,
            }
        }
        AttemptFailure::Fault(reason) => {
            emit(
                observer,
                match reason {
                    FaultReason::UnauthorizedState => ConnectEventKind::PreflightFailed,
                    _ => ConnectEventKind::Fault,
                },
                attempt,
                None,
            );
            RunOutcome::Fault(reason)
        }
        // 调用方保证可重试失败不会走到这里；真到就按内部故障停住，不热 loop。
        AttemptFailure::Retryable(_) => RunOutcome::Fault(FaultReason::Internal),
    }
}

// ------------------------------------------------------------------ 生产实现

/// 每条 connection 一个新的业务 handler（绑定握手验签公钥 + 本 connection 代际）。
pub trait HandlerFactory: Send + Sync + 'static {
    fn create(
        &self,
        authenticated_controller_key: [u8; 32],
        connection_id: &ConnectionId,
    ) -> Arc<BoardSessionHandler>;
}

/// 默认工厂：进程级共享一个 `BoardAgent`（同一 agent_epoch / sample_seq / journal 轴），
/// 只按 connection 换 handler 与代际。
pub struct SharedAgentFactory {
    dispatch: Arc<dyn BusinessDispatch>,
}

impl SharedAgentFactory {
    pub fn new(dispatch: Arc<dyn BusinessDispatch>) -> Self {
        Self { dispatch }
    }
}

impl HandlerFactory for SharedAgentFactory {
    fn create(
        &self,
        authenticated_controller_key: [u8; 32],
        connection_id: &ConnectionId,
    ) -> Arc<BoardSessionHandler> {
        BoardSessionHandler::new(Arc::new(BusinessHandler::new(
            self.dispatch.clone(),
            authenticated_controller_key,
            connection_id.as_str(),
        )))
    }
}

/// 「本地未授权」标记（§7 第 4 条）：只有管理员重新授权 **且** 人工删除本标记，
/// 板端才会再次自动重连。
#[derive(Clone, Debug)]
pub struct UnauthorizedState {
    path: PathBuf,
}

impl UnauthorizedState {
    /// 标记文件名固定，落在状态目录（身份文件父目录，0700）。
    pub const FILE_NAME: &'static str = "UNAUTHORIZED";
    /// 单行、去敏、可提交的值。
    const HEADER: &'static str = "rsetup-board-agent stopped automatic reconnect:";

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn in_state_dir(dir: impl AsRef<Path>) -> Self {
        Self::new(dir.as_ref().join(Self::FILE_NAME))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 检查：不存在 → Ok；存在（含符号链接、非常规文件、读取故障）→ 一律
    /// `Fault(UnauthorizedState)`：标记不可信即视为未授权，宁可停住等人工确认。
    pub fn check(&self) -> Result<(), AttemptFailure> {
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => Err(AttemptFailure::Fault(FaultReason::UnauthorizedState)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AttemptFailure::Fault(FaultReason::UnauthorizedState)),
        }
    }

    /// 落盘标记：`O_CREAT|O_EXCL` + 0600（O_EXCL 不会跟随既有符号链接）。
    /// 已存在视为目标状态已达成（幂等）；其它 I/O 故障返回 false，调用方必须如实上报。
    pub fn record(&self, stop: PermanentStop) -> bool {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let body = format!("{} {}\n", Self::HEADER, stop.label());
        let options = std::fs::OpenOptions::new();
        let mut options = options;
        options.write(true).create_new(true).mode(0o600);
        match options.open(&self.path) {
            Ok(mut file) => file.write_all(body.as_bytes()).is_ok(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => true,
            Err(_) => false,
        }
    }
}

/// 生产连接器：validated 配置 + **启动期载入一次**的身份密钥 + 设备描述符 +
/// handler 工厂 + 可选的本地未授权状态。
pub struct TunnelConnector {
    config: BoardConnectConfig,
    identity: Arc<IdentityKey>,
    descriptor: DeviceDescriptor,
    handlers: Arc<dyn HandlerFactory>,
    session_config: SessionConfig,
    unauthorized: Option<UnauthorizedState>,
    connect_timeout: Duration,
}

impl TunnelConnector {
    pub fn new(
        config: BoardConnectConfig,
        identity: Arc<IdentityKey>,
        descriptor: DeviceDescriptor,
        handlers: Arc<dyn HandlerFactory>,
    ) -> Self {
        Self {
            config,
            identity,
            descriptor,
            handlers,
            session_config: SessionConfig::default(),
            unauthorized: None,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }
    /// 打开「本地未授权」标记检查与落盘（生产入口必须调用）。
    pub fn with_unauthorized_state(mut self, state: UnauthorizedState) -> Self {
        self.unauthorized = Some(state);
        self
    }
    /// 会话预算覆盖：必须通过 `SessionConfig::validate`，越界归入本端故障。
    pub fn with_session_config(mut self, config: SessionConfig) -> Self {
        self.session_config = config;
        self
    }
    /// 覆盖建连期限（测试用；生产保持 10s）。
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
    /// 本端身份公钥（运维核对用；不是秘密）。
    pub fn identity_public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    async fn open_inner(
        &self,
        connection_id: ConnectionId,
    ) -> Result<Box<dyn LiveSession>, AttemptFailure> {
        // 1) DNS + TCP：有界建连，TCP_NODELAY（§6.1 建议）。
        let addresses = match tokio::time::timeout(
            self.connect_timeout,
            resolve_all(self.config.controller_addr.clone()),
        )
        .await
        {
            Ok(Ok(addresses)) => addresses,
            // 解析失败与解析超时都属于「现在连不上」，可退避重试。
            Ok(Err(())) | Err(_) => {
                return Err(AttemptFailure::Retryable(RetryReason::Resolve));
            }
        };
        let address = addresses
            .first()
            .copied()
            .ok_or(AttemptFailure::Retryable(RetryReason::Resolve))?;
        let stream = tokio::time::timeout(self.connect_timeout, TcpStream::connect(address))
            .await
            .map_err(|_| AttemptFailure::Retryable(RetryReason::Tcp))?
            .map_err(|_| AttemptFailure::Retryable(RetryReason::Tcp))?;
        // NODELAY 失败不致命（个别地址族不支持），不作为失败理由。
        let _ = stream.set_nodelay(true);

        // 2) 完整 v3 客户端握手：identity 来自启动期载入的一次；random 与 X25519
        //    临时密钥由握手库每次 fresh 生成；信任面只有显式 pin 列表（无 TOFU）。
        let handshake = ClientHandshakeConfig {
            identity: Arc::clone(&self.identity),
            trusted_servers: self.config.trusted_servers.clone(),
            descriptor: self.descriptor.clone(),
        };
        let result = client_handshake(stream, &handshake)
            .await
            .map_err(classify_handshake)?;

        // 3) 记录层 → 会话驱动（板端 = client 角色）。
        let secure = SecureIo::new(result.io, result.session_keys.into_client_record_layers());
        // 对端身份只取**已验签**结果；业务 handler 绑定的就是它，packet 无权声明身份。
        let handler = self.handlers.create(result.peer_public_key, &connection_id);
        let binding = handler.binding();
        let session_handler: Arc<dyn SessionHandler> = handler;
        let (handle, driver) = start_client_session(secure, session_handler, self.session_config)
            .map_err(|_| AttemptFailure::Retryable(RetryReason::Session))?;
        // 绑定失败说明 handler 实例被复用（代际错误）：立即 abort，不留第二条循环。
        if !binding.attach(handle.clone()) {
            handle.abort();
            return Err(AttemptFailure::Fault(FaultReason::Internal));
        }
        Ok(Box::new(TunnelSession::new(handle, driver)))
    }
}

/// 握手错误 → 失败分类：**只有验签通过的** REVOKED / APPROVAL_DENIED 才是永久停止。
///
/// 未认证信息（`UntrustedServer` / `ClientSignature`）与超时、传输错误都可重试：
/// 把「坏签名」当成吊销会让攻击者（或一次中间人）永久停住板端，规范明确禁止。
pub fn classify_handshake(error: HandshakeError) -> AttemptFailure {
    match error {
        HandshakeError::SignedRejection { reason } => match reason {
            AuthenticatedRejection::Revoked => AttemptFailure::Permanent(PermanentStop::Revoked),
            AuthenticatedRejection::ApprovalDenied => {
                AttemptFailure::Permanent(PermanentStop::ApprovalDenied)
            }
            // 合法签名但可重试（握手超时/服务端过载/未说明）→ 退避。
            _ => AttemptFailure::Retryable(RetryReason::Handshake),
        },
        // 没配可信公钥属于本端配置故障（禁止 TOFU），不是网络问题。
        HandshakeError::NoTrustedServers => AttemptFailure::Fault(FaultReason::Config),
        // 其余（含 UntrustedServer / ClientSignature / 超时 / 传输）都可重试。
        _ => AttemptFailure::Retryable(RetryReason::Handshake),
    }
}

impl BoardConnector for TunnelConnector {
    fn preflight(&self) -> Result<(), AttemptFailure> {
        // 配置必须已经通过 validated 入口；这里只再确认 pin 列表非空与预算合法。
        if self.config.trusted_servers.is_empty() {
            return Err(AttemptFailure::Fault(FaultReason::Config));
        }
        if let Some(state) = &self.unauthorized {
            state.check()?;
        }
        // 会话预算越界属于本端故障（不允许「配错就静默放宽」）。
        self.session_config
            .validate()
            .map_err(|_| AttemptFailure::Fault(FaultReason::Config))?;
        Ok(())
    }

    fn open(&self, connection_id: ConnectionId) -> OpenSessionFuture<'_> {
        Box::pin(self.open_inner(connection_id))
    }

    fn note_permanent_stop(&self, stop: PermanentStop) -> bool {
        match &self.unauthorized {
            Some(state) => state.record(stop),
            None => false,
        }
    }
}

/// 解析地址（保留有界数量，避免对端 DNS 记录无限膨胀影响本端内存）。
async fn resolve_all(address: String) -> Result<Vec<SocketAddr>, ()> {
    let mut addresses = Vec::new();
    let resolved = tokio::net::lookup_host(address).await.map_err(|_| ())?;
    for address in resolved {
        addresses.push(address);
        if addresses.len() >= 16 {
            break;
        }
    }
    Ok(addresses)
}

/// 等待 watch 通道里的结束事件（取消安全、可并发）。
async fn wait_ended(ended: &mut watch::Receiver<Option<SessionEnd>>) -> SessionEnd {
    loop {
        if let Some(end) = *ended.borrow_and_update() {
            return end;
        }
        if ended.changed().await.is_err() {
            return SessionEnd::aborted();
        }
    }
}

/// [`LiveSession`] 的真实实现：`SessionHandle` + 驱动任务的结束转发。
pub struct TunnelSession {
    handle: SessionHandle,
    /// 结束事件的唯一来源：转发任务 await 驱动 `JoinHandle` 后写入 watch。
    /// 用 watch 是为了让 `wait_end` 可并发、可重复且取消安全。
    ended: watch::Receiver<Option<SessionEnd>>,
    forwarder: JoinHandle<()>,
}

impl TunnelSession {
    fn new(handle: SessionHandle, driver: JoinHandle<SessionOutcome>) -> Self {
        let (sender, receiver) = watch::channel(None);
        let forwarder = tokio::spawn(async move {
            let end = match driver.await {
                Ok(outcome) => SessionEnd::from_close(outcome.reason),
                // 驱动任务 panic / 被取消：按连接级故障结清，不猜原因。
                Err(_) => SessionEnd::transport(),
            };
            let _ = sender.send(Some(end));
        });
        Self {
            handle,
            ended: receiver,
            forwarder,
        }
    }
}

impl LiveSession for TunnelSession {
    fn wait_ready(&mut self) -> SessionFuture<'_, Result<(), SessionEnd>> {
        let handle = self.handle.clone();
        let mut ended = self.ended.clone();
        Box::pin(async move {
            tokio::select! {
                result = handle.wait_ready() => {
                    result.map_err(|_| {
                        // 句柄侧失败：若驱动已给出原因就用它，否则按传输故障结清。
                        ended.borrow_and_update().as_ref().copied().unwrap_or_else(SessionEnd::transport)
                    })
                }
                end = wait_ended(&mut ended) => Err(end),
            }
        })
    }

    fn wait_end(&mut self) -> SessionFuture<'_, SessionEnd> {
        let mut ended = self.ended.clone();
        Box::pin(async move { wait_ended(&mut ended).await })
    }

    fn request_drain(&self) {
        self.handle.graceful_shutdown();
    }

    fn abort(&self) {
        self.handle.abort();
    }

    fn join(&mut self, deadline: Duration) -> SessionFuture<'_, bool> {
        // 转发任务只在驱动任务结束后写入事件并退出：它结束即代表驱动已回收。
        let forwarder = &self.forwarder;
        Box::pin(async move {
            tokio::time::timeout(deadline, async {
                while !forwarder.is_finished() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .is_ok()
        })
    }
}

// ------------------------------------------------------------------ 环境读取

/// 生产环境读取错误：区分「未设置」「非 UTF-8」「过长」「取值非法」。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EnvConfigError {
    /// `VarError::NotUnicode`：**绝不**当成「未设置」回落默认值。
    #[error("environment variable is not valid UTF-8")]
    NotUnicode,
    #[error("environment variable exceeds the length budget")]
    TooLong,
    #[error("connection configuration is invalid")]
    Invalid(#[from] ConnectConfigError),
}

/// 读取单个环境值：三分支显式处理，**不能**用 `env::var(key).ok()`
/// 把非 UTF-8 悄悄当成未设置（那会让一个损坏的 state path 静默回落到默认值）。
fn read_env(key: &str) -> Result<Option<String>, EnvConfigError> {
    match std::env::var(key) {
        Ok(value) => {
            if value.len() > MAX_ENV_VALUE_BYTES {
                Err(EnvConfigError::TooLong)
            } else {
                Ok(Some(value))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(EnvConfigError::NotUnicode),
    }
}

/// 生产入口的配置读取：必须走 validated 的 `from_lookup`，并叠加长度预算。
///
/// 「缺必填项」（含缺 pin → `ConnectConfigError::MissingPins`）由 validated 构造本身
/// 报告，本层不伪造同义变体；返回的错误只带类别，不回显任何环境值内容。
pub fn board_connect_config_from_env() -> Result<BoardConnectConfig, EnvConfigError> {
    // 用 Cell 收集「本端环境问题」，优先于「取值非法」：两者都需人工处理，
    // 但前者如果不报出来就会被 from_lookup 的默认值掩盖。
    let failure: std::cell::RefCell<Option<EnvConfigError>> = std::cell::RefCell::new(None);
    let mut lookup = |key: &str| -> Option<String> {
        match read_env(key) {
            Ok(value) => value,
            Err(error) => {
                let mut slot = failure.borrow_mut();
                if slot.is_none() {
                    *slot = Some(error);
                }
                None
            }
        }
    };
    let config = BoardConnectConfig::from_lookup(&mut lookup);
    if let Some(error) = failure.into_inner() {
        return Err(error);
    }
    Ok(config?)
}

/// 设备描述符：只取**显式本机配置**，缺字段就是空串（proto3 缺省语义），
/// 绝不读随机值或硬件指纹冒认设备身份。
pub fn descriptor_from_env() -> Result<DeviceDescriptor, EnvConfigError> {
    let field =
        |key: &str| -> Result<String, EnvConfigError> { Ok(read_env(key)?.unwrap_or_default()) };
    Ok(DeviceDescriptor {
        device_sn: field("RSETUP_BOARD_DEVICE_SN")?,
        device_model: field("RSETUP_BOARD_DEVICE_MODEL")?,
        fw_version: field("RSETUP_BOARD_FW_VERSION")?,
        extra_info: field("RSETUP_BOARD_EXTRA_INFO")?,
    })
}

/// 状态目录：身份文件的父目录（unit 里 `StateDirectory` 建的 0700 目录）。
pub fn state_dir_of(identity_file: &Path) -> PathBuf {
    identity_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}
