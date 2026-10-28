//! 中控进程唯一入口（集成验收 Task 0b：全量 typed 生产装配）。
//!
//! 唯一构造点：单一 `DbPool` → 单一 `Arc<HttpAuthState>` → auth/management/tasks/
//! status/events 五条使用会话认证的线共享同一认证上下文（session 预过滤、CSRF
//! 绑定表与限速桶跨端点互相可见），metrics 是独立 Basic 认证边界，web 为静态资源线；
//! 七线再经 [`build_production_router`] 合并成唯一顶层 router。
//! 本入口不构造测试网关、`connect_lazy` 占位池、固定 offline/空能力或 authz
//! epoch 0，也不构造任何返回伪造取值的常量源；设备实时视图、准入变更通知、板端
//! `BoardClient` 与状态快照都由 [`rsetup_controller::transport::manager::TransportManager`]（连接管理器）与其
//! [`TransportBoardClient`] 适配真实承担；Runtime 恢复控制器真源由生产
//! [`rsetup_controller::operations::RuntimeRecoveryController`] 装配（见 `operations` 模块），HTTP 探针与任务调度
//! 循环共享同一个实例，不存在第二套恢复模式快照。

use std::sync::Arc;

use rsetup_controller::events::EventBus;
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::production::build_production_router;
use rsetup_controller::tasks::evidence::SqlxTaskEvidenceApplier;
use rsetup_controller::tasks::recover_process;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::time::start_time_gate_with_notifier;
use rsetup_controller::transport::board_client::TransportBoardClient;
use rsetup_controller::transport::listener::{
    SqlxTaskResultSink, TransportEnv, run_controller_listener_with_sink,
};
use rsetup_controller::transport::manager::ManagerConfig;
use rsetup_controller::{ControllerConfig, DbPool, bootstrap_admin, check_identity_schema};

#[path = "main/assembly.rs"]
mod assembly;
#[path = "main/background.rs"]
mod background;
#[path = "main/lifecycle.rs"]
mod lifecycle;
#[path = "main/recovery.rs"]
mod recovery;
#[path = "main/secret_log.rs"]
mod secret_log;
#[path = "main/startup_gate.rs"]
mod startup_gate;

use self::assembly::{build_transport_manager, production_inputs};
use self::background::{BACKGROUND_GRACE, SchedulerDeps, spawn_scheduler_loop};
use self::lifecycle::{
    BoardTask, boxed, run_supervised_lifecycle, settle_background_tasks, shutdown_signal,
    watch_turns_true,
};
use self::recovery::build_recovery_controller;
use self::secret_log::LogSecretSink;
use self::startup_gate::{
    StartupTimeGate, listen_after_startup_gates, start_if_ready, time_changed_sink,
    time_gate_config_from_env,
};

use tokio::sync::{oneshot, watch};

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
    #[test]
    fn default_cargo_run_selects_readonly_server() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "default-run = \"rsetup-controller\"")
        );
    }
}
