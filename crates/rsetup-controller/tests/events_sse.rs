//! `GET /api/v1/events` SSE 读线的进程内 HTTP 集成测试（06 §4、02 §7）。
//!
//! 证据边界：本文件用**桩认证服务 + 真实 `tokio::sync::broadcast` 总线 + 真实
//! axum router**（`tower::ServiceExt::oneshot`，不经真实 socket）证明握手门禁、
//! SSE 标头、五种命名事件帧、背压 `reset`、逐事件受众隔离、权威会话/epoch
//! 复核关流与会话失效关流。它**不**证明真实 DB 权威会话（`LiveAuth` 的
//! `sessions ⋈ users` 与 `schema_meta` 读属于 `metrics_real_db` 一类的真实库
//! 验收）、真实业务发布方投递、真实浏览器 EventSource 行为或跨代理链路的
//! keep-alive 语义（06 §6 AT-18 的真实浏览器端到端仍是后续门禁）。
//!
//! 两个**分离的失效开关**是本文件的核心取证装置：
//! - `live`：进程内会话时钟（[`AuthServiceApi::is_live_session`] 的桩），
//!   内存预检能看到的失效；
//! - `authority`：**权威状态源**（[`AuthServiceApi::session_authority`] 的桩），
//!   模拟「握手之后在数据库里被降权 / 会话被撤销 / `authz_epoch` 变更 /
//!   权威源读取失败」。内存时钟看不见这些变化，只有权威复核能切断连接
//!   （06 §4「发送前验证 session 与 authz_epoch，撤权关闭旧订阅，排队旧权限
//!   数据不得发送」）。
//!
//! 读体的有限终止：SSE 本身是长连接。要么在发布完事件后释放所有 `EventBus`
//! 句柄让总线关闭 → 流正常结束；要么（权威失效类用例）**故意保持总线打开**，
//! 用超时护栏断言流是被服务端主动关掉的，而不是靠总线关闭收尾的。
//! 这不改变生产语义（生产进程持续持有句柄，连接活到客户端断开为止）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use rsetup_controller::ControllerError;
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::auth::session::token_digest;
use rsetup_controller::events::{
    EventAudience, EventBus, EventEnvelope, RESET_REASON_BUFFER_OVERFLOW,
};
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, HttpAuthState, SessionAuthority};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_security::HttpSecurityConfig;
use serde_json::Value;
use tower::ServiceExt;

const HOST: &str = "controller.local";
const ORIGIN: &str = "http://controller.local";
/// 读体终止的硬超时护栏：任何断言都不许把测试挂到 CI 超时。
const READ_BUDGET: Duration = Duration::from_secs(5);
/// 权威复核周期用例间用的「足够长」周期：让这些用例的时序只由事件驱动，
/// 不被周期复核掺和（周期复核自身另有专门用例）。
const SLOW_REVALIDATE: Duration = Duration::from_secs(3600);
/// 周期复核算例用的短周期：远小于超时护栏，让空闲连接被主动切断。
const FAST_REVALIDATE: Duration = Duration::from_millis(20);

const ALICE: [u8; 16] = [7u8; 16];
const BOB: [u8; 16] = [8u8; 16];
/// 管理员用户（`five_named_events_*` 用它的连接收齐五种事件）。
const ROOT: [u8; 16] = [9u8; 16];

// ---------------------------------------------------------------------------
// 认证桩：内存时钟开关 + 权威状态源开关，两者刻意独立
// ---------------------------------------------------------------------------

/// 权威状态源此刻的说法。握手之后翻转它，就是在模拟「数据库里发生了变更，
/// 但进程内存里一无所知」——这正是被审查询问的失效面。
#[derive(Clone, Copy, Debug)]
struct Authority {
    /// 会话在权威库里已被撤销（或用户已停用）。
    revoked: bool,
    /// 权威库里该用户**当前**的管理员标志（握手后被降级就是翻转它）。
    is_admin: bool,
    /// 权威库里当前的 `authz_epoch`（与握手基准不等即授权投影已变更）。
    epoch: u64,
    /// 权威源本身读失败（后端故障）。
    read_failed: bool,
}

