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

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use futures_core::Stream as _;
use rsetup_controller::ControllerError;
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::auth::session::token_digest;
use rsetup_controller::events::{
    EVENT_BUS_CAPACITY, EventBus, EventEnvelope, EventObjectAudience, RESET_REASON_BUFFER_OVERFLOW,
};
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, HttpAuthState, SessionAuthority};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_security::HttpSecurityConfig;
use serde_json::Value;
use tokio::time::sleep;
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
/// 活性用例注入的「权威读慢成功」耗时，**必须严格大于** [`STARVE_REVALIDATE`]：
/// 只有「一次权威读的耗时 ≥ 复核周期」才构成本用例的前提（被审的饿死条件）。
const SLOW_READ: Duration = Duration::from_millis(20);
/// 活性用例的复核周期（刻意小于 [`SLOW_READ`]）。
const STARVE_REVALIDATE: Duration = Duration::from_millis(5);
/// 活性用例的整体预算：健康实现读一帧只需两次权威读（排队 + 复核）≈ 2×[`SLOW_READ`]，
/// 本预算已是它的十几倍；被周期复核饿死时这段预算内会塞满几十次同样的读，
/// 判定与 [`STARVE_MAX_READS`] 都不依赖时序抖动放大。
const STARVE_BUDGET: Duration = Duration::from_millis(300);
/// 空闲慢读用例的复核周期（同时用作该用例的读耗时：两者相等时被审路径的
/// 读节奏是 40ms，修复后是 80ms，比例上界 2，见用例内推算）。
const PERIODIC_REVALIDATE: Duration = Duration::from_millis(40);
/// 空闲窗口的观察时长（真实时间）：健康实现约 4 次读，被审路径约 8 次。
const IDLE_WINDOW: Duration = Duration::from_millis(320);
/// 空闲窗口内允许的权威读次数上界（「复核完成后不得立刻再来一次」）。
const IDLE_MAX_READS: usize = 6;
/// 活性用例允许的权威读次数上界（同时是「没有忙轮询」的取证阈值）：健康实现的
/// 读次数只由**帧数**决定（握手 1 次 + 每帧 1 次），与被审路径的
/// 「预算 / 复核周期」量级（本用例 60 次）相差一个数量级。
const STARVE_MAX_READS: usize = 8;

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
    /// 本连接的权威 `session_authority` 被调用次数（取证「有没有忙轮询」）。
    reads: Arc<AtomicUsize>,
    /// 权威读的**人为**延迟（微秒；0 = 不延迟）。握手完成后翻成非 0，就是在
    /// 模拟「权威源确实有效，但这一次成功读很慢」。
    read_delay_us: Arc<AtomicUsize>,
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
        //
        // 持锁范围**必须**止于 `async move` 之外：延迟（模拟慢权威源）是在 await
        // 期间发生的，若把 `MutexGuard` 带进 future，同一连接的两条权威读
        // （握手基准 + 流内复核）就会互锁——那会伪装成「权威读很慢」的故障。
        let authority = *self.authority.lock().unwrap();
        let mine = *digest == self.session.digest;
        let reads = Arc::clone(&self.reads);
        let delay = Duration::from_micros(self.read_delay_us.load(Ordering::SeqCst) as u64);
        Box::pin(async move {
            reads.fetch_add(1, Ordering::SeqCst);
            if !delay.is_zero() {
                sleep(delay).await;
            }
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
    /// 本连接的权威 `session_authority` 被调用次数（取证「有没有忙轮询」）。
    reads: Arc<AtomicUsize>,
    /// 本连接权威读的**人为**延迟（微秒；0 = 不延迟）。在握手完成后翻成非 0，
    /// 就是在模拟「权威源此刻确实有效，但这一次读很慢」。
    read_delay_us: Arc<AtomicUsize>,
}

impl Conn {
    fn with_capacity(
        bus: &EventBus,
        user_id: [u8; 16],
        is_admin: bool,
        revalidate: Duration,
    ) -> Self {
        Self::with_capacity_and_reader(bus, user_id, is_admin, revalidate, 0)
    }

    /// `read_delay_us` 非 0 时给权威读注入固定延迟（活性用例的注入点）。
    fn with_capacity_and_reader(
        bus: &EventBus,
        user_id: [u8; 16],
        is_admin: bool,
        revalidate: Duration,
        read_delay_us: usize,
    ) -> Self {
        let token = hex64(user_id[0]);
        let digest = token_digest(token.as_bytes());
        Self::with_session(
            bus,
            fixture_session(user_id, is_admin, digest),
            revalidate,
            read_delay_us,
        )
    }

    /// 唯一构造点：会话形状由调用方给出（覆盖 `must_change_password` 这类特例），
    /// 令牌按 `hex64(user.id[0])` 推导，权威侧的 live/authority/计数句柄在此集中
    /// 装配。所有用例共用这里，新增桩字段不可能被某个复制件漏掉。
    fn with_session(
        bus: &EventBus,
        session: Session,
        revalidate: Duration,
        read_delay_us: usize,
    ) -> Self {
        let token = hex64(session.user.id[0]);
        let live = Arc::new(AtomicBool::new(true));
        let authority = Arc::new(Mutex::new(Authority {
            revoked: false,
            is_admin: session.user.is_admin,
            epoch: 0,
            read_failed: false,
        }));
        let reads = Arc::new(AtomicUsize::new(0));
        let read_delay_us = Arc::new(AtomicUsize::new(read_delay_us));
        let auth = Arc::new(HttpAuthState::new(
            Arc::new(StubAuth {
                token: token.clone(),
                session,
                live: Arc::clone(&live),
                authority: Arc::clone(&authority),
                reads: Arc::clone(&reads),
                read_delay_us: Arc::clone(&read_delay_us),
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
            reads,
            read_delay_us,
        }
    }

    /// 取权威状态源的共享句柄：**必须**在 `drop(conn.router)` 之前取，
    /// 之后 partial move 掉的 `conn` 不能再借用。
    fn authority(&self) -> Arc<Mutex<Authority>> {
        Arc::clone(&self.authority)
    }

    /// 同理取权威读计数句柄（`drop(conn.router)` 之后仍可读：router 释放不影响
    /// `Arc<StubAuth>` 已被 router 持有的事实，句柄在测试侧）。
    fn reads(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.reads)
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
    drain_within(response, READ_BUDGET).await
}

/// [`drain`] 的可指定预算版本：活性用例用**远小于** [`READ_BUDGET`] 的预算，
/// 把「慢到什么程度算被饿死」写成显式契约，而不是靠放宽护栏蒙过去。
async fn drain_within(response: axum::response::Response, budget: Duration) -> Vec<u8> {
    let bytes = tokio::time::timeout(budget, to_bytes(response.into_body(), 1 << 20))
        .await
        .unwrap_or_else(|_| {
            panic!("SSE 体必须在 {budget:?} 内由服务端结束；读不出来即排队事件或总线关闭被饿死")
        })
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
    let conn = Conn::with_session(&bus, session, SLOW_REVALIDATE, 0);
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
    // 本连接是管理员（`fixture(true, ..)` → ROOT）。两条对象事件取**管理员范围**
    // 受众：对象事实已不存在「任何合法会话」这一档（`EventObjectAudience` 里没有
    // 该变体），所以「五种命名事件都能收齐」这条契约只在受众覆盖本连接时成立。
    let published = [
        EventEnvelope::device_updated(&device_id, 12, EventObjectAudience::admins()),
        EventEnvelope::task_updated(
            "0e7f1c2e-0000-4000-8000-000000000001",
            7,
            EventObjectAudience::admins(),
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
            EventObjectAudience::user(BOB),
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
        EventObjectAudience::user(BOB),
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
        EventObjectAudience::user(ROOT),
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
        EventObjectAudience::admins(),
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
        EventObjectAudience::user(BOB),
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
        EventObjectAudience::user(BOB),
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
        EventObjectAudience::user(BOB),
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
        EventObjectAudience::user(ALICE),
    ));
    bus.publish(EventEnvelope::permissions_changed(ALICE, 12));
    // 限定给 bob 的事件：alice 与 root 都必须零接收。
    bus.publish(EventEnvelope::task_updated(
        "bobs-task",
        5,
        EventObjectAudience::user(BOB),
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
async fn foreign_permissions_change_is_invisible_to_the_wrong_user_and_admin_bystander() {
    // 被审缺陷（P1）：`permissions.changed` 的载荷 `id` 与受众曾经是**两个可以
    // 分别填写**的公开字段，「载荷写着 Alice、受众写着 Bob」的信封能把 Alice 的
    // authz_epoch 投到 Bob 的流上。现在两者由同一份字节生成
    // （`EventEnvelope::permissions_changed(user_id, epoch)`，受众参数根本不接受
    // 调用方给出），本用例在真实 axum 流上钉住三点：
    // 1. alice 收到的载荷 `id` **就是** alice 本人（受众与载荷同源，不错位）；
    // 2. bob 与管理员旁观者零接收（受众不是装饰，管理员也不是对象级受众的旁路）；
    // 3. 事件里那个 epoch 只属于 alice，别人读不到自己授权投影之外的任何事实。
    let bus = EventBus::with_capacity(128);
    let alice = Conn::with_capacity(&bus, ALICE, false, SLOW_REVALIDATE);
    let bob = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let root = Conn::with_capacity(&bus, ROOT, true, SLOW_REVALIDATE);
    let alice_response = handshake(&alice, events_request(&alice, true, None)).await;
    let bob_response = handshake(&bob, events_request(&bob, true, None)).await;
    let root_response = handshake(&root, events_request(&root, true, None)).await;

    bus.publish(EventEnvelope::permissions_changed(ALICE, 12));

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
        vec![Some("permissions.changed")],
        "本人必须收到自己的授权投影事件"
    );
    let data = alice_frames[0].data.as_ref().unwrap();
    assert_eq!(
        data["id"],
        Value::String(uuid::Uuid::from_bytes(ALICE).to_string()),
        "载荷 id 必须就是受众本人（与受众同源，不存在被改写成他人的写法）"
    );
    assert_eq!(data["epoch"], Value::String("12".into()));
    assert!(
        bob_frames.is_empty(),
        "Alice 的授权投影绝不允许出现在 Bob 的流上：{bob_frames:?}"
    );
    assert!(
        root_frames.is_empty(),
        "管理员旁观者同样不得收到他人的授权投影：{root_frames:?}"
    );
}

#[tokio::test]
async fn object_events_are_visible_only_to_their_own_audience_slice() {
    // **对象级**事件的两条合法受众（管理员范围 / 某一个用户）各自只对自己的
    // 连接可见。此前这条用例断言的是「AnySession 的对象事件对普通用户也可见」
    // ——那是错的：对象事实没有「广播给所有连接」这一档（受众类型里根本没有），
    // 普通用户连接只能看到明确指向自己的那一份，管理员也只能看到管理员范围那一份。
    let bus = EventBus::with_capacity(128);
    let admin = Conn::with_capacity(&bus, ROOT, true, SLOW_REVALIDATE);
    let user = Conn::with_capacity(&bus, BOB, false, SLOW_REVALIDATE);
    let admin_response = handshake(&admin, events_request(&admin, true, None)).await;
    let user_response = handshake(&user, events_request(&user, true, None)).await;

    bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        9,
        EventObjectAudience::admins(),
    ));
    bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        10,
        EventObjectAudience::user(BOB),
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
        vec![Some("device.updated")],
        "管理员只收得到管理员范围那一条，指向他人的对象事件同样不得串过来：{admin_frames:?}"
    );
    assert_eq!(
        admin_frames[0].data.as_ref().unwrap()["revision"],
        Value::String("9".into()),
        "跳过不得错位：留下的必须是管理员范围那一帧"
    );
    assert_eq!(
        user_frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("device.updated")],
        "普通用户只收得到明确指向自己的那一条：{user_frames:?}"
    );
    assert_eq!(
        user_frames[0].data.as_ref().unwrap()["revision"],
        Value::String("10".into()),
        "跳过不得错位：留下的必须是指向本人那一帧"
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
            EventObjectAudience::user(BOB)
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

// ---------------------------------------------------------------------------
// 活性：权威读耗时 ≥ 复核周期时，接收支路与总线关闭不得被饿死
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_authority_reads_do_not_starve_queued_events_or_bus_shutdown() {
    // 被审缺陷（P2）：一次**成功**的权威复核读耗时 ≥ 复核周期时，「周期复核优先
    // 于 recv」的分支会在每次复核后立刻抢先发起下一次权威读，接收支路再也排不
    // 到自己 —— 排队事件永不到达用户、总线关闭永不被发现（流挂死 + SQL 忙轮询）。
    //
    // 取证装置（不靠放大 timeout）：
    // - `SLOW_READ` > `STARVE_REVALIDATE`，把被审的饿死前提钉成构造事实；
    // - 读体只给 [`STARVE_BUDGET`] 预算（健康实现实测耗时的十几倍），到点即失败；
    // - 权威读次数上界 [`STARVE_MAX_READS`]：健康实现的读次数只由**帧数**决定，
    //   被饿死时读次数由「预算 / 复核周期」决定（本用例 60 次量级）。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity_and_reader(&bus, BOB, false, STARVE_REVALIDATE, 0);
    let reads = conn.reads();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "握手必须恰好一次权威基准读"
    );
    // 握手完成后才注入慢读：握手期那次读属于握手门禁，不参与本用例。
    conn.read_delay_us
        .store(SLOW_READ.as_micros() as usize, Ordering::SeqCst);
    let reads_after_handshake = reads.load(Ordering::SeqCst);

    drop(conn.router);
    // 两条**排队**事件（先进总线，再让接收支路去取），随后关闭总线。受众限定给
    // 本连接用户，确保它们是真要发给这条连接的帧，而不是被受众过滤丢弃的噪声。
    bus.publish(EventEnvelope::device_updated(
        &"cd".repeat(32),
        1,
        EventObjectAudience::user(BOB),
    ));
    bus.publish(EventEnvelope::task_updated(
        "queued-while-verifying",
        2,
        EventObjectAudience::user(BOB),
    ));
    drop(bus);

    let frames = parse_frames(&drain_within(response, STARVE_BUDGET).await);
    assert_eq!(
        frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("device.updated"), Some("task.updated")],
        "两条排队事件都必须在「权威读慢于复核周期」下按序送达，一条都不得被饿死"
    );
    let total = reads.load(Ordering::SeqCst);
    assert!(
        total - reads_after_handshake <= STARVE_MAX_READS,
        "两条帧只应做与帧数成正比的权威读，实测 {total} 次：周期复核正在抢先空转（忙轮询 SQL）"
    );
}

#[tokio::test]
async fn periodic_revalidation_waits_a_full_period_after_a_slow_read() {
    // 被审缺陷的另一半：`poll_next` 在**成功**复核后直接 `continue`，而 `tick`
    // 的下一个到期时刻是在它**被 poll 的那一刻**推进的，于是一次耗时 ≥ 周期的读
    // 完成后，`tick` 当场又算「已到期」，立刻再发起一次权威读——刚被权威证明仍然
    // 有效的连接在同一瞬间又被读一遍，读节奏退化成「权威源能多快就多快」。
    // 修复：下一次周期从**本次读完成之后**起算（`tick.reset()`）。
    //
    // 取证不靠放大 timeout：读耗时与复核周期都取 40ms ⇒ 被审路径节奏 ≈ 40ms/次
    // （320ms 观察窗约 8 次读），修复后 ≈ 80ms/次（约 4 次），阈值取 6 次；
    // 下界 2 次是阳性对照（周期复核必须真的在跑，不是「干脆不读」蒙过断言）。
    //
    // 必须**并发**读体：SSE 是惰性的，没人 poll 响应体时整个流都不推进，也就
    // 一次权威读都不会发生（那会让本用例退化成空断言）。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity_and_reader(
        &bus,
        BOB,
        false,
        PERIODIC_REVALIDATE,
        // 读耗时 == 复核周期：这是被审路径节奏退化的**最坏比例**配置
        // （见下面的次数推算），比例 = 1 + 周期 / 读耗时，此处取到上界 2。
        PERIODIC_REVALIDATE.as_micros() as usize,
    );
    let reads = conn.reads();
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "握手必须恰好一次权威基准读"
    );
    drop(conn.router);

    let drain = tokio::spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 1 << 20),
        )
        .await
        .expect("空闲连接必须由周期复核切断")
        .unwrap()
    });
    // 让读体先跑起来再计数。
    sleep(Duration::from_millis(10)).await;
    let baseline = reads.load(Ordering::SeqCst);
    // 观察窗口：总线上不发任何事件，只让周期复核自己跑。
    sleep(IDLE_WINDOW).await;
    let observed = reads.load(Ordering::SeqCst) - baseline;
    // 收口：权威侧撤销 → 下一次复核关流 → 读体结束。
    authority.lock().unwrap().revoked = true;
    let bytes = drain.await.expect("读体任务必须正常结束");
    assert!(
        parse_frames(&bytes).is_empty(),
        "周期复核切断空闲连接时不得外发事件帧"
    );

    assert!(
        observed >= 2,
        "空闲连接必须持续做周期权威复核（阳性对照），观察窗内只读到 {observed} 次"
    );

    assert!(
        observed <= IDLE_MAX_READS,
        "观察窗 {IDLE_WINDOW:?}（周期与读耗时各 {:?}）内读到 {observed} 次：\
         一次慢读完成后周期复核立刻又发起一次（下一次周期没有从复核完成后起算）",
        PERIODIC_REVALIDATE
    );
}

