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

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

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
use rsetup_controller::tasks::scheduler::{SchedulerPolicy, SystemMonoClock, TaskScheduler};
use rsetup_controller::tasks::scheduler_store::SqlxSchedulerStore;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::time::TimeChangedSink;
use rsetup_controller::time::evidence::TimeEvidence;
use rsetup_controller::time::{TimeGateConfig, TimeGateHandle, start_time_gate_with_notifier};
use rsetup_controller::transport::board_client::TransportBoardClient;
use rsetup_controller::transport::listener::{
    ListenerError, SqlxTaskResultSink, TransportEnv, run_controller_listener_with_sink,
};
use rsetup_controller::transport::manager::{ManagerConfig, TransportManager};
use rsetup_controller::web_router;
use rsetup_controller::{
    BootstrapSecretSink, ControllerConfig, ControllerError, DbPool, bootstrap_admin,
    check_identity_schema,
};

use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

struct LogSecretSink(std::path::PathBuf);
impl BootstrapSecretSink for LogSecretSink {
    fn emit(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), rsetup_controller::ControllerError> {
        write_private_secret(&self.0, username, password).map_err(|error| {
            rsetup_controller::ControllerError::Config(format!(
                "unable to record first-boot secret in private file: {error}; administrator recovery requires separate reviewed procedure"
            ))
        })
    }
}

/// 冷启动进入受控恢复模式的外部环境变量（05 §9）。
///
/// 旧备份恢复后需要由运维**显式**让进程以受控模式启动（撤销会话、暂停自动
/// 准入/变更调度、禁止变更任务提交），再经管理员逐项核对解除。取值 `"1"` 或
/// `"true"`（大小写不敏感、首尾空白忽略）即触发；未设置或其他取值都是正常启动，
/// **不做任何猜测式推断**（不因为「刚从备份恢复」就自动进入）。
const CONTROLLED_RECOVERY_ENV: &str = "CONTROLLER_CONTROLLED_RECOVERY";

/// 外部环境变量是否明确要求冷启动进入受控恢复模式。
///
/// 只读一个已知键、只认两个已知取值；非 UTF-8 的取值一律按「未要求」处理，
/// 不回显原始取值（可能含非预期内容），日志只打固定的去敏文案。
fn controlled_recovery_requested() -> bool {
    controlled_recovery_flag(std::env::var_os(CONTROLLED_RECOVERY_ENV).as_deref())
}

/// 对单个环境变量原始取值求值：仅 `"1"`/`"true"`（大小写不敏感、忽略首尾空白）
/// 为真。非 UTF-8 取值与未设置同为假，不回显原值。
fn controlled_recovery_flag(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|raw| {
        raw.to_str()
            .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
    })
}

/// Runtime 恢复控制器真源的生产装配形态（05 §9 / 04 §5.2）。
///
/// [`RecoveryModeProbe`] 的契约要求 `ready`/`mode` 必须来自 Runtime 当前真实快照：
/// [`RuntimeRecoveryController`] 就是这份快照本身（进程内唯一实例，正常模式启动，
/// 管理员可进入/解除受控恢复模式），绝不返回伪造取值，也不造第二套恢复模式真源。
/// [`TransportManager::current_mode`] 只委托到本装配位。
///
/// 冷启动触发：[`CONTROLLED_RECOVERY_ENV`] 置 `"1"`/`"true"` 时，本函数在返回句柄
/// **之前**就调用 [`RuntimeRecoveryController::enter_controlled_recovery`]，因此
/// transport 探针、`/system/status`、调度循环与任务提交闸门自启动第一刻起读到的
/// 都是 `controlled_recovery`——不存在「先以正常模式对外服务、再补切模式」的窗口。
fn build_recovery_controller() -> Arc<RuntimeRecoveryController> {
    new_recovery_controller(controlled_recovery_requested())
}

/// 装配形态的可测内核：`controlled_recovery = true` 即在返回前进入受控恢复模式。
fn new_recovery_controller(controlled_recovery: bool) -> Arc<RuntimeRecoveryController> {
    let controller = Arc::new(RuntimeRecoveryController::new());
    if controlled_recovery {
        controller.enter_controlled_recovery();
        eprintln!("controller 启动处于受控恢复模式 (CONTROLLER_CONTROLLED_RECOVERY=1)");
    }
    controller
}

/// 后台任务（撤销消费 / 准入复查 / 轮询驱动）的关停宽限期：三者只等 watch，不做 I/O，
/// 因此正常路径下应在本窗口内自行退出；超期才 abort 并 await 确认。
const BACKGROUND_GRACE: Duration = Duration::from_secs(2);
/// 板端监听关停的**外部**硬上界：监听内部已有自己的排空宽限期（`drain_grace` ≥ §6.6 的
/// 5s + 余量）与收尾 await，本常量只兜底「内部排空也卡住」的异常，绝不能小于内部宽限期。
const BOARD_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

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

fn write_private_secret(
    path: &std::path::Path,
    username: &str,
    password: &str,
) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    // create_new + 0600 protect the file, not its parent: deploy under a private directory.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{username}: {password}")?;
    file.sync_all()
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

/// 关停信号：Ctrl-C 或 SIGTERM 任一到达即置真 shutdown watch（唯一的关停真源）。
///
/// 关停只做**本进程侧**的收敛：停止接新连接、请求已认证会话排空（h2 GOAWAY，板端按 §7
/// 重连到新进程）、取消后台任务。绝不重发任何已发出的变更，也不写任何持久决定。
async fn shutdown_signal(shutdown: watch::Sender<bool>) {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        // `ctrl_c` 的产出是 `io::Result<()>`：忽略结果即可（任一信号到达都走同一条关停路径）。
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("controller 收到关停信号，开始优雅关停");
    let _ = shutdown.send(true);
}

// ---------------------------------------------------------------------------
// 顶层生命周期监督（审查 N1 / N2、RC2）：**生产 main 与测试共用同一实现**，不是在测试里复制逻辑。
//
// 关停宽限期只允许在**关停意图确立之后**武装（`supervise_shutdown` 的关停阶段）；把已经起表的
// `finish_board` future 在监听阶段交给监督并发 poll 就是 RC2 的真实缺陷（正常运行满宽限期即杀掉
// 健康的板端监听）。签名因此只收 `BoardTask` + `grace`，不收「已开始的结清 future」。
// ---------------------------------------------------------------------------

/// 装箱后的错误（`Send + 'static`，便于跨任务/监督边界传递；`main` 顶层再收敛为 `Box<dyn Error>`）。
type BoxedError = Box<dyn std::error::Error + Send + 'static>;

/// 把具体错误装箱为 [`BoxedError`]。
fn boxed<E>(error: E) -> BoxedError
where
    E: std::error::Error + Send + 'static,
{
    Box::new(error)
}

/// 板端监听任务的**所有权凭证**（生产由 `main` 构造，监督全程持有直到结清）。
///
/// 契约：监听任务体把最终结果写回 `done` 的**下一步就是结束**（见 `main` 的 spawn 站点），
/// 监督因此在结果到达后仍 `await` 原始句柄确认它真的退出（`JoinHandle` 被 `drop` 只会
/// detach，绝不等于「已退出」）。
struct BoardTask {
    /// **原始**监听任务的句柄：结清时要么 join，要么 `abort` + `await` 确认。
    handle: JoinHandle<()>,
    /// 结果回执。监听阶段一旦被监督观察到就置 `None`（结果转入 `observed`），从结构上杜绝
    /// 「重复 poll 已完成的接收端」。
    done: Option<oneshot::Receiver<Result<(), ListenerError>>>,
    /// 监督在监听阶段观察到的**原始**回执（外层 `Err` ＝ 发送端被丢弃＝任务 panic/被取消）。
    observed: Option<BoardOutcome>,
}

