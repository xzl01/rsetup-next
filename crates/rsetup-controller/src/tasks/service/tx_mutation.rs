//! 任务状态变更事务段（取消、时限复核、解除占用与主任务持久汇总回写）。
//!
//! 包含 cancel_in_tx、time_review_in_tx、release_lock_in_tx、
//! read_sub_task_states 与 sync_main_task_summary。

use chrono::{DateTime, Utc};
use sqlx::Transaction;
use sqlx::mysql::MySql;
use uuid::Uuid;

use super::actor;
use super::audit;
use super::authz_policy;
use super::contracts::time_evidence_error;
use super::state_rules;
use super::time_policy::parse_created_time_evidence;
use crate::authz::sqlx_repo::load_snapshot_in_tx;
use crate::error::ControllerError;
use crate::tasks::model::{ReasonCode, SubTaskState};
use crate::tasks::repository::{
    column, enum_wire, fixed_mysql_bytes, mysql_uuid, tx_execute, tx_row, tx_rows,
};
use crate::tasks::service::{
    AuthCtx, CancelError, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome,
    ReleaseRequest, TargetAuthz, TimeReviewError, TimeReviewOutcome, TimeReviewRequest,
    TimeReviewVerdict, cancel_precondition, counts_json_of, main_task_convergence,
    time_review_verdict,
};
use crate::time::evidence::TimeEvidence;

pub(super) async fn cancel_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &CancelRequest,
    ctx: &AuthCtx,
) -> Result<CancelOutcome, CancelError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 事务内完整重验 actor（active && !must_change_password）：HTTP 层快照不可信，
    // 并发停用/待改初始口令必须失败关闭；is_admin 只作 admin 直通判定输入。
    let caller = actor::read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.writable() {
        return Err(CancelError::PermissionDenied);
    }
    let caller_admin = caller.is_admin;

    let row = tx_row(
        tx,
        "SELECT st.revision, st.state, st.device_id, st.main_task_id, mt.actor_user_id, \
         mt.revision AS main_revision FROM sub_tasks st \
         JOIN main_tasks mt ON mt.id = st.main_task_id WHERE st.id = ? FOR UPDATE",
        params![req.subtask_id.as_bytes().as_slice()],
    )
    .await?;
    let Some(row) = row else {
        // 与「不存在」同投影，不泄露他人任务是否存在
        return Err(CancelError::Backend(ControllerError::NotFound));
    };
    let owner = mysql_uuid(&column::<Vec<u8>>(&row, "actor_user_id")?)?;
    if !caller_admin && owner != ctx.user_id {
        return Err(CancelError::Backend(ControllerError::NotFound));
    }
    if column::<u64>(&row, "revision")? != req.expected_revision {
        return Ok(CancelOutcome::RevisionConflict);
    }
    let state = state_rules::sub_task_state_from_str(&column::<String>(&row, "state")?)?;
    if state_rules::is_terminal_state(state) {
        return Ok(CancelOutcome::AlreadyTerminal { state });
    }
    let device_id = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "device_id")?)?;
    let authz = load_snapshot_in_tx(
        tx,
        ctx.user_id.into_bytes(),
        std::slice::from_ref(&device_id),
    )
    .await?;
    let has_reboot = matches!(
        authz_policy::check_target_authz(&authz, device_id),
        TargetAuthz::Permitted
    );
    if !cancel_precondition(state, caller_admin, has_reboot) {
        return Ok(CancelOutcome::NotCancellable { state });
    }
    let updated = tx_execute(
        tx,
        "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
         updated_at = NOW(6) WHERE id = ? AND revision = ?",
        params![
            &enum_wire(&SubTaskState::Cancelled)?,
            &enum_wire(&ReasonCode::UserCancelled)?,
            req.subtask_id.as_bytes().as_slice(),
            req.expected_revision,
        ],
    )
    .await?;
    if updated != 1 {
        return Ok(CancelOutcome::RevisionConflict);
    }
    tx_execute(
        tx,
        "DELETE FROM device_operation_locks WHERE device_id = ? AND owner_sub_task_id = ? \
         AND generation = 1",
        params![&device_id[..], req.subtask_id.as_bytes().as_slice()],
    )
    .await?;
    // 取消改写子任务状态后，主任务持久汇总必须**同事务**重算回写（state/outcome/
    // counts_json 与 revision + 1 同条 UPDATE）；只递增子任务 revision 会让主行永远
    // 停留在 running，且 counts 漏掉 cancelled。主任务行已被上面的 FOR UPDATE 锁住。
    let main_task_id = mysql_uuid(&column::<Vec<u8>>(&row, "main_task_id")?)?;
    let main_revision = column::<u64>(&row, "main_revision")?;
    sync_main_task_summary(tx, main_task_id, main_revision).await?;
    audit::insert_task_audit(
        tx,
        &audit::TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.cancel",
            target_kind: "sub_task",
            target_id: &req.subtask_id.simple().to_string(),
            params_redacted: "{}",
            outcome: "success",
            time_evidence: &crate::db::system_fallback_time_evidence(),
        },
    )
    .await?;
    Ok(CancelOutcome::Cancelled)
}

