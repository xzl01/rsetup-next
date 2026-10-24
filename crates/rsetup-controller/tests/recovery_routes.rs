//! 受控恢复模式管理端点的集成测试（05 §9 / 04 §5.2）。
//!
//! # 本文件证明什么、不证明什么（诚实边界）
//!
//! * **证明**：`POST /api/v1/system/recovery/enter|exit` 这两条管理路由的动态接线与
//!   门禁次序（会话认证 → 管理员前置 → body 校验 → 控制器判定）、响应契约
//!   （`data` 恰为 `{mode}`）、解除三段门禁的线上表现（清单不齐 400
//!   `CHECKLIST_INCOMPLETE`、观察窗未满/非受控模式 409 `INVALID_STATE`），以及
//!   **写读同源**：进入后 `GET /api/v1/system/status` 的 `recovery_mode` 立即变成
//!   `controlled_recovery`（写的是同一个 `Arc<RuntimeRecoveryController>`，不存在
//!   第二套模式快照）。传输线吊销同样实测：进入后活跃会话集合为空、审计记吊销计数。
//! * **不证明**：真实 TCP 隧道、真实板端进程、真实库 `audit_events` 账本行。传输线用
//!   `TransportManager` 内存登记 + `FakeConnection` 替身；审计断言只覆盖**进程内**去敏
//!   账本（`RuntimeRecoveryController::audit_events`），库内账本仍由网关层写入线负责。
//! * 不触达任何数据库：`SystemGateway` 用固定扁平投影替身。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rsetup_controller::ControllerError;
use rsetup_controller::HttpAuthState;
use rsetup_controller::api::ManagementFuture;
use rsetup_controller::api::system::{
    RecoveryModeProbe, RecoveryView, SystemApiState, SystemGateway, system_router,
};
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::auth::session::token_digest;
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding, SessionAuthority};
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::operations::recovery::{
    RECOVERY_AUDIT_ENTER, RECOVERY_AUDIT_EXIT, RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE,
    RECOVERY_AUDIT_OUTCOME_INVALID_STATE, RECOVERY_AUDIT_OUTCOME_OK,
    RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN, RecoveryAuditEvent, RuntimeRecoveryController,
};
use rsetup_controller::operations::retention::RecoveryRetentionPolicy;
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::tasks::adapter::TaskTimeSource;
use rsetup_controller::time::{TimeGateConfig, TimeGateHandle};
use rsetup_controller::transport::board_client::RpcReply;
use rsetup_controller::transport::manager::{
    AdmissionDecisionFuture, AdmissionTruth, CallError, ConnectionFuture, ConnectionState,
    DeviceConnection, ManagerConfig, NewSession, SystemNotice, TransportManager,
};
use rsetup_tunnel::handshake::AdmissionDecision;

const HOST: &str = "controller.test";
const ORIGIN: &str = "http://controller.test";
/// 64 位小写 hex 的 synthetic 会话 cookie（真实 cookie 名 `rsc_session`）。
const COOKIE_RAW: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// 绑定 `session.digest` 的 synthetic CSRF token。
const CSRF: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ADMIN_USERNAME: &str = "fixture_admin";

// ---------------------------------------------------------------------------
// 替身
// ---------------------------------------------------------------------------

/// 会话替身：`is_admin` 可控，其余身份字段固定且 synthetic（不含任何凭据语义）。
struct StubAuth {
    is_admin: bool,
}

impl AuthServiceApi for StubAuth {
    fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        let admin = self.is_admin;
        Box::pin(async move {
            if raw != COOKIE_RAW {
                return Err(ControllerError::InvalidArgument);
            }
            Ok(Session {
                user: IdentityUser {
                    id: *Uuid::from_u128(0x00000000000040008000000000000001).as_bytes(),
                    username: ADMIN_USERNAME.into(),
                    password_hash: "synthetic-unused".into(),
                    active: true,
                    is_admin: admin,
                    must_change_password: false,
                    revision: 1,
                },
                digest: token_digest(COOKIE_RAW.as_bytes()),
            })
        })
    }
    fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        *digest == token_digest(COOKIE_RAW.as_bytes())
    }
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        let session_live = *digest == token_digest(COOKIE_RAW.as_bytes());
        let admin = self.is_admin;
        Box::pin(async move {
            Ok(SessionAuthority {
                session_live,
                is_admin: admin,
                authz_epoch: 1,
            })
        })
    }
    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        Box::pin(async { Ok(1) })
    }
    fn change_password<'a>(
        &'a self,
        _s: &'a Session,
        _c: &'a str,
        _n: &'a str,
    ) -> AuthFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn list_sessions<'a>(
        &'a self,
        _s: &'a Session,
        _c: Option<&'a str>,
        _l: usize,
    ) -> AuthFuture<'a, SessionPage> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _i: &'a str) -> AuthFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
        Box::pin(async { Ok(0) })
    }
}

