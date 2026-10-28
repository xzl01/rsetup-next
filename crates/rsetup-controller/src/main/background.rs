//! 后台任务与调度循环域（批次 D）。
//!
//! 包含后台关停宽限期常量、任务调度与持续核验循环抽取（`spawn_scheduler_loop` 与 `SchedulerDeps`）。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use rsetup_controller::operations::RuntimeRecoveryController;
use rsetup_controller::tasks::evidence::SqlxTaskEvidenceApplier;
use rsetup_controller::tasks::scheduler::{SchedulerPolicy, SystemMonoClock, TaskScheduler};
use rsetup_controller::tasks::scheduler_store::SqlxSchedulerStore;
use rsetup_controller::transport::board_client::TransportBoardClient;
use rsetup_controller::transport::manager::TransportManager;

/// 后台任务（撤销消费 / 准入复查 / 轮询驱动）的关停宽限期：三者只等 watch，不做 I/O，
/// 因此正常路径下应在本窗口内自行退出；超期才 abort 并 await 确认。
pub(super) const BACKGROUND_GRACE: Duration = Duration::from_secs(2);

/// 生产任务调度与持续核验循环的装配依赖。
pub(super) struct SchedulerDeps {
    pub(super) pool: sqlx::MySqlPool,
    pub(super) transport: Arc<TransportManager>,
    pub(super) process_epoch: uuid::Uuid,
    pub(super) recovery: Arc<RuntimeRecoveryController>,
}

/// 启动生产任务调度与持续核验循环。
pub(super) fn spawn_scheduler_loop(
    deps: SchedulerDeps,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let sched_pool = deps.pool;
    // 存储侧与调度器侧**共用同一个**单调时钟：候选的排队截止由存储侧折算成
    // 该时钟原点的单调毫秒，调度器再以同一时钟的读数比较，原点不同即恒不等。
    let sched_clock = Arc::new(SystemMonoClock::new());
    let sched_store = Arc::new(SqlxSchedulerStore::with_clock(
        sched_pool.clone(),
        sched_clock.clone(),
    ));
    let sched_board = Arc::new(TransportBoardClient::new(deps.transport.as_device_rpc()));
    // 执行结果落库线：与板端主动上报同一个 `SqlxTaskEvidenceApplier`
    // 语义——`execute` 同步返回的合法记录经同一状态机短事务推进并回写
    // 主任务汇总；未装配时调度器只有内存计数 + 只读核验。
    let sched_applier = Arc::new(SqlxTaskEvidenceApplier::new(sched_pool.clone(), None));
    let scheduler = Arc::new(
        TaskScheduler::new(
            sched_store.clone(),
            sched_board,
            sched_clock,
            SchedulerPolicy::default(),
            deps.process_epoch,
        )
        .with_applier(sched_applier),
    );
    // 持续核验扫尾（04 §5.3/§6）：`dispatching` 超过 `observe_deadline`
    // 仍无结果的子任务收敛为 `unknown` + `RESULT_TIMEOUT`（保留设备锁），
    // 不让丢失结果的未决任务无限期霸占设备锁导致后续任务饥饿。
    let sched_reap_store = sched_store;
    let mut sched_shutdown = shutdown;
    // 恢复模式闸门：与 transport 探针、HTTP `/system/status` 是同一个
    // `Arc<RuntimeRecoveryController>`（05 §9 唯一真源）。
    let sched_recovery = deps.recovery;
    // 每轮统计「已占槽」子任务（04 §5.1：dispatching/accepted/verifying 均占槽），
    // 让跨调度周期已在看板中的任务真正计入全局 16 槽上限。
    let sched_active_pool = sched_pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(1000));
        // 扫尾节流计数：派发每 tick 一次，超时收敛每 5 秒一次
        //（观察期限本身是分钟级，无需每秒全表重判）。
        let mut reap_ticks: u32 = 0;
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
                    // 超时未决扫尾先于取槽计数：已超观察期限、结果丢失
                    // 的 `dispatching` 先收敛为 `unknown`（04 §3 保留锁、
                    // 占槽语义不变），收敛本身让库里状态可审计。
                    reap_ticks = reap_ticks.wrapping_add(1);
                    if reap_ticks.is_multiple_of(5) {
                        match sched_reap_store.converge_timed_out_dispatching().await {
                            Ok(0) => {}
                            Ok(n) => eprintln!(
                                "controller task scheduler 超时dispatching收敛为unknown: {n}"
                            ),
                            Err(e) => eprintln!(
                                "controller task scheduler 超时dispatching扫尾失败: {e}"
                            ),
                        }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use rsetup_controller::DbPool;
    use rsetup_controller::polling::SnapshotStore;
    use rsetup_controller::time::{TimeGateConfig, TimeGateHandle};
    use rsetup_controller::transport::manager::ManagerConfig;

    #[tokio::test]
    async fn scheduler_deps_field_set_is_exact() {
        // 结构性断言：装配依赖恰为 {pool, transport, process_epoch, recovery}，写全字段，杜绝 ..Default 兜底。
        let dummy_pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
            .expect("connect_lazy 离线构造不触网");
        let transport = TransportManager::new(
            Arc::new(DbPool(dummy_pool.clone())),
            Arc::new(SnapshotStore::new()),
            Arc::new(RuntimeRecoveryController::new()),
            Arc::new(TimeGateHandle::new(
                &TimeGateConfig::default(),
                uuid::Uuid::new_v4(),
            )),
            ManagerConfig::default(),
        );
        let recovery = Arc::new(RuntimeRecoveryController::new());
        let process_epoch = uuid::Uuid::new_v4();

        let deps = SchedulerDeps {
            pool: dummy_pool,
            transport,
            process_epoch,
            recovery,
        };

        // 下面四行是“字段集合恰为 {pool, transport, process_epoch, recovery}”的编译期证明：
        // 逐字段取值断言，多一个字段或少一个字段均无法通过。
        let _pool: sqlx::MySqlPool = deps.pool.clone();
        let _transport: Arc<TransportManager> = Arc::clone(&deps.transport);
        let _process_epoch: uuid::Uuid = deps.process_epoch;
        let _recovery: Arc<RuntimeRecoveryController> = Arc::clone(&deps.recovery);
        assert_eq!(deps.process_epoch, process_epoch);
    }
}