pub(super) async fn time_review_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &TimeReviewRequest,
    clock: &TimeEvidence,
    ctx: &AuthCtx,
) -> Result<TimeReviewOutcome, TimeReviewError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 管理写操作在事务内完整重验 actor：active && is_admin && !must_change_password
    let caller = actor::read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.active_admin() {
        return Err(TimeReviewError::PermissionDenied);
    }
    let task = tx_row(
        tx,
        "SELECT revision, CAST(created_time_json AS CHAR) AS created_time_json \
         FROM main_tasks WHERE id = ? FOR UPDATE",
        params![req.main_task_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(TimeReviewError::InvalidArgument)?;
    if column::<u64>(&task, "revision")? != req.expected_revision {
        return Err(TimeReviewError::RevisionConflict);
    }
    // 锁定该主任务的全部 held 子任务（按 id 字节序，单一取锁段）。
    // 比对依据是 **queue_deadline**（排队截止）：held 子任务尚未派发，其寿命由排队
    // TTL 决定，绝不得混用派发后才生效的 observe_deadline 观察窗。
    let rows = tx_rows(
        tx,
        "SELECT id, device_id, lock_generation, queue_deadline FROM sub_tasks \
         WHERE main_task_id = ? AND state = ? ORDER BY id FOR UPDATE",
        params![
            req.main_task_id.as_bytes().as_slice(),
            &enum_wire(&SubTaskState::Held)?,
        ],
    )
    .await?;
    if rows.is_empty() {
        return Err(TimeReviewError::InvalidArgument);
    }
    let mut held = Vec::with_capacity(rows.len());
    let mut queue_deadlines = Vec::with_capacity(rows.len());
    for row in &rows {
        queue_deadlines.push(column::<Option<DateTime<Utc>>>(row, "queue_deadline")?);
        held.push((
            mysql_uuid(&column::<Vec<u8>>(row, "id")?)?,
            fixed_mysql_bytes::<32>(&column::<Vec<u8>>(row, "device_id")?)?,
            column::<u64>(row, "lock_generation")?,
        ));
    }
    // 时间安全核验（纯策略，零 IO）：创建时证据缺失/不可解析、创建时或当前时钟
    // 不可信、时钟纪元不连续、恢复决策下存在过期子任务，一律无法证明原期限有效。
    let created = parse_created_time_evidence(&column::<String>(&task, "created_time_json")?);
    let verdict = time_review_verdict(created.as_ref(), clock, req.decision, &queue_deadlines);
    match verdict {
        TimeReviewVerdict::Unverifiable => return Ok(TimeReviewOutcome::Inconclusive),
        TimeReviewVerdict::Contradiction => return Err(TimeReviewError::InvalidArgument),
        TimeReviewVerdict::Resume | TimeReviewVerdict::Expire => {}
    }
    let resume = verdict == TimeReviewVerdict::Resume;
    let target_state = if resume {
        SubTaskState::Queued
    } else {
        SubTaskState::Expired
    };
    let target_reason = if resume {
        None
    } else {
        Some(enum_wire(&ReasonCode::QueueExpired)?)
    };
    for (subtask_id, device_id, lock_generation) in &held {
        let updated = tx_execute(
            tx,
            "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
             updated_at = NOW(6) WHERE id = ? AND state = ?",
            params![
                &enum_wire(&target_state)?,
                &target_reason,
                subtask_id.as_bytes().as_slice(),
                &enum_wire(&SubTaskState::Held)?,
            ],
        )
        .await?;
        if updated != 1 {
            return Err(TimeReviewError::RevisionConflict);
        }
        if !resume {
            // 过期即释放本代际设备锁；绝不触碰该设备后续任务的新 generation
            tx_execute(
                tx,
                "DELETE FROM device_operation_locks WHERE device_id = ? \
                 AND owner_sub_task_id = ? AND generation = ?",
                params![
                    &device_id[..],
                    subtask_id.as_bytes().as_slice(),
                    *lock_generation,
                ],
            )
            .await?;
        }
    }
    // 主任务持久汇总与子任务变更同事务原子回写（RT-R8-3）：只递增 revision 会让主行
    // state/outcome/counts_json 严重陈旧。
    sync_main_task_summary(tx, req.main_task_id, req.expected_revision).await?;
    let audit_params = serde_json::json!({
        "decision": if resume { "resume_original_deadline" } else { "expire" },
        "evidence_kind": format!("{:?}", req.evidence_kind),
        "affected": held.len(),
    })
    .to_string();
    audit::insert_task_audit(
        tx,
        &audit::TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.time_review",
            target_kind: "main_task",
            target_id: &req.main_task_id.simple().to_string(),
            params_redacted: &audit_params,
            outcome: "success",
            time_evidence: &serde_json::to_string(clock).map_err(|_| time_evidence_error())?,
        },
    )
    .await?;
    Ok(TimeReviewOutcome::Completed {
        affected: held.len(),
    })
}

