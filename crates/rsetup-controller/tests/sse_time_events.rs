//! 时间证据变化 → SSE `system.time.changed` 的进程内端到端接线测试（06 §4、02 §7）。
//!
//! 钉住的是**发布侧接线**（known-gaps c 节此前登记的缺口：时间门禁的后台 driver 在证据
//! 变化处不调用发布入口，因此事件流上不会出现任何时间事件）：`TimeGateDriver` 每次确认并
//! 把新的权威 `TimeEvidence` 写入快照后，必须经装配注入的通知渠道（[`TimeChangedSink`]）
//! 把变化投到进程内唯一事件总线，SSE 管理员订阅者因此收到 `system.time.changed` 帧；
//! 受众固定为管理员，同一条总线上的普通用户连接一条都收不到。
//!
//! 与相邻文件的分工：
//! - `tests/events_sse.rs` 钉 SSE 读线本身（握手门禁、命名帧、背压 reset、复核关流），
//!   时间事件在那里只以「测试直接调用 `EventBus::publish`」的形态出现；
//! - 本文件反过来钉**时间门禁这一侧**：不直接调用 `publish`，而是让**真实 driver** 在受控
//!   fake 采样下自己产生证据变化，再取证它确实出现在对的 SSE 连接上、没出现在错的连接上。
//!
//! 证据边界（诚实界定）：
//! - 全程进程内：真实 axum events 路由 + 真实 `tokio::sync::broadcast` 总线 +
//!   `tower::ServiceExt::oneshot`（不经真实 socket）+ 真实 `TimeGateDriver`；
//! - 采样与等待是可注入 fake（不访问真实网络，也不真实等待 1s..60s 退避），墙钟与源列表用
//!   生产实现；认证是内存桩，因此本文件**不**证明真实库会话、真实 NTP 投递或跨进程投递；
//! - 通知渠道的装配形状与 `main.rs` 的 `time_changed_sink` 一致：同一个
//!   [`rsetup_controller::events::EventEnvelope::system_time_changed`] 构造入口，
//!   受众不接受调用方给出。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;
use uuid::Uuid;

use rsetup_controller::ControllerError;
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::auth::session::token_digest;
use rsetup_controller::events::{EventBus, EventEnvelope, EventName};
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, HttpAuthState, SessionAuthority};
use rsetup_controller::http_events::EventsState;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::time::evidence::TimeQuality;
use rsetup_controller::time::socket::NtpSample;
use rsetup_controller::time::{
    NtpSourceList, NullTimeAuditSink, SleepFuture, Sleeper, StaticSourceList, SystemWallClock,
    TimeChangedSink, TimeGateConfig, TimeGateDeps, TimeSampleFuture, TimeSampler, WallClock,
    start_time_gate_with, start_time_gate_with_sink,
};

// ---------------------------------------------------------------------------
// 常量与受控门禁配置
// ---------------------------------------------------------------------------

const HOST: &str = "sse-time-events.test";
const ORIGIN: &str = "http://sse-time-events.test";
/// 读体终止的硬超时护栏。
const READ_BUDGET: Duration = Duration::from_secs(5);
/// 总线侧单条事件的取证预算：受控 fake 的采样在毫秒级完成，超时即「接线不存在」。
const PUBLISH_BUDGET: Duration = Duration::from_secs(2);
/// 周期复核周期：本文件不审长连接复核，取远大于用例生命周期的值。
const NO_PERIODIC_REVALIDATE: Duration = Duration::from_secs(3600);
/// 受控 NTP 源标识（同时用于断言事件载荷里的 `source`）。
const SOURCE: &str = "ntp-fake-a.invalid";

/// 管理员连接（`system.time.changed` 的唯一合法受众，06 §4）。
const ADMIN: [u8; 16] = [0xa6u8; 16];
/// 普通用户连接：任何时间事件都不属于他。
const BOB: [u8; 16] = [0xb0u8; 16];

/// 毫秒级复验节奏的门禁配置，其余取生产量级。
fn gate_config() -> TimeGateConfig {
    TimeGateConfig {
        servers: vec![SOURCE.to_string()],
        request_timeout: Duration::from_millis(50),
        startup_budget: Duration::from_millis(200),
        max_rtt_ms: 2_000,
        revalidate_interval: Duration::from_millis(20),
        max_evidence_age_ms: 60_000,
    }
}

// ---------------------------------------------------------------------------
// 时间门禁的可注入 fake：只有采样与等待两处，墙钟/源列表用生产实现
// ---------------------------------------------------------------------------

