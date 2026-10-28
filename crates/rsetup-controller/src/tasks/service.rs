//! 任务编排服务：预览签发、原子提交（幂等 + 设备唯一操作锁）、取消、时限复核与解除占用。

mod actor;
mod audit;
mod authz_policy;
mod contracts;
mod orchestration;
mod state_rules;
mod time_policy;
mod tx_mutation;
mod tx_submit;

pub use authz_policy::{TargetAuthz, snapshot_effective_permissions};
pub use contracts::{
    AuthCtx, CancelError, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome,
    ReleaseRequest, SubmitError, SubmitOutcome, SubmitRequest, TimeReviewError, TimeReviewOutcome,
    TimeReviewRequest,
};
pub use orchestration::TaskService;
pub use state_rules::{
    cancel_precondition, counts_json_of, empty_counts_json, main_task_convergence,
    submit_counts_json,
};
pub(crate) use time_policy::parse_created_time_evidence;
pub use time_policy::{
    QUEUE_TTL_MS, TimeReviewDecision, TimeReviewEvidenceKind, TimeReviewVerdict,
    advanced_reference_utc, queue_deadline_of, time_review_allows_extend, time_review_verdict,
};