/// 洪流用例的复核周期：1ms，配合 [`FLOOD_SETTLE`] 保证「开始洪流的时刻周期早已
/// 到期」，被审代码若真的跑了周期复核就必然看得见 `revoked = true`。
const FLOOD_REVALIDATE: Duration = Duration::from_millis(1);
/// 洪流用例在开始洪流前真实流逝的时间（**必须**远大于 [`FLOOD_REVALIDATE`]）：
/// 让「周期到期」成为构造事实，而不是靠运气撞上时序。
const FLOOD_SETTLE: Duration = Duration::from_millis(5);
/// 洪流用例的 poll 轮数上界（有界！不靠无限等待判定失败）。
const FLOOD_ROUNDS: usize = 48;
/// 修复后允许的最大 poll 轮数：健康实现第一次带预算的 poll 就该跑周期复核。
/// 4 轮是十几倍的余量（周期只有 1ms）。
const FLOOD_MAX_ROUNDS_TO_CLOSE: usize = 4;
/// 每轮 poll 之前发布的无关事件条数：`capacity - 1` —— 足以让 `recv()` 轮轮
/// Ready（并耗尽 tokio 的 128 步 cooperative 预算），又刻意留 1 个空位不触发
/// `Lagged`（`Lagged` 会经 `reset` 帧另开一条权威读路径，掩盖被审缺陷）。
const FLOOD_PER_ROUND: usize = EVENT_BUS_CAPACITY - 1;
/// 每轮之间的真实让出：刷新 cooperative 预算并推进定时器（模拟真实服务端
/// 一轮 I/O 驱动的间隔，不是「放宽 timeout」——判定仍然只由 poll 轮数决定）。
const FLOOD_ROUND_PAUSE: Duration = Duration::from_micros(200);
/// 「同轮到期 tick 不得吞事件」用例的周期与读耗时（读耗时 > 周期 ⇒ 一条事件
/// 的复核还没跑完，`tick` 就又到了下一个到期点）。
const TICK_SWALLOW_PERIOD: Duration = Duration::from_millis(1);
const TICK_SWALLOW_READ: Duration = Duration::from_millis(3);
/// 同上用例投事件前真实流逝的时间（> 周期）：此间没人 poll 响应体（SSE 惰性），
/// `tick` 的到期状态因此不会被提前消费 ⇒ 「第一次 poll 就同时遇到 `tick` 到期
/// 与有待取事件」成为构造事实。
const TICK_SWALLOW_SETTLE: Duration = Duration::from_millis(3);

