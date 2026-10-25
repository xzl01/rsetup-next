//! 调度存储的 SQLx 生产适配（04 §5.1 / §5.2）。
//!
//! 负责：
//! 1. `list_candidates`：只读短事务查询当前 `queued` 的子任务候选，
//!    联查设备锁、授权代际与设备在线快照属性；
//! 2. `commit_dispatch_intent`：CAS 短事务重验期望的锁代际、授权代际和 `queued` 状态，
//!    通过后原子写入 dispatching 意图（`dispatching` 状态、代际、私有摘要与观察期限）并提交。

use std::sync::Arc;

use chrono::Utc;
use sqlx::Row as _;
use sqlx::mysql::MySqlRow;
use sqlx::{MySql, Pool, Transaction};
use uuid::Uuid;

use crate::ControllerError;
use crate::tasks::model::{ReasonCode, SubTaskState};
use crate::tasks::repository::{
    column, enum_wire, fixed_mysql_bytes, mysql_uuid, task_row_decode_error,
};
use crate::tasks::scheduler::{
    CommitVerdict, DispatchCandidate, DispatchIntentWrite, FinalizeVerdict, FinalizeWrite,
    MonoClock, OBSERVE_WINDOW_MS, RevalidateFailure, SchedFuture, SchedulerStore, SystemMonoClock,
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

/// `BOOLEAN` 列（SQL 侧统一 `CAST(... AS SIGNED)`）的严格 0/1 解码：任何其它取值都
/// 是行解码失败。与 `service::strict_bool` 同语义，本模块自持一份以免跨模块耦合。
fn strict_bool(raw: i64) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(task_row_decode_error()),
    }
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
///
/// `clock` 与 `TaskScheduler` 必须是**同一个** [`MonoClock`] 实例：候选的
/// `queue_deadline_mono_ms` 由本字段映射（相对同一原点），调度器再用该时钟的读数
/// 与它比较，两者原点不同即等价于「拿别人的表比对期限」。
pub struct SqlxSchedulerStore {
    pool: Pool<MySql>,
    clock: Arc<dyn MonoClock>,
}

impl SqlxSchedulerStore {
    /// 生产装配：单调时钟缺省取 [`SystemMonoClock`]（进程启动 `Instant` 为原点）。
    pub fn new(pool: Pool<MySql>) -> Self {
        Self::with_clock(pool, Arc::new(SystemMonoClock::new()))
    }

    /// 注入与调度器共享的单调时钟（测试/受控装配用）。
    pub fn with_clock(pool: Pool<MySql>, clock: Arc<dyn MonoClock>) -> Self {
        Self { pool, clock }
    }

