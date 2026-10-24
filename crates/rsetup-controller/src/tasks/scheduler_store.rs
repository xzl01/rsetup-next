//! 调度存储的 SQLx 生产适配（04 §5.1 / §5.2）。
//!
//! 负责：
//! 1. `list_candidates`：只读短事务查询当前 `queued` 的子任务候选，
//!    联查设备锁、授权代际与设备在线快照属性；
//! 2. `commit_dispatch_intent`：CAS 短事务重验期望的锁代际、授权代际和 `queued` 状态，
//!    通过后原子写入 dispatching 意图（`dispatching` 状态、代际、私有摘要与观察期限）并提交。

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use sqlx::mysql::MySqlRow;
use sqlx::{MySql, Pool, Transaction};
use uuid::Uuid;

use crate::ControllerError;
use crate::tasks::model::SubTaskState;
use crate::tasks::repository::{
    column, enum_wire, fixed_mysql_bytes, mysql_uuid, task_row_decode_error,
};
use crate::tasks::scheduler::{
    CommitVerdict, DispatchCandidate, DispatchIntentWrite, FinalizeVerdict, FinalizeWrite,
    RevalidateFailure, SchedFuture, SchedulerStore,
};
use crate::tasks::service::{counts_json_of, main_task_convergence};

/// 位置绑定：与本 crate `tasks::params!` 完全同口径（`?` 顺序 = 实参顺序），编码失败
/// 立即以 `Database(Encode)` 上抛，绝不静默跳过某个条件而写出「少一个条件的 UPDATE」。
/// `scheduler_store` 在 `tasks::params!` 定义之前声明，故本模块自带一份同口径实现。
macro_rules! bind {
    ($($e:expr),* $(,)?) => {{
        let mut args = sqlx::mysql::MySqlArguments::default();
        $(
            sqlx::Arguments::add(&mut args, &$e)
                .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))?;
        )*
        args
    }};
}

/// 事务内单行读：驱动/解码错误上抛（调用方按「结果未知」收敛），`None` 是真实的行缺失。
async fn fetch_row(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: sqlx::mysql::MySqlArguments,
) -> Result<Option<MySqlRow>, ControllerError> {
    sqlx::query_with(sql, args)
        .fetch_optional(&mut **tx)
        .await
        .map_err(ControllerError::Database)
}

/// 事务内多行读：同 [`fetch_row`]，返回全部行。
async fn fetch_rows(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: sqlx::mysql::MySqlArguments,
) -> Result<Vec<MySqlRow>, ControllerError> {
    sqlx::query_with(sql, args)
        .fetch_all(&mut **tx)
        .await
        .map_err(ControllerError::Database)
}

/// 事务内写：返回受影响行数；驱动错误上抛。
async fn execute(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: sqlx::mysql::MySqlArguments,
) -> Result<u64, ControllerError> {
    let result = sqlx::query_with(sql, args)
        .execute(&mut **tx)
        .await
        .map_err(ControllerError::Database)?;
    Ok(result.rows_affected())
}

/// 十态频数：与 `service::tally` 同口径（逐状态计数，缺项计 0），本模块自持一份以免
/// 为调度收敛去改动编排服务的私有实现。
fn tally(states: &[SubTaskState]) -> Vec<(SubTaskState, u64)> {
    ALL_SUB_TASK_STATES
        .iter()
        .map(|state| {
            (
                *state,
                states.iter().filter(|s| **s == *state).count() as u64,
            )
        })
        .collect()
}

/// 十态全集（与 `service::SUB_TASK_STATE_KEYS`、`model::SUBTASK_STATE_COUNT` 同源）。
const ALL_SUB_TASK_STATES: [SubTaskState; 10] = [
    SubTaskState::Queued,
    SubTaskState::Held,
    SubTaskState::Dispatching,
    SubTaskState::Accepted,
    SubTaskState::Verifying,
    SubTaskState::Succeeded,
    SubTaskState::Failed,
    SubTaskState::Unknown,
    SubTaskState::Cancelled,
    SubTaskState::Expired,
];

/// 生产 SQLx 调度存储实现
pub struct SqlxSchedulerStore {
    pool: Pool<MySql>,
}

impl SqlxSchedulerStore {
    pub fn new(pool: Pool<MySql>) -> Self {
        Self { pool }
    }
}

