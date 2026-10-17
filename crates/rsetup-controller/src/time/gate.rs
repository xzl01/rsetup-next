//! 时间门禁（Runtime Task 1b/1c）：启动总预算采样、`TimeGateHandle` 就绪等待与
//! 原子时间快照，以及后台 `TimeGateDriver`（源列表重读、stale 复验、单调退避、
//! 跳钟失效、脱敏审计、线程安全通知）。
//!
//! driver 的全部外部交互（采样、源列表、墙钟、等待、审计）均为可注入抽象，
//! 受控测试不访问真实网络、不依赖真实 30s/60s 等待。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use tokio::sync::Notify;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::time::evidence::{TimeEvidence, TimeQuality};
use crate::time::socket::{NtpSample, SntpBindFuture, SntpResolver, TokioSntpResolver};

/// 启动总预算：00 §3 C-TIME 已确认约束，硬上限 30s。
pub const NTP_STARTUP_BUDGET: Duration = Duration::from_secs(30);
/// 单次请求默认超时（05 §2 待审建议默认）。
pub const DEFAULT_NTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// 单次请求超时上限（05 §2 建议“单次请求上限 3s”；本计划明确采用该建议作为实现策略）。
pub const NTP_SINGLE_REQUEST_TIMEOUT_CAP: Duration = Duration::from_secs(3);
/// 本地单调 RTT 上限默认值（05 §4 建议；本计划选定策略）。
pub const DEFAULT_NTP_MAX_RTT_MS: i64 = 2000;
/// 后台退避下限。
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// 后台退避上限。
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// 周期复验间隔默认值。
pub const DEFAULT_REVALIDATE_INTERVAL: Duration = Duration::from_secs(60);
/// 证据时效上限默认值（毫秒）。
pub const DEFAULT_MAX_EVIDENCE_AGE_MS: u64 = 300_000;
/// 就绪等待上限（05 §1：最长 ≤30s 等待就绪；`wait_ready` 超时兜底返回当前快照）。
pub const READY_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 墙钟回跳失效阈值（契约 4，默认 2s）：`last_wall - now > JUMP_BACK_EPS` 即判定跳钟。
pub const JUMP_BACK_EPS: Duration = Duration::from_secs(2);
/// 墙钟前跳失效阈值（契约 4，默认 1h）：`now - last_wall > JUMP_FORWARD_MAX` 即判定跳钟。
pub const JUMP_FORWARD_MAX: Duration = Duration::from_secs(3_600);
/// 同源参考阶跃阈值（RT-R12-2 / RT-R13-2）：与单调外推期望参考偏差 > 1s 判定不连续阶跃校正 → 换代。
pub const CLOCK_STEP_MAX_MS: i64 = 1_000;

/// 时间门禁配置。
#[derive(Clone, Debug)]
pub struct TimeGateConfig {
    pub servers: Vec<String>,
    pub request_timeout: Duration,
    pub startup_budget: Duration,
    pub max_rtt_ms: i64,
    /// 周期复验间隔：消费点 = `TimeGateDriver` NtpValid/Stale 分支的 tick 等待（契约 2，T1c）。
    /// 默认 `DEFAULT_REVALIDATE_INTERVAL`（60s）；合法性：必须 > 0。
    pub revalidate_interval: Duration,
    /// 证据时效上限（ms）：消费点 = `TimeGateDriver::evidence_stale()`（契约 2/4，T1c）与
    /// `TimeGateHandle::current()` 动态降级，`sample_age_ms > max_evidence_age_ms` 判定 stale。
    /// 默认 `DEFAULT_MAX_EVIDENCE_AGE_MS`（300000）；合法性：必须 > 0。
    pub max_evidence_age_ms: u64,
}

impl Default for TimeGateConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            request_timeout: DEFAULT_NTP_REQUEST_TIMEOUT,
            startup_budget: NTP_STARTUP_BUDGET,
            max_rtt_ms: DEFAULT_NTP_MAX_RTT_MS,
            revalidate_interval: DEFAULT_REVALIDATE_INTERVAL,
            max_evidence_age_ms: DEFAULT_MAX_EVIDENCE_AGE_MS,
        }
    }
}

impl TimeGateConfig {
    /// 合法性校验：`start_time_gate` 启动前调用一次，失败即拒绝启动（与 `request_timeout` 3s 上限校验同级）。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revalidate_interval.is_zero() {
            return Err("revalidate_interval 必须 > 0");
        }
        if self.max_evidence_age_ms == 0 {
            return Err("max_evidence_age_ms 必须 > 0");
        }
        Ok(())
    }
}