/// 监听回执的原始形态（外层 `Err(RecvError)` ＝ 监听任务 panic / 被取消）。
type BoardOutcome = Result<Result<(), ListenerError>, oneshot::error::RecvError>;

impl BoardTask {
    fn new(handle: JoinHandle<()>, done: oneshot::Receiver<Result<(), ListenerError>>) -> Self {
        Self {
            handle,
            done: Some(done),
            observed: None,
        }
    }

    /// 监听阶段是否仍需观察这条任务（回执已被取走 ⇒ 不再观察）。
    fn watchable(&self) -> bool {
        self.done.is_some()
    }

    /// 监听阶段的观察分支：等**原始回执**，**不武装任何超时**。回执已被取走时永远 pending。
    async fn observe(&mut self) -> BoardOutcome {
        match self.done.as_mut() {
            Some(done) => done.await,
            None => std::future::pending::<BoardOutcome>().await,
        }
    }
}

/// [`BoardTask::observe`] 的 `Option` 包装，供 `select!` 直接使用。
///
/// `select!` 每轮都会**构造**所有分支的 future（只是不 poll 被禁用的分支），所以构造必须无
/// 副作用：这里对 `None` 只返回一个 pending future，绝不 unwrap。
async fn observe_board(board: Option<&mut BoardTask>) -> BoardOutcome {
    match board {
        Some(task) => task.observe().await,
        None => std::future::pending::<BoardOutcome>().await,
    }
}

/// 已武装时等完**剩余**宽限期；未武装（监听阶段）时永远 pending。
async fn sleep_or_pending(remaining: Option<Duration>) {
    if let Some(remaining) = remaining {
        tokio::time::sleep(remaining).await;
    }
}