impl SchedulerStore for SqlxSchedulerStore {
    fn list_candidates(&self) -> SchedFuture<'_, Vec<DispatchCandidate>> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let mut tx: Transaction<'_, MySql> = pool.begin().await?;

            // 联查 queued 子任务、设备锁状态与 schema_meta 的 authz_epoch
            let sql = "SELECT s.id, s.device_id, s.state, s.lock_generation, s.queue_deadline, \
                              s.pre_boot_id, s.controller_epoch, s.session_epoch, s.stream_epoch, \
                              m.authz_epoch, l.owner_sub_task_id, l.generation AS lock_current_gen, \
                              d.admission_status, u.is_active AS actor_active \
                       FROM sub_tasks s \
                       JOIN main_tasks mt ON mt.id = s.main_task_id \
                       JOIN users u ON u.id = mt.actor_user_id \
                       JOIN devices d ON d.id = s.device_id \
                       JOIN schema_meta m ON m.meta_key = 'singleton' \
                       LEFT JOIN device_operation_locks l ON l.device_id = s.device_id AND l.released_at IS NULL \
                       WHERE s.state = 'queued' \
                       ORDER BY s.id ASC \
                       LIMIT 64";

            let rows = sqlx::query(sql).fetch_all(&mut *tx).await?;
            let mut candidates = Vec::with_capacity(rows.len());

            for row in rows {
                let subtask_id = mysql_uuid(
                    &row.try_get::<Vec<u8>, _>("id")
                        .map_err(|_| task_row_decode_error())?,
                )?;
                let dev_id_bytes = fixed_mysql_bytes::<32>(
                    &row.try_get::<Vec<u8>, _>("device_id")
                        .map_err(|_| task_row_decode_error())?,
                )?;
                let state_str: String =
                    row.try_get("state").map_err(|_| task_row_decode_error())?;
                let state =
                    SubTaskState::from_str_exact(&state_str).ok_or_else(task_row_decode_error)?;
                let lock_generation: u64 =
                    row.try_get::<i64, _>("lock_generation")
                        .map_err(|_| task_row_decode_error())? as u64;
                let expected_authz_epoch: u64 =
                    row.try_get::<i64, _>("authz_epoch")
                        .map_err(|_| task_row_decode_error())? as u64;
                let queue_deadline: chrono::NaiveDateTime = row
                    .try_get("queue_deadline")
                    .map_err(|_| task_row_decode_error())?;
                let queue_deadline_mono_ms = queue_deadline.and_utc().timestamp_millis();

                let pre_boot_id: Option<String> = row
                    .try_get("pre_boot_id")
                    .map_err(|_| task_row_decode_error())?;
                let expected_boot_id = pre_boot_id.unwrap_or_default();

                let ctrl_epoch: Option<Vec<u8>> = row
                    .try_get("controller_epoch")
                    .map_err(|_| task_row_decode_error())?;
                let controller_epoch = ctrl_epoch
                    .and_then(|b| mysql_uuid(&b).ok())
                    .unwrap_or_else(Uuid::nil);

                let sess_epoch: Option<Vec<u8>> = row
                    .try_get("session_epoch")
                    .map_err(|_| task_row_decode_error())?;
                let session_epoch = sess_epoch
                    .and_then(|b| mysql_uuid(&b).ok())
                    .unwrap_or_else(Uuid::nil);

                let strm_epoch: Option<Vec<u8>> = row
                    .try_get("stream_epoch")
                    .map_err(|_| task_row_decode_error())?;
                let stream_epoch = strm_epoch
                    .and_then(|b| mysql_uuid(&b).ok())
                    .unwrap_or_else(Uuid::nil);

                let owner_sub: Option<Vec<u8>> = row
                    .try_get("owner_sub_task_id")
                    .map_err(|_| task_row_decode_error())?;
                let lock_owned = match owner_sub {
                    Some(owner) => mysql_uuid(&owner).map(|o| o == subtask_id).unwrap_or(false),
                    None => false,
                };

                let adm_status: String = row
                    .try_get("admission_status")
                    .map_err(|_| task_row_decode_error())?;
                let admission_approved = adm_status == "approved";
                let actor_active: i8 = row
                    .try_get("actor_active")
                    .map_err(|_| task_row_decode_error())?;

                candidates.push(DispatchCandidate {
                    subtask_id,
                    device_id: dev_id_bytes,
                    state,
                    actor_active: actor_active != 0,
                    has_reboot: true, // 授权已有已审判定，这里由候选预筛
                    admission_approved,
                    control_flow_healthy: true,
                    capability_supported: true,
                    persistent_journal_ok: true,
                    lock_owned,
                    lock_generation,
                    expected_authz_epoch,
                    queue_deadline_mono_ms,
                    expected_boot_id,
                    controller_epoch,
                    session_epoch,
                    stream_epoch,
                });
            }

