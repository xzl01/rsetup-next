//! 任务编排服务：预览签发、原子提交（幂等 + 设备唯一操作锁）、取消、时限复核与解除占用。
//!
//! 事务纪律：一律「短事务 + 事务内零网络」，事务内绝不调用任何 Board 客户端；
//! 锁序固定为 `schema_meta → users → roles → device_groups → devices →
//! device_operation_locks`，不得逆序、不得重复锁同一行。
//!
//! 时钟纪律（RT-N5）：时限复核只接受服务端可信时钟快照（`TimeEvidence`），
//! 决不接受请求侧自报时钟或自报证据种类参与分派；任何路径都不会把
//! `created_at` / `observe_deadline` 改成更晚的值。
//!
//! 两条期限列各记各的含义，绝不互相顶替（`sub_tasks` DDL 为真相源）：
//! `queue_deadline` 是**排队截止**（04 §5.1 排队有效期），由创建时可信时基
//! （[`queue_deadline_of`]）算出，是时限复核判定 `held` 是否过期的唯一比对依据；
//! `observe_deadline` 是**观察窗截止**（04 §5.2/§5.3，自 `dispatching` 起算），
//! 由派发路径持有，复核路径只读不写、更不拿它替代排队截止。
//!
//! 行读取口径：本 crate 的 `sqlx` 未启用 `derive` 特性，原始行统一经
//! [`crate::tasks::repository::tx_rows`] / [`crate::tasks::repository::tx_row`] +
//! [`crate::tasks::repository::column`] 显式解码，checked 转换失败一律 503。

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