#[tokio::test]
async fn foreign_event_flood_does_not_starve_periodic_revalidation() {
    // 被审缺陷（P2）：被审原状的 `poll_next` 只在 `recv()` 返回 `Pending` 时才
    // 轮到排在后面的周期复核（那里的编号是 `④`=recv、`⑤`=tick；现文件已改成
    // `④` 问 tick、`⑤` 取事件、`⑥` 并联消费）。持续与本连接**无关**（受众不
    // 覆盖本连接）的事件让接收支路轮轮 Ready 并 `continue`，`tick` 因此永不被
    // poll；即使 cooperative 预算耗尽把 `recv` 逼成 `Pending`，同一批预算也让
    // `Sleep::poll` 一起 `Pending`，下一轮（预算刷新后）依然先 `recv`。所以
    // 「cooperative 最终会公平」不成立 —— 周期权威复核被无关洪流**无限期**推
    // 迟：只在数据库里被撤销（内存会话时钟仍 live）的连接可以在洪流期间一直
    // 挂着订阅不放。
    //
    // 取证装置（受控、非空洞，不靠放大 timeout）：
    // - `EventBus::new()` 默认 128 容量（与生产同形），握手方是 **Bob**，洪水
    //   指向 **Alice**（`permissions.changed` 受众钉死为 Alice ⇒ 对 Bob 不可见、
    //   不发、不占水位，但每一条都会唤醒并占用 Bob 流的接收支路）；
    // - 权威侧撤销 + 内存时钟保持 live：只有周期复核能切断这条连接；
    // - 1ms 周期先真实流逝 5ms（`tick` 早已到期）才开始洪流，每轮之间只让出
    //   200µs；判定上界是**轮数** [`FLOOD_ROUNDS`]，不是墙钟 timeout；
    // - 每轮 poll 之前才发布洪水，因此「洪流仍在进行」与「已 poll 过 N 次」
    //   同时成立 —— 不允许「停洪流之后再断言」。
    let bus = EventBus::new();
    let conn = Conn::with_capacity(&bus, BOB, false, FLOOD_REVALIDATE);
    let reads = conn.reads();
    let authority = conn.authority();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "握手必须恰好一次权威基准读"
    );
    drop(conn.router);

    authority.lock().unwrap().revoked = true;
    assert!(
        conn.live.load(Ordering::SeqCst),
        "内存时钟必须仍是 live：撤销只存在于权威源，只有权威复核能看见"
    );

    // 让 1ms 的周期真正到期（含调度余量），之后才灌入洪流。
    sleep(FLOOD_SETTLE).await;

    let mut body = Box::pin(response.into_body().into_data_stream());
    // 空 waker：本用例自己决定何时再 poll（唤醒只意味着「可以再 poll 一次」），
    // 因此不需要真实唤醒源。
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let mut bytes: Vec<u8> = Vec::new();
    let mut closed_at: Option<usize> = None;
    let mut published = 0usize;

    for round in 1..=FLOOD_ROUNDS {
        for n in 0..FLOOD_PER_ROUND {
            published += 1;
            bus.publish(EventEnvelope::permissions_changed(ALICE, n as u64));
        }
        match body.as_mut().poll_next(&mut cx) {
            Poll::Ready(Some(Ok(chunk))) => bytes.extend_from_slice(&chunk),
            Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                closed_at = Some(round);
                break;
            }
            Poll::Pending => {}
        }
        // 真实让出执行权：刷新 cooperative 预算 + 推进定时器，下一轮再灌一批。
        sleep(FLOOD_ROUND_PAUSE).await;
    }

    // 洪流期间的权威读次数快照：下面的「停洪流」诊断 poll 会把计数抬上去，
    // 判定「洪流期间周期复核有没有跑」必须用这个快照。
    let reads_during_flood = reads.load(Ordering::SeqCst);
    // 诊断用（**不是**通过条件）：洪流停止后再 poll 一次。若「洪流中永不关、
    // 洪流一停就立刻关」，即证明关流能力一直都在，只是被无关事件无限推迟。
    let closes_only_after_flood_stops = closed_at.is_none()
        && matches!(
            body.as_mut().poll_next(&mut cx),
            Poll::Ready(Some(Err(_))) | Poll::Ready(None)
        );

    let total_reads = reads.load(Ordering::SeqCst);
    // 读侧先释放（订阅随响应体一起析构），再断言订阅数。
    drop(body);
    let frames = parse_frames(&bytes);
    // 安全断言（与活性断言同样重要）：洪流里指向 Alice 的授权投影一帧都不许
    // 出现在 Bob 的流上；这条连接也**没有**合法可发的事实 ⇒ 任何帧都是泄漏。
    assert!(
        frames.is_empty(),
        "Bob 不得收到任何帧（尤其不得收到指向 Alice 的 permissions.changed）：{frames:?}"
    );
    match closed_at {
        None => panic!(
            "无关事件洪流把周期权威复核饿死了：发布 {published} 条指向 Alice 的事件、\
             poll {FLOOD_ROUNDS} 轮（周期只有 {FLOOD_REVALIDATE:?}）之后仍未关流，\
             洪流期间的权威读 {reads_during_flood} 次（只有握手那一次）、订阅仍未释放；\
             停洪流后立刻关流：{closes_only_after_flood_stops}"
        ),
        Some(round) => assert!(
            round <= FLOOD_MAX_ROUNDS_TO_CLOSE,
            "周期复核到点后应在极少轮 poll 内切断（周期 {FLOOD_REVALIDATE:?}），实测第 {round} 轮"
        ),
    }
    assert!(
        total_reads > 1,
        "关流必须由握手之后的权威复核做出，实测权威读 {total_reads} 次\
         （洪流停后才关流：{closes_only_after_flood_stops}）"
    );
    assert_eq!(bus.subscriber_count(), 0, "关流必须释放订阅");
}

