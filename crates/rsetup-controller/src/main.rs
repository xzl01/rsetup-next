//! 中控进程唯一入口（集成验收 Task 0b：全量 typed 生产装配）。
//!
//! 唯一构造点：单一 `DbPool` → 单一 `Arc<HttpAuthState>` → auth/management/tasks/
//! status/events 五条使用会话认证的线共享同一认证上下文（session 预过滤、CSRF
//! 绑定表与限速桶跨端点互相可见），metrics 是独立 Basic 认证边界，web 为静态资源线；
//! 七线再经 [`build_production_router`] 合并成唯一顶层 router。
//! 本入口不构造测试网关、`connect_lazy` 占位池、固定 offline/空能力或 authz
//! epoch 0，也不构造任何返回伪造取值的常量源；设备实时视图、准入变更通知、板端
//! `BoardClient` 与状态快照都由 [`TransportManager`]（连接管理器）与其
//! [`TransportBoardClient`] 适配真实承担；Runtime 恢复控制器真源由生产
//! [`RuntimeRecoveryController`] 装配（见 `operations` 模块），HTTP 探针与任务调度
//! 循环共享同一个实例，不存在第二套恢复模式快照。

use std::sync::Arc;

use rsetup_controller::api::adapters::build_sqlx_management_state_full;
use rsetup_controller::api::{
    install_management_cursor_secret, load_management_cursor_secret_from_env,
};
use rsetup_controller::auth::service::AuthService;
use rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository;
use rsetup_controller::authz::sqlx_repo::{
    AuthorizationSnapshotRepository, SqlxAuthorizationSnapshotRepository,
};
use rsetup_controller::events::EventBus;
use rsetup_controller::events::EventEnvelope;
use rsetup_controller::http_auth::{AppState, HttpAuthState};
use rsetup_controller::http_device_status::{DeviceStatusState, SnapshotDeviceStatusAuthorizer};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_live::LiveAuth;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::operations::RuntimeRecoveryController;
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::production::{MetricsLineInputs, ProductionInputs, build_production_router};
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::adapter::TaskTimeSource;
use rsetup_controller::tasks::evidence::SqlxTaskEvidenceApplier;
use rsetup_controller::tasks::http::HttpTasksState;
use rsetup_controller::tasks::recover_process;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::time::TimeChangedSink;
use rsetup_controller::time::evidence::TimeEvidence;
use rsetup_controller::time::{TimeGateConfig, TimeGateHandle, start_time_gate_with_notifier};
use rsetup_controller::transport::board_client::TransportBoardClient;
use rsetup_controller::transport::listener::{
    SqlxTaskResultSink, TransportEnv, run_controller_listener_with_sink,
};
use rsetup_controller::transport::manager::{ManagerConfig, TransportManager};
use rsetup_controller::web_router;
use rsetup_controller::{
    ControllerConfig, ControllerError, DbPool, bootstrap_admin, check_identity_schema,
};

#[path = "main/background.rs"]
mod background;
#[path = "main/lifecycle.rs"]
mod lifecycle;
#[path = "main/recovery.rs"]
mod recovery;
#[path = "main/secret_log.rs"]
mod secret_log;

use self::background::{BACKGROUND_GRACE, SchedulerDeps, spawn_scheduler_loop};
use self::lifecycle::{
    BoardTask, boxed, run_supervised_lifecycle, settle_background_tasks, shutdown_signal,
    watch_turns_true,
};
use self::recovery::build_recovery_controller;
use self::secret_log::LogSecretSink;

use tokio::sync::{oneshot, watch};

