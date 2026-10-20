//! 中控进程唯一入口（集成验收 Task 0b：全量 typed 生产装配）。
//!
//! 唯一构造点：单一 `DbPool` → 单一 `Arc<HttpAuthState>` → auth/management/tasks/
//! status/web 五线共享同一认证上下文（session 预过滤、CSRF 绑定表与限速桶跨端点
//! 互相可见），再经 [`build_production_router`] 合并成唯一顶层 router。
//! 本入口不构造测试网关、`connect_lazy` 占位池、固定 offline/空能力或 authz
//! epoch 0，也不构造任何返回伪造取值的常量源；尚未交付的 transport 线三个源以
//! **读取即失败**的 fail-closed 装配位接入（见 [`UnwiredTransportSources`]）。

use std::sync::Arc;

use rsetup_controller::api::adapters::build_sqlx_management_state;
use rsetup_controller::api::approvals::ManagementNotificationSink;
use rsetup_controller::api::devices::{DeviceLiveProbe, DeviceLiveView};
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::api::{
    ManagementFuture, install_management_cursor_secret, load_management_cursor_secret_from_env,
};
use rsetup_controller::auth::service::AuthService;
use rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository;
use rsetup_controller::authz::sqlx_repo::SqlxAuthorizationSnapshotRepository;
use rsetup_controller::devices::service::AdmissionChangeEvent;
use rsetup_controller::events::EventBus;
use rsetup_controller::http_auth::{AppState, HttpAuthState};
use rsetup_controller::http_device_status::DeviceStatusState;
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_live::LiveAuth;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::production::{MetricsLineInputs, ProductionInputs, build_production_router};
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::board_client::{BoardClient, BoardClientError, BoardFuture};
use rsetup_controller::tasks::http::HttpTasksState;
use rsetup_controller::tasks::recover_process;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::time::{TimeGateConfig, start_time_gate};
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
/// 真实适配器交付后，只需在此处替换为 transport 的实现，五线装配拓扑不变。
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
/// 本结构，`recover_process` 的三阶段编排与五线装配拓扑不变。
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

/// 生产装配唯一构造点。
///
/// 顺序即门禁：运行时间零点由 `main` 第一条语句提供（**不在**本函数里构造，否则
/// uptime 会系统性低估"进程已经活多久"）→ 管理 cursor 密钥（唯一来源是环境变量，
/// 缺失/非法即 `Err`，绝不以「无 cursor 密钥」降级启动）→ 共享认证上下文 →
/// 六线状态各自 `auth.clone()`。
fn production_inputs(
    db: &DbPool,
    metrics_config: Arc<MetricsConfig>,
    metrics_runtime: Arc<MetricsRuntime>,
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

    // 事件总线：进程内唯一通道句柄（broadcast 容量 128）。SSE 读线与未来的
    // 业务发布方共享同一个句柄；本切片只交付发布/订阅基础设施本身，尚无业务
    // 发布方接线，因此事件流上真实出现的只有心跳与总线自身产生的 reset，
    // 不伪造任何 device/task/permissions/time 事件。
    let events = EventsState {
        auth: auth.clone(),
        bus: EventBus::new(),
        // 长连接权威复核周期：空闲连接也必须按会话/authz_epoch/降权切断。
        revalidate_interval: rsetup_controller::http_events::DEFAULT_REVALIDATE_INTERVAL,
    };

    let transport = Arc::new(UnwiredTransportSources);
    let management = build_sqlx_management_state(
        db.clone(),
        auth.clone(),
        transport.clone(),
        transport.clone(),
        transport,
    );

    let tasks = HttpTasksState {
        api: Arc::new(SqlxTaskApi::new(
            Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
            Arc::new(SqlxTaskRepository::new(db.0.clone())),
            Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
            cursor_secret,
        )),
        auth: auth.clone(),
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
    start_if_ready(check_identity_schema(&db), || async move {
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
        // 唯一生产装配点：认证四线共享同一 Arc<HttpAuthState>（指标线是独立认证
        // 边界），指针不一致即失败关闭。
        let router = build_production_router(production_inputs(
            &db_handle,
            metrics_config,
            metrics_runtime,
        )?)?;
        let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
