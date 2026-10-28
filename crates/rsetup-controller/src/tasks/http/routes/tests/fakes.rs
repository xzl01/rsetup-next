//! 路由集成测试 Fake 设施与共享 Helper。

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use serde_json::Value;
use uuid::Uuid;

use crate::ControllerError;
use crate::auth::service::{Login, Session, SessionPage};
use crate::events::EventBus;
use crate::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding, HttpAuthState, SessionAuthority};
use crate::http_security::HttpSecurityConfig;
use crate::operations::RuntimeRecoveryController;
use crate::tasks::http::build_tasks_router;
use crate::tasks::http::contract::{
    CancelTaskRequest, CancelTaskResponse, ChildEntry, ChildrenPage, ReleaseLockRequest,
    ReleaseLockResponse, TaskApi, TaskDetail, TaskFuture, TaskPage, TimeReviewTaskRequest,
};
use crate::tasks::http::fixtures::{RAW_TOKEN, fixture_detail, fixture_session};
use crate::tasks::http::state::HttpTasksState;
use crate::tasks::model::{EvidenceKind, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{PreviewOutcome, PreviewTarget};
use crate::tasks::service::SubmitOutcome;

pub(super) const CSRF_TOKEN: &str = "csrf-token-fixture";

#[derive(Clone, Copy)]
pub(super) enum FakeFailure {
    NotFound,
    RevisionConflict,
    PermissionDenied,
    InvalidArgument,
    Database,
    ResourceExhausted,
    IdempotencyConflict,
    PreviewInvalidState,
}

impl FakeFailure {
    pub(super) fn error(self) -> ControllerError {
        match self {
            FakeFailure::NotFound => ControllerError::NotFound,
            FakeFailure::RevisionConflict => ControllerError::RevisionConflict,
            FakeFailure::PermissionDenied => ControllerError::PermissionDenied,
            FakeFailure::InvalidArgument => ControllerError::InvalidArgument,
            FakeFailure::Database => ControllerError::Database(sqlx::Error::RowNotFound),
            FakeFailure::ResourceExhausted => ControllerError::ResourceExhausted,
            FakeFailure::IdempotencyConflict => ControllerError::TaskIdempotencyConflict,
            FakeFailure::PreviewInvalidState => ControllerError::TaskPreviewInvalidState,
        }
    }
}

pub(super) struct FakeTaskApi {
    pub(super) calls: Mutex<Vec<&'static str>>,
    pub(super) failure: Option<FakeFailure>,
    /// 仅作用于 `task_detail` 的失败注入（优先于 `failure`）：用于模拟
    /// 「time_review 报 InvalidArgument 而任务不存在（detail 报 NotFound）」
    /// 的服务层组合。
    pub(super) detail_failure: Option<FakeFailure>,
    /// 为真时 `task_detail` 忽略 `failure` 注入直接返回 `detail`：用于模拟
    /// 「time_review 报 InvalidArgument 但只读复核成功」的组合（no_held 与
    /// 决策矛盾两条分支）。
    pub(super) detail_ok: bool,
    pub(super) detail: TaskDetail,
    pub(super) page: TaskPage,
    pub(super) children: ChildrenPage,
    pub(super) preview: PreviewOutcome,
    pub(super) submit: SubmitOutcome,
    pub(super) cancel: CancelTaskResponse,
    pub(super) release: ReleaseLockResponse,
}

impl FakeTaskApi {
    pub(super) fn new() -> Self {
        let detail = fixture_detail();
        Self {
            calls: Mutex::new(Vec::new()),
            failure: None,
            detail_failure: None,
            detail_ok: false,
            preview: PreviewOutcome {
                preview_token: "cd".repeat(32),
                targets: vec![
                    PreviewTarget {
                        device_id: [0xAA; 32],
                        eligible: true,
                        reason_code: None,
                    },
                    PreviewTarget {
                        device_id: [0xBB; 32],
                        eligible: false,
                        reason_code: Some(ReasonCode::DeviceBusy),
                    },
                ],
            },
            submit: SubmitOutcome::Created {
                main_task_id: detail.id,
            },
            cancel: CancelTaskResponse {
                accepted: true,
                cancelled: Some(vec![detail.id]),
            },
            release: ReleaseLockResponse {
                released: true,
                state: SubTaskState::Unknown,
            },
            children: ChildrenPage {
                items: vec![
                    ChildEntry {
                        id: Uuid::new_v4(),
                        device_id: "aa".repeat(32),
                        state: SubTaskState::Accepted,
                        reason_code: None,
                        revision: 3,
                        result_evidence: Some(EvidenceKind::BootTransitionObserved),
                        eligible: true,
                    },
                    ChildEntry {
                        id: Uuid::new_v4(),
                        device_id: "bb".repeat(32),
                        state: SubTaskState::Succeeded,
                        reason_code: None,
                        revision: 2,
                        result_evidence: None,
                        eligible: false,
                    },
                ],
                next_cursor: None,
            },
            detail: detail.clone(),
            page: TaskPage {
                items: vec![detail],
                next_cursor: None,
            },
        }
    }

    pub(super) fn record(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }

    pub(super) fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

impl TaskApi for FakeTaskApi {
    fn preview<'a>(
        &'a self,
        _actor: &'a Session,
        _device_ids: &'a [[u8; 32]],
        _group_ids: &'a [Uuid],
    ) -> TaskFuture<'a, PreviewOutcome> {
        self.record("preview");
        let outcome = self.preview.clone();
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(outcome), Err) })
    }

    fn submit<'a>(
        &'a self,
        _actor: &'a Session,
        _preview_token: &'a str,
        _idempotency_key: &'a str,
    ) -> TaskFuture<'a, SubmitOutcome> {
        self.record("submit");
        let outcome = self.submit;
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(outcome), Err) })
    }

    fn list_tasks<'a>(
        &'a self,
        _actor: &'a Session,
        _state: Option<MainTaskState>,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> TaskFuture<'a, TaskPage> {
        self.record("list_tasks");
        let page = self.page.clone();
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(page), Err) })
    }

    fn task_detail<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
    ) -> TaskFuture<'a, TaskDetail> {
        self.record("task_detail");
        let detail = self.detail.clone();
        let failure = if self.detail_ok {
            None
        } else {
            self.detail_failure.or(self.failure)
        }
        .map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(detail), Err) })
    }

    fn task_children<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> TaskFuture<'a, ChildrenPage> {
        self.record("task_children");
        let page = self.children.clone();
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(page), Err) })
    }

    fn cancel<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _request: CancelTaskRequest,
    ) -> TaskFuture<'a, CancelTaskResponse> {
        self.record("cancel");
        let response = self.cancel.clone();
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(response), Err) })
    }

    fn time_review<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _request: TimeReviewTaskRequest,
    ) -> TaskFuture<'a, TaskDetail> {
        self.record("time_review");
        let detail = self.detail.clone();
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(detail), Err) })
    }

    fn release_lock<'a>(
        &'a self,
        _actor: &'a Session,
        _subtask_id: Uuid,
        _request: ReleaseLockRequest,
    ) -> TaskFuture<'a, ReleaseLockResponse> {
        self.record("release_lock");
        let response = self.release;
        let failure = self.failure.map(FakeFailure::error);
        Box::pin(async move { failure.map_or(Ok(response), Err) })
    }
}

