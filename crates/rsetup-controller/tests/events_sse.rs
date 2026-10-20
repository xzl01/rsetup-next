//! `GET /api/v1/events` SSE 读线的进程内 HTTP 集成测试（06 §4、02 §7）。
//!
//! 证据边界：本文件用**桩认证服务 + 真实 `tokio::sync::broadcast` 总线 + 真实
//! axum router**（`tower::ServiceExt::oneshot`，不经真实 socket）证明握手门禁、
//! SSE 标头、五种命名事件帧、背压 `reset`、管理员专属事件与会话失效关流。
//! 它**不**证明真实 DB 权威会话、真实业务发布方投递、真实浏览器 EventSource
//! 行为或跨代理链路的 keep-alive 语义（06 §6 AT-18 的真实浏览器端到端仍是
//! 后续门禁）。
//!
//! 读体的有限终止：SSE 本身是长连接，测试在发布完事件后**释放测试侧唯一的
//! 另一个 `EventBus` 句柄**，总线关闭 → 流正常结束 → 才能整体读干响应体；
//! 这不改变生产语义（生产进程持续持有句柄，连接活到客户端断开为止）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use rsetup_controller::ControllerError;
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::events::{EventBus, EventEnvelope, RESET_REASON_BUFFER_OVERFLOW};
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, HttpAuthState};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_security::HttpSecurityConfig;
use serde_json::Value;
use tower::ServiceExt;

const HOST: &str = "controller.local";
const ORIGIN: &str = "http://controller.local";
/// 读体终止的硬超时护栏：任何断言都不许把测试挂到 CI 超时。
const READ_BUDGET: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// 认证桩
// ---------------------------------------------------------------------------

/// 权威 `authenticate` 桩：只认唯一合法 token；`live` 是进程内会话时钟的
/// 可翻转开关，用于证明「会话失效即关流」不依赖真实 DB。
struct StubAuth {
    token: String,
    session: Session,
    live: Arc<AtomicBool>,
}

fn fixture_session(is_admin: bool) -> Session {
    Session {
        user: IdentityUser {
            id: [7u8; 16],
            username: "operator".into(),
            password_hash: "not-a-real-hash".into(),
            active: true,
            is_admin,
            must_change_password: false,
            revision: 3,
        },
        digest: [9u8; 32],
    }
}

impl AuthServiceApi for StubAuth {
    fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        let ok = raw == self.token;
        let session = self.session.clone();
        Box::pin(async move {
            if ok {
                Ok(session)
            } else {
                Err(ControllerError::InvalidArgument)
            }
        })
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
    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }
    fn is_live_session(&self, _d: &[u8; 32]) -> bool {
        self.live.load(Ordering::SeqCst)
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

/// 装配结果：router（一次性消费）、测试侧总线句柄、会话时钟开关。
struct Fixture {
    router: Router<()>,
    bus: EventBus,
    live: Arc<AtomicBool>,
    token: String,
}

fn fixture(is_admin: bool, capacity: usize) -> Fixture {
    let token = "a".repeat(64);
    let live = Arc::new(AtomicBool::new(true));
    let auth = Arc::new(HttpAuthState::new(
        Arc::new(StubAuth {
            token: token.clone(),
            session: fixture_session(is_admin),
            live: Arc::clone(&live),
        }),
        HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
    ));
    let bus = EventBus::with_capacity(capacity);
    let router = rsetup_controller::http_events::build_events_router(EventsState {
        auth,
        bus: bus.clone(),
    });
    Fixture {
        router,
        bus,
        live,
        token,
    }
}

fn events_request(fixture: &Fixture, cookie: bool, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, HeaderValue::from_static(HOST));
    if cookie {
        builder = builder.header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", fixture.token)).unwrap(),
        );
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
    }
    builder.body(Body::empty()).unwrap()
}

/// 读干响应体（测试侧必须先关闭总线，否则本函数在预算内超时）。
async fn drain(response: axum::response::Response) -> Vec<u8> {
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("SSE 体必须在总线关闭后有限时间内结束")
        .unwrap();
    bytes.to_vec()
}

/// 一帧 SSE：`event:` 与 `data:` 字段（`data:` 解析成 JSON 值）。
#[derive(Debug, PartialEq)]
struct Frame {
    event: Option<String>,
    data: Option<Value>,
}

/// 把响应体字节切成帧序列；跳过 `:` 开头的注释帧（keep-alive 心跳）。
fn parse_frames(bytes: &[u8]) -> Vec<Frame> {
    let text = String::from_utf8(bytes.to_vec()).expect("SSE 帧恒为 UTF-8");
    let mut frames = Vec::new();
    for block in text.split("\n\n") {
        let trimmed = block.trim_matches(|c: char| c == '\r' || c == '\n');
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }
        let mut event = None;
        let mut data = None;
        for line in trimmed.lines() {
            if let Some(value) = line.strip_prefix("event: ") {
                event = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("data: ") {
                data = Some(
                    serde_json::from_str(value)
                        .unwrap_or_else(|e| panic!("data: 字段必须是 JSON：{value:?}（{e}）")),
                );
            }
        }
        frames.push(Frame { event, data });
    }
    frames
}