/// 门禁是否应关闭：关联成功可提前关闭；预算到期（`>=`）强制关闭。
pub fn gate_should_close(elapsed: Duration, budget: Duration, correlated: bool) -> bool {
    correlated || elapsed >= budget
}

/// 后台重试退避：`min(BACKOFF_MIN << attempt.min(6), BACKOFF_MAX)`，单调不降、有上限。
pub fn next_backoff(attempt: u32) -> Duration {
    (BACKOFF_MIN * (1u32 << attempt.min(6))).min(BACKOFF_MAX)
}

/// 单次请求超时选择：`min(configured, 05 §2 建议 3s 上限, 剩余总预算)`。
/// 规格 05 §2 原文“建议单次请求上限3s”是建议、非强制契约；本计划明确采用该建议作为实现策略：
/// configured > 3s 不放大突破上限、剩余 < 3s 用剩余、configured < 3s 不放大。
pub fn select_request_timeout(configured: Duration, remaining: Duration) -> Duration {
    configured
        .min(NTP_SINGLE_REQUEST_TIMEOUT_CAP)
        .min(remaining)
}

/// RT-R12-1（单元 1）：原子时间快照——时间证据与其配对的单调采样锚点由同一把 RwLock 保护，
/// 读侧在同一读锁内原子取回、写侧在同一写锁内原子写入，杜绝双锁分读时“旧 reference 配新锚点”
/// （或反之）的不一致组合；handle 与 driver（T1c）共享同一 `Arc<RwLock<TimeGateSnapshot>>`。
#[derive(Clone)]
pub(crate) struct TimeGateSnapshot {
    pub evidence: TimeEvidence,
    pub sample_instant: Option<tokio::time::Instant>,
}

#[derive(Clone)]
pub struct TimeGateHandle {
    pub(crate) snapshot: Arc<RwLock<TimeGateSnapshot>>,
    pub(crate) ready: Arc<Notify>,
    pub(crate) is_ready: Arc<AtomicBool>,
    /// 退避尝试计数：由 `TimeGateDriver` 写入（契约 3），`retry_attempt()` 只读观测。
    pub(crate) retry_attempt: Arc<AtomicU32>,
    pub(crate) retry_active: Arc<AtomicBool>,
    /// RT-R11-4（单元 B）：由 `start_time_gate` 从 `config.max_evidence_age_ms` 注入的配置阈值；
    /// `current()` 据此动态判定降级（自定义阈值如 180s 与默认 300s 均即时生效，不再固定取默认值）。
    max_evidence_age_ms: u64,
    /// RT-N4 就绪等待上限：默认 `READY_WAIT_TIMEOUT`（30s）；受控测试用 `with_ready_timeout`
    /// 注入短超时（本仓库 tokio 未启用 `test-util`，无法 pause/advance 虚拟时钟）。
    ready_timeout: Duration,
}

impl TimeGateHandle {
    /// 构造初始门禁句柄：`Stale`（不可信）且未 ready，`retry_active = true`。
    /// 不 spawn 任何任务；启动采样与后台驱动由 `start_time_gate`/`TimeGateDriver` 承接。
    pub fn new(config: &TimeGateConfig, clock_epoch: Uuid) -> Self {
        let now = Utc::now();
        Self {
            snapshot: Arc::new(RwLock::new(TimeGateSnapshot {
                // RT-R12-1：证据与锚点合并为单一原子快照；RT-R8-1：尚无采样成功，锚点为空。
                evidence: TimeEvidence {
                    system_wall_utc: now,
                    reference_utc: now,
                    quality: TimeQuality::Stale,
                    clock_epoch,
                    source: None,
                    sample_age_ms: None,
                    offset_ms: None,
                    uncertainty_ms: None,
                },
                sample_instant: None,
            })),
            ready: Arc::new(Notify::new()),
            // 初始未 ready：杜绝“启动期间 DNS/采样在途时立即拿到 Stale 直接退出”。
            is_ready: Arc::new(AtomicBool::new(false)),
            retry_attempt: Arc::new(AtomicU32::new(0)),
            // 启动采样在途/失败待后台重试期间 retry_active = true；取得 NtpValid 样本后复位。
            retry_active: Arc::new(AtomicBool::new(true)),
            // RT-R11-4（单元 B）：注入配置阈值，current() 据此动态判定降级。
            max_evidence_age_ms: config.max_evidence_age_ms,
            ready_timeout: READY_WAIT_TIMEOUT,
        }
    }

