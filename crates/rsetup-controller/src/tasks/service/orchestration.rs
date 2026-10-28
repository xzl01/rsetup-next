//! 任务编排服务入口：承接预览签发、原子提交、取消、时限复核与解除占用。
//!
//! 包含 TaskService 结构体定义与其公开入口方法。

use chrono::Utc;
use uuid::Uuid;

use super::contracts::preview_ttl_error;
use super::tx_mutation;
use crate::error::ControllerError;
use crate::tasks::preview::{
    PREVIEW_TTL, PreviewOutcome, PreviewTarget, is_canonical_uuid_v4_key, new_preview_token,
    preview_token_bytes, preview_token_digest, preview_token_nonce, validate_target_count,
};
use crate::tasks::repository::TaskRepository;
use crate::tasks::service::{
    AuthCtx, CancelError, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome,
    ReleaseRequest, SubmitError, SubmitOutcome, SubmitRequest, TimeReviewError, TimeReviewOutcome,
    TimeReviewRequest,
};
use crate::time::evidence::TimeEvidence;

#[cfg(test)]
mod tests;

/// 任务编排服务：仓储是唯一事务入口，本类型不直接持有连接池。
#[derive(Debug)]
pub struct TaskService<R: TaskRepository> {
    pub repo: R,
}

impl<R: TaskRepository> TaskService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    /// 签发预览：规范化目标 → 生成 CSPRNG 令牌 → 服务端派生 hash/nonce → 落盘。
    ///
    /// 资格判定（设备在线、能力可用等）由调用方在传入的 `targets` 中给出；
    /// 本方法不伪造资格，也不改写 `eligible` / `reason_code`。
    pub async fn preview(
        &self,
        actor_user_id: Uuid,
        targets: Vec<PreviewTarget>,
    ) -> Result<PreviewOutcome, ControllerError> {
        let mut ordered = targets;
        ordered.sort_by_key(|target| target.device_id);
        ordered.dedup_by_key(|target| target.device_id);
        validate_target_count(ordered.len())?;

        let preview_token = new_preview_token();
        let raw_token = preview_token_bytes(&preview_token).ok_or(ControllerError::Crypto)?;
        let token_hash = preview_token_digest(&raw_token);
        let token_nonce = preview_token_nonce(&raw_token);
        let seconds = i64::try_from(PREVIEW_TTL.as_secs()).map_err(|_| preview_ttl_error())?;
        let ttl = chrono::Duration::try_seconds(seconds).ok_or_else(preview_ttl_error)?;
        let expires_at = Utc::now() + ttl;
        self.repo
            .create_preview(
                token_nonce,
                actor_user_id,
                &ordered,
                &token_hash,
                expires_at,
            )
            .await?;
        Ok(PreviewOutcome {
            preview_token,
            targets: ordered,
        })
    }

    /// 原子提交：幂等重放优先，其次消费预览并按锁序创建任务。
    pub async fn submit(
        &self,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> Result<SubmitOutcome, SubmitError> {
        if !is_canonical_uuid_v4_key(&req.idempotency_key) {
            return Err(SubmitError::InvalidArgument);
        }
        let mut tx = self.repo.begin().await?;
        match self.submit_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                // ⑪ 仅提交成功后才返回 Created/Replayed
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                // 任何失败路径都显式回滚、零残留；回滚失败不覆盖原始错误
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// 取消子任务：仅 `queued`/`held`，且要求管理员或该设备的 reboot 权限。
    pub async fn cancel(
        &self,
        req: &CancelRequest,
        ctx: &AuthCtx,
    ) -> Result<CancelOutcome, CancelError> {
        let mut tx = self.repo.begin().await?;
        match tx_mutation::cancel_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// 时限复核：只以服务端时钟快照真实比对，绝不延长任何期限。
    ///
    /// `clock` 必须由服务端时钟源（TimeGate）产出，绝不来自请求体。
    /// `Inconclusive`（原期限有效性无法证明）走**显式回滚**：释放 `FOR UPDATE`
    /// 行锁、零状态变更，`held` 子任务与主任务 revision 一律保持原样。
    pub async fn time_review(
        &self,
        req: &TimeReviewRequest,
        clock: &TimeEvidence,
        ctx: &AuthCtx,
    ) -> Result<TimeReviewOutcome, TimeReviewError> {
        let mut tx = self.repo.begin().await?;
        match tx_mutation::time_review_in_tx(&mut tx, req, clock, ctx).await {
            Ok(TimeReviewOutcome::Inconclusive) => {
                let _ = tx.rollback().await;
                Ok(TimeReviewOutcome::Inconclusive)
            }
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// 解除设备占用：只删除本代际锁，子任务保持 `unknown`，绝不触碰新 generation。
    pub async fn release_lock(
        &self,
        req: &ReleaseRequest,
        ctx: &AuthCtx,
    ) -> Result<ReleaseLockOutcome, ReleaseLockError> {
        if !req.acknowledge_unknown {
            return Err(ReleaseLockError::UnknownNotAcknowledged);
        }
        if req.reason.trim().is_empty() {
            return Err(ReleaseLockError::InvalidArgument);
        }
        let mut tx = self.repo.begin().await?;
        match tx_mutation::release_lock_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }
}