/// `SystemGateway` 替身：固定**扁平**投影（对齐 `SqlxManagementGateway::status`），
/// 五个计数为十进制字符串；恢复模式与 `ready` 由探针提供，网关不持有。
struct StubSystemGateway;

impl SystemGateway for StubSystemGateway {
    fn status<'a>(&'a self, _actor: &'a Session) -> ManagementFuture<'a, Value> {
        Box::pin(async {
            Ok(json!({
                "schema_version": 4,
                "users": "1", "devices": "1", "roles": "1", "grants": "1", "groups": "1",
            }))
        })
    }
}

/// 准入真相替身：本文件不测准入语义，登记路径不读它（撤销判据是 `apply_revoke`）。
struct StubAdmissionTruth;

impl AdmissionTruth for StubAdmissionTruth {
    fn load_current(&self, _public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        Box::pin(async { Ok(AdmissionDecision::Approved) })
    }
}

/// 设备连接替身：恒 `Ready`，记录 `abort`/信令调用次数（`apply_revoke` 的可观测面）。
#[derive(Default)]
struct FakeConnection {
    aborted: AtomicUsize,
    notices: AtomicUsize,
}

impl DeviceConnection for FakeConnection {
    fn request(
        &self,
        _action: &'static str,
        _payload: Vec<u8>,
    ) -> ConnectionFuture<'_, Result<RpcReply, CallError>> {
        Box::pin(async { Err(CallError::Closed) })
    }
    fn state(&self) -> ConnectionState {
        ConnectionState::Ready
    }
    fn send_system(&self, _notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>> {
        self.notices.fetch_add(1, Ordering::AcqRel);
        Box::pin(async { Ok(()) })
    }
    fn graceful_shutdown(&self) {}
    fn abort(&self) {
        self.aborted.fetch_add(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// 装配与请求 helper
// ---------------------------------------------------------------------------

/// 装配一条 system 管理线：`probe` 与 `recovery_controller` 是**同一个**控制器句柄
/// （写读同源的装配前提），因此 enter/exit 之后 `GET /system/status` 的读数必须随之改变。
fn router(
    controller: Option<Arc<RuntimeRecoveryController>>,
    transport: Option<Arc<TransportManager>>,
    is_admin: bool,
) -> Router<()> {
    let auth = Arc::new(HttpAuthState::new(
        Arc::new(StubAuth { is_admin }),
        HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
    ));
    auth.csrf.lock().unwrap().insert(
        token_digest(COOKIE_RAW.as_bytes()),
        CsrfBinding {
            token: CSRF.to_string(),
            issued_at: Instant::now(),
        },
    );
    let probe: Arc<dyn RecoveryModeProbe> = match &controller {
        Some(controller) => Arc::<RuntimeRecoveryController>::clone(controller),
        None => Arc::new(AlwaysFailingProbe),
    };
    system_router(SystemApiState {
        auth,
        gateway: Arc::new(StubSystemGateway),
        recovery_probe: probe,
        events: None,
        time_source: None,
        recovery_controller: controller,
        transport_manager: transport,
    })
}

/// 未接入控制器时的探针：证明 `recovery_probe` 失败与恢复管理端点是两条独立路径
/// （前者 503 `NOT_READY`，后者按装配缺失另行 503）。
struct AlwaysFailingProbe;

impl RecoveryModeProbe for AlwaysFailingProbe {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        Box::pin(async { Err(ControllerError::Config("fixture recovery down".into())) })
    }
}

fn authorized(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, format!("rsc_session={COOKIE_RAW}"))
        .header(HeaderName::from_static("x-csrf-token"), CSRF)
        .header(header::CONTENT_TYPE, "application/json");
    match body {
        Some(body) => builder.body(Body::from(body.to_string())).unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// 未认证请求：只有 Host，没有任何会话 cookie。
fn unauthorized(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HOST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap()
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1_048_576)
        .await
        .expect("响应体");
    serde_json::from_slice(&bytes).expect("JSON 响应体")
}

async fn status_code_and_body(router: Router<()>, request: Request<Body>) -> (StatusCode, Value) {
    let res = router.oneshot(request).await.unwrap();
    let status = res.status();
    (status, body_json(res).await)
}

fn complete_checklist() -> Value {
    json!({
        "revocations_checked": true,
        "grants_checked": true,
        "passwords_checked": true,
        "tasks_checked": true,
    })
}

/// 装配一台带 `count` 条 ready 会话的传输管理器，返回（管理器, 连接替身句柄）。
fn transport_with_sessions(
    probe: &Arc<RuntimeRecoveryController>,
    count: usize,
) -> (Arc<TransportManager>, Vec<Arc<FakeConnection>>) {
    // 恢复探针即控制器本身：证明管理写与探针读数共用同一个真源实例。
    let probe_dyn: Arc<dyn RecoveryModeProbe> = Arc::<RuntimeRecoveryController>::clone(probe);
    let time_source: Arc<dyn TaskTimeSource> = Arc::new(TimeGateHandle::new(
        &TimeGateConfig::default(),
        Uuid::new_v4(),
    ));
    let manager = TransportManager::new(
        Arc::new(StubAdmissionTruth),
        Arc::new(SnapshotStore::new()),
        probe_dyn,
        time_source,
        ManagerConfig::default(),
    );
    let mut connections = Vec::new();
    for index in 0..count {
        let key = [(index as u8) + 1; 32];
        let connection = Arc::new(FakeConnection::default());
        let pending = manager.track_pending(key);
        let half_open = manager.half_open_permit().expect("半开名额");
        manager
            .register_ready(NewSession {
                public_key: key,
                connection: Arc::clone(&connection) as Arc<dyn DeviceConnection>,
                pending,
                half_open,
            })
            .expect("登记 ready 会话");
        connections.push(connection);
    }
    (manager, connections)
}

// ---------------------------------------------------------------------------
// 门禁：401 / 403
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recovery_endpoints_reject_unauthenticated_with_401() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    for uri in [
        "/api/v1/system/recovery/enter",
        "/api/v1/system/recovery/exit",
    ] {
        let (status, value) = status_code_and_body(
            router(Some(controller.clone()), None, true),
            unauthorized("POST", uri),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(
            value["error"]["code"].as_str(),
            Some("AUTH_REQUIRED"),
            "{uri}"
        );
        assert!(
            controller.audit_events().is_empty(),
            "未认证请求不得触达恢复模式并留下审计"
        );
    }
    assert_eq!(controller.current_mode().as_str(), "normal");
}

#[tokio::test]
async fn recovery_endpoints_reject_non_admin_with_403() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    for (uri, body) in [
        ("/api/v1/system/recovery/enter", None),
        ("/api/v1/system/recovery/exit", Some(complete_checklist())),
    ] {
        let (status, value) = status_code_and_body(
            router(Some(controller.clone()), None, false),
            authorized("POST", uri, body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(
            value["error"]["code"].as_str(),
            Some("PERMISSION_DENIED"),
            "{uri}"
        );
    }
    assert_eq!(
        controller.current_mode().as_str(),
        "normal",
        "普通用户的尝试不得切换模式"
    );
    assert!(
        controller.audit_events().is_empty(),
        "403 不得留下恢复模式审计"
    );
}

#[tokio::test]
async fn recovery_write_without_csrf_binding_is_403() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/system/recovery/enter")
        .header(header::HOST, HOST)
        .header(header::COOKIE, format!("rsc_session={COOKIE_RAW}"))
        .body(Body::empty())
        .unwrap();
    let (status, value) =
        status_code_and_body(router(Some(controller.clone()), None, true), request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value["error"]["code"].as_str(), Some("CSRF_INVALID"));
    assert_eq!(controller.current_mode().as_str(), "normal");
}

// ---------------------------------------------------------------------------
// enter：模式 + 状态探针 + 会话吊销
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_enter_switches_mode_and_status_reflects_it() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized("POST", "/api/v1/system/recovery/enter", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["mode"], "controlled_recovery");
    assert_eq!(
        controller.current_mode().as_str(),
        "controlled_recovery",
        "HTTP 写与控制器真源必须一致"
    );
    assert!(
        !controller.allows_scheduling(),
        "受控模式下自动调度必须被阻止（探针就绪 ≠ 恢复放行）"
    );

    // 写读同源：`GET /system/status` 的 `recovery_mode` 来自同一个控制器快照。
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized("GET", "/api/v1/system/status", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["recovery_mode"], "controlled_recovery");
    assert_eq!(value["data"]["ready"], true);

    // 审计：一次进入一条记录，模式为切换后的逐字串。
    let events = controller.audit_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, RECOVERY_AUDIT_ENTER);
    assert_eq!(events[0].outcome, RECOVERY_AUDIT_OUTCOME_OK);
    assert_eq!(events[0].mode, "controlled_recovery");
}