    /// 覆盖就绪等待上限（仅受控测试与本地装配使用；生产默认 30s 不变）。
    #[must_use]
    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    /// 当前是否已转 ready（driver 取得首个合格样本后置位，不回退）。
    pub fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst)
    }

    /// 当前后台退避尝试计数（driver 写入；失效/成功时归零）。
    pub fn retry_attempt(&self) -> u32 {
        self.retry_attempt.load(Ordering::SeqCst)
    }

    /// 已 ready 立即返回快照；否则等待 ready 通知，最长 `ready_timeout`（默认 30s）超时兜底返回当前快照。
    pub async fn wait_ready(&self) -> TimeEvidence {
        // RT-R7-4：修复检查-注册竞态——tokio 的 Notified 首次 poll 才入队，且 Notify 不为未来的
        // waiter 保留 permit；若 store(true) + notify_waiters() 恰在 load(false) 之后、监听器入队
        // 之前发生，通知即丢失，调用方死等满 30s。故每轮必须先创建监听器并 enable() 立即注册，
        // 再复查 is_ready。超时（Err 分支被 `let _ =` 吸收）不 panic，落到下方返回当前快照兜底。
        let _ = tokio::time::timeout(self.ready_timeout, async {
            loop {
                // 先创建通知监听器并注册，防止通知在检查后发出而丢失
                let notified = self.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                // 一进门已 ready 则立即 break 返回快照（耗时 0）；未 ready 则挂上监听再查状态
                if self.is_ready.load(Ordering::SeqCst) {
                    break;
                }
                notified.await;
            }
        })
        .await;
        self.current().await
    }

    /// RT-R8-1：sample_age_ms 不冻结在写入时刻——读取时按采样单调锚点动态派生
    /// （快照存储 age + 锚点经过时间），使读取与 stale 判定看到的 age 随时间推进；
    /// 无锚点（从未采样成功）时原样返回快照。
    /// RT-R10-2 / RT-R11-4（单元 B）：派生 age 超过 `self.max_evidence_age_ms`（由
    /// `start_time_gate` 从 `config.max_evidence_age_ms` 注入）且快照为 `NtpValid` 时，
    /// 读取侧即时把返回值降级为 `SystemFallback`；只降级返回值、不回写快照，持久降级、
    /// attempt 复位与审计由 driver 契约 2（T1c）承接。
    pub async fn current(&self) -> TimeEvidence {
        // RT-R12-1：evidence 与 sample_instant 在同一读锁内原子克隆，派生 age 必对该证据配对的
        // 锚点计算，杜绝旧 reference 配新锚点的竞态。
        let snap = self.snapshot.read().await.clone();
        let mut evidence = snap.evidence;
        if let Some(instant) = snap.sample_instant {
            let elapsed_ms = tokio::time::Instant::now()
                .duration_since(instant)
                .as_millis() as u64;
            let derived_age_ms = evidence
                .sample_age_ms
                .unwrap_or(0)
                .saturating_add(elapsed_ms);
            evidence.sample_age_ms = Some(derived_age_ms);
            if derived_age_ms > self.max_evidence_age_ms
                && matches!(evidence.quality, TimeQuality::NtpValid)
            {
                evidence.quality = TimeQuality::SystemFallback;
            }
        }
        // RT-R12-3（05 规格 L26–30）：快照质量处于 SystemFallback/Stale，或刚被动态降级为
        // SystemFallback 时，把返回值刷新为当前系统墙钟，杜绝时间冻结与排队寿命提前消耗。
        if matches!(
            evidence.quality,
            TimeQuality::SystemFallback | TimeQuality::Stale
        ) {
            let now_wall = Utc::now();
            evidence.system_wall_utc = now_wall;
            evidence.reference_utc = now_wall;
        }
        evidence
    }

    pub fn background_retry_active(&self) -> bool {
        self.retry_active.load(Ordering::SeqCst)
    }
}