/// 可编程采样器：队列逐步消费（`true` 成功样本 / `false` 失败），耗尽后固定 `sticky`。
/// 成功样本的 `offset_ms` 按第 N 次调用递增，让「载荷出自哪一次采样」可断言。
///
/// 队列**刻意以 `false` 收尾**：最后一次成功之后全部失败，`NtpValid` 分支的复验失败既不再
/// `transition` 也不失效（证据远未超龄），于是不再产生新事件——用例因此能断言**精确**的
/// 事件条数，而不是给一个靠 sleep 撑出来的宽容窗口。
struct FakeSampler {
    queue: Mutex<VecDeque<bool>>,
    sticky: bool,
    calls: AtomicUsize,
}

impl FakeSampler {
    fn new(outcomes: &[bool]) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(outcomes.iter().copied().collect()),
            sticky: outcomes.last().copied().unwrap_or(false),
            calls: AtomicUsize::new(0),
        })
    }
}

impl TimeSampler for FakeSampler {
    fn sample<'a>(&'a self, _servers: &'a [String]) -> TimeSampleFuture<'a> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let ok = match self.queue.lock().unwrap().pop_front() {
            Some(ok) => ok,
            None => self.sticky,
        };
        let reference_utc = Utc::now();
        Box::pin(async move {
            ok.then(|| {
                (
                    NtpSample {
                        offset_ms: 1_000 + 7 * index as i64,
                        rtt_ms: 20,
                        reference_utc,
                        uncertainty_ms: 10,
                    },
                    SOURCE.to_string(),
                )
            })
        })
    }
}

/// 等待抽象：把真实等待截到 5ms（生产退避是 1s..60s，不截就要真等）。
struct CappedSleeper;

impl Sleeper for CappedSleeper {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        Box::pin(tokio::time::sleep(duration.min(Duration::from_millis(5))))
    }
}

/// driver 的完整受控依赖装配（墙钟与源列表 = 生产实现）。
fn gate_deps(sampler: Arc<FakeSampler>) -> TimeGateDeps {
    let sampler: Arc<dyn TimeSampler> = sampler;
    let sources: Arc<dyn NtpSourceList> = Arc::new(StaticSourceList::new(vec![SOURCE.to_string()]));
    let wall_clock: Arc<dyn WallClock> = Arc::new(SystemWallClock);
    let sleeper: Arc<dyn Sleeper> = Arc::new(CappedSleeper);
    TimeGateDeps {
        sampler,
        sources,
        wall_clock,
        sleeper,
        audit: Arc::new(NullTimeAuditSink),
    }
}

/// 与 `main.rs` 的 `time_changed_sink` **同形**的生产挂接：driver 每次确认新的权威快照，
/// 就把 [`EventEnvelope::system_time_changed`](rsetup_controller::events::EventEnvelope::system_time_changed)
/// 投到传入的总线句柄上（受众由该构造钉死为管理员，载荷就是 `TimeEvidence` 本身）。
///
/// 这里额外把总线装进 `Mutex<Option<..>>` 槽位，只是**测试**需要一个「停止发布并关闭通道」
/// 的手柄：driver 的后台循环没有退出点，SSE 长连接要靠通道关闭才结束读体。生产进程全程
/// 持有总线，不需要这层槽位。
fn production_sink(bus: &EventBus) -> (TimeChangedSink, Arc<Mutex<Option<EventBus>>>) {
    let slot = Arc::new(Mutex::new(Some(bus.clone())));
    let holder = Arc::clone(&slot);
    let sink: TimeChangedSink = Arc::new(move |evidence| {
        if let Some(bus) = holder.lock().unwrap().as_ref() {
            bus.publish(EventEnvelope::system_time_changed(evidence));
        }
    });
    (sink, slot)
}

// ---------------------------------------------------------------------------
// SSE 侧：桩认证 + 真实 events 路由；多个连接挂同一条总线以取证受众隔离
// ---------------------------------------------------------------------------