#[tokio::test]
async fn enter_revokes_every_active_transport_session() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let (transport, connections) = transport_with_sessions(&controller, 2);
    assert_eq!(transport.active_devices().len(), 2, "前置：两条活跃会话");

    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), Some(Arc::clone(&transport)), true),
        authorized("POST", "/api/v1/system/recovery/enter", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["mode"], "controlled_recovery");
    assert!(
        transport.active_devices().is_empty(),
        "进入受控恢复模式必须切断全部活跃会话"
    );

    let events = controller.audit_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].sessions_revoked, 2,
        "审计必须记吊销的活跃会话数（去敏：只有计数，无公钥原文）"
    );

    // 吊销不是「从表上抹掉」而已：`apply_revoke` 走 `TYPE_SYSTEM` 信令 + 延迟强制
    // `abort`。信令投递与切断发生在管理器 spawn 的任务上，按上界轮询到切断为止。
    let deadline = Instant::now() + Duration::from_secs(5);
    for connection in &connections {
        while connection.aborted.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            connection.aborted.load(Ordering::Acquire) > 0,
            "被吊销的会话必须真的被切断"
        );
    }
}

#[tokio::test]
async fn repeated_enter_does_not_reset_the_observation_window() {
    let controller = Arc::new(RuntimeRecoveryController::with_retention_policy(
        RecoveryRetentionPolicy::after_entry(Duration::from_secs(3_600)),
    ));
    for _ in 0..2 {
        let (status, value) = status_code_and_body(
            router(Some(controller.clone()), None, true),
            authorized("POST", "/api/v1/system/recovery/enter", None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["data"]["mode"], "controlled_recovery");
    }
    // 观察窗自**首次**进入起算：重复进入后立刻解除仍应因窗口未满被拒。
    let (status, _) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        controller.audit_events().last().map(|event| event.outcome),
        Some(RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN)
    );
}

