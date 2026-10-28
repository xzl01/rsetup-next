//! 任务编排审计事件生成与持久化。
//!
//! 包含进程级序号源（`TASK_PROCESS_EPOCH` / `TASK_EVENT_SEQ`）、
//! 审计原因脱敏（`redact_reason`）、入参载荷结构体（`TaskAudit`）
//! 以及 11 列脱敏审计落库写入（`insert_task_audit`）。

use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use sqlx::Transaction;
use sqlx::mysql::MySql;
use uuid::Uuid;

use crate::error::ControllerError;
use crate::tasks::repository::tx_execute;

#[cfg(test)]
mod tests;

/// 进程内审计事件序号；与 `audit_events` 的 `(process_epoch, event_seq)` 唯一键同源。
static TASK_PROCESS_EPOCH: OnceLock<Uuid> = OnceLock::new();
static TASK_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// 11 字段脱敏审计的入参集合。
#[derive(Clone, Copy, Debug)]
pub(super) struct TaskAudit<'a> {
    pub(super) actor_user_id: Option<Uuid>,
    pub(super) event_type: &'a str,
    pub(super) target_kind: &'a str,
    pub(super) target_id: &'a str,
    pub(super) params_redacted: &'a str,
    pub(super) outcome: &'a str,
    pub(super) time_evidence: &'a str,
}

/// 审计原因脱敏：只保留词数指纹，绝不把自由文本原样写进审计。
pub(super) fn redact_reason(reason: &str) -> String {
    format!("redacted:{}", reason.split_whitespace().count())
}

/// 11 字段脱敏审计写入：`id` 与 `time_evidence` 由服务端生成，绝不取请求自报值。
pub(super) async fn insert_task_audit(
    tx: &mut Transaction<'_, MySql>,
    audit: &TaskAudit<'_>,
) -> Result<(), ControllerError> {
    let seq = crate::db::next_event_seq(&TASK_EVENT_SEQ)?;
    let epoch = TASK_PROCESS_EPOCH.get_or_init(Uuid::new_v4);
    let actor_user_id = audit.actor_user_id;
    let actor_user_bytes: Option<&[u8]> = actor_user_id.as_ref().map(|id| id.as_bytes().as_slice());
    tx_execute(
        tx,
        "INSERT INTO audit_events (id, actor_kind, actor_user_id, event_type, target_kind, \
         target_id, params_redacted, outcome, time_evidence, process_epoch, event_seq) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            Uuid::new_v4().as_bytes().as_slice(),
            "user",
            actor_user_bytes,
            audit.event_type,
            audit.target_kind,
            audit.target_id,
            audit.params_redacted,
            audit.outcome,
            audit.time_evidence,
            epoch.as_bytes().as_slice(),
            seq,
        ],
    )
    .await?;
    Ok(())
}