/// 启动时间门禁（每次进程启动只调用一次）：构造生产依赖装配后交给 `start_time_gate_with`。
///
/// 初始 quality 为 `Stale`（不可信）且未 ready；spawn `TimeGateDriver`，driver 先做一轮
/// 启动采样（契约 8：成功 → `NtpValid` + ready；`None` → `SystemFallback` 并进入退避驱动），
/// 随后进入后台循环（源列表重读、周期/stale 复验、单调退避、跳钟失效、脱敏审计）。
/// 必须在 tokio 运行时内调用（内部 `tokio::spawn`）。
/// 注意：startup gate 结束只表示 NTP 门结束，不替代 DB/bootstrap/恢复的 ready（05 §1）。
pub fn start_time_gate(config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle {
    let deps = TimeGateDeps::production(&config);
    start_time_gate_with(config, clock_epoch, deps)
}

/// `start_time_gate` 的可注入装配（受控测试注入 fake 采样/源列表/墙钟/等待/审计）。
/// 合法性校验失败即拒绝启动（与 `request_timeout` 3s 上限校验同级）。
pub fn start_time_gate_with(
    config: TimeGateConfig,
    clock_epoch: Uuid,
    deps: TimeGateDeps,
) -> TimeGateHandle {
    config
        .validate()
        .expect("TimeGateConfig 非法，拒绝启动时间门禁");
    let handle = TimeGateHandle::new(&config, clock_epoch);
    let driver = TimeGateDriver::new(&handle, config, deps);
    tokio::spawn(driver.run());
    handle
}

/// 64位唯一发送时戳/nonce：当前墙钟纳秒与进程内唯一序列混合，实践中不重复
/// （与 `query_sntp` 的 request_nonce 同参）。
fn startup_nonce() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ SEQ.fetch_add(0xBF58_476D_1CE4_E5B9, Ordering::Relaxed)
}

/// 启动 NTP 总预算采样（启动路径只调用一次；后台重读/重试是其他任务）：
/// 绝对 deadline 只创建一次 = `now + min(budget, NTP_STARTUP_BUDGET)`（硬 ≤30s），
/// socket bind、DNS、多源与全部重试都在同一个 `timeout_at(deadline, ...)` 内完成；
/// 每次请求超时由 `select_request_timeout(request_timeout, remaining)` 选定
/// = min(configured, 05 §2 建议 3s 上限, 剩余预算)，不按源/阶段重建 deadline；
/// 空源或一轮全部失败有界等待至 deadline（唯一 await，不忙循环）。
/// 成功返回 `(样本, 产生该样本的源服务器字符串)`，由调用方写 `NtpValid` `TimeEvidence`；
/// 超时/空源/全部失败返回 `None`，由调用方回退 `SystemFallback`。
pub async fn run_startup_sample(
    bind: crate::time::socket::SntpBindFuture,
    resolver: &dyn crate::time::socket::SntpResolver,
    servers: &[String],
    request_timeout: Duration,
    budget: Duration,
    max_rtt_ms: i64,
    now: tokio::time::Instant,
) -> Option<(crate::time::socket::NtpSample, String)> {
    // 绝对 deadline 只创建一次：budget 硬封顶 30s，之后绝不按源/阶段重建。
    let deadline = now + budget.min(NTP_STARTUP_BUDGET);

    // 绝对 deadline 到期（Err 分支 → unwrap_or_default → None）：本轮 DNS/发送/接收全部
    // 取消，调用方回退 SystemFallback。
    tokio::time::timeout_at(deadline, async move {
        // socket bind 也在同一绝对 deadline 内
        let socket = bind.await.ok()?;
        for server in servers {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            // 单次请求 = min(configured, 05 §2 建议 3s 上限, 剩余预算)
            let per = select_request_timeout(request_timeout, remaining);
            match crate::time::socket::query_sntp(
                &socket,
                resolver,
                server,
                startup_nonce(),
                per,
                max_rtt_ms,
            )
            .await
            {
                Ok(sample) => return Some((sample, server.clone())),
                Err(_) => continue,
            }
        }
        // 空源或一轮全部失败：有界等待至绝对 deadline（唯一 await，不忙循环），
        // 重读节奏即退避节奏（后台重读是其他任务）。
        tokio::time::sleep_until(deadline).await;
        None
    })
    .await
    .unwrap_or_default()
}

// ============================================================================
// Runtime Task 1c：后台 TimeGateDriver（源列表重读、stale 复验、单调退避、
// 跳钟失效、脱敏审计、线程安全通知）与其可注入依赖抽象。
// ============================================================================