    /// 超时未决任务的持续核验扫尾（04 §5.3/§6）：把**已超过观察期限**
    /// （`state = 'dispatching' AND observe_deadline < NOW(6)`）且仍未收到结果的
    /// 子任务收敛为 `unknown` + `RESULT_TIMEOUT`，同事务回写主任务汇总。
    ///
    /// 存在的理由：`list_candidates` 只扫 `queued`，一旦 `dispatching` 的结果丢失，
    /// 没有任何路径会再碰它——`unknown` 属非终态、保留设备锁（04 §3），锁在后续
    /// 证据到达前不释放是规范要求，但**无限期占锁**会让同设备的后续任务被
    /// `DeviceBusy` 饿死；本方法是这个缺口的周期兜底。
    ///
    /// 语义边界：
    /// - 逐项独立短事务：单项 CAS 未命中（并发核验已改写状态，或时限复核把
    ///   `observe_deadline` 改得更晚）或写失败**绝不**影响其余候选、绝不重试；
    /// - 条件更新以**服务端墙钟**在 UPDATE 语句内重判 `observe_deadline < NOW(6)`，
    ///   读判与写判之间被核验路径续期的行必然不命中；
    /// - 返回实际收敛落库的子任务数（供调用方观测）。
    pub async fn converge_timed_out_dispatching(&self) -> Result<u64, ControllerError> {
        // 只读扫描取候选 id（无行锁，批量上限防单轮长尾）。
        let rows = sqlx::query(
            "SELECT id FROM sub_tasks \
             WHERE state = 'dispatching' AND observe_deadline IS NOT NULL \
               AND observe_deadline < NOW(6) \
             ORDER BY id ASC \
             LIMIT 64",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut converged = 0_u64;
        for row in rows {
            let subtask_id = mysql_uuid(&column::<Vec<u8>>(&row, "id")?)?;
            // 单项错误（连接/解码）折叠为「本次未收敛」：不改判、不重发；
            // 但必须留可观测痕迹——静默吞掉解码类错误会让整类缺陷（如
            // `BIGINT UNSIGNED` 误按 `i64` 解码）长期不可见。
            match self.converge_dispatch_timeout_one(subtask_id).await {
                Ok(true) => converged += 1,
                Ok(false) => {}
                Err(error) => {
                    eprintln!(
                        "converge_dispatch_timeout_one 单项失败 (subtask_id={subtask_id}): {error}"
                    );
                }
            }
        }
        Ok(converged)
    }

    /// [`Self::converge_timed_out_dispatching`] 的单项事务：与
    /// `finalize_subtask` 同一加锁顺序（先 `main_tasks` 后 `sub_tasks`，避免同一
    /// 主任务上的并发写互相死锁），`unknown` **保留**设备锁（04 §3）。
    async fn converge_dispatch_timeout_one(
        &self,
        subtask_id: Uuid,
    ) -> Result<bool, ControllerError> {
        let unknown_state = enum_wire(&SubTaskState::Unknown)?;
        let reason_code = enum_wire(&ReasonCode::ResultTimeout)?;

        let mut tx: Transaction<'_, MySql> = self.pool.begin().await?;
        let outcome: Result<bool, ControllerError> = async {
            // 1. 归属定位（不加行锁）：主任务取**库里**的值。
            let owner = fetch_row(
                &mut tx,
                "SELECT main_task_id FROM sub_tasks WHERE id = ?",
                bind!(subtask_id.as_bytes().as_slice()),
            )
            .await?;
            let Some(owner) = owner else {
                return Ok(false);
            };
            let main_task_id = mysql_uuid(&column::<Vec<u8>>(&owner, "main_task_id")?)?;

            // 2. 先锁主任务行取 revision。
            let main_row = fetch_row(
                &mut tx,
                "SELECT revision FROM main_tasks WHERE id = ? FOR UPDATE",
                bind!(main_task_id.as_bytes().as_slice()),
            )
            .await?;
            let Some(main_row) = main_row else {
                return Ok(false);
            };
            // v4 schema：`main_tasks.revision` 是 `BIGINT UNSIGNED NOT NULL`，
            // 必须按 `u64` 解码（驱动对带 UNSIGNED 标志的列拒绝 `i64`），CAS
            // 绑定时同样以 `u64` 原样回绑。
            let main_revision = column::<u64>(&main_row, "revision")?;

            // 3. 条件更新 `dispatching → unknown` + `RESULT_TIMEOUT`：WHERE 内用
            //    服务端墙钟**重判**观察期限与状态（行锁随行取得），读判与写判之间
            //    被核验路径续期/改判的行必然 0 行命中。
            let updated = execute(
                &mut tx,
                "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
                 updated_at = NOW(6) \
                 WHERE id = ? AND state = 'dispatching' AND observe_deadline IS NOT NULL \
                   AND observe_deadline < NOW(6)",
                bind!(
                    &unknown_state,
                    &reason_code,
                    subtask_id.as_bytes().as_slice()
                ),
            )
            .await?;
            if updated != 1 {
                return Ok(false);
            }

            // 4. `unknown` 保留设备锁（04 §3）：本事务**不**触碰 device_operation_locks。

            // 5. 同事务回写主任务汇总（RT-R8-3）：重查全部子状态 → 重算收敛态与
            //    十态计数 → 带 revision CAS 落库。
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
                // 主行 revision 已被并发推进：整体回滚，本次收敛确定未落库。
                return Ok(false);
            }
            Ok(true)
        }
        .await;

        match outcome {
            Ok(true) => {
                tx.commit().await?;
                Ok(true)
            }
            Ok(false) => {
                tx.rollback().await.ok();
                Ok(false)
            }
            Err(error) => {
                tx.rollback().await.ok();
                Err(error)
            }
        }
    }
}

