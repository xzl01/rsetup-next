//! 任务编排服务公开契约 DTO 与错误映射。
//!
//! 包含任务提交流程（`Submit*`）、取消流程（`Cancel*`）、时限复核流程（`TimeReview*`）、
//! 以及解除设备占用流程（`Release*`）的请求参数、执行结果与专用错误类型，以及与
//! [`ControllerError`] 之间的双向映射。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::ControllerError;
use crate::tasks::model::SubTaskState;
use crate::tasks::service::{TimeReviewDecision, TimeReviewEvidenceKind};
use crate::time::evidence::TimeEvidence;

/// HTTP 层由 `auth::service::Session` 产出的廉价前置快照；服务层在事务内重验
/// actor 行，**不信**本结构的 `is_admin` 布尔。
#[derive(Clone, Debug)]
pub struct AuthCtx {
    pub user_id: Uuid,
    pub is_admin: bool,
}

/// 提交请求：目标清单**只**来自服务端预览快照，客户端自报目标/摘要/counts 一律不接受，
/// 因此本结构不提供 `target` / `target_hash` / `counts_json` 字段。
#[derive(Clone, Debug)]
pub struct SubmitRequest {
    pub idempotency_key: String,
    pub request_hash: [u8; 32],
    /// 以下两字段均为服务端对 `preview_token` 的派生值，客户端永不携带。
    pub preview_token_hash: [u8; 32],
    pub preview_token_nonce: [u8; 32],
    pub observe_deadline: DateTime<Utc>,
    /// 创建时经服务端时钟取出的证据快照，与主任务同一事务持久化（RT-R8-2）。
    pub time_evidence: TimeEvidence,
}

/// 提交结果：`Created` 为首次创建，`Replayed` 为幂等重放（零副作用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    Created { main_task_id: Uuid },
    Replayed { main_task_id: Uuid },
}

/// 提交错误：请求期短路、事务内重验与存储透传三类。
#[derive(Debug)]
pub enum SubmitError {
    InvalidArgument,
    ActorInactive,
    DeviceBusyCount { busy: usize, total: usize },
    PreviewExpired,
    PreviewConsumed,
    IdempotencyConflict,
    NotVisible,
    Backend(ControllerError),
}

impl From<ControllerError> for SubmitError {
    fn from(err: ControllerError) -> Self {
        SubmitError::Backend(err)
    }
}

impl From<sqlx::Error> for SubmitError {
    fn from(err: sqlx::Error) -> Self {
        SubmitError::Backend(ControllerError::Database(err))
    }
}

impl SubmitError {
    /// HTTP 边界的显式转换（`TaskApi` adapter 使用，不以 `?` 直接传播本类型）。
    pub fn into_controller_error(self) -> ControllerError {
        self.into()
    }
}

impl From<SubmitError> for ControllerError {
    fn from(err: SubmitError) -> Self {
        match err {
            SubmitError::InvalidArgument => ControllerError::InvalidArgument,
            SubmitError::ActorInactive => ControllerError::PermissionDenied,
            // 防御兜底：忙目标在提交路径内落为 failed 子任务，不整体报错，更不得映射 409。
            SubmitError::DeviceBusyCount { .. } => ControllerError::InvalidArgument,
            SubmitError::PreviewExpired | SubmitError::PreviewConsumed => {
                ControllerError::TaskPreviewInvalidState
            }
            SubmitError::IdempotencyConflict => ControllerError::TaskIdempotencyConflict,
            SubmitError::NotVisible => ControllerError::NotFound,
            SubmitError::Backend(err) => err,
        }
    }
}

/// 取消请求。
#[derive(Clone, Debug)]
pub struct CancelRequest {
    pub subtask_id: Uuid,
    pub expected_revision: u64,
}

/// 取消结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    AlreadyTerminal { state: SubTaskState },
    NotCancellable { state: SubTaskState },
    RevisionConflict,
}

/// 取消错误（授权拒绝与存储透传）。
#[derive(Debug)]
pub enum CancelError {
    PermissionDenied,
    Backend(ControllerError),
}

impl From<ControllerError> for CancelError {
    fn from(err: ControllerError) -> Self {
        CancelError::Backend(err)
    }
}