pub(super) async fn release_lock_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &ReleaseRequest,
    ctx: &AuthCtx,
) -> Result<ReleaseLockOutcome, ReleaseLockError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 管理写操作在事务内完整重验 actor：active && is_admin && !must_change_password
    let caller = actor::read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.active_admin() {
        return Err(ReleaseLockError::PermissionDenied);
    }
    let row = tx_row(
        tx,
        "SELECT revision, state, lock_generation, device_id FROM sub_tasks WHERE id = ? FOR UPDATE",
        params![req.subtask_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(ReleaseLockError::SubTaskNotFound)?;
    if column::<u64>(&row, "revision")? != req.expected_revision {
        return Err(ReleaseLockError::RevisionConflict);
    }
    // CAS 之前先真实解析状态：非法值即 DB 损坏（503），绝不当作 unknown 放行
    if state_rules::sub_task_state_from_str(&column::<String>(&row, "state")?)?
        != SubTaskState::Unknown
    {
        return Err(ReleaseLockError::InvalidArgument);
    }
    let device_id = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "device_id")?)?;
    let lock_generation = column::<u64>(&row, "lock_generation")?;
    let deleted = tx_execute(
        tx,
        "DELETE FROM device_operation_locks WHERE device_id = ? AND owner_sub_task_id = ? \
         AND generation = ?",
        params![
            &device_id[..],
            req.subtask_id.as_bytes().as_slice(),
            lock_generation,
        ],
    )
    .await?;
    if deleted != 1 {
        return Err(ReleaseLockError::RevisionConflict);
    }
    let updated = tx_execute(
        tx,
        "UPDATE sub_tasks SET revision = revision + 1, updated_at = NOW(6) \
         WHERE id = ? AND revision = ?",
        params![req.subtask_id.as_bytes().as_slice(), req.expected_revision],
    )
    .await?;
    if updated != 1 {
        return Err(ReleaseLockError::RevisionConflict);
    }
    let audit_params = serde_json::json!({
        "reason": audit::redact_reason(&req.reason),
        "acknowledge_unknown": true,
    })
    .to_string();
    audit::insert_task_audit(
        tx,
        &audit::TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.release_lock",
            target_kind: "sub_task",
            target_id: &req.subtask_id.simple().to_string(),
            params_redacted: &audit_params,
            outcome: "success",
            time_evidence: &crate::db::system_fallback_time_evidence(),
        },
    )
    .await?;
    Ok(ReleaseLockOutcome::Released {
        revision: req.expected_revision + 1,
    })
}

/// 读回主任务全部子状态（主任务收敛与 counts 的唯一真相源）。
async fn read_sub_task_states(
    tx: &mut Transaction<'_, MySql>,
    main_task_id: Uuid,
) -> Result<Vec<SubTaskState>, ControllerError> {
    let rows = tx_rows(
        tx,
        "SELECT state FROM sub_tasks WHERE main_task_id = ? ORDER BY id",
        params![main_task_id.as_bytes().as_slice()],
    )
    .await?;
    let mut states = Vec::with_capacity(rows.len());
    for row in &rows {
        states.push(state_rules::sub_task_state_from_str(&column::<String>(
            row, "state",
        )?)?);
    }
    Ok(states)
}

/// 同事务回写主任务持久汇总：重查全部子任务最新状态 → 重算 `state`/`outcome` 与
/// 十态 `counts_json` → 连同 `revision + 1` 一条 UPDATE 落库（子任务变更与主行
/// 汇总同提交、同回滚，不存在「只动子任务、主行陈旧」的中间态）。
///
/// 调用方必须已在本事务内锁住主任务行（`FOR UPDATE`）；`expected_revision` 作 CAS
/// 兜底，0 行即并发冲突（503/409 语义由调用方错误类型承载）。
async fn sync_main_task_summary(
    tx: &mut Transaction<'_, MySql>,
    main_task_id: Uuid,
    expected_revision: u64,
) -> Result<(), ControllerError> {
    let states = read_sub_task_states(tx, main_task_id).await?;
    let (state, outcome) = main_task_convergence(&states);
    let updated = tx_execute(
        tx,
        "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, revision = revision + 1, \
         updated_at = NOW(6) WHERE id = ? AND revision = ?",
        params![
            &enum_wire(&state)?,
            &match outcome {
                None => None,
                Some(outcome) => Some(enum_wire(&outcome)?),
            },
            &counts_json_of(&state_rules::tally(&states)),
            main_task_id.as_bytes().as_slice(),
            expected_revision,
        ],
    )
    .await?;
    if updated != 1 {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(())
}