/// 权威 `authenticate` 桩：只认自己的 token；`live` 是进程内会话时钟的开关，
/// `authority` 是权威状态源的开关。
struct StubAuth {
    token: String,
    session: Session,
    live: Arc<AtomicBool>,
    authority: Arc<Mutex<Authority>>,
}

fn hex64(seed: u8) -> String {
    (0..64)
        .map(|i| std::char::from_digit(u32::from((seed + i as u8) % 10), 16).unwrap())
        .collect()
}

fn fixture_session(user_id: [u8; 16], is_admin: bool, digest: [u8; 32]) -> Session {
    Session {
        user: IdentityUser {
            id: user_id,
            username: "operator".into(),
            password_hash: "not-a-real-hash".into(),
            active: true,
            is_admin,
            must_change_password: false,
            revision: 3,
        },
        digest,
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
        Box::pin(async { Ok(self.authority.lock().unwrap().epoch) })
    }
    fn is_live_session(&self, _d: &[u8; 32]) -> bool {
        self.live.load(Ordering::SeqCst)
    }
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        // 权威侧结论**只**来自 `authority`：内存时钟的 `live` 开关在这里刻意
        // 不参与，两个失效通道才能被分别取证。
        let authority = *self.authority.lock().unwrap();
        let mine = *digest == self.session.digest;
        Box::pin(async move {
            if authority.read_failed {
                return Err(ControllerError::Config(
                    "synthetic authority read fault".into(),
                ));
            }
            Ok(SessionAuthority {
                session_live: mine && !authority.revoked,
                is_admin: authority.is_admin,
                authz_epoch: authority.epoch,
            })
        })
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

/// 一条已装配的 SSE 连接：自己的 router/token/两个失效开关。**总线由外部
/// 注入**，因此多个不同用户/不同权限的连接可以挂在同一条总线上，用来证明
/// 受众隔离（而不是每条连接一条总线各自自说自话）。
struct Conn {
    router: Router<()>,
    token: String,
    live: Arc<AtomicBool>,
    authority: Arc<Mutex<Authority>>,
}

impl Conn {
    fn with_capacity(
        bus: &EventBus,
        user_id: [u8; 16],
        is_admin: bool,
        revalidate: Duration,
    ) -> Self {
        let token = hex64(user_id[0]);
        let digest = token_digest(token.as_bytes());
        let live = Arc::new(AtomicBool::new(true));
        let authority = Arc::new(Mutex::new(Authority {
            revoked: false,
            is_admin,
            epoch: 0,
            read_failed: false,
        }));
        let auth = Arc::new(HttpAuthState::new(
            Arc::new(StubAuth {
                token: token.clone(),
                session: fixture_session(user_id, is_admin, digest),
                live: Arc::clone(&live),
                authority: Arc::clone(&authority),
            }),
            HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
        ));
        let router = rsetup_controller::http_events::build_events_router(EventsState {
            auth,
            // 每个连接持有同一条通道的克隆句柄。
            bus: bus.clone(),
            revalidate_interval: revalidate,
        });
        Self {
            router,
            token,
            live,
            authority,
        }
    }

    /// 取权威状态源的共享句柄：**必须**在 `drop(conn.router)` 之前取，
    /// 之后 partial move 掉的 `conn` 不能再借用。
    fn authority(&self) -> Arc<Mutex<Authority>> {
        Arc::clone(&self.authority)
    }
}

/// 单连接用例的装配体：连接 + 测试侧总线句柄。
struct Fixture {
    conn: Conn,
    bus: EventBus,
}

/// 便捷装配：普通用户连接（`is_admin = false`）。`capacity` 只影响总线。
fn fixture(is_admin: bool, capacity: usize) -> Fixture {
    let bus = EventBus::with_capacity(capacity);
    let conn = Conn::with_capacity(
        &bus,
        if is_admin { ROOT } else { BOB },
        is_admin,
        SLOW_REVALIDATE,
    );
    let _ = capacity;
    Fixture { conn, bus }
}

fn events_request(conn: &Conn, cookie: bool, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, HeaderValue::from_static(HOST));
    if cookie {
        builder = builder.header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", conn.token)).unwrap(),
        );
    }
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
    }
    builder.body(Body::empty()).unwrap()
}