/// `watch` 变 `true`（或发送端被丢弃）时结束。作为 HTTP `with_graceful_shutdown` 的驱动：让
/// 「外部关停信号」与「board 意外退出」两条关停来源**共用同一个优雅退出通道**（审查 N1）。
/// 发送端被关闭按「已关停」处理＝fail-closed，绝不永久挂起。
async fn watch_turns_true(mut rx: watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        // 发送端被丢弃：没有任何一方会再置真，按「已关停」处理，绝不永久挂起。
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// 结清板端监听（审查 P1-3 / N1 / RC2）：`grace` 是**从关停意图确立时刻**算起的**剩余**宽限期，
/// 因此只允许在关停意图确立之后调用（[`supervise_shutdown`] 的关停阶段）；监听阶段调用它就是
/// RC2 根因。产出 `Some(err)` 交给顶层返回，`None` 表示正常结束。
async fn finish_board(board: Option<BoardTask>, grace: Duration) -> Option<BoxedError> {
    // `?` 而不是 `let..else`：`None`＝「没有板端监听需要结清」，与产出类型同为 `Option`。
    let BoardTask {
        handle,
        done,
        observed,
    } = board?;
    // 回执已被监督取走（`observed`）⇒ **绝不**重复 poll。否则在剩余宽限期内等它**自己**跑完
    // GOAWAY 排空与收尾：`timeout` 先 poll 内部 future，所以宽限期剩 0 时刚到达的结果仍被采纳。
    let outcome = match (done, observed) {
        (Some(done), None) => match tokio::time::timeout(grace, done).await {
            Ok(outcome) => outcome,
            // 超期：内部排空本身也卡住，兜底 abort **原始任务** + await 确认退出。
            Err(_elapsed) => {
                handle.abort();
                let _ = handle.await;
                return Some(boxed(std::io::Error::other(
                    "板端接入监听未在剩余宽限期内完成排空（已 abort 原始任务并确认退出）",
                )));
            }
        },
        (None, Some(outcome)) => outcome,
        // 结构上不可达（`done`/`observed` 由监督成对维护）；真到此处按「回执不可用」失败关闭：
        // 结清原始任务后上抛错误，既不 panic 也不静默放行。
        (Some(_), Some(_)) | (None, None) => {
            handle.abort();
            let _ = handle.await;
            return Some(boxed(std::io::Error::other(
                "板端接入监听回执不可用（已 abort 原始任务并确认退出）",
            )));
        }
    };
    match outcome {
        // board 自己返回：`Err` 是失败，`Ok` 是收到关停后的正常收尾。结果已产出**仍要 join
        // 原始任务**（任务体在写回结果后立刻结束），drop 句柄只是 detach、不确认它已退出。
        Ok(result) => {
            let error = result.err().map(boxed);
            match handle.await {
                Ok(()) => error,
                Err(join) => error.or_else(|| {
                    Some(boxed(std::io::Error::other(format!(
                        "板端接入监听任务未正常退出（{join}）"
                    ))))
                }),
            }
        }
        // 发送端被丢弃：监听任务 panic 或被外部取消。句柄仍要结清（abort + await 确认）。
        Err(_recv) => {
            handle.abort();
            let _ = handle.await;
            Some(boxed(std::io::Error::other(
                "板端接入监听任务提前结束（panic 或被取消）",
            )))
        }
    }
}

/// N1 / RC2：把 HTTP 生命周期与板端监听放进**同一次受控监督**，两阶段收尾。
///
/// **监听阶段**（`armed == None`）只观察三路事件，**不武装任何关停超时**：
/// * HTTP 自身结束（正常关停或失败）；
/// * 板端监听的**原始回执**（`Err`、发送端被丢弃＝panic/取消、或未经请求的提前 `Ok`）——任一
///   结束都**立即**广播关停，驱动仍在跑的 HTTP 走优雅退出（HTTP 的 `with_graceful_shutdown`
///   由同一个 watch 驱动，见 [`watch_turns_true`]），绝不「静默半死」；
/// * OS 关停 watch 变 `true`（真实唯一的外部关停源；发送端被关闭按 fail-closed 关停处理）。
///
/// **关停阶段**：关停意图确立的**那一刻**才起表（RC2 根因正是旧接线在监听器启动时就把带宽限期
/// 的 [`finish_board`] future 交给本函数并发 poll，于是进程正常运行满 `BOARD_SHUTDOWN_GRACE`
/// 就 abort 掉完全健康的板端监听、广播关停、HTTP 拒连并非 0 退出）。board 侧在**剩余**宽限期内
/// 由 [`finish_board`] 结清，超期 `abort` **原始任务**并 `await` 确认。
///
/// 其余不变量：
/// * 宽限期锚在**关停意图**上而不是「HTTP 全部等完」：HTTP 仍有活跃长请求时，板端的有界清理
///   照常超期 abort，不被延后；
/// * HTTP 的原始 future 始终被 `Box::pin` **持有**，`select!` 只借用；板端结清后继续 await
///   同一个 future 直到收尾完成，绝不丢弃/detach 未收尾的 HTTP；
/// * 每个分支一旦观察完即被禁用（回执从 `BoardTask` 上摘走），绝不重复 poll 同一 future、
///   绝不双消费、绝不吞异常；即使「外部关停」与「board 结果」同轮 ready 也只消费一次；
/// * 板端提前 `Ok` 算不算「未经请求」，只在 board 完成分支**自己广播关停之前**取外部证据（已
///   武装，或外部关停 watch 的当前值已为 `true`）：`biased` 把该分支排在 watch 分支之前，同轮
///   ready 时 `armed` 尚未起表，用自己刚写进去的 `true` 判定会把正常的 SIGTERM 关停误记成失败；
///   反过来，也没有任何外部证据的提前 `Ok` 一律 fail-closed 上抛（见 `rc2_board_ok_*` 两条用例）；
/// * 没有板端监听（`None`）时路径同样正确：HTTP 结束或关停信号 → 广播 → 返回。
///
/// 返回 `(http 结果, board 错误)`；错误优先级由调用方决定（HTTP 自身失败优先，board 随后）。
async fn supervise_shutdown<H>(
    http: H,
    mut board: Option<BoardTask>,
    grace: Duration,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> (Result<(), BoxedError>, Option<BoxedError>)
where
    H: Future<Output = Result<(), BoxedError>>,
{
    let mut http = Box::pin(http);
    // 关停意图确立的时刻：`None`＝仍在监听阶段，不武装 board 宽限期。进入时 watch 已为 `true`
    // （SIGTERM 落在绑端口与进入监督之间的真实竞态）即按「意图已确立」起表。
    let mut armed: Option<tokio::time::Instant> = if *shutdown_rx.borrow() {
        Some(tokio::time::Instant::now())
    } else {
        None
    };
    // HTTP 原始 future 的结果（`None`＝仍在途）。
    let mut http_result: Option<Result<(), BoxedError>> = None;
    // 板端在**任何关停意图之前**就自行返回 `Ok` 的 fail-closed 错误（监听已死但没有错误上抛）。
    let mut unprompted: Option<BoxedError> = None;

    loop {
        let board_watchable = board.as_ref().is_some_and(BoardTask::watchable);
        // 已武装 ⇒ 剩余宽限期；未武装 ⇒ `None`（监听阶段不设任何超时）。
        let drain_wait = armed.map(|at| {
            let elapsed = tokio::time::Instant::now()
                .checked_duration_since(at)
                .unwrap_or_default();
            grace.saturating_sub(elapsed)
        });
        tokio::select! {
            biased;
            result = http.as_mut(), if http_result.is_none() => {
                http_result = Some(result);
                // HTTP 自身结束（正常或失败）也要关停：驱动 board 与后台三任务退出。
                let _ = shutdown_tx.send(true);
                armed.get_or_insert_with(tokio::time::Instant::now);
            }
            outcome = observe_board(board.as_mut()), if board_watchable => {
                // 摘走回执并记下**原始**结果：之后这一分支被禁用，绝不再 poll 同一条 future。
                //
                // 「关停意图是否已确立」必须在**本分支广播关停之前**取证据，而且只认**本分支之外**
                // 的来源：本分支下一行的 `send(true)` 是**自己**广播的关停，若拿它之后的 watch 值来
                // 判定，就把真正未经请求的提前 `Ok` 也当成意图（fail-open）。两条外部证据：
                // ① 之前轮次已武装（HTTP 先结束，或 watch 分支已处理过关停）；
                // ② 进入本分支**此刻** watch 的当前值已为 `true`。`biased` 顺序把本分支排在 watch
                //    分支**之前**：外部关停（SIGTERM → 同一个 watch）与板端回执在同一轮 select 之前
                //    同时 ready 时，先执行的是这里，watch 分支还没机会起表，`armed` 仍是 `None`——
                //    这一轮读到的当前值就是那条缺失的外部证据（本轮 Low 的根因）。
                // 发送端被关闭的 fail-closed 约定只作用在 [`watch_turns_true`]：本函数按**引用**持有
                // 发送端（`&watch::Sender`），它在整个监督期间必然活着，`closed` 在此不可达，因此
                // 不把它列成证据来源（真到 `closed` 也只会落回 fail-closed 一侧）。
                let external_shutdown = armed.is_some() || *shutdown_rx.borrow();
                let _ = shutdown_tx.send(true);
                if let Some(task) = board.as_mut() {
                    task.done = None;
                    if !external_shutdown && matches!(outcome, Ok(Ok(()))) {
                        unprompted = Some(boxed(std::io::Error::other(
                            "板端接入监听在收到任何关停意图前提前结束（已广播关停）",
                        )));
                    }
                    task.observed = Some(outcome);
                }
                armed.get_or_insert_with(tokio::time::Instant::now);
            }
            _ = watch_turns_true(shutdown_rx.clone()), if armed.is_none() => {
                // OS 关停信号 ⇒ **此刻**才起表等 board 有界排空（监听阶段永不到点）。
                armed = Some(tokio::time::Instant::now());
            }
            _ = sleep_or_pending(drain_wait), if board_watchable && drain_wait.is_some() => {
                // 剩余宽限期到点而 board 仍在途：立刻去结清（abort 原任务 + await 确认），
                // 不因 HTTP 仍有活跃长请求而延后板端的有界清理。
                break;
            }
            else => break,
        }
    }

    // ===== 关停阶段 =====
    // 到这里关停意图必然已确立（HTTP 结束/失败、OS 关停信号、board 结束三者之一），宽限期从
    // **意图确立时刻**起算，这里取剩余量（HTTP 仍在收尾也照常倒计时）。
    let remaining = armed.map_or(grace, |at| {
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(at)
            .unwrap_or_default();
        grace.saturating_sub(elapsed)
    });
    let board_error = finish_board(board, remaining).await;
    // HTTP 原始 future 若仍在途，板端结清后仍要把它收尾完成：`http` 全程被 `Box::pin` 持有，
    // 这里继续 await **同一个** future，绝不 detach。
    let http_result = match http_result {
        Some(result) => result,
        None => http.as_mut().await,
    };
    (http_result, board_error.or(unprompted))
}

/// **生产生命周期接线**（`main` 与回归测试共用同一个函数）：HTTP serve future + 板端任务凭证 +
/// 关停 watch → 监督结果；`BOARD_SHUTDOWN_GRACE` 也只在这里绑定一次。
///
/// 之所以要有这层，而不是让测试各自拼 `supervise_shutdown`：上一轮独立复审的盲点正是「helper
/// 单测全绿，但 `main` 的接线仍是旧的」。接线唯一化后，谁把已起表的 `finish_board` future 塞回
/// 生产路径，直接跑本函数的 `rc2_*` 用例就会变红。
async fn run_supervised_lifecycle<H>(
    http: H,
    board: Option<BoardTask>,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> (Result<(), BoxedError>, Option<BoxedError>)
where
    H: Future<Output = Result<(), BoxedError>>,
{
    supervise_shutdown(http, board, BOARD_SHUTDOWN_GRACE, shutdown_tx, shutdown_rx).await
}

/// 后台任务收敛（审查 N2）：先给宽限期让它们**自己**退出（三者只等 watch，不做 I/O），超期则
/// **直接 abort 原始 `JoinHandle` 再 await 确认其真正结束**。
///
/// 关键：绝不把 owner 的 `JoinHandle` 塞进一个 wrapper 任务、再 abort wrapper——那只会 `drop`
/// 内层句柄＝detach 原任务，原任务从未被 abort、也没被 join（「wrapper 状态」不是「owner 状态」）。
async fn settle_background_tasks(mut handles: Vec<JoinHandle<()>>, grace: Duration) {
    if handles.is_empty() {
        return;
    }
    // 宽限期内轮询**原始句柄**的完成状态：后台任务正常应随关停广播很快自行退出。
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        handles.retain(|handle| !handle.is_finished());
        if handles.is_empty() {
            return;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
    // 超期仍在途：abort **原始句柄**并 await 确认退出（`abort` 本身不等待，必须再 await；await
    // 一个已 abort 的句柄会在其 future 真正被 drop 后返回 `Err(Cancelled)`，即原任务确已结束）。
    for handle in handles {
        if !handle.is_finished() {
            handle.abort();
        }
        let _ = handle.await;
    }
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
                let sched_pool = db_for_scheduler;
                let sched_store = Arc::new(SqlxSchedulerStore::new(sched_pool.clone()));
                let sched_board =
                    Arc::new(TransportBoardClient::new(transport_manager.as_device_rpc()));
                let sched_clock = Arc::new(SystemMonoClock::new());
                let scheduler = Arc::new(TaskScheduler::new(
                    sched_store,
                    sched_board,
                    sched_clock,
                    SchedulerPolicy::default(),
                    process_epoch,
                ));
                let mut sched_shutdown = shutdown_rx.clone();
                // 恢复模式闸门：与 transport 探针、HTTP `/system/status` 是同一个
                // `Arc<RuntimeRecoveryController>`（05 §9 唯一真源）。
                let sched_recovery = Arc::clone(&recovery_for_scheduler);
                // 每轮统计「已占槽」子任务（04 §5.1：dispatching/accepted/verifying 均占槽），
                // 让跨调度周期已在看板中的任务真正计入全局 16 槽上限。
                let sched_active_pool = sched_pool.clone();
                let sched_task = tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_millis(1000));
                    loop {
                        tokio::select! {
                            _ = sched_shutdown.changed() => {
                                if *sched_shutdown.borrow() {
                                    break;
                                }
                            }
                            _ = interval.tick() => {
                                // 受控恢复模式（05 §9）：跳过调度派发，不发任何
                                // prepare/execute；管理写不受本闸门影响（05 §9 允许
                                // 登录、改密、注销、审计、只读核验与退出恢复模式）。
                                if !sched_recovery.allows_scheduling() {
                                    continue;
                                }
                                let active_slots: usize = sqlx::query_scalar::<_, i64>(
                                    "SELECT COUNT(*) FROM sub_tasks WHERE state IN ('dispatching', 'accepted', 'verifying')"
                                )
                                .fetch_one(&sched_active_pool)
                                .await
                                .unwrap_or(0)
                                as usize;
                                if let Err(e) = scheduler.run_once(active_slots).await {
                                    eprintln!("controller task scheduler 单轮派发失败: {e}");
                                }
                            }
                        }
                    }
                });

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
    use std::task::Poll;
    use std::time::Instant;
    use std::time::{Duration, SystemTime};
    use tokio::task::AbortHandle;

    use rsetup_controller::time::evidence::TimeQuality;
    use rsetup_controller::time::socket::NtpSample;
    use rsetup_controller::time::{
        NullTimeAuditSink, SleepFuture, Sleeper, StaticSourceList, TimeGateConfig, TimeGateDeps,
        TimeGateDriver, TimeSampleFuture, TimeSampler, WallClock,
    };

    const FAKE_NTP_SOURCE: &str = "fake.main-assembly.local";

    /// 冷启动受控恢复触发器（05 §9）：环境变量真值只认 `"1"`/`"true"`（大小写不敏感、
    /// 忽略首尾空白），未设置/其他取值/非 UTF-8 一律按正常启动，不做猜测式推断。
    #[test]
    fn controlled_recovery_flag_only_accepts_one_and_true() {
        for raw in ["1", "true", "TRUE", "True", " 1 ", "1 ", "\ttrue\n"] {
            assert!(
                controlled_recovery_flag(Some(std::ffi::OsStr::new(raw))),
                "{raw:?} 应触发受控恢复模式"
            );
        }
        for raw in ["", "0", "false", "no", "10", "true1", " "] {
            assert!(
                !controlled_recovery_flag(Some(std::ffi::OsStr::new(raw))),
                "{raw:?} 不应触发受控恢复模式"
            );
        }
        assert!(!controlled_recovery_flag(None), "未设置即正常启动");
        // 非 UTF-8 取值（Unix）按「未要求」处理，绝不解码后回显。
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = std::ffi::OsStr::from_bytes(&[0xFF, b'1']);
            assert!(!controlled_recovery_flag(Some(raw)));
        }
    }

    /// 装配内核必须在返回句柄**之前**进入受控模式：探针、调度循环与提交闸门读到的
    /// 是同一个实例，启动第一刻即为 `controlled_recovery`。
    #[test]
    fn new_recovery_controller_enters_controlled_recovery_before_returning() {
        use rsetup_controller::operations::RecoveryOperation;

        let triggered = new_recovery_controller(true);
        assert_eq!(
            triggered.current_mode(),
            rsetup_controller::operations::RecoveryMode::ControlledRecovery
        );
        assert!(!triggered.allows_scheduling());
        assert!(!triggered.permits(RecoveryOperation::MutationTask));
        assert!(
            triggered.permits(RecoveryOperation::AdminLogin),
            "受控模式必须放行管理写"
        );
        assert!(triggered.entered_at().is_some(), "进入时刻必须就位");

        let plain = new_recovery_controller(false);
        assert_eq!(
            plain.current_mode(),
            rsetup_controller::operations::RecoveryMode::Normal
        );
        assert!(plain.allows_scheduling());
        assert!(plain.permits(RecoveryOperation::MutationTask));
    }

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

    #[test]
    fn secret_sink_reports_actual_create_new_failure_without_overwrite() {
        let path =
            std::env::temp_dir().join(format!("rsetup-secret-failure-{}", uuid::Uuid::new_v4()));
        let sink = LogSecretSink(path.clone());
        write_private_secret(&path, "admin", "first").unwrap();
        let result = sink.emit("admin", "second");
        assert!(result.is_err(), "sink must report write failure");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "admin: first\n");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn first_boot_secret_is_private_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("rsetup-controller-secret-{}", uuid::Uuid::new_v4()));
        write_private_secret(&path, "admin", "first").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_private_secret(&path, "admin", "second").is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("first"));
        std::fs::remove_file(path).unwrap();
    }

    // -----------------------------------------------------------------------
    // 审查 N1 / N2：生命周期监督 helper 的常驻回归
    //
    // 这些用例跑的是 **main 生产路径调用的同一组 helper**（`watch_turns_true` /
    // `finish_board` / `supervise_shutdown` / `settle_background_tasks`），不是在测试里
    // 复制一份同样的 select 逻辑。断言的是「原任务真的被 abort 并 await」「HTTP 这个
    // **永不自返**的 future 也被完整收尾」，而不是「wrapper 返回了」。
    // -----------------------------------------------------------------------

    /// 放入被测任务内部的「原任务已被 drop」见证：只有原 future 真正结束（正常收尾或被
    /// abort 后展开）才会置位；`JoinHandle` 被 drop（detach）不会让它结束。
    struct DropWitness(Arc<AtomicBool>);

    impl DropWitness {
        fn new() -> (Self, Arc<AtomicBool>) {
            let flag = Arc::new(AtomicBool::new(false));
            (Self(Arc::clone(&flag)), flag)
        }
    }

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// 生产 HTTP 侧的形状：**除了关停广播之外没有任何返回路径**（`with_graceful_shutdown`
    /// 的驱动就是 [`watch_turns_true`]）。用它证明「board 意外退出」能驱动 HTTP 收尾。
    async fn http_that_never_returns_by_itself(
        shutdown: watch::Receiver<bool>,
        exited: Arc<AtomicBool>,
    ) -> Result<(), BoxedError> {
        watch_turns_true(shutdown).await;
        exited.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// 健康的板端监听：只等**同一个**关停 watch，收到后才收尾（生产 `run_controller_listener`
    /// 的关停入口形状：watch 变真 → GOAWAY 排空 → 返回 `Ok`）。
    async fn healthy_board_listener(shutdown: watch::Receiver<bool>) -> Result<(), ListenerError> {
        watch_turns_true(shutdown).await;
        Ok(())
    }

    /// 板端监听原任务的只读探针：`AbortHandle` 只用于观测（测试绝不 abort），外加两个见证——
    /// `finished`＝原任务**自己**跑到任务体结尾（被 abort 不会置位），`dropped`＝原 future 真的
    /// 被展开（abort 与正常结束都会置位；仅 drop `JoinHandle` 不会）。
    struct BoardProbe {
        abort: AbortHandle,
        finished: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl BoardProbe {
        /// 原任务仍在途（既没被 abort，也没自己跑完）。
        fn alive(&self) -> bool {
            !self.abort.is_finished() && !self.dropped.load(Ordering::SeqCst)
        }
    }

    /// 生产 `main` 的板端监听形状：真实 `tokio::spawn` 任务 + `done` 回执，监听体把结果写回
    /// `done` 的**下一步就是结束任务体**（生产同序，见 `BoardTask` 的契约）。
    fn spawn_board_task<F>(listener: F) -> (BoardTask, BoardProbe)
    where
        F: Future<Output = Result<(), ListenerError>> + Send + 'static,
    {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            let outcome = listener.await;
            let _ = done_tx.send(outcome);
            finished_flag.store(true, Ordering::SeqCst);
        });
        let probe = BoardProbe {
            abort: handle.abort_handle(),
            finished,
            dropped,
        };
        (BoardTask::new(handle, done_rx), probe)
    }

    #[tokio::test]
    async fn n1_board_error_broadcasts_shutdown_and_finishes_the_http_that_never_returns() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        // board 侧：真实监听任务把失败写回 `done`（生产同一路径，错误经 `finish_board` 上交）。
        let (board_task, probe) =
            spawn_board_task(async { Err(ListenerError::Config("board listen failed".into())) });

        // HTTP 自身永不返回 ⇒ 若监督不广播关停，本用例只会超时失败（即「静默半死」）。
        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("board 结束后监督必须返回（HTTP 永不自行返回）");

        let error = board_error.expect("board 的失败必须原样交给顶层，不得只存在 done 回执里");
        assert!(
            error.to_string().contains("board listen failed"),
            "监听自身的错误要原样可见，实测 {error}"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出，不是失败");
        assert!(
            http_exited.load(Ordering::SeqCst),
            "HTTP future 必须被**收尾完成**（select 丢弃未完成 future 也算「退出」，故断言其自身的完成标志）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "board 意外退出必须把关停广播到同一个 watch（后台任务共用它）"
        );
        assert!(
            probe.finished.load(Ordering::SeqCst) && probe.dropped.load(Ordering::SeqCst),
            "原任务已自行返回结果，监督必须 join 它确认退出（不是留下 detach 的在途任务）"
        );
    }

    #[tokio::test]
    async fn n1_board_task_panic_is_observed_and_its_original_task_is_aborted() {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        // 真实常驻任务：**永不返回**，只因为 `done_tx` 被丢弃（＝监听任务 panic/被取消）而
        // 让 done_rx 结束。句柄属于 `finish_board`，观测只经预先取得的 `AbortHandle`。
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let task = BoardTask::new(handle, done_rx);
        drop(done_tx);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            finish_board(Some(task), Duration::from_millis(200)),
        )
        .await
        .expect("done 结束即应返回，不等宽限期")
        .expect("panic/取消必须产出错误");
        assert!(!error.to_string().is_empty(), "错误文本去敏但必须存在");
        assert!(
            abort_handle.is_finished(),
            "原任务必须被 abort 并 await 确认，而不是留下在途任务"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "原 future 必须真的被展开（drop 见证置位）"
        );
    }

    #[tokio::test]
    async fn n1_board_task_overrunning_the_grace_is_aborted_not_detached() {
        let (_done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let task = BoardTask::new(handle, done_rx);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            finish_board(Some(task), Duration::from_millis(50)),
        )
        .await
        .expect("宽限期到点必须返回")
        .expect("超期未排空必须产出错误");
        assert!(!error.to_string().is_empty());
        assert!(
            abort_handle.is_finished(),
            "超期兜底必须 abort **原始任务**并 await 确认退出"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        // `_done_tx` 在此随作用域结束被丢弃：监听仍在途时不得让 `finish_board` 永久挂起。
    }

    #[tokio::test]
    async fn n1_supervise_joins_both_sides_when_board_task_hangs_after_panic() {
        // 端到端（**生产同一条接线**）：board = 永不返回且回执被关闭（＝监听任务 panic/取消）的
        // 真实原任务，HTTP = 永不自返。监督必须即时广播关停、收尾 HTTP，并 abort **原始任务**后
        // 把 board 的错误交回顶层。
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let board_task = BoardTask::new(handle, done_rx);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        drop(done_tx);

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            supervise_shutdown(
                http,
                Some(board_task),
                Duration::from_millis(80),
                &shutdown_tx,
                shutdown_rx.clone(),
            ),
        )
        .await
        .expect("两侧都必须结清");
        assert!(board_error.is_some(), "board 的取消错误必须上交");
        assert!(http_result.is_ok());
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
        assert!(
            abort_handle.is_finished(),
            "board 原任务必须被 abort 并 await 确认（回执已关闭 ⇒ 不必等满宽限期）"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(*shutdown_rx.borrow(), "关停必须已广播给后台任务");
    }

    #[tokio::test]
    async fn n2_settle_aborts_the_original_owner_tasks_not_a_wrapper() {
        // ① 会自己退出的任务：宽限期内正常收尾，不得被 abort。
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let graceful_exited = Arc::new(AtomicBool::new(false));
        let graceful_flag = Arc::clone(&graceful_exited);
        let graceful = tokio::spawn(async move {
            let mut rx = shutdown_rx;
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
            graceful_flag.store(true, Ordering::SeqCst);
        });

        // ② **不响应关停**的真实 pending 任务：只有 abort 原任务才会结束它。
        let (witness, dropped) = DropWitness::new();
        let stubborn = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let stubborn_abort = stubborn.abort_handle();
        // 生产的先后顺序：关停广播**已经**由 `supervise_shutdown` 发出，`settle` 只负责结清。
        let _ = shutdown_tx.send(true);

        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(5),
            settle_background_tasks(vec![graceful, stubborn], Duration::from_millis(120)),
        )
        .await
        .expect("收敛必须有界");
        let elapsed = started.elapsed();

        assert!(
            graceful_exited.load(Ordering::SeqCst),
            "会自行退出的任务必须在宽限期内正常收尾"
        );
        assert!(
            stubborn_abort.is_finished(),
            "不响应关停的原任务必须在超期后被 abort 并 await 确认（不是 wrapper 状态）"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "原 future 必须被展开；仅 drop `JoinHandle`（detach）永远不会置位"
        );
        assert!(
            elapsed >= Duration::from_millis(100),
            "②必须真的用满宽限期（实测 {elapsed:?}）"
        );
    }

    // -----------------------------------------------------------------------
    // RC2：单次生产生命周期的监听期（真实进程 b52 复现的根因）
    //
    // 根因口径：生产 main 在**监听器刚启动**时就把 `finish_board(Some(task), BOARD_SHUTDOWN_GRACE)`
    // 这个 future 交给 `supervise_shutdown` 并发 poll，于是宽限期计时器在监听阶段就 armed；
    // 进程正常运行满 30s（无任何 SIGTERM、board 监听完全健康）即 abort 原任务 → 广播关停 →
    // HTTP 拒连 → 非 0 退出。下面这组用例跑的是**生产同一条接线**（同一个
    // `supervise_shutdown` + 同一个 `BOARD_SHUTDOWN_GRACE` 常量 + 真实 `tokio::spawn` 监听任务），
    // 不是在测试里复制一份 select。
    // -----------------------------------------------------------------------

    /// 把生产监督跑到后台任务里（虚拟时钟用例共用）：参数与 `main` 的调用**逐位一致**。
    fn spawn_supervisor<H>(
        http: H,
        board: Option<BoardTask>,
        shutdown_tx: &watch::Sender<bool>,
        shutdown_rx: &watch::Receiver<bool>,
    ) -> JoinHandle<(Result<(), BoxedError>, Option<BoxedError>)>
    where
        H: Future<Output = Result<(), BoxedError>> + Send + 'static,
    {
        let tx = shutdown_tx.clone();
        let rx = shutdown_rx.clone();
        // 与 `main` **同一个函数、同一份宽限期常量**：测试不另选 grace，也不复制接线。
        tokio::spawn(async move { run_supervised_lifecycle(http, board, &tx, rx).await })
    }

    /// 让已 spawn 的监督/监听任务真的被 poll（在虚拟时钟上建立它自己的等待或计时器）。
    async fn drive(rounds: usize) {
        for _ in 0..rounds {
            tokio::task::yield_now().await;
        }
    }

    /// **RC2 主用例**：监听阶段推进 **2× 生产宽限期**且不发任何关停信号 ⇒ 监督必须仍在监听：
    /// board 原任务在途、HTTP 未收尾、关停 watch 仍为 `false`。
    ///
    /// 旧接线（监听器启动即把带宽限期的 `finish_board` future 交给监督并发 poll）在本用例上 **RED**：
    /// 虚拟时间一到 30s 就 abort 健康监听 → 广播关停 → HTTP 收尾 → 监督返回。
    #[tokio::test(start_paused = true)]
    async fn rc2_listening_phase_does_not_arm_the_board_grace() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;

        let facts = format!(
            "board 原任务在途={} HTTP 已收尾={} 关停已广播={} 监督已返回={}",
            probe.alive(),
            http_exited.load(Ordering::SeqCst),
            *shutdown_rx.borrow(),
            supervisor.is_finished(),
        );
        assert!(
            probe.alive(),
            "监听期（无任何关停意图）绝不得 abort 仍然健康的板端监听原任务；实测 {facts}"
        );
        assert!(
            !http_exited.load(Ordering::SeqCst),
            "监听期绝不得关闭 HTTP（正常服务 2× 宽限期后拒连就是 RC2）；实测 {facts}"
        );
        assert!(
            !*shutdown_rx.borrow(),
            "监听期绝不得广播关停（后台三任务与 board 共用这同一个 watch）；实测 {facts}"
        );
        assert!(
            !supervisor.is_finished(),
            "没有任何关停意图时监督必须一直停在监听阶段；实测 {facts}"
        );
    }

    /// 关停意图确立的**那一刻**才起表：监听期推进 2× 宽限期无动作，`send(true)` 之后健康监听在
    /// **远小于**宽限期的时间内自行排空收尾，两侧都以「无错误」结束（RC2 之后的正常关停）。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_intent_arms_the_grace_and_a_healthy_board_drains_by_itself() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;
        assert!(
            probe.alive() && !supervisor.is_finished(),
            "关停意图确立前不得动 board，也不得结束监督"
        );

        // 生产里这一步是 SIGTERM → 同一个 watch；**此刻**宽限期才起表。
        let armed_at = tokio::time::Instant::now();
        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("关停后必须在宽限期内结清（两侧都 join）")
            .expect("监督任务不得 panic");
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(armed_at)
            .unwrap_or_default();

        assert!(http_result.is_ok(), "HTTP 走优雅退出，不是失败");
        assert!(board_error.is_none(), "健康监听自行排空收尾不得被记成失败");
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须**自行**收尾（被 abort 不会置位这个见证）"
        );
        assert!(probe.dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(
            probe.abort.is_finished(),
            "监督返回时原任务必须确已终止；自行结束还是被 abort 由上面的 `finished` 见证区分"
        );
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
        assert!(
            elapsed < BOARD_SHUTDOWN_GRACE,
            "关停后应在远小于宽限期内完成（实测 {elapsed:?}）"
        );
    }

    /// 宽限期锚在**关停意图**而不是「HTTP 全部等完」：关停后 HTTP 仍在处理活跃长请求时，板端的
    /// 有界清理到期即 **abort 原始任务**（drop 见证置位＝不是 detach），随后 HTTP 仍被收尾完成。
    #[tokio::test(start_paused = true)]
    async fn rc2_grace_is_anchored_at_shutdown_intent_not_at_http_completion() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // board：完全不响应关停（既不返回也不结束任务体）＝内部排空卡死。
        let (board_task, probe) =
            spawn_board_task(std::future::pending::<Result<(), ListenerError>>());
        // HTTP：关停广播后仍在处理活跃长请求，只有 release 结束才收尾（生产的活跃长连接形状）。
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let http_exited = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_exited);
        let http = async move {
            let _ = release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        let _ = shutdown_tx.send(true);
        drive(4).await;
        // 越过宽限期：板端必须有界清理，HTTP 的长请求不得把它延后。
        tokio::time::advance(BOARD_SHUTDOWN_GRACE + Duration::from_millis(10)).await;
        drive(8).await;

        let facts = format!(
            "board 原 future 已展开={} board 自行收尾={} HTTP 已收尾={} 监督已返回={}",
            probe.dropped.load(Ordering::SeqCst),
            probe.finished.load(Ordering::SeqCst),
            http_exited.load(Ordering::SeqCst),
            supervisor.is_finished(),
        );
        assert!(
            probe.dropped.load(Ordering::SeqCst),
            "关停意图后越过宽限期必须 abort **原始** board 任务并 await 确认（detach 不会展开原 future）；实测 {facts}"
        );
        assert!(
            !probe.finished.load(Ordering::SeqCst),
            "这条路径是 abort 退出，不得被记成自行收尾；实测 {facts}"
        );
        assert!(
            !http_exited.load(Ordering::SeqCst),
            "本用例前提：HTTP 仍在活跃长请求中（宽限期不得锚在它上面）；实测 {facts}"
        );

        // 放行 HTTP：监督必须收尾返回，并把板端超期的错误交回顶层。
        drop(release_tx);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("HTTP 收尾后监督必须返回（HTTP 原 future 必须持有到收尾，不得 detach）")
            .expect("监督任务不得 panic");
        assert!(http_result.is_ok(), "HTTP 走的是优雅退出，不是失败");
        assert!(
            board_error.is_some(),
            "板端排空超期的错误必须上交，不得静默成功（fail-open）"
        );
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须被收尾完成");
    }

    /// 没有板端监听（`TransportEnv::Disabled`）：监听期同样不得自行结束；关停信号到来才收尾，
    /// 且不得凭空产出 board 错误。
    #[tokio::test(start_paused = true)]
    async fn rc2_without_board_task_the_supervisor_still_shuts_down_cleanly() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, None, &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;
        assert!(
            !supervisor.is_finished() && !*shutdown_rx.borrow(),
            "无 board 时监听期同样没有任何超时可武装，监督必须一直在监听"
        );

        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("关停后必须返回")
            .expect("监督任务不得 panic");
        assert!(http_result.is_ok(), "HTTP 优雅退出");
        assert!(board_error.is_none(), "没有板端监听不得凭空产出错误");
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
    }

    /// 关停意图在进入监督**之前**就已确立（SIGTERM 落在绑端口与进入监督之间的真实竞态）：立刻
    /// 起表、正常收尾，不静默等满 30s，也不把「监听自行返回 `Ok`」误记成未经请求的提前结束。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_already_established_before_supervise_starts_arms_immediately() {
        let (shutdown_tx, shutdown_rx) = watch::channel(true);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let started = tokio::time::Instant::now();
        let (http_result, board_error) = tokio::time::timeout(
            BOARD_SHUTDOWN_GRACE,
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("关停已确立 ⇒ 必须立刻收尾，不等满宽限期");
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(started)
            .unwrap_or_default();

        assert!(http_result.is_ok());
        assert!(board_error.is_none(), "已确立关停后的正常收尾不得产出错误");
        assert!(probe.finished.load(Ordering::SeqCst), "board 必须自行收尾");
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            elapsed < BOARD_SHUTDOWN_GRACE,
            "关停已确立时不得静默等满宽限期（实测 {elapsed:?}）"
        );
    }

    /// 外部关停与 board 失败**同一轮** ready：只消费一次、不重复 poll、错误不被吞。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_and_board_failure_in_the_same_round_are_consumed_once() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) =
            spawn_board_task(async { Err(ListenerError::Config("board listen failed".into())) });
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        // 两条关停来源同时到位（board 的失败在上一轮就已 ready）。
        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("监督必须返回（不得重复 poll 已完成的分支而永久挂起）")
            .expect("监督任务不得 panic");

        let error = board_error.expect("board 的失败必须原样上交，不得被同轮关停信号吞掉");
        assert!(error.to_string().contains("board listen failed"), "{error}");
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须被 join"
        );
    }

    /// 监听在**任何关停意图之前**就自行返回 `Ok`：监听已死但没有错误上抛，必须 fail-closed 上抛
    /// （不再静默 exit 0），同时仍立即广播关停让 HTTP 收尾。
    #[tokio::test]
    async fn rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(async { Ok(()) });
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("board 提前结束必须驱动监督返回");

        assert!(
            board_error.is_some(),
            "未经请求的提前结束必须 fail-closed 上抛，不得静默 exit 0"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(
            *shutdown_rx.borrow(),
            "仍要立即广播关停：HTTP 与后台三任务不得对着已死的监听继续服务"
        );
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须被 join"
        );
    }

    // -----------------------------------------------------------------------
    // RC2-Low：board 完成分支排在 watch 分支**之前**造成的同轮竞态
    //
    // 窗口（`tokio::select! { biased; .. }`，分支顺序 HTTP → board 完成 → watch → 宽限期）：
    // SIGTERM 把外部关停 watch 置真与板端提前 `Ok` 在**同一次 select 之前**同时到位，而 HTTP 因
    // 活跃长请求仍 pending（HTTP 分支不 ready，不会先替 `armed` 起表）时，`biased` 顺序下先执行
    // 的是 board 完成分支：它**自己** `send(true)` 之后只看 `armed.is_none()`，而 watch 分支还没
    // 机会起表 ⇒ 把「外部早已确立的关停意图」当成未经请求的提前结束 ⇒ 顶层拿着一条本该是正常
    // 优雅关停的路径以非 0 退出。
    //
    // 这组用例把外部事件**精确安排在监督的两次 poll 之间**（手动 poll，不交给调度器排序，也不
    // 复制 select、不用 sleep 碰时序）；被测对象仍是生产 `run_supervised_lifecycle`，宽限期由
    // 生产 `BOARD_SHUTDOWN_GRACE` 决定。
    // -----------------------------------------------------------------------

    /// **手动 poll 一次**（不经调度器）：用于把外部事件精确安排在监督两次 poll 之间，让这条
    /// 「同轮两个分支同时 ready」的竞态可确定性复现，而不是靠任务先后顺序赌一次。
    /// 唤醒走 `Waker::noop()`：监督只在被 poll 时才前进，唤醒本身没有意义。
    fn poll_once<F>(future: &mut F) -> Poll<F::Output>
    where
        F: Future + Unpin,
    {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        std::pin::Pin::new(future).poll(&mut cx)
    }

    /// 手动 poll 到就绪：监督挂在**别的真实任务**（板端原任务的收尾、对它的 join）上时让出一次，
    /// 让 runtime 去 poll 那个任务，然后再继续手动驱动监督。
    async fn poll_until_ready<F>(future: &mut F) -> F::Output
    where
        F: Future + Unpin,
    {
        loop {
            if let Poll::Ready(output) = poll_once(future) {
                return output;
            }
            tokio::task::yield_now().await;
        }
    }

    /// 与 `spawn_board_task` **同形状**的板端原任务，区别只有「写回 `done`」这一步由测试执行：
    /// 真实 `tokio::spawn` 原任务、只等收尾门、放行后**立刻**结束任务体（＝生产契约「写回结果的
    /// 下一步就是结束」），`finish_board` 照旧 join 这个真实任务。回执交给测试是为了把「回执
    /// 已 ready」与「外部关停已置真」钉进监督同两次 poll 之间的同一段同步代码里。
    struct RacyBoardFixture {
        task: BoardTask,
        probe: BoardProbe,
        report: oneshot::Sender<Result<(), ListenerError>>,
        finish: oneshot::Sender<()>,
    }

    fn spawn_racy_board_task() -> RacyBoardFixture {
        let (report, done_rx) = oneshot::channel();
        let (finish, finish_rx) = oneshot::channel::<()>();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            let _ = finish_rx.await;
            finished_flag.store(true, Ordering::SeqCst);
        });
        let probe = BoardProbe {
            abort: handle.abort_handle(),
            finished,
            dropped,
        };
        RacyBoardFixture {
            task: BoardTask::new(handle, done_rx),
            probe,
            report,
            finish,
        }
    }

    /// **本竞态主用例**：监督已在监听阶段 poll 过 ⇒ 两次 poll 之间「外部关停 watch 先为真、随后
    /// 板端提前 `Ok`」⇒ 收尾必须 `(Ok, None)`：外部意图已在板端回执之前确立，健康监听的收尾不得
    /// 记成未经请求的提前结束（修复前这里产出 `Some(..)`，顶层据此非 0 退出 ⇒ RED）。
    #[tokio::test(start_paused = true)]
    async fn rc2_external_shutdown_arriving_before_the_board_receipt_is_not_unprompted() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let board = spawn_racy_board_task();
        // 人为的活跃长请求 HTTP：除了放行之外没有任何返回路径，因而**不** ready（否则 `biased`
        // 顺序下 HTTP 分支会先给 `armed` 起表，本竞态就不成立）。
        let (http_release, http_release_rx) = oneshot::channel::<()>();
        let http_released = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_released);
        let http = async move {
            let _ = http_release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        tokio::pin!(http);
        let mut supervised = Box::pin(run_supervised_lifecycle(
            http,
            Some(board.task),
            &shutdown_tx,
            shutdown_rx.clone(),
        ));

        // ① 监督先在监听阶段 poll 过：不起表、不广播、板端原任务仍在途。
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：监督必须仍在监听阶段挂起"
        );
        assert!(!*shutdown_rx.borrow(), "前提：监听阶段绝不得广播关停");
        assert!(board.probe.alive(), "前提：板端原任务必须还在途");

        // ② **两次 poll 之间**：外部关停先到（生产＝SIGTERM → 同一个 watch），**随后**板端提前 `Ok`。
        // 两者都先于下一次 poll 成立 ⇒ 同一次 select 里 board 完成分支与 watch 分支同时 ready，
        // 而 `biased` 顺序下先执行的是 board 完成分支。
        let _ = shutdown_tx.send(true);
        board.report.send(Ok(())).expect("板端回执必须写回成功");

        // ③ 下一次 poll（旧代码在这里把外部已确立的关停误记成「未经请求的提前结束」）。
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：HTTP 仍在活跃长请求中，监督不得已返回"
        );

        // ④ 收尾：放行板端原任务的收尾与 HTTP 的活跃长请求。
        drop(board.finish);
        drop(http_release);
        let (http_result, board_error) = poll_until_ready(&mut supervised).await;

        let facts = format!(
            "board 错误={} HTTP 结果={} 板端自行收尾={} 板端原 future 已展开={} HTTP 已收尾={}",
            board_error
                .as_ref()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "无".to_owned()),
            http_result.is_ok(),
            board.probe.finished.load(Ordering::SeqCst),
            board.probe.dropped.load(Ordering::SeqCst),
            http_released.load(Ordering::SeqCst),
        );
        assert!(
            http_result.is_ok(),
            "HTTP 走的是优雅退出，不是失败；实测 {facts}"
        );
        assert!(
            board_error.is_none(),
            "关停意图在板端回执**之前**就已由外部确立（同一个 watch 已为真），健康监听随后的收尾不得记成未经请求的提前结束（那会让正常的 SIGTERM 关停非 0 退出）；实测 {facts}"
        );
        assert!(
            board.probe.finished.load(Ordering::SeqCst),
            "板端原任务必须**自行**收尾（被 abort 不会置位这个见证）；实测 {facts}"
        );
        assert!(
            board.probe.dropped.load(Ordering::SeqCst),
            "板端原 future 必须被展开；实测 {facts}"
        );
        assert!(
            board.probe.abort.is_finished(),
            "监督返回时板端原任务必须确已终止"
        );
        assert!(
            http_released.load(Ordering::SeqCst),
            "HTTP 必须被收尾完成（不是 detach 掉未收尾的 future）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "关停仍必须广播给后台三任务（共用同一个 watch）"
        );
    }

    /// **反向用例（同一驱动机制）**：两次 poll 之间只有板端提前 `Ok`、没有任何外部关停证据
    /// （watch 仍 `false`、`armed` 仍 `None`）⇒ 仍必须 fail-closed 上抛。它钉住修复的边界：
    /// board 完成分支**自己**广播的那次 `send(true)` 不能自证关停意图，否则真正的「监听已死却
    /// 无错误上抛」会被放行（fail-open）。
    #[tokio::test(start_paused = true)]
    async fn rc2_board_ok_between_two_polls_is_still_fail_closed_without_external_evidence() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let board = spawn_racy_board_task();
        // 人为的活跃长请求 HTTP：除了放行之外没有任何返回路径，因而**不** ready（否则 `biased`
        // 顺序下 HTTP 分支会先给 `armed` 起表，本竞态就不成立）。
        let (http_release, http_release_rx) = oneshot::channel::<()>();
        let http_released = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_released);
        let http = async move {
            let _ = http_release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        tokio::pin!(http);
        let mut supervised = Box::pin(run_supervised_lifecycle(
            http,
            Some(board.task),
            &shutdown_tx,
            shutdown_rx.clone(),
        ));

        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：监督必须仍在监听阶段挂起"
        );
        // 两次 poll 之间**只有**板端提前 `Ok`：没有任何外部关停证据。
        board.report.send(Ok(())).expect("板端回执必须写回成功");
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：HTTP 仍在活跃长请求中，监督不得已返回"
        );

        drop(board.finish);
        drop(http_release);
        let (http_result, board_error) = poll_until_ready(&mut supervised).await;

        let error = board_error.expect("没有任何外部关停证据的提前 `Ok` 必须 fail-closed 上抛");
        assert!(
            error.to_string().contains("关停意图前"),
            "错误必须如实说明是未经请求的提前结束，实测 {error}"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(
            http_released.load(Ordering::SeqCst),
            "HTTP 必须被收尾完成（select 丢弃未收尾 future 也算失败）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "仍要立即广播关停：HTTP 与后台三任务不得对着已死的监听继续服务"
        );
    }

    /// HTTP 自身失败（`axum::serve` 返回 `Err`）：必须广播关停，board 侧仍自行排空收尾，
    /// 顶层优先拿到 HTTP 的错误。
    #[tokio::test]
    async fn rc2_http_failure_broadcasts_shutdown_and_board_drains_by_itself() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http = async { Err::<(), BoxedError>(boxed(std::io::Error::other("serve failed"))) };

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("HTTP 失败后监督必须返回");

        assert!(http_result.is_err(), "HTTP 自身的失败要交回顶层");
        assert!(
            board_error.is_none(),
            "board 是收到关停后自行收尾，不得记成失败"
        );
        assert!(
            *shutdown_rx.borrow(),
            "HTTP 失败必须广播关停（board 与后台三任务共用同一个 watch）"
        );
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 必须自行排空收尾（不是被 abort）"
        );
    }

    /// `finish_board` 拿到结果后仍要 **join 原始任务**：结果写回后任务体还在收尾时，结清必须等
    /// 它真正结束（旧实现直接返回并把句柄 drop＝detach，在本用例上是 RED）。
    #[tokio::test(start_paused = true)]
    async fn rc2_finish_board_joins_the_original_task_after_its_result_arrives() {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            // 结果已产出（生产里 send 就是任务体最后一步，这里刻意延长收尾以便观测所有权）。
            let _ = done_tx.send(Ok(()));
            tokio::time::sleep(Duration::from_millis(50)).await;
            finished_flag.store(true, Ordering::SeqCst);
        });
        let abort = handle.abort_handle();

        let no_error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_board(
                Some(BoardTask::new(handle, done_rx)),
                Duration::from_secs(30),
            ),
        )
        .await
        .expect("结果已产出 ⇒ 结清不得等满宽限期")
        .is_none();
        assert!(no_error, "正常收尾不得产出错误");
        assert!(
            finished.load(Ordering::SeqCst),
            "原任务必须被 **join** 到真正结束；只拿结果就返回＝drop 句柄 detach"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(
            abort.is_finished(),
            "结清返回时原任务必须确已终止（自行跑完，而不是留下 detach 的在途任务）"
        );
    }

    #[tokio::test]
    async fn n2_settle_returns_early_when_every_owner_exits_by_itself() {
        let handles = (0..3)
            .map(|_| {
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                })
            })
            .collect();
        let started = Instant::now();
        settle_background_tasks(handles, Duration::from_secs(30)).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "全部 owner 自行退出时不得等满 30s 宽限期（实测 {elapsed:?}）"
        );
    }
}