/// 时间审计事件（契约 5）：每次状态变迁写入一条，载荷脱敏——只含下列字段，
/// **不含** NTP 源原始地址、nonce、socket 端口等未脱敏信息。
/// 账本的 `process_epoch`/`event_seq`/`time_evidence` 仍由唯一 producer
/// `audit::insert_audit`（Identity 拥有）内部取值，driver 事件不携带、不另立第二 producer。
#[derive(Clone, Debug)]
pub struct TimeAuditEvent {
    /// 事件种类：`ntp_startup` | `ntp_recovered` | `ntp_revalidated` | `stale_revalidated`
    /// | `evidence_stale` | `wall_clock_jump` | `clock_epoch_renewed`（换代事件）。
    pub kind: &'static str,
    /// 变迁前 quality。
    pub from: TimeQuality,
    /// 变迁后 quality。
    pub to: TimeQuality,
    /// 脱敏变迁原因（与 kind 一致）。
    pub reason: &'static str,
    pub offset_ms: Option<i64>,
    pub uncertainty_ms: Option<i64>,
    pub reference_utc: DateTime<Utc>,
    /// 进程时钟纪元，取自 `TimeEvidence.clock_epoch`（非账本 process_epoch）。
    pub clock_epoch: Uuid,
    /// 当前退避尝试计数：失效事件记复位后的 0；恢复/复验事件记复位前已消耗的尝试数。
    pub attempt: u32,
}

/// driver 侧审计 sink 抽象（契约 5）：生产装配用薄桥接实现把事件写入独立事务并调用
/// 账本唯一 producer `audit::insert_audit`（复用 R1/R2 审计接线，不新增 producer）；
/// 受控测试用内存 Vec sink 断言事件序列。
pub trait TimeAuditSink: Send + Sync {
    fn record(&self, event: TimeAuditEvent);
}

/// 空审计 sink：生产审计桥接（`audit::insert_audit` 事务接线）属集成阶段装配，
/// 本任务不新增第二个账本 producer。
pub struct NullTimeAuditSink;

impl TimeAuditSink for NullTimeAuditSink {
    fn record(&self, _event: TimeAuditEvent) {}
}

/// 一轮采样 future：`Some((样本, 产生该样本的源字符串))`；失败/超时/空源为 `None`。
pub type TimeSampleFuture<'a> =
    Pin<Box<dyn Future<Output = Option<(NtpSample, String)>> + Send + 'a>>;

/// 采样抽象（契约 2/3/8 的可注入点）：生产实现复用 `run_startup_sample` 的总预算采样逻辑，
/// 受控测试注入可编程 fake，不访问真实网络。
pub trait TimeSampler: Send + Sync {
    fn sample<'a>(&'a self, servers: &'a [String]) -> TimeSampleFuture<'a>;
}

/// 源列表读取 future。
pub type SourceListFuture<'a> = Pin<Box<dyn Future<Output = Vec<String>> + Send + 'a>>;

/// NTP 源列表读取抽象（契约 1）：driver 每轮循环开头重读最新源列表，而非启动时固化。
/// 生产实现由配置源提供（本任务提供静态实现，配置中心重读接线属集成阶段）；
/// 受控测试注入可修改源列表的 fake。
pub trait NtpSourceList: Send + Sync {
    fn read(&self) -> SourceListFuture<'_>;
}

/// 墙钟抽象（契约 4 的可注入点）：生产 = `SystemTime::now()`，受控测试注入可编程跳钟。
pub trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// 生产墙钟。
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// 等待 future。
pub type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 等待抽象（契约 2/3 的可注入点）：生产 = `tokio::time::sleep`。
/// 本仓库 tokio 未启用 `test-util`（无 pause/advance），受控测试注入“记录请求时长 + 短真实等待”
/// 的 fake，从而对退避/复验节奏做确定性断言，且绝不真实等待 1s..60s 或 30s。
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_>;
}

/// 生产等待实现。
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// 生产采样器：每轮以 `run_startup_sample` 的总预算逻辑采样（bind/DNS/多源/重试同一绝对 deadline）。
pub struct StartupTimeSampler {
    config: TimeGateConfig,
    resolver: Arc<dyn SntpResolver>,
}

impl StartupTimeSampler {
    pub fn new(config: TimeGateConfig, resolver: Arc<dyn SntpResolver>) -> Self {
        Self { config, resolver }
    }
}

impl TimeSampler for StartupTimeSampler {
    fn sample<'a>(&'a self, servers: &'a [String]) -> TimeSampleFuture<'a> {
        let config = self.config.clone();
        let resolver = Arc::clone(&self.resolver);
        let servers = servers.to_vec();
        Box::pin(async move {
            let bind: SntpBindFuture = Box::pin(async {
                tokio::net::UdpSocket::bind("0.0.0.0:0")
                    .await
                    .map_err(|_| crate::time::ntp::NtpError::Io)
            });
            run_startup_sample(
                bind,
                resolver.as_ref(),
                &servers,
                config.request_timeout,
                config.startup_budget,
                config.max_rtt_ms,
                tokio::time::Instant::now(),
            )
            .await
        })
    }
}