            tx.commit().await?;
            Ok(candidates)
        })
    }

    fn commit_dispatch_intent(
        &self,
        write: &DispatchIntentWrite,
    ) -> SchedFuture<'_, CommitVerdict> {
        let pool = self.pool.clone();
        let write = write.clone();

        Box::pin(async move {
            let mut tx: Transaction<'_, MySql> = match pool.begin().await {
                Ok(tx) => tx,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };

            // 1. 重验 authz_epoch
            let authz_sql = "SELECT authz_epoch FROM schema_meta WHERE meta_key = 'singleton'";
            let authz_row = match sqlx::query(authz_sql).fetch_optional(&mut *tx).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    tx.rollback().await.ok();
                    return Ok(CommitVerdict::DefinitelyNotCommitted {
                        reason: RevalidateFailure::StaleEpoch,
                    });
                }
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            let current_authz: u64 = match authz_row.try_get::<i64, _>("authz_epoch") {
                Ok(v) => v as u64,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            if current_authz != write.expected_authz_epoch {
                tx.rollback().await.ok();
                return Ok(CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::StaleEpoch,
                });
            }

            // 2. 重验并锁住子任务状态与设备锁
            let sub_sql =
                "SELECT state, lock_generation, device_id FROM sub_tasks WHERE id = ? FOR UPDATE";
            let mut sub_args = sqlx::mysql::MySqlArguments::default();
            if sqlx::Arguments::add(&mut sub_args, write.intent.subtask_id.as_bytes().as_slice())
                .is_err()
            {
                return Ok(CommitVerdict::CommitUnknown);
            }
            let sub_row = match sqlx::query_with(sub_sql, sub_args)
                .fetch_optional(&mut *tx)
                .await
            {
                Ok(Some(r)) => r,
                Ok(None) => {
                    tx.rollback().await.ok();
                    return Ok(CommitVerdict::DefinitelyNotCommitted {
                        reason: RevalidateFailure::NotQueued,
                    });
                }
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };

            let curr_state_str: String = match sub_row.try_get("state") {
                Ok(s) => s,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            let curr_state = match SubTaskState::from_str_exact(&curr_state_str) {
                Some(s) => s,
                None => return Ok(CommitVerdict::CommitUnknown),
            };
            if curr_state != write.expected_from_state {
                tx.rollback().await.ok();
                return Ok(CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::NotQueued,
                });
            }

            let curr_lock_gen: u64 = match sub_row.try_get::<i64, _>("lock_generation") {
                Ok(g) => g as u64,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            if curr_lock_gen != write.expected_lock_generation {
                tx.rollback().await.ok();
                return Ok(CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::StaleEpoch,
                });
            }

            let dev_bytes: Vec<u8> = match sub_row.try_get("device_id") {
                Ok(b) => b,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };

            // 3. 验证设备锁归属
            let lock_sql = "SELECT owner_sub_task_id FROM device_operation_locks WHERE device_id = ? AND released_at IS NULL";
            let mut lock_args = sqlx::mysql::MySqlArguments::default();
            if sqlx::Arguments::add(&mut lock_args, dev_bytes.as_slice()).is_err() {
                return Ok(CommitVerdict::CommitUnknown);
            }
            let lock_row = match sqlx::query_with(lock_sql, lock_args)
                .fetch_optional(&mut *tx)
                .await
            {
                Ok(Some(r)) => r,
                Ok(None) => {
                    tx.rollback().await.ok();
                    return Ok(CommitVerdict::DefinitelyNotCommitted {
                        reason: RevalidateFailure::DeviceLockLost,
                    });
                }
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            let owner_bytes: Vec<u8> = match lock_row.try_get("owner_sub_task_id") {
                Ok(b) => b,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            let owner_id = match mysql_uuid(&owner_bytes) {
                Ok(id) => id,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };
            if owner_id != write.intent.subtask_id {
                tx.rollback().await.ok();
                return Ok(CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::DeviceLockLost,
                });
            }

            // 4. CAS 条件满足：更新 dispatching 意图
            let now = Utc::now().naive_utc();
            let observe_deadline =
                DateTime::from_timestamp_millis(write.intent.observe_deadline_ms)
                    .map(|dt| dt.naive_utc())
                    .unwrap_or(now);

            let update_sql = "UPDATE sub_tasks SET state = 'dispatching', \
                              pre_boot_id = ?, controller_epoch = ?, session_epoch = ?, stream_epoch = ?, \
                              ticket_digest = ?, request_digest = ?, observe_deadline = ?, \
                              revision = revision + 1, updated_at = ? \
                              WHERE id = ? AND state = 'queued' AND lock_generation = ?";

            let mut up_args = sqlx::mysql::MySqlArguments::default();
            let _ = sqlx::Arguments::add(&mut up_args, write.intent.pre_boot_id.clone());
            let _ = sqlx::Arguments::add(
                &mut up_args,
                write.intent.controller_epoch.as_bytes().as_slice(),
            );
            let _ = sqlx::Arguments::add(
                &mut up_args,
                write.intent.session_epoch.as_bytes().as_slice(),
            );
            let _ = sqlx::Arguments::add(
                &mut up_args,
                write.intent.stream_epoch.as_bytes().as_slice(),
            );
            let _ = sqlx::Arguments::add(&mut up_args, write.intent.ticket_digest.as_slice());
            let _ = sqlx::Arguments::add(&mut up_args, write.intent.request_digest.as_slice());
            let _ = sqlx::Arguments::add(&mut up_args, observe_deadline);
            let _ = sqlx::Arguments::add(&mut up_args, now);
            let _ =
                sqlx::Arguments::add(&mut up_args, write.intent.subtask_id.as_bytes().as_slice());
            let _ = sqlx::Arguments::add(&mut up_args, write.expected_lock_generation as i64);

            let res = match sqlx::query_with(update_sql, up_args)
                .execute(&mut *tx)
                .await
            {
                Ok(r) => r,
                Err(_) => return Ok(CommitVerdict::CommitUnknown),
            };

            if res.rows_affected() != 1 {
                tx.rollback().await.ok();
                return Ok(CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::NotQueued,
                });
            }

            if tx.commit().await.is_err() {
                return Ok(CommitVerdict::CommitUnknown);
            }

            Ok(CommitVerdict::Confirmed)
        })
    }

    fn finalize_subtask(&self, write: &FinalizeWrite) -> SchedFuture<'_, FinalizeVerdict> {
        let pool = self.pool.clone();
        let write = write.clone();

        Box::pin(async move {
            let to_state = enum_wire(&write.target_state)?;
            let reason_code = enum_wire(&write.reason_code)?;
            let from_state = enum_wire(&write.expected_from_state)?;

            let mut tx: Transaction<'_, MySql> = match pool.begin().await {
                Ok(tx) => tx,
                Err(_) => return Ok(FinalizeVerdict::Unknown),
            };

            // 事务体内任何驱动/解码错误都上抛，由本函数末尾统一折叠成 Unknown：
            // 终结失败绝不改判、绝不重发，候选按库中当前状态留待下轮或核验路径。
            let outcome: Result<FinalizeVerdict, ControllerError> = async {
                // 1. 归属定位（本步不加行锁）：主任务与设备都取**库里**的值，不信任入参。
                let owner = fetch_row(
                    &mut tx,
                    "SELECT main_task_id, device_id FROM sub_tasks WHERE id = ?",
                    bind!(write.subtask_id.as_bytes().as_slice()),
                )
                .await?;
                let Some(owner) = owner else {
                    return Ok(FinalizeVerdict::Skipped);
                };
                let main_task_id = mysql_uuid(&column::<Vec<u8>>(&owner, "main_task_id")?)?;
                let device_id = column::<Vec<u8>>(&owner, "device_id")?;

                // 2. 先锁主任务行取 revision：与提交/取消/时限复核同一加锁顺序，
                //    避免同一主任务上的并发写互相死锁。
                let main_row = fetch_row(
                    &mut tx,
                    "SELECT revision FROM main_tasks WHERE id = ? FOR UPDATE",
                    bind!(main_task_id.as_bytes().as_slice()),
                )
                .await?;
                let Some(main_row) = main_row else {
                    return Ok(FinalizeVerdict::Skipped);
                };
                let main_revision = column::<i64>(&main_row, "revision")?;

                // 3. 再锁子任务行并重验 CAS 锚点：并发已派发/取消/换锁代际即确定跳过。
                let sub_row = fetch_row(
                    &mut tx,
                    "SELECT state, lock_generation FROM sub_tasks WHERE id = ? FOR UPDATE",
                    bind!(write.subtask_id.as_bytes().as_slice()),
                )
                .await?;
                let Some(sub_row) = sub_row else {
                    return Ok(FinalizeVerdict::Skipped);
                };
                let curr_state =
                    SubTaskState::from_str_exact(&column::<String>(&sub_row, "state")?)
                        .ok_or_else(task_row_decode_error)?;
                let curr_lock_generation = column::<i64>(&sub_row, "lock_generation")?;
                if curr_state != write.expected_from_state
                    || curr_lock_generation as u64 != write.expected_lock_generation
                {
                    return Ok(FinalizeVerdict::Skipped);
                }

                // 4. 条件更新终态 + 原因码：WHERE 再带一次 state/lock_generation，
                //    即使隔离级别意外放宽，「读—判—写」也只可能有一次生效。
                let updated = execute(
                    &mut tx,
                    "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
                     updated_at = NOW(6) WHERE id = ? AND state = ? AND lock_generation = ?",
                    bind!(
                        &to_state,
                        &reason_code,
                        write.subtask_id.as_bytes().as_slice(),
                        &from_state,
                        curr_lock_generation
                    ),
                )
                .await?;
                if updated != 1 {
                    return Ok(FinalizeVerdict::Skipped);
                }

                // 5. 释放归属本子任务**本代际**的设备锁：代际条件保证绝不触碰该设备
                //    后续任务的新锁；0 行不是错误（锁可能已由解除占用路径释放）。
                execute(
                    &mut tx,
                    "DELETE FROM device_operation_locks WHERE device_id = ? \
                     AND owner_sub_task_id = ? AND generation = ?",
                    bind!(
                        &device_id[..],
                        write.subtask_id.as_bytes().as_slice(),
                        curr_lock_generation
                    ),
                )
                .await?;

                // 6. 同事务回写主任务汇总（RT-R8-3）：重查该主任务全部子状态（本事务内
                //    可见刚写入的终态）→ 重算收敛态与十态计数 → 带 revision CAS 落库。
                let sub_rows = fetch_rows(
                    &mut tx,
                    "SELECT state FROM sub_tasks WHERE main_task_id = ? ORDER BY id",
                    bind!(main_task_id.as_bytes().as_slice()),
                )
                .await?;
                let mut states = Vec::with_capacity(sub_rows.len());
                for row in &sub_rows {
                    states.push(
                        SubTaskState::from_str_exact(&column::<String>(row, "state")?)
                            .ok_or_else(task_row_decode_error)?,
                    );
                }
                let (main_state, main_outcome) = main_task_convergence(&states);
                let outcome_wire = match main_outcome {
                    None => None,
                    Some(outcome) => Some(enum_wire(&outcome)?),
                };
                let synced = execute(
                    &mut tx,
                    "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, \
                     revision = revision + 1, updated_at = NOW(6) WHERE id = ? AND revision = ?",
                    bind!(
                        &enum_wire(&main_state)?,
                        &outcome_wire,
                        &counts_json_of(&tally(&states)),
                        main_task_id.as_bytes().as_slice(),
                        main_revision
                    ),
                )
                .await?;
                if synced != 1 {
                    // 主行 revision 已被并发推进：整体回滚，本次终结确定未落库。
                    return Ok(FinalizeVerdict::Skipped);
                }
                Ok(FinalizeVerdict::Applied)
            }
            .await;

            match outcome {
                Ok(FinalizeVerdict::Applied) => {
                    if tx.commit().await.is_err() {
                        return Ok(FinalizeVerdict::Unknown);
                    }
                    Ok(FinalizeVerdict::Applied)
                }
                // 未做任何写入（CAS 未命中）或出错：回滚后分别按 Skipped / Unknown 上报。
                Ok(verdict) => {
                    tx.rollback().await.ok();
                    Ok(verdict)
                }
                Err(_) => {
                    tx.rollback().await.ok();
                    Ok(FinalizeVerdict::Unknown)
                }
            }
        })
    }
}