/// 板端 transport 线的唯一构造点：连接管理器 + 与设备状态读线**共享**的快照 store。
///
/// 三条硬约束在此固定：
/// 1. 准入真相是 `devices` 表本身（`Arc::new(db.clone())` 实现
///    [`AdmissionTruth`](rsetup_controller::transport::manager::AdmissionTruth)，只读
///    `load`，不缓存、不写库）；
/// 2. 时间证据来自**同一个** `TimeGateHandle`（`task_time_source`），管理器不新建采样器；
/// 3. 快照 store 由本函数拥有并交给 `production_inputs` 的状态读线，轮询写入方与 HTTP
///    读取方共用同一个 `Arc<SnapshotStore>`，不存在第二份新鲜度真源。
fn build_transport_manager(
    db: &DbPool,
    time_gate: &TimeGateHandle,
    recovery: Arc<RuntimeRecoveryController>,
    config: ManagerConfig,
) -> Result<(Arc<TransportManager>, Arc<SnapshotStore>), ControllerError> {
    let snapshot = Arc::new(SnapshotStore::new());
    // 构造期校验（`try_new`）：任一预算越界即失败关闭，绝不带着被静默放宽的限额启动。
    let manager = TransportManager::try_new(
        Arc::new(db.clone()),
        Arc::clone(&snapshot),
        recovery,
        task_time_source(time_gate),
        config,
    )
    .map_err(|reason| ControllerError::Config(reason.to_string()))?;
    Ok((manager, snapshot))
}

