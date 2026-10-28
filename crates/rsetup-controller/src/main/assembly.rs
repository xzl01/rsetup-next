//! 生产服务装配域（批次 E）。
//!
//! 包含板端连接管理器装配（`build_transport_manager`）、任务线时间证据源装配
//! （`task_time_source`）与生产装配唯一构造点（`production_inputs`）。

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
use rsetup_controller::http_auth::{AppState, HttpAuthState};
use rsetup_controller::http_device_status::{DeviceStatusState, SnapshotDeviceStatusAuthorizer};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_live::LiveAuth;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::operations::RuntimeRecoveryController;
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::production::{MetricsLineInputs, ProductionInputs};
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::adapter::TaskTimeSource;
use rsetup_controller::tasks::http::HttpTasksState;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::time::TimeGateHandle;
use rsetup_controller::transport::manager::{ManagerConfig, TransportManager};
use rsetup_controller::web_router;
use rsetup_controller::{ControllerError, DbPool};

/// 板端 transport 线的唯一构造点：连接管理器 + 与设备状态读线**共享**的快照 store。
///
/// 三条硬约束在此固定：
/// 1. 准入真相是 `devices` 表本身（`Arc::new(db.clone())` 实现
///    [`AdmissionTruth`](rsetup_controller::transport::manager::AdmissionTruth)，只读
///    `load`，不缓存、不写库）；
/// 2. 时间证据来自**同一个** `TimeGateHandle`（`task_time_source`），管理器不新建采样器；
/// 3. 快照 store 由本函数拥有并交给 `production_inputs` 的状态读线，轮询写入方与 HTTP
///    读取方共用同一个 `Arc<SnapshotStore>`，不存在第二份新鲜度真源。
pub(super) fn build_transport_manager(
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

/// 任务线时间源的顶层唯一装配：把**同一个** `TimeGateHandle` 交给任务适配器。
///
/// `TimeGateHandle` 内部状态（权威快照 / ready / 退避计数）全在 `Arc` 后，克隆只是
/// 再拿一个句柄，因此任务线读到的与启动门禁写的是同一份动态快照：不会新建第二个
/// 采样器、不会二次启动门禁、也不会把一次 `NtpValid` 缓存成永久可信。
pub(super) fn task_time_source(time_gate: &TimeGateHandle) -> Arc<dyn TaskTimeSource> {
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
pub(super) fn production_inputs(
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

#[cfg(test)]
mod tests {
    // assembly 域无内联测试（§5：四符号在 main.rs 测试区零直接引用，不凑测试覆写）。
}
