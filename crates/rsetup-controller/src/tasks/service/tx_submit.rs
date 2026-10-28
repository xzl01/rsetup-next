//! 任务提交事务段与提交流程 helper。
//!
//! 包含 constant_time_eq、PreviewSnapshotFixed、NewSubTask，
//! 以及 submit_in_tx 固有实现与提交流程 7 个 private helper。

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sqlx::Transaction;
use sqlx::mysql::{MySql, MySqlArguments};
use uuid::Uuid;

use super::actor;
use super::audit;
use super::authz_policy;
use super::contracts::time_evidence_error;
use super::state_rules;
use super::time_policy::queue_deadline_of;
use crate::authz::sqlx_repo::load_snapshot_in_tx;
use crate::error::ControllerError;
use crate::tasks::model::{MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{
    decode_preview_targets, device_target_hash, preview_generation_bytes, preview_mono_valid,
    validate_target_count,
};
use crate::tasks::repository::{
    TaskRepository, column, enum_wire, fixed_mysql_bytes, mysql_uuid, task_row_decode_error,
    tx_execute, tx_row, tx_rows,
};
use crate::tasks::service::{
    AuthCtx, SubmitError, SubmitOutcome, SubmitRequest, TargetAuthz, TaskService, counts_json_of,
    empty_counts_json, main_task_convergence,
};

#[cfg(test)]
mod tests;

/// 恒定时间字节比较：不短路，避免把摘要差异做成时序侧信道。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 预览快照在锁内读出并校验后的固定值。
#[derive(Clone, Debug)]
struct PreviewSnapshotFixed {
    target_hash: [u8; 32],
    targets_json: String,
    preview_generation: [u8; 16],
}

/// 待写入的子任务行（避免 7 参数以上的 helper 签名）。
///
/// 两条期限列语义独立、绝不互替：`queue_deadline` 是排队截止（服务端可信时基 +
/// 排队 TTL，时限复核的唯一比对依据）；`observe_deadline` 是观察窗截止（04 §5.2
/// 派发期语义，复核路径只读不写）。
#[derive(Clone, Copy, Debug)]
struct NewSubTask {
    subtask_id: Uuid,
    main_task_id: Uuid,
    device_id: [u8; 32],
    state: SubTaskState,
    reason_code: Option<ReasonCode>,
    queue_deadline: DateTime<Utc>,
    observe_deadline: DateTime<Utc>,
}

impl<R: TaskRepository> TaskService<R> {
    pub(super) async fn submit_in_tx(
        &self,
        tx: &mut Transaction<'_, MySql>,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> Result<SubmitOutcome, SubmitError> {
        // ② 写事务 guard：期望版本来自编译期配置，绝不从数据库行推断
        crate::integrity::lock_integrity_guard(tx).await?;

        // ③ actor 事务内重验（`users` 环节的唯一取锁点）：账号失效或待改初始口令即拒绝
        let actor = actor::read_actor_in_tx(tx, ctx.user_id).await?;
        if !actor.writable() {
            return Err(SubmitError::ActorInactive);
        }

        // ④ 先查持久幂等记录：存在即重放或冲突，绝不先消费预览再判幂等
        let idempotency_row = tx_row(
            tx,
            "SELECT id, request_hash FROM main_tasks WHERE actor_user_id = ? AND idempotency_key = ?",
            params![ctx.user_id.as_bytes().as_slice(), &req.idempotency_key],
        )
        .await?;
        if let Some(row) = idempotency_row {
            let main_task_id = mysql_uuid(&column::<Vec<u8>>(&row, "id")?)?;
            let stored = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "request_hash")?)?;
            if !constant_time_eq(&stored, &req.request_hash) {
                return Err(SubmitError::IdempotencyConflict);
            }
            // 同一请求此前已成功：即使预览此刻已过期或被消费，仍直接重放，零副作用
            return Ok(SubmitOutcome::Replayed { main_task_id });
        }

        // ⑤ 首次提交才判定有效性、原子消费预览并读出固定目标清单（唯一真相源）
        let snapshot = lock_preview_in_tx(tx, ctx.user_id, req).await?;
        let targets = decode_preview_targets(&snapshot.targets_json)
            .ok_or_else(|| SubmitError::Backend(task_row_decode_error()))?;
        validate_target_count(targets.len())
            .map_err(|_| SubmitError::Backend(task_row_decode_error()))?;
        if !constant_time_eq(&device_target_hash(&targets, &[]), &snapshot.target_hash) {
            // 快照摘要与清单不一致：DB 内容被污染，fail-closed
            return Err(SubmitError::Backend(task_row_decode_error()));
        }

        // ⑥ 锁序：roles → device_groups → devices（devices 段按 public_key 字节序）
        lock_authz_rows_in_order(tx, ctx.user_id, &targets).await?;

        // ⑦ 主任务先落库：counts 先写十键全零占位，第 ⑩ 步按真实结果回写
        let main_task_id = Uuid::new_v4();
        let created_time_json =
            serde_json::to_string(&req.time_evidence).map_err(|_| time_evidence_error())?;
        tx_execute(
            tx,
            "INSERT INTO main_tasks (id, actor_user_id, idempotency_key, request_hash, \
             target_hash, preview_token_hash, state, outcome, revision, counts_json, \
             created_time_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, 1, ?, ?, NOW(6), NOW(6))",
            params![
                main_task_id.as_bytes().as_slice(),
                ctx.user_id.as_bytes().as_slice(),
                &req.idempotency_key,
                &req.request_hash[..],
                &snapshot.target_hash[..],
                &req.preview_token_hash[..],
                &enum_wire(&MainTaskState::Queued)?,
                &empty_counts_json(),
                &created_time_json,
            ],
        )
        .await?;

        // ⑧ 逐目标重验当前授权并按锁序申请设备锁（同一快照、同一事务、零网络）
        let authz = load_snapshot_in_tx(tx, ctx.user_id.into_bytes(), &targets).await?;
        // 排队截止由服务端创建时证据在同一可信时基上算出（与 time_review 的比对同源）；
        // 观察窗截止属派发期语义，随请求携带的服务端值原样落 observe_deadline，两列不互替。
        let queue_deadline =
            queue_deadline_of(&req.time_evidence).ok_or_else(time_evidence_error)?;
        let mut created = Vec::with_capacity(targets.len());
        for device_id in &targets {
            let (state, reason_code) = match authz_policy::check_target_authz(&authz, *device_id) {
                TargetAuthz::NotVisible => return Err(SubmitError::NotVisible),
                // 可见但无权：保留固定目标清单、不申请设备锁，直接落 failed
                TargetAuthz::VisibleForbidden => {
                    (SubTaskState::Failed, Some(ReasonCode::PermissionDenied))
                }
                TargetAuthz::Permitted => (SubTaskState::Queued, None),
            };
            let subtask_id = Uuid::new_v4();
            insert_sub_task(
                tx,
                &NewSubTask {
                    subtask_id,
                    main_task_id,
                    device_id: *device_id,
                    state,
                    reason_code,
                    queue_deadline,
                    observe_deadline: req.observe_deadline,
                },
            )
            .await?;
            // 三种落库结局（queued / failed:PERMISSION_DENIED / failed:DEVICE_BUSY）
            // **一律**计入 created：counts_json 与主任务收敛必须与子任务行一一对应，
            // 漏计任一失败目标都会让主任务永远停在非终态。
            let final_state = match state {
                SubTaskState::Queued => {
                    if acquire_device_lock(tx, *device_id, main_task_id, subtask_id).await? {
                        SubTaskState::Queued
                    } else {
                        // 设备唯一操作锁 CAS 未插入即设备忙：不排队、不等待，就地落 failed
                        mark_sub_task_failed(tx, subtask_id, ReasonCode::DeviceBusy).await?;
                        SubTaskState::Failed
                    }
                }
                other => other,
            };
            created.push(final_state);
        }

        // ⑩ counts 真实回写 + 主任务收敛（与实际子状态分布同源，不写假持久化）
        let (state, outcome) = main_task_convergence(&created);
        let counts_json = counts_json_of(&state_rules::tally(&created));
        tx_execute(
            tx,
            "UPDATE main_tasks SET state = ?, outcome = ?, revision = revision + 1, \
             counts_json = ?, updated_at = NOW(6) WHERE id = ?",
            params![
                &enum_wire(&state)?,
                &match outcome {
                    None => None,
                    Some(outcome) => Some(enum_wire(&outcome)?),
                },
                &counts_json,
                main_task_id.as_bytes().as_slice(),
            ],
        )
        .await?;

        // ⑨ 同事务写 11 字段脱敏审计（不含设备 id、令牌与摘要）
        let audit_params = serde_json::json!({
            "targets": targets.len(),
            "queued": created.iter().filter(|s| **s == SubTaskState::Queued).count(),
            "failed": created.iter().filter(|s| **s == SubTaskState::Failed).count(),
        })
        .to_string();
        audit::insert_task_audit(
            tx,
            &audit::TaskAudit {
                actor_user_id: Some(ctx.user_id),
                event_type: "task.submit",
                target_kind: "main_task",
                target_id: &main_task_id.simple().to_string(),
                params_redacted: &audit_params,
                outcome: "success",
                time_evidence: &created_time_json,
            },
        )
        .await?;

        Ok(SubmitOutcome::Created { main_task_id })
    }
}

