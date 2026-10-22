//! 中控进程唯一入口（集成验收 Task 0b：全量 typed 生产装配）。
//!
//! 唯一构造点：单一 `DbPool` → 单一 `Arc<HttpAuthState>` → auth/management/tasks/
//! status/events 五条使用会话认证的线共享同一认证上下文（session 预过滤、CSRF
//! 绑定表与限速桶跨端点互相可见），metrics 是独立 Basic 认证边界，web 为静态资源线；
//! 七线再经 [`build_production_router`] 合并成唯一顶层 router。
//! 本入口不构造测试网关、`connect_lazy` 占位池、固定 offline/空能力或 authz
//! epoch 0，也不构造任何返回伪造取值的常量源；尚未交付的 transport 线三个源以
//! **读取即失败**的 fail-closed 装配位接入（见 [`UnwiredTransportSources`]）。

use std::sync::Arc;

use rsetup_controller::api::adapters::build_sqlx_management_state_with_events;
use rsetup_controller::api::approvals::ManagementNotificationSink;
use rsetup_controller::api::devices::{DeviceLiveProbe, DeviceLiveView};
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::api::{
    ManagementFuture, install_management_cursor_secret, load_management_cursor_secret_from_env,
};
use rsetup_controller::auth::service::AuthService;
use rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository;
use rsetup_controller::authz::sqlx_repo::{
    AuthorizationSnapshotRepository, SqlxAuthorizationSnapshotRepository,
};
use rsetup_controller::devices::service::AdmissionChangeEvent;
use rsetup_controller::events::EventBus;
use rsetup_controller::http_auth::{AppState, HttpAuthState};
use rsetup_controller::http_device_status::{DeviceStatusState, SnapshotDeviceStatusAuthorizer};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_live::LiveAuth;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::production::{MetricsLineInputs, ProductionInputs, build_production_router};
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::adapter::TaskTimeSource;
use rsetup_controller::tasks::board_client::{BoardClient, BoardClientError, BoardFuture};
use rsetup_controller::tasks::http::HttpTasksState;
use rsetup_controller::tasks::recover_process;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::time::evidence::TimeEvidence;
use rsetup_controller::time::{TimeGateConfig, TimeGateHandle, start_time_gate};
use rsetup_controller::web_router;
use rsetup_controller::{
    BootstrapSecretSink, ControllerConfig, ControllerError, DbPool, bootstrap_admin,
    check_identity_schema,
};

use rsetup_protocol::wire::{
    Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket, TaskRecord,
};

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

/// Transport 线三个真实源的 **fail-closed** 装配位：设备实时视图探针、恢复模式
/// 探针与 commit 后 typed 通知通道。
///
/// Transport 的 typed 设备缓存/恢复控制器/通知适配器尚未交付（见 Task 0b 报告
/// concerns）。本装配位的唯一行为是**读取即失败上抛**：`DeviceLiveProbe` 与
/// `RecoveryModeProbe` 的契约本身就规定「读取失败必须失败上抛（503），不得降级为
/// offline/空能力 200」「不可读时上抛错误映射 503 `NOT_READY`」，因此这里返回固定
/// 去敏 `Config` 是契约内的正确失败形态，绝不返回伪造的 `Offline`、空能力或
/// `ready = true`，也绝不返回 `Ok(())` 假装通知已入队。
///
/// 真实适配器交付后，只需在此处替换为 transport 的实现，七线装配拓扑不变。
struct UnwiredTransportSources;

impl DeviceLiveProbe for UnwiredTransportSources {
    fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "device live cache not wired".into(),
            ))
        })
    }
}

impl RecoveryModeProbe for UnwiredTransportSources {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "recovery controller not wired".into(),
            ))
        })
    }
}

impl ManagementNotificationSink for UnwiredTransportSources {
    fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
        Err(ControllerError::Config(
            "admission notification channel not wired".into(),
        ))
    }
}

/// 板端 `BoardClient` 的 **fail-closed** 装配位：真实 transport（连接管理器/板端
/// 控制流）尚未交付，本装配位对 6 个方法一律回 `DeviceOffline`（设备控制流不在线/
/// 不可达），绝不返回伪造的 `Capabilities`/`DeviceStatus`/`ClockSample`/`TaskRecord`，
/// 也绝不假装 `Ok(None)`（那会被恢复路径误读为 `TASK_NOT_FOUND(1006)` 并计入已核验）。
///
/// 唯一被启动期消费的入口是 [`BoardClient::task_record`]（见 `recover_process` 阶段 2）：
/// 返回 `Err(DeviceOffline)` 经 `query_board_task_record` 记为「未核验」outcome 但**绝不**
/// 让恢复上抛失败，`queued` 子任务仍按不可信期限保守置 `held`。真实适配器交付后只需替换
/// 本结构，`recover_process` 的三阶段编排与七线装配拓扑不变。
struct UnwiredBoardClient;