/// 静态源列表（生产占位）：返回装配时给定的配置源；配置源热重读由集成阶段替换。
pub struct StaticSourceList {
    servers: Vec<String>,
}

impl StaticSourceList {
    pub fn new(servers: Vec<String>) -> Self {
        Self { servers }
    }
}

impl NtpSourceList for StaticSourceList {
    fn read(&self) -> SourceListFuture<'_> {
        let servers = self.servers.clone();
        Box::pin(async move { servers })
    }
}

/// driver 依赖装配：全部外部交互可注入。
pub struct TimeGateDeps {
    pub sampler: Arc<dyn TimeSampler>,
    pub sources: Arc<dyn NtpSourceList>,
    pub wall_clock: Arc<dyn WallClock>,
    pub sleeper: Arc<dyn Sleeper>,
    pub audit: Arc<dyn TimeAuditSink>,
}

impl TimeGateDeps {
    /// 生产装配（采样/源列表/墙钟/等待为生产实现；审计桥接属集成阶段）。
    pub fn production(config: &TimeGateConfig) -> Self {
        Self {
            sampler: Arc::new(StartupTimeSampler::new(
                config.clone(),
                Arc::new(TokioSntpResolver),
            )),
            sources: Arc::new(StaticSourceList::new(config.servers.clone())),
            wall_clock: Arc::new(SystemWallClock),
            sleeper: Arc::new(TokioSleeper),
            audit: Arc::new(NullTimeAuditSink),
        }
    }
}

/// 后台时间门禁 driver（契约 1–8）。
pub struct TimeGateDriver {
    /// RT-R12-1（单元 1）：与 TimeGateHandle 共享的权威快照（契约 6）——证据与锚点合并为
    /// 单一 `TimeGateSnapshot`，成功时在同一个 write 锁内原子写入 `evidence` 与
    /// `sample_instant`，杜绝双锁竞态。
    snapshot: Arc<RwLock<TimeGateSnapshot>>,
    is_ready: Arc<AtomicBool>,
    ready: Arc<Notify>,
    retry_active: Arc<AtomicBool>,
    retry_attempt: Arc<AtomicU32>,
    audit: Arc<dyn TimeAuditSink>,
    config: TimeGateConfig,
    sampler: Arc<dyn TimeSampler>,
    sources: Arc<dyn NtpSourceList>,
    wall_clock: Arc<dyn WallClock>,
    sleeper: Arc<dyn Sleeper>,
    /// 退避尝试计数（契约 3）。
    attempt: u32,
    /// 跳钟检测基线（契约 4）。
    last_wall: Option<SystemTime>,
}

impl TimeGateDriver {
    /// 与 `handle` 共享权威快照/通知/退避状态构造 driver。
    pub fn new(handle: &TimeGateHandle, config: TimeGateConfig, deps: TimeGateDeps) -> Self {
        Self {
            snapshot: Arc::clone(&handle.snapshot),
            is_ready: Arc::clone(&handle.is_ready),
            ready: Arc::clone(&handle.ready),
            retry_active: Arc::clone(&handle.retry_active),
            retry_attempt: Arc::clone(&handle.retry_attempt),
            audit: deps.audit,
            config,
            sampler: deps.sampler,
            sources: deps.sources,
            wall_clock: deps.wall_clock,
            sleeper: deps.sleeper,
            attempt: 0,
            last_wall: None,
        }
    }

    /// 启动采样（契约 8）+ 后台循环。
    pub async fn run(mut self) {
        self.startup().await;
        self.run_background().await;
    }