/// 读干响应体（测试侧必须先关闭总线或让服务端主动关流，否则本函数在预算内超时）。
async fn drain(response: axum::response::Response) -> Vec<u8> {
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("SSE 体必须在总线关闭或连接被主动关闭后有限时间内结束")
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

/// 握手并断言 200；返回响应体（连接已建立）。
async fn handshake(conn: &Conn, request: Request<Body>) -> axum::response::Response {
    let response = conn.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
}

/// 释放 router 与测试侧句柄 → 所有 Sender 析构 → 总线关闭 → 读体终止。
async fn close_bus_and_drain(
    conn: Conn,
    bus: EventBus,
    response: axum::response::Response,
) -> Vec<Frame> {
    drop(conn.router);
    drop(bus);
    parse_frames(&drain(response).await)
}

// ---------------------------------------------------------------------------
// 握手门禁
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthenticated_events_returns_401_and_never_subscribes() {
    let fixture = fixture(true, 128);
    let response = fixture
        .conn
        .router
        .clone()
        .oneshot(events_request(&fixture.conn, false, None))
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
    let fixture = fixture(true, 128);
    // 错误 Host：403 CSRF_INVALID，零订阅。
    let request = Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, "wrong.host")
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", fixture.conn.token)).unwrap(),
        )
        .body(Body::empty())
        .unwrap();
    let response = fixture.conn.router.clone().oneshot(request).await.unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);

    // 携带不等价的 Origin（02 §7 握手校验 Origin 允许列表）：403，零订阅。
    let response = fixture
        .conn
        .router
        .clone()
        .oneshot(events_request(
            &fixture.conn,
            true,
            Some("http://evil.example"),
        ))
        .await
        .unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);

    // Origin 精确等于允许值：握手成功（阳性控制，证明上一条不是恒 403）。
    let response = fixture
        .conn
        .router
        .clone()
        .oneshot(events_request(&fixture.conn, true, Some(ORIGIN)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fixture.bus.subscriber_count(), 1);
    drop(response);
}

