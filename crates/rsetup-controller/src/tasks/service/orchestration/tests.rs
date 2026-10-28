use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use sqlx::Transaction;
use sqlx::mysql::MySql;
use uuid::Uuid;

use super::*;
use crate::error::ControllerError;
use crate::tasks::model::ReasonCode;
use crate::tasks::preview::{
    PreviewTarget, preview_token_bytes, preview_token_digest, preview_token_nonce,
};
use crate::tasks::repository::TaskRepository;
use crate::tasks::service::{
    AuthCtx, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome, ReleaseRequest,
    SubmitError, SubmitOutcome, SubmitRequest, TimeReviewError, TimeReviewOutcome,
    TimeReviewRequest,
};
use crate::time::evidence::{TimeEvidence, TimeQuality};

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
    ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send {
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
    ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send {
        async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
    }

    fn time_review(
        &self,
        _req: &TimeReviewRequest,
        _clock: &TimeEvidence,
        _ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send {
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