/// 从环境变量装配时间门禁配置（每次进程启动只调用一次，随 `start_time_gate` 消费）。
///
/// 基线是 [`TimeGateConfig::default()`]：`servers` 为空 → 采样器无源可用 → 启动采样在
/// 总预算内直接回退 `SystemFallback`，安全 fail-closed 与 fallback 规则完全不变。仅当
/// `NTP_SERVERS` 环境变量存在时按其切分（逗号与任意空白分隔，忽略空段）填充 `servers`。
fn time_gate_config_from_env() -> TimeGateConfig {
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
fn time_changed_sink(bus: &EventBus) -> TimeChangedSink {
    let bus = bus.clone();
    Arc::new(move |evidence| {
        bus.publish(EventEnvelope::system_time_changed(evidence));
    })
}

/// 任务线时间源的顶层唯一装配：把**同一个** `TimeGateHandle` 交给任务适配器。
///
/// `TimeGateHandle` 内部状态（权威快照 / ready / 退避计数）全在 `Arc` 后，克隆只是
/// 再拿一个句柄，因此任务线读到的与启动门禁写的是同一份动态快照：不会新建第二个
/// 采样器、不会二次启动门禁、也不会把一次 `NtpValid` 缓存成永久可信。
fn task_time_source(time_gate: &TimeGateHandle) -> Arc<dyn TaskTimeSource> {
    Arc::new(time_gate.clone())
}

/// 生产装配唯一构造点。
///
/// 顺序即门禁：运行时间零点由 `main` 第一条语句提供（**不在**本函数里构造，否则
/// uptime 会系统性低估"进程已经活多久"）→ 管理 cursor 密钥（唯一来源是环境变量，
/// 缺失/非法即 `Err`，绝不以「无 cursor 密钥」降级启动）→ 共享认证上下文 →
/// 六线状态各自 `auth.clone()`。任务线的时间证据源由 `time_gate` 注入（同一进程
/// 唯一门禁句柄），本函数不再另造时钟源；`recovery` 是恢复模式唯一真源句柄，交给
/// 任务线的提交闸门（05 §9「禁止变更任务调度/提交」），与 transport 探针、调度
/// 循环为同一个实例，本函数不新建第二套模式。
#[allow(clippy::too_many_arguments)]
fn production_inputs(
    db: &DbPool,
    metrics_config: Arc<MetricsConfig>,
    metrics_runtime: Arc<MetricsRuntime>,
    time_gate: &TimeGateHandle,
    transport: Arc<TransportManager>,
    snapshot: Arc<SnapshotStore>,
    recovery: Arc<RuntimeRecoveryController>,
    event_bus: &EventBus,
) -> Result<ProductionInputs, ControllerError> {
    let cursor_secret = load_management_cursor_secret_from_env()?;
    install_management_cursor_secret(&cursor_secret)?;

    let auth = Arc::new(HttpAuthState::new(
        Arc::new(LiveAuth::new(
            Arc::new(AuthService::new(Arc::new(SqlxIdentityRepository::new(
                db.clone(),
            )))?),
            db.clone(),
        )),
        HttpSecurityConfig::from_env()?,
    ));

    // 事件总线：进程内唯一通道句柄（broadcast 容量 128），由 `main` 构造后传入——
    // 它必须在时间门禁启动**之前**就位，时间证据变化的通知才能挂到同一条通道上。
    // SSE 读线与各条业务发布方共享同一个句柄（`Clone` 不新建通道）：任务线在任务状态变化时投
    // `task.updated`，管理线在授权变更成功时投 `permissions.changed`，时间线在 driver 每次
    // 确认新权威快照时投 `system.time.changed`（挂接点见 `main` 的 `time_changed_sink`；
    // 受众固定为管理员，见 `EventEnvelope::system_time_changed`）。
    // 除这些真实变化外，事件流上只会出现心跳与总线自身产生的 reset，不伪造任何事件。
    let events = EventsState {
        auth: auth.clone(),
        bus: event_bus.clone(),
        // 长连接权威复核周期：空闲连接也必须按会话/authz_epoch/降权切断。
        revalidate_interval: rsetup_controller::http_events::DEFAULT_REVALIDATE_INTERVAL,
    };

    // 三条 transport 源都由同一个连接管理器承担（同一份登记表，不是三份互不相干的视图）：
    // 设备实时视图只报当前已认证会话的真实状态与 typed 能力，准入变更经有界通道即时
    // 撤销活跃会话与挂起握手，恢复模式委托到生产 [`RuntimeRecoveryController`]（见
    // [`build_recovery_controller`]，与任务调度循环共享同一实例）。
    let management = build_sqlx_management_state_full(
        db.clone(),
        auth.clone(),
        transport.clone(),
        transport.clone(),
        transport.clone(),
        Some(events.bus.clone()),
        Some(time_gate.clone()),
        Some(recovery.clone()),
        Some(transport),
    );

    // 授权快照仓储：任务线与设备状态读线共用同一个只读快照入口（同一个 DbPool，
    // 不各建一份池），设备状态线的 `device.status.read` 判定只能经它求值。
    let authz_snapshots = Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone()));
    // 显式向上转型：状态线的授权器只依赖只读快照接口，不依赖 Sqlx 具体类型。
    let status_snapshots: Arc<dyn AuthorizationSnapshotRepository> = authz_snapshots.clone();

    let tasks = HttpTasksState {
        api: Arc::new(SqlxTaskApi::with_time_source(
            Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
            Arc::new(SqlxTaskRepository::new(db.0.clone())),
            Arc::clone(&authz_snapshots),
            cursor_secret,
            // 与启动门禁同一个句柄：submit 的创建证据与 time_review 的比对时钟都由它
            // 的当前快照给出（NTP 成功即被业务消费，失联/超龄即回到不可信）。
            task_time_source(time_gate),
        )),
        auth: auth.clone(),
        events: Some(events.bus.clone()),
        // 受控恢复模式闸门（05 §9「禁止变更任务调度/提交」）：与 transport 探针、
        // `/system/status`、调度循环共用同一个 `Arc<RuntimeRecoveryController>`，
        // 提交端点每条请求都实时读它的当前模式，不缓存模式快照。
        recovery: Some(recovery),
    };

    Ok(ProductionInputs {
        auth: AppState {
            db: db.clone(),
            auth: auth.clone(),
        },
        management,
        tasks,
        events,
        status: DeviceStatusState {
            auth,
            // 真实 typed 内存快照源（polling::SnapshotStore）：代际/序号校验 + 单调
            // 30s 新鲜度的本进程缓存。写入方是 transport 线的轮询驱动（同一个
            // `Arc<SnapshotStore>`，由 `build_transport_manager` 拥有），因此不存在第二
            // 份新鲜度真源；每条设备都必须先由认证连接管理器 confirm_generation，未确认
            // 代际的 update 一律 NoActiveGeneration 拒绝（代际不由样本自行建立）。尚无样本
            // 的设备真实地报告 snapshot=null / freshness=unknown / age_ms=null。
            snapshot,
            // H1：`device.status.read` 的对象级授权判定（02 §5 + 01 §4/A-06）。
            // 与任务线共用同一个授权快照仓储；缓存有无样本都不改变该判定，
            // 因此无样本不再可能像此前那样被任何已登录用户读到。
            authorizer: Arc::new(SnapshotDeviceStatusAuthorizer::new(status_snapshots)),
        },
        metrics: MetricsLineInputs {
            config: metrics_config,
            runtime: metrics_runtime,
        },
        web: web_router(),
    })
}