#[tokio::test]
async fn temporary_password_session_is_refused_with_403() {
    // 02 §7「已改密会话」：must_change_password 一律 403 PASSWORD_CHANGE_REQUIRED。
    let bus = EventBus::new();
    let token = hex64(BOB[0]);
    let mut session = fixture_session(BOB, false, token_digest(token.as_bytes()));
    session.user.must_change_password = true;
    let conn = Conn {
        router: rsetup_controller::http_events::build_events_router(EventsState {
            auth: Arc::new(HttpAuthState::new(
                Arc::new(StubAuth {
                    token: token.clone(),
                    session,
                    live: Arc::new(AtomicBool::new(true)),
                    authority: Arc::new(Mutex::new(Authority {
                        revoked: false,
                        is_admin: false,
                        epoch: 0,
                        read_failed: false,
                    })),
                }),
                HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
            )),
            bus: bus.clone(),
            revalidate_interval: SLOW_REVALIDATE,
        }),
        token,
        live: Arc::new(AtomicBool::new(true)),
        authority: Arc::new(Mutex::new(Authority {
            revoked: false,
            is_admin: false,
            epoch: 0,
            read_failed: false,
        })),
    };
    let response = conn
        .router
        .clone()
        .oneshot(events_request(&conn, true, None))
        .await
        .unwrap();
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
    let response = handshake(&fixture.conn, events_request(&fixture.conn, true, None)).await;
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
    let response = handshake(&fixture.conn, events_request(&fixture.conn, true, None)).await;
    // 装配侧（router 内的 EventsState）也持有一个 Sender 句柄：要读干响应体，
    // 必须先释放它，再释放测试侧句柄，总线才真正关闭。
    drop(fixture.conn.router);

    let device_id = "ab".repeat(32);
    let published = [
        EventEnvelope::device_updated(&device_id, 12, EventAudience::AnySession),
        EventEnvelope::task_updated(
            "0e7f1c2e-0000-4000-8000-000000000001",
            7,
            EventAudience::AnySession,
        ),
        // 授权投影事件的受众由构造钉死为本人（= 本连接的用户）。
        EventEnvelope::permissions_changed(ROOT, 41),
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
    let bus = EventBus::with_capacity(2);
    let conn = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    drop(conn.router);
    for revision in 0..6 {
        bus.publish(EventEnvelope::task_updated(
            "t",
            revision,
            EventAudience::AnySession,
        ));
    }
    drop(bus);
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
    let response = handshake(&fixture.conn, events_request(&fixture.conn, true, None)).await;
    drop(fixture.conn.router);
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
    fixture.bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        3,
        EventAudience::AnySession,
    ));
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
    // 进程内会话时钟失效（登出/撤销/闲置到期）后：已排队但未发出的事件绝不
    // 再外发，流直接结束。
    let fixture = fixture(true, 128);
    let response = handshake(&fixture.conn, events_request(&fixture.conn, true, None)).await;
    // 失效前后各投一帧，断言两帧都没有泄漏到线上。
    fixture.bus.publish(EventEnvelope::task_updated(
        "around-revoke",
        1,
        EventAudience::AnySession,
    ));
    fixture.conn.live.store(false, Ordering::SeqCst);
    fixture
        .bus
        .publish(EventEnvelope::permissions_changed(ROOT, 99));
    let frames = close_bus_and_drain(fixture.conn, fixture.bus, response).await;
    assert!(
        frames.is_empty(),
        "会话失效后不得再外发任何事件帧（含失效前排队的旧授权数据）：{frames:?}"
    );
}

// ---------------------------------------------------------------------------
// 权威复核：握手后的降权 / epoch 变更 / 数据库撤销 / 权威源故障
// ---------------------------------------------------------------------------

#[tokio::test]
async fn demoted_admin_connection_is_closed_and_sends_nothing_more() {
    // 握手时是管理员 → 之后在权威库里被降级为普通用户。已被降权的连接不得
    // 再收管理员专属的 system.time.changed，也不得再收任何后续事件。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity(&bus, ROOT, true, SLOW_REVALIDATE);
    let authority = conn.authority();
    // 先按管理员握手建连（阳性基准），之后再降级：时序必须是「握手 → 降权」。
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    // 权威库里现在说：不是管理员了。只降级，不撤销——单独证明 `is_admin`
    // 这一维度的门禁，而不是靠撤销蒙混过关。
    authority.lock().unwrap().is_admin = false;
    let evidence = {
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
            sample_age_ms: Some(1),
            offset_ms: None,
            uncertainty_ms: None,
        }
    };
    // 交还 router 侧的总线 Sender（测试侧 `bus` 句柄全程不释放：总线不关闭）。
    drop(conn.router);
    bus.publish(EventEnvelope::system_time_changed(&evidence));
    bus.publish(EventEnvelope::device_updated(
        &"ee".repeat(32),
        1,
        EventAudience::AnySession,
    ));
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("降权后必须被关流")
        .unwrap();
    let frames = parse_frames(&bytes);
    assert!(
        frames.is_empty(),
        "被降权的管理员连接不得再收任何事件（尤其不得收 system.time.changed）：{frames:?}"
    );
    drop(bus);
}