/// 锁定并校验预览快照，随后原子单次消费（`DELETE`；禁止 `UPDATE consumed_at` 两段式）。
async fn lock_preview_in_tx(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
    req: &SubmitRequest,
) -> Result<PreviewSnapshotFixed, SubmitError> {
    let row = tx_row(
        tx,
        "SELECT target_hash, CAST(targets_json AS CHAR) AS targets_json, preview_generation, \
         preview_token_hash, consumed_at, expires_at, NOW(6) AS db_now \
         FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? FOR UPDATE",
        params![&req.preview_token_nonce[..], user_id.as_bytes().as_slice()],
    )
    .await?;
    let Some(row) = row else {
        return Err(classify_missing_preview(tx, &req.preview_token_nonce).await?);
    };
    // 恒定时间核验：持久摘要必须等于请求 token 的同一派生摘要
    let stored_hash = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "preview_token_hash")?)?;
    if !constant_time_eq(&stored_hash, &req.preview_token_hash) {
        // 与「行不存在/已消费」同投影，不泄露差异
        return Err(SubmitError::PreviewConsumed);
    }
    if column::<Option<DateTime<Utc>>>(&row, "consumed_at")?.is_some() {
        return Err(SubmitError::PreviewConsumed);
    }
    let snapshot = PreviewSnapshotFixed {
        target_hash: fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "target_hash")?)?,
        targets_json: column::<String>(&row, "targets_json")?,
        preview_generation: fixed_mysql_bytes::<16>(&column::<Vec<u8>>(
            &row,
            "preview_generation",
        )?)?,
    };
    // 有效性三重判定，缺一即 PreviewExpired：
    // ① 进程启动代际（跨代际 token 无法重新派生匹配）；
    // ② 进程内单调 TTL（墙钟回拨不得延长）；
    // ③ DB 时钟上限仅作跨进程兜底，不单独作为有效性依据。
    if snapshot.preview_generation != preview_generation_bytes() {
        return Err(SubmitError::PreviewExpired);
    }
    if !preview_mono_valid(&req.preview_token_nonce) {
        return Err(SubmitError::PreviewExpired);
    }
    if column::<DateTime<Utc>>(&row, "expires_at")? <= column::<DateTime<Utc>>(&row, "db_now")? {
        return Err(SubmitError::PreviewExpired);
    }

    let deleted = tx_execute(
        tx,
        "DELETE FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? \
         AND consumed_at IS NULL AND expires_at > NOW(6) AND preview_generation = ?",
        params![
            &req.preview_token_nonce[..],
            user_id.as_bytes().as_slice(),
            &snapshot.preview_generation[..],
        ],
    )
    .await?;
    if deleted != 1 {
        return Err(classify_missing_preview(tx, &req.preview_token_nonce).await?);
    }
    Ok(snapshot)
}