    /// 仅后台循环（不做启动采样）：供已完成启动采样的装配或受控测试在既有状态上接管。
    pub async fn run_background(mut self) {
        loop {
            // 契约 4：每轮采样前先做跳钟检测；last_wall 无论是否检出跳变均每轮更新。
            if self.wall_jumped() {
                // RT-R8-1（跳钟换代）：跳钟即原时钟参考系彻底失效——除失效降级 SystemFallback，
                // 同一写锁内重新生成 clock_epoch；旧纪元的证据与一切期限/寿命比较不得跨纪元继续信任。
                self.invalidate("wall_clock_jump", true).await;
            }
            // 契约 1：每轮循环开头重读最新源列表，而非启动时固化。
            let servers = self.read_source_list().await;
            // 契约 6：每 tick 读权威快照决定分支（读锁在同一语句内释放，不与写锁重入）。
            let quality = self.snapshot.read().await.evidence.quality;
            match quality {
                TimeQuality::NtpValid => {
                    // 契约 2：等待 revalidate_interval 后复验（复用总预算采样逻辑）。
                    self.sleeper.sleep(self.config.revalidate_interval).await;
                    match self.sampler.sample(&servers).await {
                        Some((sample, source)) => {
                            self.transition(sample, source, "ntp_revalidated").await;
                        }
                        // 复验失败：保留当前证据；仅当 age（含锚点经过时间）超上限才失效降级。
                        None if self.evidence_stale().await => {
                            self.invalidate("evidence_stale", false).await;
                        }
                        None => {}
                    }
                }
                TimeQuality::Stale => {
                    // 契约 2：Stale 视为不可信——维持当前 gate 状态（不放行、不降级放行），
                    // 按 revalidate_interval 复验；复验失败且 age 超上限才失效为 SystemFallback。
                    self.sleeper.sleep(self.config.revalidate_interval).await;
                    match self.sampler.sample(&servers).await {
                        Some((sample, source)) => {
                            self.transition(sample, source, "stale_revalidated").await;
                        }
                        None if self.evidence_stale().await => {
                            self.invalidate("evidence_stale", false).await;
                        }
                        None => {}
                    }
                }
                TimeQuality::SystemFallback => {
                    // 契约 3：失败每轮 attempt += 1，按 next_backoff 单调退避（封顶 BACKOFF_MAX）。
                    self.set_attempt(self.attempt.saturating_add(1));
                    self.sleeper.sleep(next_backoff(self.attempt)).await;
                    // 成功即由 transition 归零 attempt；失败则下轮按 attempt+1 继续退避。
                    if let Some((sample, source)) = self.sampler.sample(&servers).await {
                        self.transition(sample, source, "ntp_recovered").await;
                    }
                }
            }
        }
    }

    /// 契约 8：启动采样一次（生产复用 `run_startup_sample` 的总预算逻辑）；
    /// 成功 → `NtpValid` + ready，`None` → `SystemFallback` 并由后台循环进入退避重试。
    async fn startup(&mut self) {
        let servers = self.read_source_list().await;
        match self.sampler.sample(&servers).await {
            Some((sample, source)) => self.transition(sample, source, "ntp_startup").await,
            None => self.invalidate("ntp_startup", false).await,
        }
    }

    async fn read_source_list(&self) -> Vec<String> {
        (self.sources.read()).await
    }

    /// 契约 4：与上一轮 `last_wall` 比较，回跳 > `JUMP_BACK_EPS` 或前跳 > `JUMP_FORWARD_MAX`
    /// 判定跳钟；`last_wall` 每轮读取后均更新（首轮仅建立基线，不判跳）。
    fn wall_jumped(&mut self) -> bool {
        let now = self.wall_clock.now();
        let jumped = match self.last_wall {
            None => false,
            Some(previous) => {
                let backward = previous.duration_since(now).unwrap_or_default();
                let forward = now.duration_since(previous).unwrap_or_default();
                backward > JUMP_BACK_EPS || forward > JUMP_FORWARD_MAX
            }
        };
        self.last_wall = Some(now);
        jumped
    }

    /// RT-R8-1（动态 age）：判定值 = 快照存储 `sample_age_ms` + 锚点经过时间
    /// （`sample_instant` 为 `Some` 时取单调经过毫秒，`None` 时按 0），age 随读取动态推进；
    /// 判定标准：派生值 > `config.max_evidence_age_ms`（契约 2/4）。
    async fn evidence_stale(&self) -> bool {
        let snap = self.snapshot.read().await;
        let stored_age_ms = snap.evidence.sample_age_ms.unwrap_or(0);
        let elapsed_ms = snap
            .sample_instant
            .map(|instant| {
                tokio::time::Instant::now()
                    .duration_since(instant)
                    .as_millis() as u64
            })
            .unwrap_or(0);
        stored_age_ms.saturating_add(elapsed_ms) > self.config.max_evidence_age_ms
    }