async fn error_code(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    (
        status,
        body["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// 握手门禁
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_events_returns_401_and_never_subscribes() {
    let fixture = fixture(true, 128);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, false, None))
        .await
        .unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未登录必须 401");
    assert_eq!(code, "AUTH_REQUIRED");
    // 门禁拒绝发生在订阅之前：绝不允许给未认证连接挂上广播订阅。
    assert_eq!(fixture.bus.subscriber_count(), 0);
}

#[tokio::test]
async fn foreign_host_and_mismatched_origin_are_refused_before_subscribing() {
    // 错误 Host：403 CSRF_INVALID，零订阅。
    let fixture = fixture(true, 128);
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, "wrong.host")
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", fixture.token)).unwrap(),
        )
        .body(Body::empty())
        .unwrap();
    let response = fixture.router.clone().oneshot(request).await.unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);

    // 携带不等价的 Origin（02 §7 握手校验 Origin 允许列表）：403，零订阅。
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, Some("http://evil.example")))
        .await
        .unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);

    // Origin 精确等于允许值：握手成功（阳性控制，证明上一条不是恒 403）。
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, Some(ORIGIN)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.bus.subscriber_count(), 1);
    drop(response);
}

#[tokio::test]
async fn temporary_password_session_is_refused_with_403() {
    // 02 §7「已改密会话」：must_change_password 一律 403 PASSWORD_CHANGE_REQUIRED。
    let token = "b".repeat(64);
    let live = Arc::new(AtomicBool::new(true));
    let mut session = fixture_session(true);
    session.user.must_change_password = true;
    let auth = Arc::new(HttpAuthState::new(
        Arc::new(StubAuth {
            token: token.clone(),
            session,
            live: Arc::clone(&live),
        }),
        HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
    ));
    let bus = EventBus::new();
    let router = rsetup_controller::http_events::build_events_router(EventsState {
        auth,
        bus: bus.clone(),
    });
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, HeaderValue::from_static(HOST))
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={token}")).unwrap(),
        )
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");
    assert_eq!(bus.subscriber_count(), 0);
}

// ---------------------------------------------------------------------------
// 流本身
// ---------------------------------------------------------------------------

#[tokio::test]
async fn authenticated_handshake_is_an_open_event_stream_and_disconnect_releases_it() {
    let fixture = fixture(true, 128);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
        "Content-Type 必须是 text/event-stream"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache"),
        "Cache-Control 必须是 no-cache"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok()),
        Some("keep-alive"),
        "必须显式声明 Connection: keep-alive"
    );
    // 握手成功即建立且只建立一个订阅。
    assert_eq!(fixture.bus.subscriber_count(), 1);

    // 客户端断开（响应体被丢弃）→ 订阅立刻释放，不等下一条事件。
    drop(response);
    assert_eq!(
        fixture.bus.subscriber_count(),
        0,
        "断开后必须立刻释放广播订阅"
    );
}

#[tokio::test]
async fn five_named_events_arrive_as_named_frames_with_json_data() {
    let fixture = fixture(true, 128);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // 装配侧（router 内的 EventsState）也持有一个 Sender 句柄：要读干响应体，
    // 必须先释放它，再释放测试侧句柄，总线才真正关闭。
    drop(fixture.router);

    let device_id = "ab".repeat(32);
    let published = [
        EventEnvelope::device_updated(&device_id, 12),
        EventEnvelope::task_updated("0e7f1c2e-0000-4000-8000-000000000001", 7),
        EventEnvelope::permissions_changed("0e7f1c2e-0000-4000-8000-000000000002", 41),
        EventEnvelope::system_time_changed(&{
            use chrono::{TimeZone, Utc};
            use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
            use uuid::Uuid;
            let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
            TimeEvidence {
                system_wall_utc: now,
                reference_utc: now,
                quality: TimeQuality::NtpValid,
                clock_epoch: Uuid::new_v4(),
                source: Some("ntp.test".into()),
                sample_age_ms: Some(1_500),
                offset_ms: Some(-5),
                uncertainty_ms: Some(15),
            }
        }),
        EventEnvelope::reset("generation_expired"),
    ];
    for envelope in &published {
        assert_eq!(fixture.bus.publish(envelope.clone()), 1, "已建立一条订阅");
    }
    // 释放测试侧句柄 → 总线关闭 → 读体终止。
    drop(fixture.bus);
    let frames = parse_frames(&drain(response).await);

    let names: Vec<Option<&str>> = frames
        .iter()
        .map(|f| f.event.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            Some("device.updated"),
            Some("task.updated"),
            Some("permissions.changed"),
            Some("system.time.changed"),
            Some("reset"),
        ],
        "五种命名事件必须逐帧带 event: 字段，且不落到默认 message"
    );
    // 每帧都带 JSON data，且 kind/id/revision 形状与发布侧一致。
    let data: Vec<Value> = frames
        .iter()
        .map(|f| f.data.clone().expect("每帧必须携带 data: JSON"))
        .collect();
    assert_eq!(data[0]["kind"], Value::String("device".into()));
    assert_eq!(data[0]["id"], Value::String(device_id.clone()));
    assert_eq!(data[0]["revision"], Value::String("12".into()));
    assert_eq!(data[1]["kind"], Value::String("task".into()));
    assert_eq!(data[2]["epoch"], Value::String("41".into()));
    assert_eq!(data[3]["kind"], Value::String("system_time".into()));
    assert_eq!(
        data[3]["evidence"]["quality"],
        Value::String("ntp_valid".into())
    );
    assert_eq!(
        data[4]["reason"],
        Value::String("generation_expired".into())
    );
}