/// 只认自己那枚令牌的认证桩；会话与权威值全程恒定（被审对象是发布侧接线，
/// 不是长连接鉴权，复核关流由 `tests/events_sse.rs` 专门取证）。
struct StubAuth {
    token: String,
    session: Session,
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
        true
    }
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        let live = *digest == self.session.digest;
        let is_admin = self.session.user.is_admin;
        Box::pin(async move {
            Ok(SessionAuthority {
                session_live: live,
                is_admin,
                authz_epoch: 0,
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

/// 挂在给定总线上的一个 SSE 连接。`router` 由调用方在取证结束后释放（路由状态持有一个
/// 总线句柄；handler 返回响应后其自身那份已随 handler 一起析构，通道才会真正关闭）。
struct Conn {
    router: Router<()>,
    token: String,
}

fn hex64(seed: u8) -> String {
    (0..64)
        .map(|i| std::char::from_digit(u32::from((seed + i as u8) % 10), 16).unwrap())
        .collect()
}

impl Conn {
    fn new(bus: &EventBus, user_id: [u8; 16], is_admin: bool) -> Self {
        let token = hex64(user_id[0]);
        let session = Session {
            user: IdentityUser {
                id: user_id,
                username: if is_admin { "admin" } else { "bob" }.to_string(),
                password_hash: "not-a-real-hash".into(),
                active: true,
                is_admin,
                must_change_password: false,
                revision: 3,
            },
            digest: token_digest(token.as_bytes()),
        };
        let auth = Arc::new(HttpAuthState::new(
            Arc::new(StubAuth {
                token: token.clone(),
                session,
            }),
            HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
        ));
        let router = rsetup_controller::http_events::build_events_router(EventsState {
            auth,
            // 同一条通道的克隆句柄：多条连接共用一条总线，才能取证受众隔离。
            bus: bus.clone(),
            revalidate_interval: NO_PERIODIC_REVALIDATE,
        });
        Self { router, token }
    }

    /// 握手并断言 200，返回尚未结束的 SSE 响应。
    async fn handshake(&self) -> axum::response::Response {
        let request = Request::builder()
            .method("GET")
            .uri("/api/v1/events")
            .header(header::HOST, HeaderValue::from_static(HOST))
            .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
            .header(
                header::COOKIE,
                HeaderValue::from_str(&format!("rsc_session={}", self.token)).unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "SSE 握手必须 200");
        response
    }
}

/// 一次性释放全部总线句柄（router 与测试侧句柄）→ 所有 `Sender` 析构 → 总线关闭 → 各条流
/// 的读体终止；已排队的事件在通道关闭后仍会投递完毕（`tests/events_sse.rs` 同形取证）。
///
/// 挂在同一条总线上的多条连接**必须**一起释放：只释放一部分时通道仍未关闭，那些流的读体会
/// 一直读到超时护栏为止。
async fn close_bus_and_drain(
    routers: Vec<Router<()>>,
    bus: EventBus,
    streams: Vec<axum::response::Response>,
) -> Vec<Vec<Frame>> {
    drop(routers);
    drop(bus);
    let mut drained = Vec::with_capacity(streams.len());
    for stream in streams {
        drained.push(parse_frames(&drain(stream).await));
    }
    drained
}

async fn drain(response: axum::response::Response) -> Vec<u8> {
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .unwrap_or_else(|_| panic!("SSE 体必须在 {READ_BUDGET:?} 内由服务端结束"))
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

/// 一条流上的全部时间事件帧。
fn time_frames(frames: &[Frame]) -> Vec<&Frame> {
    frames
        .iter()
        .filter(|frame| frame.event.as_deref() == Some("system.time.changed"))
        .collect()
}

/// 一条流上时间事件的 `evidence.quality` 序列（逐帧、保持顺序）。
fn delivered_qualities(frames: &[Frame]) -> Vec<Option<String>> {
    time_frames(frames)
        .iter()
        .map(|frame| {
            frame
                .data
                .as_ref()
                .and_then(|data| data["evidence"]["quality"].as_str())
                .map(str::to_string)
        })
        .collect()
}

/// 取总线上下一条 `system.time.changed`（在预算内取不到即「driver 没有调用发布入口」）。
async fn next_time_event(receiver: &mut broadcast::Receiver<EventEnvelope>) -> EventEnvelope {
    let outcome = tokio::time::timeout(PUBLISH_BUDGET, async {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.name() == EventName::SystemTimeChanged {
                        return Some(event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return None,
            }
        }
    })
    .await;
    outcome
        .unwrap_or_else(|_| panic!("{PUBLISH_BUDGET:?} 内总线上看不到 system.time.changed"))
        .expect("事件通道提前关闭且未投递 system.time.changed")
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 主用例：driver 确认一份新的权威时间证据 → 管理员 SSE 连接收到 `system.time.changed`，
/// 载荷就是门禁刚提交的那份 `TimeEvidence`；同一条总线上的普通用户连接一条都收不到。
#[tokio::test]
async fn confirmed_time_sample_reaches_admin_sse_stream_and_not_regular_user() {
    let bus = EventBus::new();
    let admin = Conn::new(&bus, ADMIN, true);
    let bystander = Conn::new(&bus, BOB, false);
    let admin_stream = admin.handshake().await;
    let bystander_stream = bystander.handshake().await;
    // 总线侧第二订阅者：直接核对 envelope 的名称与载荷形状。
    let mut receiver = bus.subscribe();

    let clock_epoch = Uuid::new_v4();
    let (sink, slot) = production_sink(&bus);
    let handle = start_time_gate_with_sink(
        gate_config(),
        clock_epoch,
        gate_deps(FakeSampler::new(&[true, false])),
        Some(sink),
    );

    // 就绪 = driver 已把新证据写进权威快照；此刻通知渠道必须已经投出过一条事件。
    let authoritative = handle.wait_ready().await;
    assert_eq!(authoritative.quality, TimeQuality::NtpValid);

    let event = next_time_event(&mut receiver).await;
    assert_eq!(event.name(), EventName::SystemTimeChanged);
    let data = event.data().clone();
    assert_eq!(data["kind"], json!("system_time"));
    let evidence = data["evidence"].clone();
    assert_eq!(evidence["quality"], json!("ntp_valid"));
    assert_eq!(evidence["source"], json!(SOURCE));
    assert_eq!(
        evidence["clock_epoch"],
        json!(authoritative.clock_epoch.to_string()),
        "事件载荷必须来自门禁刚提交的那份权威快照（clock_epoch 一致）"
    );
    assert_eq!(
        evidence["offset_ms"],
        json!(1_000),
        "载荷来自本次采样成功的证据，不是旧值"
    );
    assert_eq!(
        evidence["sample_age_ms"],
        json!("0"),
        "十进制字符串形状与 GET /system/time 同源"
    );

    // 停止发布 + 一次性释放全部总线句柄 → 两条流各自收尾，取证事件落到哪条连接上。
    *slot.lock().unwrap() = None;
    let drained = close_bus_and_drain(
        vec![admin.router, bystander.router],
        bus,
        vec![admin_stream, bystander_stream],
    )
    .await;
    let admin_frames = &drained[0];
    let bystander_frames = &drained[1];

    let delivered = time_frames(admin_frames);
    assert_eq!(
        delivered.len(),
        1,
        "管理员连接必须恰好收到一次 system.time.changed：{admin_frames:?}"
    );
    assert_eq!(
        delivered[0].data.as_ref().unwrap()["kind"],
        json!("system_time")
    );
    assert!(
        time_frames(bystander_frames).is_empty(),
        "时间事件仅管理员：普通用户连接上不得出现 {bystander_frames:?}"
    );
}

/// 启动回退与后续恢复都算「时间证据变了」：`Stale → SystemFallback → NtpValid` 两次权威
/// 快照更新各投一条事件——降级同样要让管理员看见，不是只在转好时才发。
#[tokio::test]
async fn startup_fallback_then_recovery_each_publish_one_event() {
    let bus = EventBus::new();
    let admin = Conn::new(&bus, ADMIN, true);
    let admin_stream = admin.handshake().await;
    let mut receiver = bus.subscribe();

    let (sink, slot) = production_sink(&bus);
    // 启动采样失败 → 失效降级 SystemFallback；后台退避重试成功 → NtpValid；此后复验恒失败，
    // 证据未超龄 → 不再产生新事件。
    let handle = start_time_gate_with_sink(
        gate_config(),
        Uuid::new_v4(),
        gate_deps(FakeSampler::new(&[false, true, false])),
        Some(sink),
    );

    let fallback = next_time_event(&mut receiver).await;
    let fallback_data = fallback.data().clone();
    assert_eq!(
        fallback_data["evidence"]["quality"],
        json!("system_fallback"),
        "启动采样失败即权威证据变化，必须可见"
    );
    assert_eq!(
        fallback_data["evidence"]["source"],
        Value::Null,
        "失效降级清空来源，载荷如实反映快照而不是留着旧源"
    );

    let recovered = next_time_event(&mut receiver).await;
    assert_eq!(
        recovered.data().clone()["evidence"]["quality"],
        json!("ntp_valid")
    );
    assert!(
        handle.is_ready(),
        "退避重试成功后门禁转 ready：事件与 ready 同源"
    );

    *slot.lock().unwrap() = None;
    let drained = close_bus_and_drain(vec![admin.router], bus, vec![admin_stream]).await;
    let frames = &drained[0];
    assert_eq!(
        delivered_qualities(frames),
        vec![
            Some("system_fallback".to_string()),
            Some("ntp_valid".to_string())
        ],
        "两次权威快照更新各投一条，顺序与快照演进一致：{frames:?}"
    );
}

/// 反面对照：`start_time_gate_with`（不挂通知渠道的受控/测试装配）在总线上留不下任何事件
/// ——时间事件只有装配注入的那一个发布方，不存在别处偷偷造的第二套。
#[tokio::test]
async fn gate_without_notifier_publishes_no_time_event() {
    let bus = EventBus::new();
    let mut receiver = bus.subscribe();

    let handle = start_time_gate_with(
        gate_config(),
        Uuid::new_v4(),
        gate_deps(FakeSampler::new(&[true, false])),
    );
    // 等到 ready，再让后台循环跑满几个复验周期：换作挂了通知渠道的装配，这期间必然发过事件。
    let evidence = handle.wait_ready().await;
    assert_eq!(evidence.quality, TimeQuality::NtpValid);
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert!(
        matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ),
        "未挂通知渠道的装配不得在总线上留下任何事件"
    );
}