#[tokio::test]
async fn authz_epoch_change_closes_connection_without_leaking_events() {
    // 握手之后授权 epoch 变更（授权投影已过期）：连接必须关闭，待发事件丢弃。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    authority.lock().unwrap().epoch = 8;
    // 交还 router 侧的总线 Sender（测试侧 `bus` 句柄全程不释放：总线不关闭）。
    drop(conn.router);
    bus.publish(EventEnvelope::task_updated(
        "after-epoch-bump",
        1,
        EventAudience::AnySession,
    ));
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("epoch 变更后必须被关流")
        .unwrap();
    assert!(
        parse_frames(&bytes).is_empty(),
        "authz_epoch 变更后不得再外发事件帧"
    );
    assert_eq!(bus.subscriber_count(), 0, "关流必须释放订阅");
    drop(bus);
}

#[tokio::test]
async fn session_revoked_in_authority_store_closes_connection_without_leaking_events() {
    // 权威库里撤销会话（内存时钟仍认为 live）：只有权威复核能看见这次撤销。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    authority.lock().unwrap().revoked = true;
    assert!(
        conn.live.load(Ordering::SeqCst),
        "本用例的内存时钟必须仍是 live，撤销只能来自权威源"
    );
    // 交还 router 侧的总线 Sender（测试侧 `bus` 句柄全程不释放：总线不关闭）。
    drop(conn.router);
    bus.publish(EventEnvelope::task_updated(
        "after-revoke",
        1,
        EventAudience::AnySession,
    ));
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("权威侧撤销后必须被关流")
        .unwrap();
    assert!(
        parse_frames(&bytes).is_empty(),
        "会话已被权威侧撤销，不得再外发事件帧"
    );
    assert_eq!(bus.subscriber_count(), 0);
    drop(bus);
}

#[tokio::test]
async fn authority_read_failure_fails_closed_instead_of_trusting_the_stale_stream() {
    // 权威源读失败 ≠ 仍然有效：必须关流（宁断不泄），而不是继续按握手期权限发。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    authority.lock().unwrap().read_failed = true;
    // 交还 router 侧的总线 Sender（测试侧 `bus` 句柄全程不释放：总线不关闭）。
    drop(conn.router);
    bus.publish(EventEnvelope::device_updated(
        &"ff".repeat(32),
        1,
        EventAudience::AnySession,
    ));
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("权威源故障必须关流")
        .unwrap();
    assert!(
        parse_frames(&bytes).is_empty(),
        "无法证明授权仍然有效时不得外发事件帧"
    );
    drop(bus);
}

#[tokio::test]
async fn idle_connection_is_closed_by_periodic_revalidation() {
    // 长时间没有任何事件的空闲连接：权威侧撤销后也必须被周期复核切断，
    // 否则「数据库里已注销的会话」会永远挂着订阅。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity(&bus, BOB, false, FAST_REVALIDATE);
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    // 交还 router 侧的总线 Sender（测试侧 `bus` 句柄全程不释放：总线不关闭）。
    drop(conn.router);
    authority.lock().unwrap().revoked = true;
    let started = Instant::now();
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .expect("空闲连接必须由周期复核关闭")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "周期复核必须在若干周期内切断，实测 {:?}",
        started.elapsed()
    );
    assert!(parse_frames(&bytes).is_empty(), "空闲关流不得外发事件帧");
    drop(bus);
}

// ---------------------------------------------------------------------------
// 受众隔离：同一总线上不同用户 / 不同权限范围
// ---------------------------------------------------------------------------