/// 预览缺失/消费失败的原因判别（必须在回滚**之前**执行）。
async fn classify_missing_preview(
    tx: &mut Transaction<'_, MySql>,
    token_nonce: &[u8; 32],
) -> Result<SubmitError, ControllerError> {
    let row = tx_row(
        tx,
        "SELECT consumed_at FROM task_previews WHERE token_nonce = ?",
        params![&token_nonce[..]],
    )
    .await?;
    Ok(match row {
        Some(row) if column::<Option<DateTime<Utc>>>(&row, "consumed_at")?.is_some() => {
            SubmitError::PreviewConsumed
        }
        _ => SubmitError::PreviewExpired,
    })
}

/// 第 ⑥ 步锁序的唯一实现：三段严格 `roles → device_groups → devices`，每段按主键
/// 字节序升序 `FOR UPDATE`（`BTreeSet<Vec<u8>>` 自然序 = 升序 + 去重），空集合安全跳过。
/// `grants` 引用集以普通 SELECT 读出，本身不是锁目标；`device_operation_locks` 的 CAS
/// 只在本函数返回后进入。
async fn lock_authz_rows_in_order(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
    targets: &[[u8; 32]],
) -> Result<(), SubmitError> {
    let rows = tx_rows(
        tx,
        "SELECT role_id, scope_group_id FROM grants WHERE user_id = ?",
        params![user_id.as_bytes().as_slice()],
    )
    .await?;
    let mut role_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut group_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
    for row in &rows {
        if let Some(role_id) = column::<Option<Vec<u8>>>(row, "role_id")? {
            role_ids.insert(role_id);
        }
        if let Some(group_id) = column::<Option<Vec<u8>>>(row, "scope_group_id")? {
            group_ids.insert(group_id);
        }
    }
    lock_table_in_byte_order(tx, "SELECT id FROM roles WHERE id IN", &role_ids).await?;
    lock_table_in_byte_order(tx, "SELECT id FROM device_groups WHERE id IN", &group_ids).await?;
    let device_ids: BTreeSet<Vec<u8>> = targets.iter().map(|id| id.to_vec()).collect();
    lock_table_in_byte_order(
        tx,
        "SELECT public_key FROM devices WHERE public_key IN",
        &device_ids,
    )
    .await
}