impl BoardClient for UnwiredBoardClient {
    fn capabilities<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn status<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn clock<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn prepare<'a>(
        &'a self,
        _device: &'a [u8; 32],
        _sub_task_id: uuid::Uuid,
        _expected_boot_id: &'a str,
    ) -> BoardFuture<'a, RebootTicket> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn execute<'a>(
        &'a self,
        _device: &'a [u8; 32],
        _ticket: &'a RebootTicket,
        _request: &'a RebootExecute,
    ) -> BoardFuture<'a, TaskRecord> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn task_record<'a>(
        &'a self,
        _device: &'a [u8; 32],
        _sub_task_id: uuid::Uuid,
    ) -> BoardFuture<'a, Option<TaskRecord>> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
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
/// 唯一门禁句柄），本函数不再另造时钟源。
fn production_inputs(
    db: &DbPool,
    metrics_config: Arc<MetricsConfig>,
    metrics_runtime: Arc<MetricsRuntime>,
    time_gate: &TimeGateHandle,
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

    // 事件总线：进程内唯一通道句柄（broadcast 容量 128）。SSE 读线与各条业务发布方
    // 共享同一个句柄（`Clone` 不新建通道）：任务线在任务状态变化时投 `task.updated`，
    // 管理线在授权变更成功时投 `permissions.changed`、并暴露
    // `SystemApiState::publish_system_time_changed` 作为时间证据变化的发布入口
    // （时间采样侧尚未调用它，因此当前不会有时间事件）。
    // 除这些真实变化外，事件流上只会出现心跳与总线自身产生的 reset，不伪造任何事件。
    let events = EventsState {
        auth: auth.clone(),
        bus: EventBus::new(),
        // 长连接权威复核周期：空闲连接也必须按会话/authz_epoch/降权切断。
        revalidate_interval: rsetup_controller::http_events::DEFAULT_REVALIDATE_INTERVAL,
    };

    let transport = Arc::new(UnwiredTransportSources);
    let management = build_sqlx_management_state_with_events(
        db.clone(),
        auth.clone(),
        transport.clone(),
        transport.clone(),
        transport,
        Some(events.bus.clone()),
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
            // 30s 新鲜度的本进程缓存。transport 线的轮询与认证连接管理器尚未接入，
            // 当前还没有任何写入方，因此它对任何设备都真实地报告「尚无样本」
            // （snapshot=null / freshness=unknown / age_ms=null），不伪造快照、时间
            // 证据或新鲜度；接入时轮询驱动共享同一个 Arc<SnapshotStore>，但每条设备
            // 都必须先由认证连接管理器 confirm_generation，未确认代际的 update 一律
            // NoActiveGeneration 拒绝（代际不由样本自行建立）。
            snapshot: Arc::new(SnapshotStore::new()),
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
    // 单调进程时钟纪元 clock_epoch 只在此生成一次；`start_time_gate` 在 tokio 运行时内 spawn
    // 后台 driver（startup 预算采样，就绪或超时后对外提供时间快照），本进程全程持有 handle。
    // servers 为空（未设 NTP_SERVERS）即走 SystemFallback，安全 fail-closed 与 fallback 规则不变。
    let clock_epoch = uuid::Uuid::new_v4();
    let time_gate = start_time_gate(time_gate_config_from_env(), clock_epoch);
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
    start_if_ready(check_identity_schema(&db), move || async move {
        bootstrap_admin(&db_handle, &LogSecretSink(secret_path)).await?;
        // 启动恢复：schema 校验通过 + 管理员 bootstrap 完成后、对外监听之前，对未决任务做
        // 只读核验（阶段 1 只读短事务读未决集 → 阶段 2 事务外板端核验 → 阶段 3 写入短事务落
        // recovery_checks 与 queued→held）。板端 transport 未接线（UnwiredBoardClient）：
        // task_record 一律回未连接错误，被记为未核验但绝不让恢复上抛，恢复路径永不发送。
        // 失败即上抛（fail-closed），未对账完成前不进入对外服务。
        let process_epoch = uuid::Uuid::new_v4();
        let task_repo = SqlxTaskRepository::new(db_handle.0.clone());
        let board_client = UnwiredBoardClient;
        let recovery = recover_process(&task_repo, &board_client, process_epoch).await?;
        eprintln!(
            "controller 启动恢复对账完成: process_epoch={process_epoch} \
             已核验={} 置held={} 置expired={} 发送={}",
            recovery.read_only_verified,
            recovery.marked_held,
            recovery.marked_expired,
            recovery.execute_calls,
        );
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
                )?)?;
                Ok::<_, Box<dyn std::error::Error>>(router)
            },
            // 联合门禁全部完成后才绑端口。
            move |router| async move {
                let listener = tokio::net::TcpListener::bind(&listen_address).await?;
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await?;
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
}