async fn start_if_ready<C, F, Fut>(check: C, start: F) -> Result<(), Box<dyn std::error::Error>>
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
struct StartupTimeGate {
    /// 门禁句柄克隆：与唯一 driver 共享 ready/权威快照，供日志与收敛读取。
    gate: TimeGateHandle,
    /// 已在计时的后台 `wait_ready` 任务；`Option` 只为让 `Drop` 与 `finish` 都能取走它。
    waiter: Option<tokio::task::JoinHandle<TimeEvidence>>,
}

impl StartupTimeGate {
    /// 从这一刻起开始并行等待就绪。**必须**紧跟 `start_time_gate` 之后、任何阻塞式启动
    /// 准备工作（建池/schema/bootstrap/恢复/装配）之前调用，否则 30s 预算又会推迟到那些
    /// 工作结束之后重新起算。
    fn start(gate: &TimeGateHandle) -> Self {
        let waiting_gate = gate.clone();
        let waiter = tokio::spawn(async move { waiting_gate.wait_ready().await });
        Self {
            gate: gate.clone(),
            waiter: Some(waiter),
        }
    }

    /// 门禁是否已转 ready（不等待，也不消费等待预算）。
    fn is_ready(&self) -> bool {
        self.gate.is_ready()
    }

    /// 收敛时间启动门：只补等**剩余**预算，然后按收敛时刻的权威快照给出证据。
    ///
    /// 后台任务被取消或 panic 都不上抛——门禁状态由权威快照如实表达（不可信即
    /// `SystemFallback`/`Stale`），绝不 panic、也不把「没等到」伪装成 ready。
    async fn finish(&mut self) -> TimeEvidence {
        if let Some(waiter) = self.waiter.as_mut() {
            let _ = waiter.await;
        }
        // 证据以收敛这一刻的快照为准：准备阶段比预算更慢时，`wait_ready` 的返回值是
        // 预算耗尽时刻的读数，直接沿用会把几秒前的墙钟当本刻读数（`current()` 对
        // `SystemFallback`/`Stale` 会刷新为当前墙钟，并动态派生样本年龄）。
        self.gate.current().await
    }