pub(super) struct FakeAuth {
    pub(super) session: Session,
}

impl AuthServiceApi for FakeAuth {
    fn login<'a>(&'a self, _username: &'a str, _password: &'a str) -> AuthFuture<'a, Login> {
        Box::pin(async { unreachable!("任务路由不得触达 login") })
    }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        let session = self.session.clone();
        let live = raw == RAW_TOKEN;
        Box::pin(async move {
            if live {
                Ok(session)
            } else {
                Err(ControllerError::InvalidArgument)
            }
        })
    }
    fn change_password<'a>(
        &'a self,
        _session: &'a Session,
        _current: &'a str,
        _new_password: &'a str,
    ) -> AuthFuture<'a, ()> {
        Box::pin(async { unreachable!("任务路由不得触达 change_password") })
    }
    fn logout<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, ()> {
        Box::pin(async { unreachable!("任务路由不得触达 logout") })
    }
    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        Box::pin(async { unreachable!("任务路由不得触达 authz_epoch") })
    }
    fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        digest == &self.session.digest
    }
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        // 任务线 fake：权威侧结论与内存时钟同构，管理员标志取 fake 会话用户。
        // 任务路由不建 SSE 流，本实现只为边界完整性存在，刻意不做宽松默认。
        let session_live = self.session.digest == *digest;
        let is_admin = self.session.user.is_admin;
        Box::pin(async move {
            Ok(SessionAuthority {
                session_live,
                is_admin,
                authz_epoch: 0,
            })
        })
    }
    fn list_sessions<'a>(
        &'a self,
        _session: &'a Session,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> AuthFuture<'a, SessionPage> {
        Box::pin(async { unreachable!("任务路由不得触达 list_sessions") })
    }
    fn revoke_by_alias<'a>(&'a self, _session: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
        Box::pin(async { unreachable!("任务路由不得触达 revoke_by_alias") })
    }
    fn revoke_others<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, u64> {
        Box::pin(async { unreachable!("任务路由不得触达 revoke_others") })
    }
}

