//! 运行时运维守则（05 §8、05 §9 / 04 §5.2）：保留期清理守则与受控恢复模式。
//!
//! - [`recovery`]：[`RecoveryMode`] 与进程内唯一恢复模式真源
//!   [`RuntimeRecoveryController`](recovery::RuntimeRecoveryController)，
//!   并以生产形态实现 `api::system::RecoveryModeProbe`；
//! - [`retention`]：终态任务/审计/幂等数据的保留期清理守则——
//!   [`is_safe_to_delete`](retention::is_safe_to_delete) 是唯一删除准入判定，
//!   [`clean_retention_data`](retention::clean_retention_data) 的每条 SQL 都受其约束；
//!   同一文件另载解除受控恢复模式前的最短保留观察窗守则
//!   （[`RecoveryRetentionPolicy`](retention::RecoveryRetentionPolicy)）。

pub mod recovery;
pub mod retention;

pub use recovery::{
    MAX_RECOVERY_AUDIT_EVENTS, RECOVERY_AUDIT_ENTER, RECOVERY_AUDIT_EXIT,
    RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE, RECOVERY_AUDIT_OUTCOME_INVALID_STATE,
    RECOVERY_AUDIT_OUTCOME_OK, RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN, RecoveryAuditEvent,
    RecoveryExitChecklist, RecoveryMode, RecoveryOperation, RuntimeRecoveryController,
};
pub use retention::{
    CLEANUP_BATCH_LIMIT, DEFAULT_AUDIT_TTL_DAYS, DEFAULT_COMPLETED_TASK_TTL_DAYS,
    DEFAULT_IDEMPOTENCY_TTL_HOURS, RecoveryRetentionPolicy, RetentionPolicy, RetentionSummary,
    age_days, audit_cleanup_sql, clean_retention_data, is_deletable_terminal, is_safe_to_delete,
    main_task_cleanup_sql, subtask_cleanup_sql,
};