/// 一段共用的按字节序 `FOR UPDATE` 锁表；空集合安全跳过。
async fn lock_table_in_byte_order(
    tx: &mut Transaction<'_, MySql>,
    prefix: &str,
    ids: &BTreeSet<Vec<u8>>,
) -> Result<(), SubmitError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!("{prefix} ({placeholders}) FOR UPDATE");
    let mut args = MySqlArguments::default();
    for id in ids {
        sqlx::Arguments::add(&mut args, &id[..]).map_err(|err| {
            SubmitError::Backend(ControllerError::Database(sqlx::Error::Encode(err)))
        })?;
    }
    tx_execute(tx, &sql, args).await?;
    Ok(())
}

/// 落一条子任务。`sub_tasks` 无 `outcome` 列（0004 DDL 真相源），
/// 子任务结局由 `state` + `reason_code` 表达。
///
/// `queue_deadline` 与 `observe_deadline` **分别**绑定各自的值：把排队截止写进观察
/// 窗列（或反之）会让复核拿观察窗判排队寿命，属跨生命周期混用。
async fn insert_sub_task(
    tx: &mut Transaction<'_, MySql>,
    row: &NewSubTask,
) -> Result<(), SubmitError> {
    tx_execute(
        tx,
        "INSERT INTO sub_tasks (id, main_task_id, device_id, state, reason_code, revision, \
         queue_deadline, observe_deadline, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?, NOW(6), NOW(6))",
        params![
            row.subtask_id.as_bytes().as_slice(),
            row.main_task_id.as_bytes().as_slice(),
            &row.device_id[..],
            &enum_wire(&row.state)?,
            &match row.reason_code {
                None => None,
                Some(code) => Some(enum_wire(&code)?),
            },
            row.queue_deadline,
            row.observe_deadline,
        ],
    )
    .await?;
    Ok(())
}

/// 设备唯一操作锁 CAS：条件插入成功（恰 1 行）即抢到，并把本代际 `generation = 1`
/// 回写子任务；未插入即设备忙，由调用方就地落 `failed`，不排队、不等待。
async fn acquire_device_lock(
    tx: &mut Transaction<'_, MySql>,
    device_id: [u8; 32],
    main_task_id: Uuid,
    subtask_id: Uuid,
) -> Result<bool, SubmitError> {
    let acquired = tx_execute(
        tx,
        "INSERT INTO device_operation_locks \
         (device_id, main_task_id, owner_sub_task_id, generation, acquired_at) \
         SELECT ?, ?, ?, 1, NOW(6) FROM DUAL \
         WHERE NOT EXISTS (SELECT 1 FROM device_operation_locks WHERE device_id = ?)",
        params![
            &device_id[..],
            main_task_id.as_bytes().as_slice(),
            subtask_id.as_bytes().as_slice(),
            &device_id[..],
        ],
    )
    .await?;
    if acquired != 1 {
        return Ok(false);
    }
    tx_execute(
        tx,
        "UPDATE sub_tasks SET lock_generation = 1 WHERE id = ?",
        params![subtask_id.as_bytes().as_slice()],
    )
    .await?;
    Ok(true)
}

/// 把子任务就地改写为 `failed` + 指定原因码（提交期设备忙的唯一实现）。
async fn mark_sub_task_failed(
    tx: &mut Transaction<'_, MySql>,
    subtask_id: Uuid,
    reason_code: ReasonCode,
) -> Result<(), SubmitError> {
    tx_execute(
        tx,
        "UPDATE sub_tasks SET state = ?, reason_code = ? WHERE id = ?",
        params![
            &enum_wire(&SubTaskState::Failed)?,
            &enum_wire(&reason_code)?,
            subtask_id.as_bytes().as_slice(),
        ],
    )
    .await?;
    Ok(())
}
