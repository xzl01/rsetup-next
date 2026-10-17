//! 时间门禁（Runtime Task 1b）：启动总预算采样、`TimeGateHandle` 就绪等待与
//! 原子时间快照。
//!
//! 后台 `TimeGateDriver`（源列表重读、stale 复验、跳钟失效、审计）归 T1c；
//! 本模块的 `TimeGateSnapshot`/notify 结构与其契约兼容（RT-R12-1/RT-R7-4）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::time::evidence::{TimeEvidence, TimeQuality};

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

pub struct TimeGateHandle {
    pub(crate) snapshot: Arc<RwLock<TimeGateSnapshot>>,
    ready: Arc<tokio::sync::Notify>,
    is_ready: Arc<AtomicBool>,
    /// 退避尝试计数：T1b 只持有状态，读写由 T1c 后台 driver 承接。
    #[allow(dead_code)]
    retry_attempt: Arc<AtomicU32>,
    retry_active: Arc<AtomicBool>,
    /// RT-R11-4（单元 B）：由 `start_time_gate` 从 `config.max_evidence_age_ms` 注入的配置阈值；
    /// `current()` 据此动态判定降级（自定义阈值如 180s 与默认 300s 均即时生效，不再固定取默认值）。
    max_evidence_age_ms: u64,
}

impl TimeGateHandle {
    /// 已 ready 立即返回快照；否则等待 ready 通知，最长 30s 超时兜底返回当前快照。
    pub async fn wait_ready(&self) -> TimeEvidence {
        // RT-R7-4：修复检查-注册竞态——tokio 的 Notified 首次 poll 才入队，且 Notify 不为未来的
        // waiter 保留 permit；若 store(true) + notify_waiters() 恰在 load(false) 之后、监听器入队
        // 之前发生，通知即丢失，调用方死等满 30s。故每轮必须先创建监听器并 enable() 立即注册，
        // 再复查 is_ready。超时（Err 分支被 `let _ =` 吸收）不 panic，落到下方返回当前快照兜底。
        let _ = tokio::time::timeout(Duration::from_secs(30), async {
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

/// 启动时间门禁（`run_startup_sample` 的启动侧消费者，每次进程启动只调用一次）。
///
/// 初始 quality 为 `Stale`（不可信）且未 ready；spawn 启动采样任务，采样成功在同一写锁内
/// 原子写 `NtpValid` 证据与单调锚点，再 `is_ready = true` + `notify_waiters()`；失败写
/// `SystemFallback` 并保持 `retry_active = true`（后台退避重试由 T1c driver 承接）。
/// 必须在 tokio 运行时内调用（内部 `tokio::spawn`）。
/// 注意：startup gate 结束只表示 NTP 门结束，不替代 DB/bootstrap/恢复的 ready（05 §1）。
pub fn start_time_gate(config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle {
    // 合法性校验失败即拒绝启动（与 request_timeout 3s 上限校验同级）。
    config
        .validate()
        .expect("TimeGateConfig 非法，拒绝启动时间门禁");
    let now = Utc::now();
    let snapshot = Arc::new(RwLock::new(TimeGateSnapshot {
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
    }));
    let handle = TimeGateHandle {
        snapshot: Arc::clone(&snapshot),
        ready: Arc::new(tokio::sync::Notify::new()),
        // 初始未 ready：杜绝“启动期间 DNS/采样在途时立即拿到 Stale 直接退出”。
        is_ready: Arc::new(AtomicBool::new(false)),
        retry_attempt: Arc::new(AtomicU32::new(0)),
        // 启动采样在途/失败待后台重试期间 retry_active = true；取得 NtpValid 样本后复位。
        retry_active: Arc::new(AtomicBool::new(true)),
        // RT-R11-4（单元 B）：注入配置阈值，current() 据此动态判定降级。
        max_evidence_age_ms: config.max_evidence_age_ms,
    };

    let ready = Arc::clone(&handle.ready);
    let is_ready = Arc::clone(&handle.is_ready);
    let retry_active = Arc::clone(&handle.retry_active);
    tokio::spawn(async move {
        let bind: crate::time::socket::SntpBindFuture = Box::pin(async {
            tokio::net::UdpSocket::bind("0.0.0.0:0")
                .await
                .map_err(|_| crate::time::ntp::NtpError::Io)
        });
        let outcome = run_startup_sample(
            bind,
            &crate::time::socket::TokioSntpResolver,
            &config.servers,
            config.request_timeout,
            config.startup_budget,
            config.max_rtt_ms,
            tokio::time::Instant::now(),
        )
        .await;
        match outcome {
            Some((sample, source)) => {
                let mut snap = snapshot.write().await;
                // 同一写锁内原子写证据与单调锚点（RT-R12-1）。
                snap.evidence.system_wall_utc = Utc::now();
                snap.evidence.reference_utc = sample.reference_utc;
                snap.evidence.quality = TimeQuality::NtpValid;
                snap.evidence.source = Some(source);
                snap.evidence.sample_age_ms = Some(0);
                snap.evidence.offset_ms = Some(sample.offset_ms);
                snap.evidence.uncertainty_ms = Some(sample.uncertainty_ms);
                snap.sample_instant = Some(tokio::time::Instant::now());
                drop(snap);
                is_ready.store(true, Ordering::SeqCst);
                ready.notify_waiters();
                retry_active.store(false, Ordering::SeqCst);
            }
            None => {
                let mut snap = snapshot.write().await;
                let wall = Utc::now();
                snap.evidence.system_wall_utc = wall;
                snap.evidence.reference_utc = wall;
                snap.evidence.quality = TimeQuality::SystemFallback;
                snap.sample_instant = None;
                // 回退 SystemFallback；后台退避重试由 T1c driver 承接，retry_active 保持 true。
            }
        }
    });
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