#[tokio::test]
async fn expired_tick_does_not_swallow_events_pending_in_the_same_round() {
    // 上一条的**对偶**守卫：修「周期复核被洪流饿死」时很容易把 `tick` 提前到
    // `recv` 之前，若实现是「先 poll recv 取走事件、再因 `tick` 到期 while
    // continue」，刚取走的那条事件就会被吞（或五条被合并成一帧）。本用例把
    // 「每次权威复核回到循环顶部时 `tick` 必然又已到期」钉成构造事实
    // （读耗时 3ms > 周期 1ms），然后要求 5 条命名事件**逐帧**按序送达。
    let bus = EventBus::with_capacity(128);
    let conn = Conn::with_capacity_and_reader(
        &bus,
        BOB,
        false,
        TICK_SWALLOW_PERIOD,
        TICK_SWALLOW_READ.as_micros() as usize,
    );
    let reads = conn.reads();
    let response = handshake(&conn, events_request(&conn, true, None)).await;
    drop(conn.router);

    // 关键时序：先让 `tick` **确确实实到期**（此间没人 poll 响应体，SSE 惰性 ⇒
    // 到期状态不会被提前消费掉），再投事件。于是「第一次 poll 就同时遇到
    // `tick` 到期 + 有新事件」成为构造事实 —— 坏修法（把「tick 优先」写成
    // 「本轮吞掉刚取到的事件」）在这里就会少一帧。
    sleep(TICK_SWALLOW_SETTLE).await;

    // 五条**指向 Bob** 的对象事件（受众各不相同的 revision，便于证明没被合并）。
    for revision in 1..=5u64 {
        bus.publish(EventEnvelope::device_updated(
            &"ab".repeat(32),
            revision,
            EventObjectAudience::user(BOB),
        ));
    }
    drop(bus);

    let frames = parse_frames(&drain_within(response, STARVE_BUDGET).await);
    let revisions: Vec<Value> = frames
        .iter()
        .map(|f| {
            f.data
                .as_ref()
                .unwrap_or_else(|| panic!("每帧必须带 data：{frames:?}"))["revision"]
                .clone()
        })
        .collect();
    assert_eq!(
        frames
            .iter()
            .map(|f| f.event.as_deref())
            .collect::<Vec<_>>(),
        vec!["device.updated"; 5]
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>(),
        "五条事件必须各自成帧，一条都不能被到期的周期复核吞掉或合并：{frames:?}"
    );
    assert_eq!(
        revisions,
        (1..=5u64)
            .map(|n| Value::String(n.to_string()))
            .collect::<Vec<_>>(),
        "帧序与 revision 必须逐条对齐（不丢、不重、不错位）"
    );
    // 复核仍由慢读驱动（每帧一次），但没有退化成一个 tick 一次读的忙轮询。
    let total = reads.load(Ordering::SeqCst);
    assert!(
        total >= 6,
        "每条帧至少要一次权威复核（阳性对照），实测 {total} 次"
    );
    assert!(
        total <= 24,
        "到期的 tick 不得把每条事件放大成多次权威读（忙轮询 SQL），实测 {total} 次"
    );
}