pub(super) fn app(
    api: Arc<FakeTaskApi>,
    is_admin: bool,
    must_change_password: bool,
    with_csrf: bool,
) -> Router<()> {
    app_with_events(api, is_admin, must_change_password, with_csrf, None)
}

/// 同 [`app`]，但可注入事件总线：`None` 即「未开启 SSE 事件线」的装配形态。
pub(super) fn app_with_events(
    api: Arc<FakeTaskApi>,
    is_admin: bool,
    must_change_password: bool,
    with_csrf: bool,
    events: Option<EventBus>,
) -> Router<()> {
    build_app(api, is_admin, must_change_password, with_csrf, events, None)
}

/// 同 [`app`]，但注入受控恢复模式唯一真源（05 §9 提交闸门测试入口）：
/// 会话为非临时密码的管理员、CSRF 已绑定，排除其它门禁的干扰。
pub(super) fn app_with_recovery(
    api: Arc<FakeTaskApi>,
    recovery: Arc<RuntimeRecoveryController>,
) -> Router<()> {
    build_app(api, true, false, true, None, Some(recovery))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_app(
    api: Arc<FakeTaskApi>,
    is_admin: bool,
    must_change_password: bool,
    with_csrf: bool,
    events: Option<EventBus>,
    recovery: Option<Arc<RuntimeRecoveryController>>,
) -> Router<()> {
    let session = fixture_session(is_admin, must_change_password);
    let auth = Arc::new(HttpAuthState::new(
        Arc::new(FakeAuth {
            session: session.clone(),
        }),
        HttpSecurityConfig {
            allowed_hosts: vec!["localhost".into()],
            allowed_origin: Some("http://localhost".into()),
            trust_tls: false,
        },
    ));
    if with_csrf {
        auth.csrf.lock().unwrap().insert(
            session.digest,
            CsrfBinding {
                token: CSRF_TOKEN.to_string(),
                issued_at: Instant::now(),
            },
        );
    }
    build_tasks_router(HttpTasksState {
        api,
        auth,
        events,
        recovery,
    })
}

pub(super) fn get_request(uri: &str, with_cookie: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", "localhost");
    if with_cookie {
        builder = builder.header("cookie", format!("rsc_session={RAW_TOKEN}"));
    }
    builder.body(Body::empty()).unwrap()
}

pub(super) fn post_request(
    uri: &str,
    body: &str,
    with_cookie: bool,
    idempotency: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "localhost")
        .header("origin", "http://localhost")
        .header("content-type", "application/json")
        .header("x-csrf-token", CSRF_TOKEN);
    if with_cookie {
        builder = builder.header("cookie", format!("rsc_session={RAW_TOKEN}"));
    }
    if let Some(key) = idempotency {
        builder = builder.header("idempotency-key", key);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

pub(super) async fn envelope(resp: Response) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1_048_576).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

pub(super) const CREATE_TASK_BODY: &str = r#"{"preview_token":"00","confirm":true}"#;