    /// 后台等待任务的取消句柄克隆（仅供测试观察「失败路径是否真的取消了在途等待」；
    /// 不参与生产逻辑，构造句柄本身不会取消任务）。
    #[cfg(test)]
    fn waiter_handle(&self) -> Option<tokio::task::AbortHandle> {
        self.waiter
            .as_ref()
            .map(tokio::task::JoinHandle::abort_handle)
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
async fn listen_after_startup_gates<R, P, PF, L, LF>(
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 运行时间零点：main 函数体第一条语句即记录（`MetricsRuntime::new` 内部取
    // `Instant::now()` 作单调锚点），包含配置解析、建池、schema 检查与绑端口在
    // 内的整个启动过程都计入 uptime（§5.2 语义「进程启动至今」）。
    let metrics_runtime = Arc::new(MetricsRuntime::new());
    let config = ControllerConfig::from_env()?;
    // 指标配置门禁：在连接数据库与绑定端口之前完成（§3.6 fail-closed 无副作用）。
    let metrics_config = Arc::new(MetricsConfig::from_env()?);
    // NTP 时间门禁：在建立数据库连接与启动任何生产服务之前启动（每次进程启动只调用一次）。
    // 事件总线**必须先于门禁**构造：它是进程内唯一通道句柄，门禁的「时间证据已变化」通知与
    // 后面 SSE 读线、任务线、管理线用的必须是同一个句柄（Clone 不新建通道），因此不能等到
    // `production_inputs` 里再建。
    let event_bus = EventBus::new();
    // 单调进程时钟纪元 clock_epoch 只在此生成一次；`start_time_gate_with_notifier` 在 tokio
    // 运行时内 spawn 后台 driver（startup 预算采样，就绪或超时后对外提供时间快照），本进程
    // 全程持有 handle。driver 每次确认新的权威时间证据，都经 `time_changed_sink` 向总线投
    // `system.time.changed`（06 §4，受众固定为管理员）。
    // servers 为空（未设 NTP_SERVERS）即走 SystemFallback，安全 fail-closed 与 fallback 规则不变。
    let clock_epoch = uuid::Uuid::new_v4();
    let time_gate = start_time_gate_with_notifier(
        time_gate_config_from_env(),
        clock_epoch,
        Some(time_changed_sink(&event_bus)),
    );
    // 就绪等待**从这一刻就开始**：`StartupTimeGate::start` 立即 spawn 一次 `wait_ready`，让
    // `ready_timeout`（生产 30s）与下面的建池、schema 检查、admin bootstrap、启动恢复、
    // 生产装配全程并行消耗，任一步失败即随栈展开取消（见 `StartupTimeGate` 的时序说明）。
    // 放在 `DbPool::connect` 之前是刻意的：`wait_ready` 的超时从被 poll 的一刻起算，若等到
    // `listen_after_startup_gates` 内部才调用，慢 DB + NTP 恒失败就会在准备耗时之外再叠加
    // 一整段 30s。
    let startup_time_gate = StartupTimeGate::start(&time_gate);
    eprintln!(
        "controller 时间门禁已启动: clock_epoch={clock_epoch} ready={}",
        time_gate.is_ready()
    );
    let db = DbPool::connect(&config).await?;
    let secret_path = std::env::var_os("CONTROLLER_BOOTSTRAP_SECRET_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "controller-bootstrap-secret.log".into());
    // DbPool 是 Arc 句柄：clone 只是把**同一个池**移交启动闭包（schema 检查仍借用
    // 外层句柄），不新建池。
    let db_handle = db.clone();
    // 时间门禁句柄同样是 Arc 语义的克隆：整个进程只有 `start_time_gate` 那一个 driver，
    // 任务线注入与监听前收敛共用它（克隆指向同一份权威快照/ready 状态）。
    let task_line_gate = time_gate.clone();
    let listen_address = config.listen_address.clone();
    // transport 线唯一构造点（在启动恢复之前）：恢复路径只需要一个 BoardClient，
    // 而 BoardClient 与后台任务、监听共享同一个管理器与同一个快照 store。
    // 板端接入的环境装配**上提到这里**：管理器预算与监听预算必须是同一份（审查 P1-5），
    // 否则 `TransportConfig` 解析出的半开/会话上限与 `ManagerConfig::default()` 各算各的。
    // 非法（含非 UTF-8、空串、0、超硬上界）一律 `Err` → 进程失败关闭，绝不回退默认值；
    // 只有 `RSETUP_CONTROLLER_KEY_FILE` **完全未设置**才是明确的「不启用板端接入」。
    let transport_env = TransportEnv::from_env()?;
    let manager_config = match &transport_env {
        TransportEnv::Enabled(config) => config.manager_config(),
        TransportEnv::Disabled => ManagerConfig::default(),
    };
    // 恢复模式唯一真源（05 §9）：transport 线探针、HTTP `/system/status` 与下面的任务
    // 调度循环共享同一个 `Arc<RuntimeRecoveryController>`，不存在第二套模式快照。
    let recovery_controller = build_recovery_controller();
    let recovery_for_scheduler = Arc::clone(&recovery_controller);
    // 任务提交闸门的句柄：与调度循环、transport 探针同一个实例（同一模式真源）。
    let recovery_for_tasks = Arc::clone(&recovery_controller);
    let (transport_manager, transport_snapshot) = build_transport_manager(
        &db_handle,
        &task_line_gate,
        recovery_controller,
        manager_config,
    )?;
    let transport_for_router = Arc::clone(&transport_manager);
    let db_for_transport = db_handle.clone();
    start_if_ready(check_identity_schema(&db), move || async move {
        bootstrap_admin(&db_handle, &LogSecretSink(secret_path)).await?;
        // 启动恢复：schema 校验通过 + 管理员 bootstrap 完成后、对外监听之前，对未决任务做
        // 只读核验（阶段 1 只读短事务读未决集 → 阶段 2 事务外板端核验 → 阶段 3 写入短事务落
        // recovery_checks 与 queued→held）。板端尚未建立连接时，
        // task_record 真实回未连接错误，被记为未核验但绝不让恢复上抛；恢复流程不下发任务。
        // 失败即上抛（fail-closed），未对账完成前不进入对外服务。
        let process_epoch = uuid::Uuid::new_v4();
        let task_repo = SqlxTaskRepository::new(db_handle.0.clone());
        // 板端 `BoardClient` 的真实实现：六方法经连接管理器按**已验签公钥的当前会话**路由。
        // 此刻进程刚起来、监听尚未开始，登记表必为空 → `task_record` 真实回
        // `Err(DeviceOffline)`，经 `query_board_task_record` 记为「未核验」但绝不让恢复
        // 上抛（与旧的 fail-closed 装配位语义一致）；恢复流程永不产生任务下发（prepare/execute），
        // 此刻登记表为空，核验也只立即返回未连接，不出网。
        let board_client = TransportBoardClient::new(transport_manager.as_device_rpc());
        let recovery = recover_process(&task_repo, &board_client, process_epoch).await?;
        eprintln!(
            "controller 启动恢复对账完成: process_epoch={process_epoch} \
             已核验={} 置held={} 置expired={} 发送={}",
            recovery.read_only_verified,
            recovery.marked_held,
            recovery.marked_expired,
            recovery.execute_calls,
        );
        let db_for_scheduler = db_handle.0.clone();
        listen_after_startup_gates(
            // 时间这条线：自 `start_time_gate` 之后就在并行计时的等待句柄（唯一一次
            // `wait_ready`，预算不叠加）。
            startup_time_gate,
            // DB 这条线（bootstrap + 恢复 + 唯一生产装配点）：任一失败即上抛，不监听。
            || async move {
                // 唯一生产装配点：认证四线共享同一 Arc<HttpAuthState>（指标线是独立认证
                // 边界），指针不一致即失败关闭。任务线时间证据源注入同一个门禁句柄。
                let router = build_production_router(production_inputs(
                    &db_handle,
                    metrics_config,
                    metrics_runtime,
                    &task_line_gate,
                    transport_for_router,
                    transport_snapshot,
                    // 恢复模式唯一真源的第三个消费方：任务提交闸门（05 §9）。
                    recovery_for_tasks,
                    // 与门禁通知同一个通道句柄：SSE 读线因此收得到时间线投出的事件。
                    &event_bus,
                )?)?;
                Ok::<_, Box<dyn std::error::Error>>(router)
            },
            // 联合门禁全部完成后才绑端口。
            move |router| async move {
                let listener = tokio::net::TcpListener::bind(&listen_address).await?;
                // 单一关停广播：HTTP 优雅关停与 transport 后台任务/监听共用一个 watch，
                // 撤销通道消费、准入复查、轮询驱动与板端监听都随它退出。
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                // 后台四任务：撤销消费 / 准入复查 / 轮询驱动 / 任务调度与持续核验，只等同一个关停 watch。
                let revoke_task = transport_manager.spawn_revoke_drain(shutdown_rx.clone());
                let recheck_task = transport_manager.spawn_admission_recheck(shutdown_rx.clone());
                let poll_task = transport_manager.spawn_poll_driver(shutdown_rx.clone());

                // 启动生产任务调度与持续核验循环
                let sched_task = spawn_scheduler_loop(
                    SchedulerDeps {
                        pool: db_for_scheduler,
                        transport: transport_manager.clone(),
                        process_epoch,
                        recovery: recovery_for_scheduler,
                    },
                    shutdown_rx.clone(),
                );

                // OS 关停信号 → 广播到**同一个** watch（HTTP 优雅关停、后台任务、板端监听共用它）。
                // 关键（审查 N1）：HTTP 的优雅关停不再只等 OS 信号，而是观察这个 watch，于是
                // 「board 意外退出」也能驱动 HTTP 收尾；OS 信号只是这个 watch 的**一个**来源。
                tokio::spawn(shutdown_signal(shutdown_tx.clone()));
                // 板端接入监听（审查 P1-3 / N1）：`BoardTask` 真实持有 `JoinHandle` + 结果 oneshot
                // （句柄 `drop` 只 detach，绝不等于「已退出」）。监听的 `Err`/panic/提前退出经下面的
                // 监督即时广播关停，绝不「bind 失败只存 done_rx、HTTP 永不终止就静默半死」。
                let board_task: Option<BoardTask> = match transport_env {
                    TransportEnv::Enabled(transport_config) => {
                        eprintln!(
                            "controller 板端接入监听启动: {} (max_half_open={} max_sessions={})",
                            transport_config.listen,
                            transport_config.manager.max_half_open,
                            transport_config.manager.max_sessions
                        );
                        let listen_manager = Arc::clone(&transport_manager);
                        let listen_db = db_for_transport.clone();
                        let board_shutdown = shutdown_rx.clone();
                        let (done_tx, done_rx) = oneshot::channel();
                        let applier =
                            Arc::new(SqlxTaskEvidenceApplier::new(listen_db.0.clone(), None));
                        let sink = SqlxTaskResultSink::new(applier);
                        let handle = tokio::spawn(async move {
                            let outcome = run_controller_listener_with_sink(
                                transport_config,
                                listen_db,
                                listen_manager,
                                board_shutdown,
                                sink,
                            )
                            .await;
                            let _ = done_tx.send(outcome);
                        });
                        Some(BoardTask::new(handle, done_rx))
                    }
                    TransportEnv::Disabled => {
                        // 唯一的「不启用」语义：身份文件**完全未设置**。管理/状态线照常服务，
                        // 设备侧如实报告不在线；非法配置在 `TransportEnv::from_env` 就已失败关闭。
                        eprintln!(
                            "controller 板端接入监听未启用: RSETUP_CONTROLLER_KEY_FILE 未设置"
                        );
                        None
                    }
                };
                // HTTP 生命周期与板端监听放**同一次受控监督**（审查 N1 / RC2）：
                // * HTTP 的优雅关停由 watch 驱动（`watch_turns_true`）——OS 信号或 board 意外退出都会
                //   让它收尾；HTTP future 被 `Box::pin` 持有，select 只借用、绝不丢弃未收尾的它；
                // * board 交出的始终是**任务凭证本身**（`BoardTask`）加宽限期常量，由监督在**关停意图
                //   确立之后**才调用 `finish_board` 结清（监听阶段不武装超时＝RC2 修复点）。这里绝
                //   不能再提前 `finish_board(..)` 把已起表的 future 塞给监督——那正是 RC2 根因。
                let http_graceful = shutdown_rx.clone();
                let http = async move {
                    axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .with_graceful_shutdown(watch_turns_true(http_graceful))
                    .await
                    .map_err(boxed)
                };
                let (http_result, board_error) =
                    run_supervised_lifecycle(http, board_task, &shutdown_tx, shutdown_rx.clone())
                        .await;
                // 后台四任务收敛（审查 N2）：关停广播已由 `supervise_shutdown` 发出，给宽限期让它们
                // **自己**退出，超期直接 abort **原始 `JoinHandle`** 并 await 确认退出（不是 abort 一个
                // wrapper、把真实 owner detach）。
                settle_background_tasks(
                    vec![revoke_task, recheck_task, poll_task, sched_task],
                    BACKGROUND_GRACE,
                )
                .await;
                // 错误优先级：HTTP 自身失败先报（它决定端口是否可用），板端监听失败随后。
                if let Err(error) = http_result {
                    let error: Box<dyn std::error::Error> = error;
                    return Err(error);
                }
                if let Some(error) = board_error {
                    let error: Box<dyn std::error::Error> = error;
                    return Err(error);
                }
                Ok::<(), Box<dyn std::error::Error>>(())
            },
        )
        .await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Instant;
    use std::time::{Duration, SystemTime};

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

    #[test]
    fn default_cargo_run_selects_readonly_server() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "default-run = \"rsetup-controller\"")
        );
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
