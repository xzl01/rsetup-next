//! `SqlxSchedulerStore` 对 v4 schema 的类型契约与调度入口回归。
//!
//! v4 schema 中 `main_tasks.revision`、`sub_tasks.revision`、
//! `sub_tasks.lock_generation`、`schema_meta.authz_epoch` 全部是
//! `BIGINT UNSIGNED NOT NULL`。SQLx MySQL 驱动的可解码性由
//! `Type::compatible` 严格把关：`i64` 仅在列**不带** `UNSIGNED` 标志时兼容，
//! 命中 `BIGINT UNSIGNED` 一律抛 `ColumnDecode`（本 crate 折叠为 503 DB 损坏）。
//! 调度存储曾把这些列按 `i64` 读取，在真实库上必现全量解码失败；本文件在驱动层
//! 把该契约钉死，并验证各调度入口无库时可调用且 fail-closed。

use std::time::Duration;

use rsetup_controller::ControllerError;
use rsetup_controller::tasks::model::{ReasonCode, SubTaskState, TaskIntent};
use rsetup_controller::tasks::scheduler::{
    CommitVerdict, DispatchIntentWrite, FinalizeVerdict, FinalizeWrite, SchedulerStore,
};
use rsetup_controller::tasks::scheduler_store::SqlxSchedulerStore;
use sqlx::{MySql, Type, TypeInfo as _};
use uuid::Uuid;

#[test]
fn sqlx_scheduler_store_type_implements_scheduler_store() {
    // 单元编译与类型约束测试：证明 SqlxSchedulerStore 确实实现了 SchedulerStore
    fn assert_impl<T: SchedulerStore>() {}
    assert_impl::<SqlxSchedulerStore>();
}

/// 类型契约钉死：`BIGINT UNSIGNED` 列（v4 的 revision/lock_generation/authz_epoch）
/// 必须按 `u64` 读写，按 `i64` 解码必被判不兼容——这正是缺陷的根因。
///
/// 不依赖数据库：直接对驱动侧 `Type::compatible` 断言，语义与 `try_get` /
/// `Arguments::add` 命中真实列时的判定完全一致。
#[test]
fn bigint_unsigned_columns_decode_as_u64_never_as_i64() {
    // `u64`/`i64` 的 `type_info()` 恰好分别带/不带 UNSIGNED 标志，即列类型
    // `BIGINT UNSIGNED` 与 `BIGINT`（`CAST(... AS SIGNED)` 产出后者）。
    let bigint_unsigned = <u64 as Type<MySql>>::type_info();
    let bigint_signed = <i64 as Type<MySql>>::type_info();
    assert_eq!(bigint_unsigned.name(), "BIGINT UNSIGNED");
    assert_eq!(bigint_signed.name(), "BIGINT");

    // 缺陷根因回归：`i64` 对 `BIGINT UNSIGNED` 必被判不兼容（真实列上
    // `try_get::<i64, _>` 即抛 ColumnDecode，绝不静默放行）。
    assert!(!<i64 as Type<MySql>>::compatible(&bigint_unsigned));
    // 修复后契约：这些列唯一的正确读取类型是 `u64`（解码与 CAS 绑定同型）。
    assert!(<u64 as Type<MySql>>::compatible(&bigint_unsigned));

    // 反向契约：`CAST(d.archived AS SIGNED)` 这类带符号列仍按 `i64` 读取，
    // 不得为绕开上述失败把带符号列也改成 `u64`。
    assert!(<i64 as Type<MySql>>::compatible(&bigint_signed));
    assert!(!<u64 as Type<MySql>>::compatible(&bigint_signed));
}

/// 懒连接池：构造不发起网络 I/O，首条语句起才会连 `127.0.0.1:1`
/// （本机保留端口，常规开发环境无人监听，连接立即被拒）。
fn unreachable_pool() -> sqlx::MySqlPool {
    sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_secs(2))
        .connect_lazy("mysql://rsetup-test:rsetup-test@127.0.0.1:1/rsetup-test")
        .expect("懒连接池仅在建池时解析连接串，本串必须合法")
}

/// 调度入口的可调用性与 fail-closed 行为（无真实库）：
/// - 固有入口 `converge_timed_out_dispatching` 与 trait 入口 `list_candidates`
///   在连不上库时必须返回 `Err`（绝不谎报收敛/候选）；
/// - `commit_dispatch_intent` / `finalize_subtask` 的既有契约是把连接失败折叠为
///   `CommitUnknown` / `FinalizeVerdict::Unknown`——绝不能是 `Confirmed` / `Applied`。
///
/// 沙箱环境若对该端口有监听导致连接挂起，则跳过该步断言（外层超时兜底，
/// 本测试不因此失败，也不因此永挂）。
#[tokio::test]
async fn scheduler_entrypoints_are_callable_and_fail_closed_without_db() {
    let store = SqlxSchedulerStore::new(unreachable_pool());

    // 固有入口：超时 dispatching 收敛扫描（本轮整改的方法）。
    match tokio::time::timeout(
        Duration::from_secs(15),
        store.converge_timed_out_dispatching(),
    )
    .await
    {
        Err(_elapsed) => {}
        Ok(res) => {
            let err: ControllerError =
                res.expect_err("数据库不可达时必须 Err fail-closed，绝不谎报收敛成功");
            // 错误可 Display（收敛循环的 eprintln 可观测性依赖这一点）。
            assert!(!err.to_string().is_empty());
        }
    }

    let store_ref: &dyn SchedulerStore = &store;

    // trait 入口 1：候选扫描，连接失败原样上抛 Err。
    match tokio::time::timeout(Duration::from_secs(15), store_ref.list_candidates()).await {
        Err(_elapsed) => {}
        Ok(res) => assert!(
            res.is_err(),
            "数据库不可达时候选扫描必须 Err，绝不返回伪造候选"
        ),
    }

    // trait 入口 2：提交意图短事务，契约是折叠为 CommitUnknown。
    let write = DispatchIntentWrite {
        intent: TaskIntent {
            subtask_id: Uuid::nil(),
            controller_epoch: Uuid::nil(),
            session_epoch: Uuid::nil(),
            stream_epoch: Uuid::nil(),
            pre_boot_id: String::new(),
            ticket_digest: [0_u8; 32],
            request_digest: [0_u8; 32],
            observe_deadline_ms: 1_000,
        },
        expected_lock_generation: 7,
        expected_authz_epoch: 3,
        expected_from_state: SubTaskState::Queued,
    };
    match tokio::time::timeout(
        Duration::from_secs(15),
        store_ref.commit_dispatch_intent(&write),
    )
    .await
    {
        Err(_elapsed) => {}
        Ok(res) => {
            let verdict = res.expect("提交入口的连接失败按契约折叠为 CommitUnknown，不上抛");
            assert_eq!(verdict, CommitVerdict::CommitUnknown);
        }
    }

    // trait 入口 3：终结短事务，契约是折叠为 Unknown。
    let finalize = FinalizeWrite {
        subtask_id: Uuid::nil(),
        target_state: SubTaskState::Expired,
        reason_code: ReasonCode::QueueExpired,
        expected_from_state: SubTaskState::Queued,
        expected_lock_generation: 7,
    };
    match tokio::time::timeout(
        Duration::from_secs(15),
        store_ref.finalize_subtask(&finalize),
    )
    .await
    {
        Err(_elapsed) => {}
        Ok(res) => {
            let verdict = res.expect("终结入口的连接失败按契约折叠为 Unknown，不上抛");
            assert_eq!(verdict, FinalizeVerdict::Unknown);
        }
    }
}