    /// 取得合格样本：更新证据、必要时换代（RT-R12-2 / RT-R13-2）、写脱敏审计、必要时转 ready。
    async fn transition(&mut self, sample: NtpSample, source: String, kind: &'static str) {
        let attempt_before_reset = self.attempt;
        let (from, clock_epoch, renewed) = {
            // 契约 6：证据与锚点在同一写锁内原子写入；换代判定也在同一写锁内基于旧快照完成。
            let mut snap = self.snapshot.write().await;
            let from = snap.evidence.quality;
            // 单调外推期望参考 = 旧 reference + 锚点经过时间；偏差 > 1s 判定不连续阶跃校正。
            let elapsed_ms = snap
                .sample_instant
                .map(|instant| {
                    tokio::time::Instant::now()
                        .duration_since(instant)
                        .as_millis()
                        .min(i64::MAX as u128) as i64
                })
                .unwrap_or(0);
            let expected_ref_ms = snap
                .evidence
                .reference_utc
                .timestamp_millis()
                .saturating_add(elapsed_ms);
            let step_ms = (i128::from(sample.reference_utc.timestamp_millis())
                - i128::from(expected_ref_ms))
            .abs();
            let discontinuous = match snap.evidence.source.as_deref() {
                // 首次取得样本：无前序参考系可比对，沿用调用方给定的进程 clock_epoch。
                None => false,
                Some(previous) => previous != source || step_ms > i128::from(CLOCK_STEP_MAX_MS),
            };
            if discontinuous {
                snap.evidence.clock_epoch = Uuid::new_v4();
            }
            let clock_epoch = snap.evidence.clock_epoch;
            snap.evidence.system_wall_utc = Utc::now();
            snap.evidence.reference_utc = sample.reference_utc;
            snap.evidence.quality = TimeQuality::NtpValid;
            snap.evidence.source = Some(source);
            snap.evidence.sample_age_ms = Some(0);
            snap.evidence.offset_ms = Some(sample.offset_ms);
            snap.evidence.uncertainty_ms = Some(sample.uncertainty_ms);
            snap.sample_instant = Some(tokio::time::Instant::now());
            (from, clock_epoch, discontinuous)
        };
        self.set_attempt(0);
        self.retry_active.store(false, Ordering::SeqCst);
        let uncertainty_ms = i64::try_from(sample.uncertainty_ms).ok();
        self.audit.record(TimeAuditEvent {
            kind,
            from,
            to: TimeQuality::NtpValid,
            reason: kind,
            offset_ms: Some(sample.offset_ms),
            uncertainty_ms,
            reference_utc: sample.reference_utc,
            clock_epoch,
            attempt: attempt_before_reset,
        });
        if renewed {
            // 换代审计（RT-R12-2 / RT-R13-2）：紧跟触发换代的状态变迁之后写入。
            self.audit.record(TimeAuditEvent {
                kind: "clock_epoch_renewed",
                from,
                to: TimeQuality::NtpValid,
                reason: "clock_epoch_renewed",
                offset_ms: Some(sample.offset_ms),
                uncertainty_ms,
                reference_utc: sample.reference_utc,
                clock_epoch,
                attempt: attempt_before_reset,
            });
        }
        // 契约 6：转 ready 统一在此 store + notify_waiters（wait_ready 先注册后复查，不丢通知）。
        self.is_ready.store(true, Ordering::SeqCst);
        self.ready.notify_waiters();
    }

    /// 失效降级：同一写锁内清空证据（source/offset/uncertainty/age 与单调锚点）、置
    /// `SystemFallback`，必要时换代（跳钟，RT-R8-1）；`attempt = 0`、`retry_active = true`，
    /// 随后写脱敏审计（契约 5）。
    async fn invalidate(&mut self, kind: &'static str, renew_epoch: bool) {
        let (from, clock_epoch, wall) = {
            let mut snap = self.snapshot.write().await;
            let from = snap.evidence.quality;
            snap.evidence.source = None;
            snap.evidence.offset_ms = None;
            snap.evidence.uncertainty_ms = None;
            snap.evidence.sample_age_ms = None;
            snap.evidence.quality = TimeQuality::SystemFallback;
            snap.sample_instant = None;
            if renew_epoch {
                snap.evidence.clock_epoch = Uuid::new_v4();
            }
            let clock_epoch = snap.evidence.clock_epoch;
            let wall = Utc::now();
            snap.evidence.system_wall_utc = wall;
            snap.evidence.reference_utc = wall;
            (from, clock_epoch, wall)
        };
        self.set_attempt(0);
        self.retry_active.store(true, Ordering::SeqCst);
        self.audit.record(TimeAuditEvent {
            kind,
            from,
            to: TimeQuality::SystemFallback,
            reason: kind,
            offset_ms: None,
            uncertainty_ms: None,
            reference_utc: wall,
            clock_epoch,
            attempt: 0,
        });
    }

    fn set_attempt(&mut self, attempt: u32) {
        self.attempt = attempt;
        self.retry_attempt.store(attempt, Ordering::SeqCst);
    }
}