impl SchedulerStore for SqlxSchedulerStore {
    fn list_candidates(&self) -> SchedFuture<'_, Vec<DispatchCandidate>> {
        let pool = self.pool.clone();
        let clock = Arc::clone(&self.clock);
        Box::pin(async move {
            let mut tx: Transaction<'_, MySql> = pool.begin().await?;

            // 联查 queued 子任务、设备锁状态与 schema_meta 的 authz_epoch。
            // v4 schema 口径：`devices` 主键是 `public_key BINARY(32)`（无自增 id）、
            // 准入由 `admission_state`/`review_decision`/`archived` 三列共同决定；
            // `users` 的启用列是 `active`；`schema_meta` 主键是 `singleton TINYINT`。
            let sql = "SELECT s.id, s.device_id, s.state, s.lock_generation, s.queue_deadline, \
                              s.pre_boot_id, s.controller_epoch, s.session_epoch, s.stream_epoch, \
                              m.authz_epoch, l.owner_sub_task_id, l.generation AS lock_current_gen, \
                              d.admission_state, d.review_decision, \
                              CAST(d.archived AS SIGNED) AS dev_archived, \
                              CAST(u.active AS SIGNED) AS actor_active \
                       FROM sub_tasks s \
                       JOIN main_tasks mt ON mt.id = s.main_task_id \
                       JOIN users u ON u.id = mt.actor_user_id \
                       JOIN devices d ON d.public_key = s.device_id \
                       JOIN schema_meta m ON m.singleton = 1 \
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
                // v4 schema：`sub_tasks.lock_generation` 与 `schema_meta.authz_epoch`
                // 均为 `BIGINT UNSIGNED NOT NULL`，必须按 `u64` 解码（驱动对带
                // UNSIGNED 标志的列拒绝 `i64`）。
                let lock_generation: u64 = row
                    .try_get::<u64, _>("lock_generation")
                    .map_err(|_| task_row_decode_error())?;
                let expected_authz_epoch: u64 = row
                    .try_get::<u64, _>("authz_epoch")
                    .map_err(|_| task_row_decode_error())?;
                let queue_deadline: chrono::NaiveDateTime = row
                    .try_get("queue_deadline")
                    .map_err(|_| task_row_decode_error())?;
                // `queue_deadline` 是**服务端 UTC 墙钟**；调度器一侧比较的是
                // [`MonoClock`] 的单调毫秒（进程启动为原点）。这里只做一次「剩余毫秒」
                // 搬运：以本次读数所在的单调原点为基准加上墙钟差值，绝不把 1.7×10¹²
                // 量级的 epoch 毫秒当成单调毫秒（那会让 TTL 永不生效）。时钟此刻不可读
                // 时按原点 0 折算，得到的期限必然早于调度器后续任一读数，即 fail-closed
                // 判过期，不会误放行。
                let now_utc = Utc::now();
                let queue_deadline_utc = queue_deadline.and_utc();
                let remaining_ms = (queue_deadline_utc - now_utc).num_milliseconds();
                let now_mono_ms = clock.now_ms().unwrap_or(0);
                let queue_deadline_mono_ms = now_mono_ms.saturating_add(remaining_ms);

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

                // 准入口径（与 `db.rs` 写侧、`authz::service` 读侧同源）：状态机列
                // `admission_state` 取 `APPROVED`、审计列 `review_decision` 取
                // `approved`、且设备未归档，三者同时成立才算已批准。
                let admission_state: String = row
                    .try_get("admission_state")
                    .map_err(|_| task_row_decode_error())?;
                let review_decision: String = row
                    .try_get("review_decision")
                    .map_err(|_| task_row_decode_error())?;
                let dev_archived = strict_bool(column::<i64>(&row, "dev_archived")?)?;
                let actor_active = strict_bool(column::<i64>(&row, "actor_active")?)?;
                let admission_approved =
                    admission_state == "APPROVED" && review_decision == "approved" && !dev_archived;

                candidates.push(DispatchCandidate {
                    subtask_id,
                    device_id: dev_id_bytes,
                    state,
                    actor_active,
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
            let authz_sql = "SELECT authz_epoch FROM schema_meta WHERE singleton = 1";
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
            // `schema_meta.authz_epoch` 是 `BIGINT UNSIGNED NOT NULL`：按 `u64` 解码。
            let current_authz: u64 = match authz_row.try_get::<u64, _>("authz_epoch") {
                Ok(v) => v,
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

            // `sub_tasks.lock_generation` 是 `BIGINT UNSIGNED NOT NULL`：按 `u64` 解码。
            let curr_lock_gen: u64 = match sub_row.try_get::<u64, _>("lock_generation") {
                Ok(g) => g,
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

            // 4. CAS 条件满足：更新 dispatching 意图。
            //    `observe_deadline` 落库列是**服务端 UTC 墙钟**，而意图里的
            //    `observe_deadline_ms` 是调度器单调毫秒（相对进程原点，不能直接
            //    `from_timestamp_millis`，否则落库成 1970 年附近）：以本次提交时刻的
            //    服务端墙钟为基线累加同一观察窗时长落库。
            let now = Utc::now().naive_utc();
            let window = chrono::Duration::milliseconds(OBSERVE_WINDOW_MS);
            let observe_deadline = now.checked_add_signed(window).unwrap_or(now);

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
            // CAS 锚点 `lock_generation` 列为 `BIGINT UNSIGNED`：直接以 `u64` 绑定。
            let _ = sqlx::Arguments::add(&mut up_args, write.expected_lock_generation);

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
                // v4 schema：`main_tasks.revision` 是 `BIGINT UNSIGNED NOT NULL`，
                // 必须按 `u64` 解码，CAS 绑定时同样以 `u64` 原样回绑。
                let main_revision = column::<u64>(&main_row, "revision")?;

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
                // v4 schema：`sub_tasks.lock_generation` 是 `BIGINT UNSIGNED NOT NULL`，
                // 必须按 `u64` 解码；与 `write.expected_lock_generation: u64` 同型直接比较。
                let curr_lock_generation = column::<u64>(&sub_row, "lock_generation")?;
                if curr_state != write.expected_from_state
                    || curr_lock_generation != write.expected_lock_generation
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