// ---------------------------------------------------------------------------
// exit：三段解除门禁的线上表现
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exit_with_incomplete_checklist_is_400_and_keeps_recovery_mode() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    controller.enter_controlled_recovery();

    // 四项里任一项为 `false` 都必须被拒（含全 false），且模式保持受控。
    let incomplete = [
        json!({"revocations_checked": false, "grants_checked": true, "passwords_checked": true, "tasks_checked": true}),
        json!({"revocations_checked": true, "grants_checked": false, "passwords_checked": true, "tasks_checked": true}),
        json!({"revocations_checked": true, "grants_checked": true, "passwords_checked": false, "tasks_checked": true}),
        json!({"revocations_checked": true, "grants_checked": true, "passwords_checked": true, "tasks_checked": false}),
        json!({"revocations_checked": false, "grants_checked": false, "passwords_checked": false, "tasks_checked": false}),
    ];
    for body in incomplete {
        let (status, value) = status_code_and_body(
            router(Some(controller.clone()), None, true),
            authorized("POST", "/api/v1/system/recovery/exit", Some(body.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            value["error"]["code"].as_str(),
            Some("CHECKLIST_INCOMPLETE"),
            "{body}"
        );
        assert_eq!(
            controller.current_mode().as_str(),
            "controlled_recovery",
            "清单不齐不得解除：{body}"
        );
    }

    // 四项齐全即解除成功。
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["mode"], "normal");

    // 审计逐条留痕：1 条进入 + 5 条被拒 + 1 条成功。
    let events = controller.audit_events();
    assert_eq!(events.len(), 7);
    assert_eq!(events[0].event, RECOVERY_AUDIT_ENTER);
    assert!(
        events[1..6]
            .iter()
            .all(|event| event.outcome == RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE),
        "每一次被拒的尝试都要留一条核对清单不齐的审计"
    );
    assert_eq!(
        events.last().map(|event| event.outcome),
        Some(RECOVERY_AUDIT_OUTCOME_OK)
    );
}