impl From<sqlx::Error> for CancelError {
    fn from(err: sqlx::Error) -> Self {
        CancelError::Backend(ControllerError::Database(err))
    }
}

impl From<CancelError> for ControllerError {
    fn from(err: CancelError) -> Self {
        match err {
            CancelError::PermissionDenied => ControllerError::PermissionDenied,
            CancelError::Backend(err) => err,
        }
    }
}

/// 时限复核请求：`evidence_kind` 仅记审计；分派只依据服务端时钟快照的真实比对。
#[derive(Clone, Debug)]
pub struct TimeReviewRequest {
    pub main_task_id: Uuid,
    pub expected_revision: u64,
    pub decision: TimeReviewDecision,
    pub evidence_kind: TimeReviewEvidenceKind,
}

/// 时限复核结果。
#[derive(Debug)]
pub enum TimeReviewOutcome {
    Completed {
        affected: usize,
    },
    /// 原排队期限的有效性无法证明（当前或创建时时钟非 `NtpValid`、时钟纪元不连续、
    /// 创建证据缺失/不可解析，或恢复决策下已存在过期子任务）：
    /// 全部保持 `held`、零状态变更（事务回滚）。
    Inconclusive,
}

/// 时限复核错误。
#[derive(Debug)]
pub enum TimeReviewError {
    InvalidArgument,
    PermissionDenied,
    RevisionConflict,
    Backend(ControllerError),
}

impl From<ControllerError> for TimeReviewError {
    fn from(err: ControllerError) -> Self {
        TimeReviewError::Backend(err)
    }
}

impl From<sqlx::Error> for TimeReviewError {
    fn from(err: sqlx::Error) -> Self {
        TimeReviewError::Backend(ControllerError::Database(err))
    }
}

impl From<TimeReviewError> for ControllerError {
    fn from(err: TimeReviewError) -> Self {
        match err {
            TimeReviewError::InvalidArgument => ControllerError::InvalidArgument,
            TimeReviewError::PermissionDenied => ControllerError::PermissionDenied,
            TimeReviewError::RevisionConflict => ControllerError::RevisionConflict,
            TimeReviewError::Backend(err) => err,
        }
    }
}

/// 解除设备占用请求：`unknown` 子任务保持原状态，只删除本代际的设备锁。
#[derive(Clone, Debug)]
pub struct ReleaseRequest {
    pub subtask_id: Uuid,
    pub expected_revision: u64,
    pub acknowledge_unknown: bool,
    pub reason: String,
}

/// 解除占用结果。
#[derive(Debug)]
pub enum ReleaseLockOutcome {
    Released { revision: u64 },
}

/// 解除占用错误。
#[derive(Debug)]
pub enum ReleaseLockError {
    InvalidArgument,
    PermissionDenied,
    UnknownNotAcknowledged,
    SubTaskNotFound,
    RevisionConflict,
    Backend(ControllerError),
}

impl From<ControllerError> for ReleaseLockError {
    fn from(err: ControllerError) -> Self {
        ReleaseLockError::Backend(err)
    }
}

impl From<sqlx::Error> for ReleaseLockError {
    fn from(err: sqlx::Error) -> Self {
        ReleaseLockError::Backend(ControllerError::Database(err))
    }
}

impl From<ReleaseLockError> for ControllerError {
    fn from(err: ReleaseLockError) -> Self {
        match err {
            ReleaseLockError::InvalidArgument => ControllerError::InvalidArgument,
            ReleaseLockError::PermissionDenied => ControllerError::PermissionDenied,
            ReleaseLockError::UnknownNotAcknowledged => ControllerError::InvalidArgument,
            ReleaseLockError::SubTaskNotFound => ControllerError::NotFound,
            ReleaseLockError::RevisionConflict => ControllerError::RevisionConflict,
            ReleaseLockError::Backend(err) => err,
        }
    }
}

pub(super) fn preview_ttl_error() -> ControllerError {
    ControllerError::Config("preview ttl out of range".into())
}

pub(super) fn time_evidence_error() -> ControllerError {
    ControllerError::Config("time evidence is not serializable".into())
}

#[cfg(test)]
mod tests;