#[tokio::test]
async fn lagged_subscription_gets_reset_then_keeps_streaming() {
    // 容量 2 的总线 + 客户端尚未读体 → 必然 Lagged（06 §4「满即 reset」）。
    let fixture = fixture(true, 2);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(fixture.router);
    for revision in 0..6 {
        fixture
            .bus
            .publish(EventEnvelope::task_updated("t", revision));
    }
    drop(fixture.bus);
    let frames = parse_frames(&drain(response).await);
    assert!(!frames.is_empty(), "落后订阅必须收到帧，而不是静默空响应");
    let first = &frames[0];
    assert_eq!(
        first.event.as_deref(),
        Some("reset"),
        "溢出首帧必须是 reset"
    );
    assert_eq!(
        first.data.as_ref().unwrap()["reason"],
        Value::String(RESET_REASON_BUFFER_OVERFLOW.into()),
        "reset 必须带稳定原因码，供客户端区分背压与代际失效"
    );
    // reset 之后订阅继续：客户端可在同一连接上收到后续事件（不是一次性响应）。
    assert!(
        frames[1..]
            .iter()
            .any(|f| f.event.as_deref() == Some("task.updated")),
        "reset 后必须继续投递，而不是结束整个流：{frames:?}"
    );
    // 溢出帧不得携带任何丢失事件的载荷（只提示重取）。
}

#[tokio::test]
async fn time_event_is_admin_only_on_a_shared_bus() {
    // 同一个总线：管理员收到时间事件，普通用户连接必须收不到（06 §4「时间事件仅管理员」）。
    let fixture = fixture(false, 128);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    drop(fixture.router);
    let evidence = {
        use chrono::{TimeZone, Utc};
        use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
        use uuid::Uuid;
        let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        TimeEvidence {
            system_wall_utc: now,
            reference_utc: now,
            quality: TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: None,
            offset_ms: None,
            uncertainty_ms: None,
        }
    };
    fixture
        .bus
        .publish(EventEnvelope::system_time_changed(&evidence));
    fixture
        .bus
        .publish(EventEnvelope::device_updated(&"cd".repeat(32), 3));
    drop(fixture.bus);
    let frames = parse_frames(&drain(response).await);
    let names: Vec<Option<&str>> = frames.iter().map(|f| f.event.as_deref()).collect();
    assert_eq!(
        names,
        vec![Some("device.updated")],
        "普通用户连接上只能出现非管理员事件"
    );
}

#[tokio::test]
async fn revoked_session_closes_stream_without_leaking_queued_events() {
    // 会话在进程会话时钟里失效后：已排队但未发出的事件绝不再外发，流直接结束。
    let fixture = fixture(true, 128);
    let response = fixture
        .router
        .clone()
        .oneshot(events_request(&fixture, true, None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // 会话在本进程会话时钟里失效（登出/撤销/闲置到期）后，任何事件都必须
    // 关流而不是继续外发：这里在失效前后各投一帧，断言两帧都没有泄漏到线上。
    fixture
        .bus
        .publish(EventEnvelope::task_updated("around-revoke", 1));
    fixture.live.store(false, Ordering::SeqCst);
    fixture
        .bus
        .publish(EventEnvelope::permissions_changed("u", 99));
    drop(fixture.bus);
    let frames = parse_frames(&drain(response).await);
    assert!(
        frames.is_empty(),
        "会话失效后不得再外发任何事件帧（含失效前排队的旧授权数据）：{frames:?}"
    );
}

// ---------------------------------------------------------------------------
// 发布侧与装配
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_without_connection_is_dropped_without_error() {
    // 无人订阅时发布不阻塞、不报错、不累积（事件只是变化提示）。
    let bus = EventBus::new();
    assert_eq!(bus.publish(EventEnvelope::task_updated("t", 1)), 0);
    assert_eq!(bus.subscriber_count(), 0);
}

#[tokio::test]
async fn header_map_helper_rejects_duplicated_origin() {
    // 阳性/负性对照：重复 Origin 头属于握手期失败，绝不 reduce 成第一个值。
    let fixture = fixture(true, 128);
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, HeaderValue::from_static(HOST))
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", fixture.token)).unwrap(),
        )
        .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
        .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
        .body(Body::empty())
        .unwrap();
    let response = fixture.router.clone().oneshot(request).await.unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);
}
