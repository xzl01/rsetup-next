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

use chrono::Utc;
use uuid::Uuid;

use crate::error::ControllerError;
use crate::tasks::preview::{
    PREVIEW_TTL, PreviewOutcome, PreviewTarget, is_canonical_uuid_v4_key, new_preview_token,
    preview_token_bytes, preview_token_digest, preview_token_nonce, validate_target_count,
};
use crate::tasks::repository::TaskRepository;
use crate::time::evidence::TimeEvidence;

mod actor;
mod audit;
mod authz_policy;
mod contracts;
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
pub use state_rules::{
    cancel_precondition, counts_json_of, empty_counts_json, main_task_convergence,
    submit_counts_json,
};
pub(crate) use time_policy::parse_created_time_evidence;
pub use time_policy::{
    QUEUE_TTL_MS, TimeReviewDecision, TimeReviewEvidenceKind, TimeReviewVerdict,
    advanced_reference_utc, queue_deadline_of, time_review_allows_extend, time_review_verdict,
};

use contracts::preview_ttl_error;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::model::ReasonCode;
    use crate::tasks::preview::{preview_token_bytes, preview_token_digest, preview_token_nonce};
    use crate::time::evidence::TimeQuality;
    use chrono::DateTime;
    use sqlx::Transaction;
    use sqlx::mysql::MySql;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 落盘记录：`(token_nonce, actor, targets, token_hash, expires_at)`。
    type RecordedPreview = ([u8; 32], Uuid, Vec<PreviewTarget>, [u8; 32], DateTime<Utc>);

    /// 零 DB 假仓储：只记录预览落盘、其余路径一律以存储错误短路。
    #[derive(Default)]
    struct FakeRepo {
        previews: Mutex<Vec<RecordedPreview>>,
        begins: AtomicUsize,
    }

    // 与生产实现同风格：future 的 Send 边界写进签名，故豁免 manual_async_fn。
    #[allow(clippy::manual_async_fn)]
    impl TaskRepository for FakeRepo {
        fn begin(
            &self,
        ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send
        {
            async move {
                self.begins.fetch_add(1, Ordering::Relaxed);
                Err(ControllerError::Database(sqlx::Error::RowNotFound))
            }
        }

        fn create_preview(
            &self,
            token_nonce: [u8; 32],
            actor_user_id: Uuid,
            targets: &[PreviewTarget],
            token_hash: &[u8; 32],
            expires_at: DateTime<Utc>,
        ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send {
            async move {
                self.previews.lock().expect("unpoisoned").push((
                    token_nonce,
                    actor_user_id,
                    targets.to_vec(),
                    *token_hash,
                    expires_at,
                ));
                Ok(())
            }
        }

        fn consume_preview_and_create_tasks(
            &self,
            _req: &SubmitRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send
        {
            async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
        }

        fn find_main_task(
            &self,
            _main_task_id: Uuid,
        ) -> impl std::future::Future<
            Output = Result<Option<crate::tasks::repository::MainTaskRow>, ControllerError>,
        > + Send {
            async { Ok(None) }
        }

        fn list_subtasks_page(
            &self,
            _main_task_id: Uuid,
            _cursor: Option<String>,
            _limit: usize,
        ) -> impl std::future::Future<
            Output = Result<
                (Vec<crate::tasks::repository::SubTaskRow>, Option<String>),
                ControllerError,
            >,
        > + Send {
            async { Ok((Vec::new(), None)) }
        }

        fn cancel_subtask(
            &self,
            _req: &CancelRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send
        {
            async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
        }

        fn time_review(
            &self,
            _req: &TimeReviewRequest,
            _clock: &TimeEvidence,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send
        {
            async {
                Err(TimeReviewError::Backend(ControllerError::Database(
                    sqlx::Error::RowNotFound,
                )))
            }
        }

        fn release_lock(
            &self,
            _req: &ReleaseRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<ReleaseLockOutcome, ReleaseLockError>> + Send
        {
            async {
                Err(ReleaseLockError::Backend(ControllerError::Database(
                    sqlx::Error::RowNotFound,
                )))
            }
        }
    }

    fn target(seed: u8, eligible: bool) -> PreviewTarget {
        PreviewTarget {
            device_id: [seed; 32],
            eligible,
            reason_code: if eligible {
                None
            } else {
                Some(ReasonCode::DeviceOffline)
            },
        }
    }

    fn submit_request(key: &str) -> SubmitRequest {
        let now = Utc::now();
        SubmitRequest {
            idempotency_key: key.to_owned(),
            request_hash: [1u8; 32],
            preview_token_hash: [2u8; 32],
            preview_token_nonce: [3u8; 32],
            observe_deadline: now + chrono::Duration::seconds(60),
            time_evidence: TimeEvidence {
                system_wall_utc: now,
                reference_utc: now,
                quality: TimeQuality::NtpValid,
                clock_epoch: Uuid::new_v4(),
                source: None,
                sample_age_ms: None,
                offset_ms: None,
                uncertainty_ms: None,
            },
        }
    }

    #[tokio::test]
    async fn preview_derives_hash_and_nonce_from_the_returned_token() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let actor = Uuid::new_v4();
        let outcome = service
            .preview(
                actor,
                vec![target(0x02, true), target(0x01, false), target(0x02, true)],
            )
            .await
            .expect("preview");

        // 对外只回一枚 64 位小写 hex token
        let raw = preview_token_bytes(&outcome.preview_token).expect("canonical token");
        // 目标按字节序升序去重，资格判定原样保留（不伪造资格）
        assert_eq!(
            outcome
                .targets
                .iter()
                .map(|target| target.device_id)
                .collect::<Vec<_>>(),
            vec![[0x01u8; 32], [0x02u8; 32]]
        );
        assert_eq!(
            outcome.targets[0].reason_code,
            Some(ReasonCode::DeviceOffline)
        );

        let stored = repo.previews.lock().expect("unpoisoned");
        assert_eq!(stored.len(), 1);
        let (nonce, stored_actor, stored_targets, hash, expires_at) = &stored[0];
        // RT-N2 闭环：token → 同一对派生函数 → 落库值
        assert_eq!(nonce, &preview_token_nonce(&raw));
        assert_eq!(hash, &preview_token_digest(&raw));
        assert_eq!(stored_actor, &actor);
        assert_eq!(stored_targets.len(), 2);
        assert!(*expires_at > Utc::now());
    }

    #[tokio::test]
    async fn preview_rejects_empty_and_oversized_batches_without_writing() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        assert!(matches!(
            service.preview(Uuid::new_v4(), Vec::new()).await,
            Err(ControllerError::InvalidArgument)
        ));
        // 1025 个互不相同的目标：去重后仍超限，必须在落盘前拒绝
        let too_many = (0..1025u32)
            .map(|i| {
                let mut device_id = [0u8; 32];
                device_id[..4].copy_from_slice(&i.to_be_bytes());
                PreviewTarget {
                    device_id,
                    eligible: true,
                    reason_code: None,
                }
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            service.preview(Uuid::new_v4(), too_many).await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(repo.previews.lock().expect("unpoisoned").is_empty());
    }

    #[tokio::test]
    async fn submit_rejects_non_canonical_idempotency_key_before_any_transaction() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: false,
        };
        for key in [
            "",
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "not-an-uuid",
        ] {
            let err = service.submit(&submit_request(key), &ctx).await;
            assert!(
                matches!(err, Err(SubmitError::InvalidArgument)),
                "键 {key:?} 必须 400"
            );
        }
        assert_eq!(repo.begins.load(Ordering::Relaxed), 0, "非法键不得开启事务");
    }

    #[tokio::test]
    async fn submit_propagates_storage_faults_as_backend_not_400() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: false,
        };
        let err = service
            .submit(
                &submit_request("3f2504e0-4f89-41d3-9a0c-0305e82c3301"),
                &ctx,
            )
            .await
            .expect_err("fake repo always fails to begin");
        assert!(matches!(
            err,
            SubmitError::Backend(ControllerError::Database(_))
        ));
        assert_eq!(repo.begins.load(Ordering::Relaxed), 1);
        // 503 语义保真：Backend(Database) 原样透传，不降级为 400/409
        assert!(matches!(
            err.into_controller_error(),
            ControllerError::Database(_)
        ));
    }

    #[tokio::test]
    async fn release_lock_validates_acknowledgement_before_any_transaction() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: true,
        };
        let base = ReleaseRequest {
            subtask_id: Uuid::new_v4(),
            expected_revision: 4,
            acknowledge_unknown: false,
            reason: "operator confirmed".to_owned(),
        };
        assert!(matches!(
            service.release_lock(&base, &ctx).await,
            Err(ReleaseLockError::UnknownNotAcknowledged)
        ));
        let blank = ReleaseRequest {
            acknowledge_unknown: true,
            reason: "   ".to_owned(),
            ..base.clone()
        };
        assert!(matches!(
            service.release_lock(&blank, &ctx).await,
            Err(ReleaseLockError::InvalidArgument)
        ));
        assert_eq!(repo.begins.load(Ordering::Relaxed), 0);
    }
}