#[tokio::test]
async fn private_event_reaches_only_its_target_user_on_a_shared_bus() {
    // 同一条总线挂三条连接：alice（目标用户）、bob（另一普通用户）、
    // root（管理员旁观者）。限定给 alice 的私有事件只允许 alice 收到；
    // bob 零接收，管理员也**不得**因为「是管理员」而绕过对象级受众。
    let bus = EventBus::with_capacity(128);
    let alice = Conn::with_capacity(&bus, ALICE, false, SLOW_REVALIDATE);
    let bob = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let root = Conn::with_capacity(&bus, ROOT, true, SLOW_REVALIDATE);
    let alice_response = handshake(&alice, events_request(&alice, true, None)).await;
    let bob_response = handshake(&bob, events_request(&bob, true, None)).await;
    let root_response = handshake(&root, events_request(&root, true, None)).await;
    assert_eq!(
        bus.subscriber_count(),
        3,
        "三条连接必须共用同一条总线才能证明隔离"
    );

    let device_id = "ab".repeat(32);
    // alice 的私有设备事件 + alice 的授权投影事件（受众由构造钉死为 alice）。
    bus.publish(EventEnvelope::device_updated(
        &device_id,
        4,
        EventAudience::User(ALICE),
    ));
    bus.publish(EventEnvelope::permissions_changed(ALICE, 12));
    // 限定给 bob 的事件：alice 与 root 都必须零接收。
    bus.publish(EventEnvelope::task_updated(
        "bobs-task",
        5,
        EventAudience::User(BOB),
    ));

    drop(alice.router);
    drop(bob.router);
    drop(root.router);
    drop(bus);

    let alice_frames = parse_frames(&drain(alice_response).await);
    let bob_frames = parse_frames(&drain(bob_response).await);
    let root_frames = parse_frames(&drain(root_response).await);

    assert_eq!(
        alice_frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("device.updated"), Some("permissions.changed")],
        "目标用户必须收到自己的私有事件"
    );
    assert_eq!(
        alice_frames[0].data.as_ref().unwrap()["id"],
        Value::String(device_id)
    );
    assert_eq!(
        bob_frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("task.updated")],
        "bob 只能收到限定给自己的那一条，不得串到 alice 的事件：{bob_frames:?}"
    );
    assert!(
        root_frames.is_empty(),
        "管理员不是对象级受众的旁路：他人私有事件必须零接收：{root_frames:?}"
    );
}

#[tokio::test]
async fn admin_scoped_object_event_is_skipped_for_non_admin_connections() {
    // 只有管理员可见的**对象级**事件（`EventAudience::Admins`）：管理员收到，
    // 普通用户连接静默跳过且不占水位；同时管理员连接不会收到限定他人的事件。
    let bus = EventBus::with_capacity(128);
    let admin = Conn::with_capacity(&bus, ROOT, true, SLOW_REVALIDATE);
    let user = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let admin_response = handshake(&admin, events_request(&admin, true, None)).await;
    let user_response = handshake(&user, events_request(&user, true, None)).await;

    bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        9,
        EventAudience::Admins,
    ));
    bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        10,
        EventAudience::AnySession,
    ));

    drop(admin.router);
    drop(user.router);
    drop(bus);

    let admin_frames = parse_frames(&drain(admin_response).await);
    let user_frames = parse_frames(&drain(user_response).await);
    assert_eq!(
        admin_frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("device.updated"), Some("device.updated")],
        "管理员连接两条都收得到"
    );
    assert_eq!(
        user_frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("device.updated")],
        "普通用户只收得到 AnySession 那一条：{user_frames:?}"
    );
    assert_eq!(
        user_frames[0].data.as_ref().unwrap()["revision"],
        Value::String("10".into()),
        "跳过不得错位：留下的必须是 AnySession 那一帧"
    );
}

// ---------------------------------------------------------------------------
// 发布侧与装配
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_without_connection_is_dropped_without_error() {
    // 无人订阅时发布不阻塞、不报错、不累积（事件只是变化提示）。
    let bus = EventBus::new();
    assert_eq!(
        bus.publish(EventEnvelope::task_updated(
            "t",
            1,
            EventAudience::AnySession
        )),
        0
    );
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
            HeaderValue::from_str(&format!("rsc_session={}", fixture.conn.token)).unwrap(),
        )
        .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
        .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
        .body(Body::empty())
        .unwrap();
    let response = fixture.conn.router.clone().oneshot(request).await.unwrap();
    let (status, code) = error_code(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code, "CSRF_INVALID");
    assert_eq!(fixture.bus.subscriber_count(), 0);
}