#[tokio::test]
async fn exit_body_is_strict() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    controller.enter_controlled_recovery();
    for body in [
        // 缺一个必填字段
        json!({"revocations_checked": true, "grants_checked": true, "passwords_checked": true}),
        // 未知字段
        json!({"revocations_checked": true, "grants_checked": true, "passwords_checked": true, "tasks_checked": true, "force": true}),
        // 类型不符
        json!({"revocations_checked": "true", "grants_checked": true, "passwords_checked": true, "tasks_checked": true}),
        // 空对象
        json!({}),
    ] {
        let (status, value) = status_code_and_body(
            router(Some(controller.clone()), None, true),
            authorized("POST", "/api/v1/system/recovery/exit", Some(body.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            value["error"]["code"].as_str(),
            Some("INVALID_ARGUMENT"),
            "{body}"
        );
        assert_eq!(controller.current_mode().as_str(), "controlled_recovery");
    }
    assert!(
        controller
            .audit_events()
            .iter()
            .all(|event| event.event == RECOVERY_AUDIT_ENTER),
        "body 校验失败的请求不得触达解除判定，也就不得留下 exit 审计"
    );
}

#[tokio::test]
async fn exit_waits_for_the_observation_window_then_switches_to_normal() {
    let controller = Arc::new(RuntimeRecoveryController::with_retention_policy(
        RecoveryRetentionPolicy::after_entry(Duration::from_millis(40)),
    ));
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized("POST", "/api/v1/system/recovery/enter", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["mode"], "controlled_recovery");

    // 观察窗未满：409 INVALID_STATE，模式不变。
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"].as_str(), Some("INVALID_STATE"));
    assert_eq!(controller.current_mode().as_str(), "controlled_recovery");

    // 度满窗口：解除成功切回 normal，状态探针同步。
    tokio::time::sleep(Duration::from_millis(60)).await;
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["mode"], "normal");
    assert!(controller.allows_scheduling(), "解除后自动调度必须恢复放行");

    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized("GET", "/api/v1/system/status", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["recovery_mode"], "normal");

    let outcomes = controller
        .audit_events()
        .iter()
        .map(|event| (event.event, event.outcome))
        .collect::<Vec<(&'static str, &'static str)>>();
    assert_eq!(
        outcomes,
        vec![
            (RECOVERY_AUDIT_ENTER, RECOVERY_AUDIT_OUTCOME_OK),
            (RECOVERY_AUDIT_EXIT, RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN),
            (RECOVERY_AUDIT_EXIT, RECOVERY_AUDIT_OUTCOME_OK),
        ],
    );
}

#[tokio::test]
async fn exit_outside_recovery_mode_is_409() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let (status, value) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(value["error"]["code"].as_str(), Some("INVALID_STATE"));
    assert_eq!(controller.current_mode().as_str(), "normal");
    assert_eq!(
        controller.audit_events().last().map(|event| event.outcome),
        Some(RECOVERY_AUDIT_OUTCOME_INVALID_STATE),
        "非受控模式的解除尝试也要留审计"
    );
}

// ---------------------------------------------------------------------------
// 装配缺失与去敏
// ---------------------------------------------------------------------------

/// 既有装配（未接入恢复控制器）：路由仍挂载，但在会话与管理员门禁之后 503
/// `NOT_READY`——不伪造模式切换，也不需要装配方新增必填参数。
#[tokio::test]
async fn recovery_endpoints_without_controller_are_503_not_ready() {
    for uri in [
        "/api/v1/system/recovery/enter",
        "/api/v1/system/recovery/exit",
    ] {
        let (status, value) = status_code_and_body(
            router(None, None, true),
            authorized("POST", uri, Some(complete_checklist())),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(value["error"]["code"].as_str(), Some("NOT_READY"), "{uri}");
    }
}

/// 审计去敏：进程内账本里找不到任何身份/凭据/公钥字节的痕迹。
#[tokio::test]
async fn recovery_audit_records_carry_no_credentials_or_identity() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let (transport, _connections) = transport_with_sessions(&controller, 1);
    let (status, _) = status_code_and_body(
        router(Some(controller.clone()), Some(transport), true),
        authorized("POST", "/api/v1/system/recovery/enter", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = status_code_and_body(
        router(Some(controller.clone()), None, true),
        authorized(
            "POST",
            "/api/v1/system/recovery/exit",
            Some(complete_checklist()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let events: Vec<RecoveryAuditEvent> = controller.audit_events();
    assert_eq!(events.len(), 2);
    let text = format!("{events:?}").to_lowercase();
    for forbidden in [
        COOKIE_RAW.to_lowercase().as_str(),
        CSRF.to_lowercase().as_str(),
        ADMIN_USERNAME,
        "token",
        "cookie",
        "csrf",
        // `passwords_checked` 是核对清单的 schema 键名，不是凭据；被排除的是凭据形态本身。
        "password_hash",
        "public_key",
        "digest",
    ] {
        assert!(
            !text.contains(forbidden),
            "去敏审计不得出现 {forbidden}：{text}"
        );
    }
    // 审计里唯一「会话」相关的可观测量是计数。
    assert_eq!(events[0].sessions_revoked, 1);
    assert_eq!(events[1].sessions_revoked, 0);
    assert_eq!(
        events[1].checklist.map(|list| list.is_complete()),
        Some(true)
    );
}
