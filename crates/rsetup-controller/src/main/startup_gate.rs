//! 启动门禁与时序域（批次 E）。
//!
//! 包含时间门禁配置装配（`time_gate_config_from_env`）、时间变更通知生产挂接（`time_changed_sink`）、
//! 准备检查门禁（`start_if_ready`）、时间启动门并行等待句柄（`StartupTimeGate`）与
//! 监听前联合门禁（`listen_after_startup_gates`）。

use std::sync::Arc;

use rsetup_controller::events::EventBus;
use rsetup_controller::events::EventEnvelope;
use rsetup_controller::time::TimeChangedSink;
use rsetup_controller::time::evidence::TimeEvidence;
use rsetup_controller::time::{TimeGateConfig, TimeGateHandle};

/// 从环境变量装配时间门禁配置（每次进程启动只调用一次，随 `start_time_gate` 消费）。
///
/// 基线是 [`TimeGateConfig::default()`]：`servers` 为空 → 采样器无源可用 → 启动采样在
/// 总预算内直接回退 `SystemFallback`，安全 fail-closed 与 fallback 规则完全不变。仅当
/// `NTP_SERVERS` 环境变量存在时按其切分（逗号与任意空白分隔，忽略空段）填充 `servers`。
pub(super) fn time_gate_config_from_env() -> TimeGateConfig {
    let servers = std::env::var("NTP_SERVERS")
        .map(|raw| {
            raw.split(|c: char| c == ',' || c.is_whitespace())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    TimeGateConfig {
        servers,
        ..TimeGateConfig::default()
    }
}

/// 时间门禁 → 进程内事件总线的生产挂接（06 §4 `system.time.changed`）。
///
/// driver 每次**确认并把新的权威时间证据写入快照**后调用它一次（启动采样、周期复验、
/// 退避恢复、stale/跳钟失效）。这里只负责将变化投到进程内唯一通道：事件形状与
/// 「仅管理员」受众由 [`EventEnvelope::system_time_changed`] 钉死，与
/// `SystemApiState::publish_system_time_changed` 是同一个构造入口，不存在第二套时间字段
/// 或第二个受众决定点。[`EventBus::publish`] 永不阻塞、永不上抛、无订阅者即丢弃，
/// 因此在 driver 的任务上直接调用是安全的（时间线不会因为没人订阅 SSE 而变慢或失败）。
pub(super) fn time_changed_sink(bus: &EventBus) -> TimeChangedSink {
    let bus = bus.clone();
    Arc::new(move |evidence| {
        bus.publish(EventEnvelope::system_time_changed(evidence));
    })
}

pub(super) async fn start_if_ready<C, F, Fut>(
    check: C,
    start: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    C: std::future::Future<Output = Result<(), rsetup_controller::ControllerError>>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    check.await?;
    start().await
}

/// 时间启动门的 **并行** 等待句柄：把 `ready_timeout` 的计时起点钉在门禁启动的那一刻。
///
/// [`TimeGateHandle::wait_ready`] 内部是 `timeout(self.ready_timeout, ...)`，超时预算从
/// **本方法被首次 poll 的一刻**才起算，而不是从 `start_time_gate` 起算。因此按
/// 「DB 连接 → schema 检查 → admin bootstrap → 启动恢复 → 生产装配 → 再等时间门」的串行
/// 顺序去调用它时，只要 NTP 恒失败（无网络/未配 `NTP_SERVERS`：启动采样在预算内回退
/// `SystemFallback`，而 fallback **既不置 `is_ready` 也不 `notify_waiters`**），慢启动就会
/// 在自身耗时之外**再叠加一整段 30s**（DB 花 40s 就要等 40s + 30s 才开始监听）。
///
/// 本句柄在 `start_time_gate` 返回后**立即** spawn 一次 `wait_ready`，让这 30s 预算从门禁
/// 启动的第一毫秒起与整个启动准备阶段并行消耗。三条路径的等待上界都是**一段**
/// `ready_timeout`，绝不叠加：
/// - 准备工作在预算内失败：句柄随栈展开 [`Drop`]，`Drop` 立即 `abort` 在途等待，错误
///   毫秒级上抛，绝不因等时间门而拖延失败；
/// - 准备工作耗时已超预算：后台等待早已带着超时快照返回，`finish` 耗时 ≈ 0；
/// - 准备工作耗时在预算内：`finish` 最多补等剩余部分。
///
/// 全程复用同一门禁 driver 的 `is_ready`/权威快照（`TimeGateHandle` 的状态全在 `Arc` 后），
/// 不新建第二个采样器，也不改变 05 §1 的 fallback 放行语义。
pub(super) struct StartupTimeGate {
    /// 门禁句柄克隆：与唯一 driver 共享 ready/权威快照，供日志与收敛读取。
    gate: TimeGateHandle,
    /// 已在计时的后台 `wait_ready` 任务；`Option` 只为让 `Drop` 与 `finish` 都能取走它。
    waiter: Option<tokio::task::JoinHandle<TimeEvidence>>,
}

impl StartupTimeGate {
    /// 从这一刻起开始并行等待就绪。**必须**紧跟 `start_time_gate` 之后、任何阻塞式启动
    /// 准备工作（建池/schema/bootstrap/恢复/装配）之前调用，否则 30s 预算又会推迟到那些
    /// 工作结束之后重新起算。
    pub(super) fn start(gate: &TimeGateHandle) -> Self {
        let waiting_gate = gate.clone();
        let waiter = tokio::spawn(async move { waiting_gate.wait_ready().await });
        Self {
            gate: gate.clone(),
            waiter: Some(waiter),
        }
    }

    /// 门禁是否已转 ready（不等待，也不消费等待预算）。
    pub(super) fn is_ready(&self) -> bool {
        self.gate.is_ready()
    }

    /// 收敛时间启动门：只补等**剩余**预算，然后按收敛时刻的权威快照给出证据。
    ///
    /// 后台任务被取消或 panic 都不上抛——门禁状态由权威快照如实表达（不可信即
    /// `SystemFallback`/`Stale`），绝不 panic、也不把「没等到」伪装成 ready。
    pub(super) async fn finish(&mut self) -> TimeEvidence {
        if let Some(waiter) = self.waiter.as_mut() {
            let _ = waiter.await;
        }
        // 证据以收敛这一刻的快照为准：准备阶段比预算更慢时，`wait_ready` 的返回值是
        // 预算耗尽时刻的读数，直接沿用会把几秒前的墙钟当本刻读数（`current()` 对
        // `SystemFallback`/`Stale` 会刷新为当前墙钟，并动态派生样本年龄）。
        self.gate.current().await
    }
}

impl Drop for StartupTimeGate {
    fn drop(&mut self) {
        // 没走到 `finish` 的所有路径（建池/schema/bootstrap/恢复任一步失败，或调用方提前
        // 返回）都在此取消：失败即刻上抛，绝不在后台继续挂着一次最长 `ready_timeout` 的
        // 就绪等待。
        if let Some(waiter) = self.waiter.take() {
            waiter.abort();
        }
    }
}

/// 监听前的联合门禁（05 §1：关卡结束前不开放管理业务 API 与板端接入）。
///
/// - `pre_listen` 代表 DB 这条线：幂等 admin 初始化 + 未决任务/设备锁恢复 + 顶层 router
///   装配，任一失败即**上抛且不等待时间门**（更不绑端口），保持既有 fail-closed 顺序；
/// - 成功后才收敛时间启动门：`time_gate` 是 [`StartupTimeGate`]，其 `ready_timeout`
///   （生产默认 `READY_WAIT_TIMEOUT` = 30s）自 `start_time_gate` 之后即与 `pre_listen`
///   **并行**计时，这里最多补等剩余部分。30s 只封顶 NTP 引入的等待，**不是**在 DB/恢复
///   之后再叠加 30s，也不新建任何采样器；
/// - 门禁「有结果」不等于「NTP 已同步」：到期无样本按规范以 `SystemFallback` 墙钟
///   启动（05 §1 明文），质量如实不可信，任务线的 time_review 继续 fail-closed。
pub(super) async fn listen_after_startup_gates<R, P, PF, L, LF>(
    mut time_gate: StartupTimeGate,
    pre_listen: P,
    listen: L,
) -> Result<(), Box<dyn std::error::Error>>
where
    P: FnOnce() -> PF,
    PF: std::future::Future<Output = Result<R, Box<dyn std::error::Error>>>,
    L: FnOnce(R) -> LF,
    LF: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    // DB 这条线先推进：任一步失败即 `?` 上抛，`time_gate` 随栈展开 Drop 并 abort 在途的
    // 就绪等待，毫秒级退出，绝不为了等时间门而拖延失败（也不绑端口）。
    let prepared = pre_listen().await?;
    // 到这里 30s 预算已与上面全程并行推进（未耗尽则只补等剩余部分）。
    let evidence: TimeEvidence = time_gate.finish().await;
    eprintln!(
        "controller 时间启动门结束（quality={:?}, clock_epoch={}, ready={}），准备对外监听",
        evidence.quality,
        evidence.clock_epoch,
        time_gate.is_ready(),
    );
    listen(prepared).await
}

#[cfg(test)]
impl StartupTimeGate {
    /// 后台等待任务的取消句柄克隆（仅供测试观察「失败路径是否真的取消了在途等待」；
    /// 不参与生产逻辑，构造句柄本身不会取消任务）。
    fn waiter_handle(&self) -> Option<tokio::task::AbortHandle> {
        self.waiter
            .as_ref()
            .map(tokio::task::JoinHandle::abort_handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Instant;
    use std::time::{Duration, SystemTime};

    use crate::assembly::task_time_source;
    use rsetup_controller::time::evidence::TimeQuality;
    use rsetup_controller::time::socket::NtpSample;
    use rsetup_controller::time::{
        NullTimeAuditSink, SleepFuture, Sleeper, StaticSourceList, TimeGateConfig, TimeGateDeps,
        TimeGateDriver, TimeSampleFuture, TimeSampler, WallClock,
    };

    const FAKE_NTP_SOURCE: &str = "fake.main-assembly.local";

    /// 可编程墙钟（跳钟用例）。
    struct ControlledWallClock(Mutex<SystemTime>);

    impl ControlledWallClock {
        fn at(now: SystemTime) -> Arc<Self> {
            Arc::new(Self(Mutex::new(now)))
        }
        fn set(&self, now: SystemTime) {
            *self.0.lock().expect("wall lock") = now;
        }
    }

    impl WallClock for ControlledWallClock {
        fn now(&self) -> SystemTime {
            *self.0.lock().expect("wall lock")
        }
    }

    /// 真实等待封顶到 5ms 的等待替身：受控测试不真实等待退避/复验节奏。
    struct ShortSleeper;

    impl Sleeper for ShortSleeper {
        fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
            let capped = duration.min(Duration::from_millis(5));
            Box::pin(async move { tokio::time::sleep(capped).await })
        }
    }

    /// 阻塞直到显式放行的采样器：放门前 `wait_ready` 必须一直等待。
    struct BlockedUntilRelease {
        release: tokio::sync::watch::Receiver<bool>,
    }

    impl TimeSampler for BlockedUntilRelease {
        fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
            let mut release = self.release.clone();
            Box::pin(async move {
                while !*release.borrow_and_update() {
                    if release.changed().await.is_err() {
                        return None;
                    }
                }
                Some((
                    NtpSample {
                        offset_ms: 0,
                        rtt_ms: 1,
                        reference_utc: chrono::Utc::now(),
                        uncertainty_ms: 1,
                    },
                    FAKE_NTP_SOURCE.to_owned(),
                ))
            })
        }
    }

    /// 恒失败采样器：本进程从未取得合格样本（fallback 语义）。
    struct NoSampleSampler;

    impl TimeSampler for NoSampleSampler {
        fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
            Box::pin(async move { None })
        }
    }

    /// 跟随可编程墙钟的合格采样器（跳钟换代用例）。
    struct WallFollowingSampler {
        wall: Arc<ControlledWallClock>,
    }

    impl TimeSampler for WallFollowingSampler {
        fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
            let reference = chrono::DateTime::<chrono::Utc>::from(self.wall.now());
            Box::pin(async move {
                Some((
                    NtpSample {
                        offset_ms: 0,
                        rtt_ms: 1,
                        reference_utc: reference,
                        uncertainty_ms: 1,
                    },
                    FAKE_NTP_SOURCE.to_owned(),
                ))
            })
        }
    }

    /// 受控门禁装配：走生产 `TimeGateDriver::run()` 同一条路径，只用公开的
    /// `with_ready_timeout` 把就绪等待上限压到测试尺度（生产仍为 30s）。
    fn spawn_controlled_gate(
        config: TimeGateConfig,
        deps: TimeGateDeps,
        clock_epoch: uuid::Uuid,
        ready_timeout: Duration,
    ) -> TimeGateHandle {
        let handle = TimeGateHandle::new(&config, clock_epoch).with_ready_timeout(ready_timeout);
        let driver = TimeGateDriver::new(&handle, config, deps);
        tokio::spawn(driver.run());
        handle
    }

    fn gate_deps(sampler: Arc<dyn TimeSampler>, wall: Arc<ControlledWallClock>) -> TimeGateDeps {
        TimeGateDeps {
            sampler,
            sources: Arc::new(StaticSourceList::new(vec![FAKE_NTP_SOURCE.to_owned()])),
            wall_clock: wall,
            sleeper: Arc::new(ShortSleeper),
            audit: Arc::new(NullTimeAuditSink),
        }
    }

    fn gate_config() -> TimeGateConfig {
        TimeGateConfig {
            servers: vec![FAKE_NTP_SOURCE.to_owned()],
            revalidate_interval: Duration::from_millis(5),
            max_evidence_age_ms: 300_000,
            ..TimeGateConfig::default()
        }
    }

    fn boxed_err() -> Box<dyn std::error::Error> {
        Box::new(std::io::Error::other("bootstrap/recovery failed"))
    }

    #[tokio::test]
    async fn listener_holds_until_the_time_gate_finishes() {
        // 采样器在监听计数出现前不得放行；`released` 由放行线程置位。监听闭包**入口**
        // 读到的 released 必须为 true，等价于「时间启动门结束前绝不进入监听」。
        let (release_tx, release_rx) = tokio::sync::watch::channel(false);
        let released = Arc::new(AtomicBool::new(false));
        let wall = ControlledWallClock::at(SystemTime::now());
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(
                Arc::new(BlockedUntilRelease {
                    release: release_rx,
                }),
                wall,
            ),
            uuid::Uuid::new_v4(),
            Duration::from_secs(30),
        );

        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let listen_calls = Arc::new(AtomicUsize::new(0));
        let released_at_listen = Arc::new(AtomicBool::new(true));

        // 后台放行：pre_listen 之后必须仍有一段时间门禁未结束。
        let order_for_pre = Arc::clone(&order);
        let order_for_listen = Arc::clone(&order);
        let listen_calls_for_listen = Arc::clone(&listen_calls);
        let released_for_listen = Arc::clone(&released);
        let released_at_listen_for_listen = Arc::clone(&released_at_listen);
        let releaser = {
            let order = Arc::clone(&order);
            let released = Arc::clone(&released);
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while order.lock().expect("order").is_empty() {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                })
                .await
                .expect("pre_listen 应先于门禁结束完成");
                tokio::time::sleep(Duration::from_millis(50)).await;
                released.store(true, Ordering::SeqCst);
                release_tx.send(true).expect("release sampler");
            })
        };

        listen_after_startup_gates(
            StartupTimeGate::start(&gate),
            move || async move {
                order_for_pre.lock().expect("order").push("pre-listen");
                Ok::<(), Box<dyn std::error::Error>>(())
            },
            move |()| {
                listen_calls_for_listen.fetch_add(1, Ordering::SeqCst);
                released_at_listen_for_listen
                    .store(released_for_listen.load(Ordering::SeqCst), Ordering::SeqCst);
                order_for_listen.lock().expect("order").push("listen");
                async { Ok(()) }
            },
        )
        .await
        .expect("联合门禁应成功");
        releaser.await.expect("releaser join");

        assert!(
            released_at_listen.load(Ordering::SeqCst),
            "监听发生在时间启动门结束之前"
        );
        assert_eq!(
            listen_calls.load(Ordering::SeqCst),
            1,
            "门禁结束后恰好监听一次"
        );
        assert_eq!(
            *order.lock().expect("order"),
            vec!["pre-listen", "listen"],
            "顺序必须是 DB/恢复 → 等待时间门 → 监听"
        );
        assert!(gate.is_ready());
    }

    #[tokio::test]
    async fn fallback_gate_still_listens_but_the_clock_stays_untrusted() {
        let wall = ControlledWallClock::at(SystemTime::now());
        let clock_epoch = uuid::Uuid::new_v4();
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(Arc::new(NoSampleSampler), wall),
            clock_epoch,
            Duration::from_millis(120),
        );
        let listen_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_listen = Arc::clone(&listen_calls);

        listen_after_startup_gates(
            StartupTimeGate::start(&gate),
            || async move { Ok::<(), Box<dyn std::error::Error>>(()) },
            move |()| {
                calls_for_listen.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await
        .expect("05 §1：到期无样本允许以 system_fallback 启动");

        assert_eq!(listen_calls.load(Ordering::SeqCst), 1);
        assert!(!gate.is_ready(), "无合格样本不得转 ready");
        // 启动门「有结果」≠「已同步」：注入给任务线的同一句柄必须仍报不可信质量。
        let evidence = task_time_source(&gate).current_evidence().await;
        assert_eq!(evidence.quality, TimeQuality::SystemFallback);
        assert!(evidence.source.is_none());
        assert!(evidence.offset_ms.is_none() && evidence.uncertainty_ms.is_none());
    }

    #[tokio::test]
    async fn bootstrap_or_recovery_failure_never_reaches_the_listener() {
        // 生产口径的 30s 就绪等待上限：DB/恢复失败时必须直接上抛，既不监听也不叠加等待，
        // 而且那次已在途的就绪等待必须被**取消**（不是留在运行时里继续等满 30s）。
        let wall = ControlledWallClock::at(SystemTime::now());
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(Arc::new(NoSampleSampler), wall),
            uuid::Uuid::new_v4(),
            Duration::from_secs(30),
        );
        let startup_gate = StartupTimeGate::start(&gate);
        let waiter = startup_gate.waiter_handle().expect("就绪等待任务应已启动");
        let listen_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_listen = Arc::clone(&listen_calls);
        let started = Instant::now();

        let result = listen_after_startup_gates(
            startup_gate,
            || async move {
                let err: Box<dyn std::error::Error> = boxed_err();
                Err::<(), _>(err)
            },
            move |()| {
                calls_for_listen.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await;

        assert!(result.is_err(), "DB/bootstrap/恢复失败必须上抛");
        assert_eq!(listen_calls.load(Ordering::SeqCst), 0, "失败即不监听");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "失败路径不得再等待时间门的 30s 上限：实际 {:?}",
            started.elapsed()
        );
        // 取消是异步生效的：给后台任务一个有界观察窗口。本用例的预算是 30s，因此在窗口内
        // 进入 finished 只可能是「失败路径把它取消了」，不可能是它自己等完了。
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while !waiter.is_finished() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            waiter.is_finished(),
            "失败路径未取消在途的时间门就绪等待，等于把 30s 预算留在后台"
        );
    }

    #[tokio::test]
    async fn pre_listen_beyond_the_ready_timeout_adds_no_extra_wait() {
        // P2 回归锁定：`wait_ready` 的超时从**被 poll 的一刻**起算，若在准备阶段之后才调用，
        // NTP 恒失败（无样本 → `SystemFallback`，既不 ready 也不通知）时总启动时长会变成
        // `pre_listen + ready_timeout`。并行等待下，准备阶段已跨过预算，门禁收敛必须耗时 ≈ 0。
        const READY_TIMEOUT: Duration = Duration::from_millis(300);
        const PRE_LISTEN: Duration = Duration::from_millis(900);
        let wall = ControlledWallClock::at(SystemTime::now());
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(Arc::new(NoSampleSampler), wall),
            uuid::Uuid::new_v4(),
            READY_TIMEOUT,
        );
        let listen_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_listen = Arc::clone(&listen_calls);

        let started = Instant::now();
        let result = listen_after_startup_gates(
            StartupTimeGate::start(&gate),
            move || async move {
                tokio::time::sleep(PRE_LISTEN).await;
                Ok::<(), Box<dyn std::error::Error>>(())
            },
            move |()| {
                calls_for_listen.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await;
        result.expect("fallback 允许启动（05 §1）");
        let total = started.elapsed();

        assert_eq!(
            listen_calls.load(Ordering::SeqCst),
            1,
            "门禁收敛后恰好监听一次"
        );
        assert!(!gate.is_ready(), "恒失败采样不得转 ready");
        // 总时长紧贴准备阶段本身（预算并行被覆盖），叠加实现会是 1200ms。
        assert!(
            total < PRE_LISTEN + Duration::from_millis(150),
            "时间门在 pre_listen 之后又叠加了等待：总耗时 {total:?}，准备阶段仅 {PRE_LISTEN:?}"
        );
        assert!(
            total >= PRE_LISTEN - Duration::from_millis(20),
            "准备阶段未真实推进，用例无效：总耗时 {total:?}"
        );
    }

    #[tokio::test]
    async fn pre_listen_within_the_ready_timeout_only_finishes_the_remainder() {
        // 半程用例：准备阶段跨过预算的一部分（250ms/500ms）时，总时长必须仍受**同一段**
        // 预算封顶（≈500ms），而不是准备耗时 + 完整预算（≈750ms）。
        const READY_TIMEOUT: Duration = Duration::from_millis(500);
        const PRE_LISTEN: Duration = Duration::from_millis(250);
        let wall = ControlledWallClock::at(SystemTime::now());
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(Arc::new(NoSampleSampler), wall),
            uuid::Uuid::new_v4(),
            READY_TIMEOUT,
        );
        let listen_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_listen = Arc::clone(&listen_calls);

        let started = Instant::now();
        listen_after_startup_gates(
            StartupTimeGate::start(&gate),
            move || async move {
                tokio::time::sleep(PRE_LISTEN).await;
                Ok::<(), Box<dyn std::error::Error>>(())
            },
            move |()| {
                calls_for_listen.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            },
        )
        .await
        .expect("fallback 允许启动（05 §1）");
        let total = started.elapsed();

        assert_eq!(listen_calls.load(Ordering::SeqCst), 1);
        assert!(
            total < READY_TIMEOUT + Duration::from_millis(80),
            "总耗时被准备阶段与完整预算叠加：实际 {total:?}，预算 {READY_TIMEOUT:?}"
        );
        assert!(
            total >= READY_TIMEOUT - Duration::from_millis(60),
            "疑似跳过时间门：实际 {total:?}，预算 {READY_TIMEOUT:?}"
        );
    }

    #[tokio::test]
    async fn injected_time_source_tracks_the_same_gate_snapshot() {
        let wall = ControlledWallClock::at(SystemTime::now());
        let clock_epoch = uuid::Uuid::new_v4();
        let sampler = Arc::new(WallFollowingSampler {
            wall: Arc::clone(&wall),
        });
        let gate = spawn_controlled_gate(
            gate_config(),
            gate_deps(sampler, Arc::clone(&wall)),
            clock_epoch,
            Duration::from_secs(30),
        );
        let source = task_time_source(&gate);
        // 先让启动采样落地（与生产监听前同一次等待），再取注入源的读数。
        assert_eq!(gate.wait_ready().await.quality, TimeQuality::NtpValid);

        let first = source.current_evidence().await;
        assert_eq!(first.quality, TimeQuality::NtpValid);
        assert_eq!(
            first.clock_epoch, clock_epoch,
            "注入源必须来自同一进程门禁纪元"
        );
        assert_eq!(first.source.as_deref(), Some(FAKE_NTP_SOURCE));

        // 墙钟回跳 10s：注入源的下一次读取必须反映同一 driver 的换代（不是缓存副本）。
        wall.set(SystemTime::now() - Duration::from_secs(10));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let current = source.current_evidence().await;
            if current.clock_epoch != clock_epoch {
                assert_ne!(current.quality, TimeQuality::Stale);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "注入源未跟踪门禁换代，等于把一次 NtpValid 当永久有效"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn startup_blocks_bootstrap_and_listener_when_schema_not_ready() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let result = start_if_ready(
            async {
                Err(rsetup_controller::ControllerError::SchemaNotReady {
                    found: Some(1),
                    required: 2,
                })
            },
            || async {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await;
        assert!(matches!(
            result
                .unwrap_err()
                .downcast_ref::<rsetup_controller::ControllerError>(),
            Some(rsetup_controller::ControllerError::SchemaNotReady {
                found: Some(1),
                required: 2
            })
        ));
        assert_eq!(calls.get(), 0);
        start_if_ready(async { Ok(()) }, || async {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(calls.get(), 1);
    }
}
