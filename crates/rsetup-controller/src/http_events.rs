//! SSE 事件读组件（完整 V1 目标的服务端切片；06 §4、02 §7）。
//!
//! 只提供**独立组件子 router** [`build_events_router`]，路径集合恰为
//! `GET /api/v1/events` 一条；顶层生产装配唯一归属
//! [`crate::production::build_production_router`]。
//!
//! 握手顺序与 `src/http_auth.rs`、`src/tasks/http.rs`、
//! `src/http_device_status.rs` 完全一致，**不自写** Host/Cookie/CSRF：
//! ①[`authenticate_read`]`(EndpointId::EventsRead)`（单值 Host 允许列表 →
//! 严格单值会话 Cookie → 进程内活会话预检 → DB 权威 authenticate → 读限速
//! 50/s；未登录恒 401 `AUTH_REQUIRED`）→ ②[`require_business_session`]
//! （临时密码会话一律 403 `PASSWORD_CHANGE_REQUIRED`，02 §7「已改密会话」）→
//! ③Origin 允许列表（02 §7「握手校验 Host/Origin 允许列表」：`Origin` 缺失即
//! 同源 GET，只由 Host 允许列表把关；携带时必须等于 `allowed_origin`，重复或
//! 非 ASCII 失败关闭 403）→ ④订阅总线并进入 SSE 流。
//!
//! 响应标头：`Content-Type: text/event-stream`、`Cache-Control: no-cache`
//! （二者由 axum `Sse` 写入）与显式 `Connection: keep-alive`。诚实界定：
//! `Connection` 是 hop-by-hop 头，真实 HTTP/1.1 连接上的最终值由 hyper 依
//! 连接复用状态决定，本层只负责按契约把它写进响应。
//!
//! 流的生命周期（06 §4）：
//! - 每次将要外发一帧之前，先在**进程内会话时钟**里复查本连接绑定的
//!   `session.digest`；已失效（登出/撤销/闲置到期）即结束流并释放订阅，
//!   已排队的旧授权数据不再外发（权威判定仍在 DB 侧 `authenticate`，这里
//!   只是必要非充分的内存预检）；
//! - 管理员专属事件（`system.time.changed`）对非管理员订阅者直接跳过，
//!   不发送、不占水位（06 §4「时间事件仅管理员」）；
//! - 订阅者落后于总线容量（`Lagged`，即 06 §4 的「每连接最多 128 条待发，
//!   满时发送 reset」）时，外发一帧 `event: reset` 并**继续**该订阅：客户端
//!   据此清空对象水位、全量 GET 重取；服务端不补发丢失事件、不做历史重放；
//! - 客户端断开 → axum 丢弃响应体 → 本流与其 `Drop` 一起释放广播订阅，
//!   不等下一次事件才退出；总线关闭（所有 `EventBus` 句柄析构）同样结束流。
//!
//! 本切片**不**实现 `Last-Event-ID` 断档重放（06 §4 明示其只用于断档识别、
//! 不保证重放）、也不实现逐事件的授权快照/epoch 复核（发布方在投递前完成
//! 可见性过滤）；二者都在模块文档留痕，不假称已完成。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use axum::Router;
use axum::extract::{FromRef, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::IntoResponse;
use axum::response::Response;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use futures_core::Stream;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;

use crate::events::{EventBus, EventEnvelope, EventName, RESET_REASON_BUFFER_OVERFLOW};
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{authenticate_read, get_single_origin, origin_host_invalid};
use crate::tasks::http::require_business_session;

/// SSE 事件读路由的显式状态：共享认证上下文 + 事件总线。
///
/// `auth` 必须与 auth/management/tasks/status 线来自**同一个**
/// `Arc<HttpAuthState>`（共享 session 预过滤、CSRF 绑定表与限速桶），由
/// `build_production_router` 以 `Arc::ptr_eq` 强制校验；`bus` 是同一通道
/// 的克隆句柄（[`EventBus::clone`] 不新建通道）。
#[derive(Clone)]
pub struct EventsState {
    pub auth: Arc<HttpAuthState>,
    pub bus: EventBus,
}

impl FromRef<EventsState> for Arc<HttpAuthState> {
    fn from_ref(state: &EventsState) -> Self {
        Arc::clone(&state.auth)
    }
}

/// 事件读子路由：恰 1 条端点 `GET /api/v1/events`。
pub fn build_events_router(state: EventsState) -> Router<()> {
    Router::new()
        .route("/api/v1/events", get(events_handler))
        .with_state(state)
}

/// `GET /api/v1/events`：见模块头的握手顺序。
async fn events_handler(State(state): State<EventsState>, headers: HeaderMap) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::EventsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_business_session(&actor) {
        return response;
    }
    if let Err(response) = check_handshake_origin(&state.auth, &headers) {
        return response;
    }
    let stream = SseEventStream::new(
        state.bus.subscribe(),
        Arc::clone(&state.auth),
        actor.digest,
        actor.user.is_admin,
    );
    let mut response = Sse::new(stream)
        // 15s 注释心跳：让中间代理与半关闭连接不至于把长连接当空闲回收。
        .keep_alive(KeepAlive::default())
        .into_response();
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    response
}

/// 握手期 Origin 允许列表（02 §7）：未携带 `Origin` 即同源 GET，由 Host
/// 允许列表把关；携带时必须精确等于 `allowed_origin`，重复头/非 ASCII 失败关闭。
// Err 变体是 axum `Response`（≥128 字节）：与 `http_gate` 的同形签名一致，
// 失败路径直接返回固定错误响应、不在热路径装箱，故按函数抑制该 lint。
#[allow(clippy::result_large_err)]
fn check_handshake_origin(state: &HttpAuthState, headers: &HeaderMap) -> Result<(), Response> {
    match get_single_origin(headers) {
        Ok(None) => Ok(()),
        Ok(Some(origin)) if state.config.allowed_origin.as_deref() == Some(origin) => Ok(()),
        Ok(Some(_)) | Err(()) => Err(origin_host_invalid()),
    }
}

/// 把一个命名事件投影成 SSE 帧（`event: <name>\ndata: <JSON>\n\n`）。
///
/// `serde_json::Value` 的序列化恒成功；万一失败也只降级为**空对象**载荷，
/// 事件名不变，绝不外发半截 JSON，也不因此中断整个流。
fn sse_frame(name: EventName, data: &serde_json::Value) -> Event {
    Event::default()
        .event(name.as_str())
        .json_data(data)
        .unwrap_or_else(|_| Event::default().event(name.as_str()).data("{}"))
}

/// `recv()` 一次完成的结果：投递结果 + 原样取回的 `Receiver`。
type RecvOutcome = (Result<EventEnvelope, RecvError>, Receiver<EventEnvelope>);

/// 常驻在流里的 boxed `recv_frame` future（见 `SseEventStream::pending` 字段）。
type PendingRecv = Pin<Box<dyn Future<Output = RecvOutcome> + Send>>;

/// `recv()` future 与 `Receiver` 成对搬运：`Receiver` 不能被 `&mut` 借用进
/// `'static` future，因此每次完成后把同一个 `Receiver` 连同结果一起取回，
/// 立刻重建下一个 future（等价于 `tokio-stream` 的 `BroadcastStream`，
/// 但不必为此引入 `tokio-stream`/`tokio-util` 两个新依赖）。
async fn recv_frame(mut rx: Receiver<EventEnvelope>) -> RecvOutcome {
    let result = rx.recv().await;
    (result, rx)
}

/// 单条 SSE 连接的订阅流。
///
/// `Item = Result<Event, std::convert::Infallible>`：本流自身不产生错误，
/// 唯一的外发失败（会话失效/总线关闭）表现为**正常结束**而不是错误帧。
struct SseEventStream {
    pending: PendingRecv,
    auth: Arc<HttpAuthState>,
    digest: [u8; 32],
    admin: bool,
    closed: bool,
}

impl SseEventStream {
    fn new(
        rx: Receiver<EventEnvelope>,
        auth: Arc<HttpAuthState>,
        digest: [u8; 32],
        admin: bool,
    ) -> Self {
        Self {
            pending: Box::pin(recv_frame(rx)),
            auth,
            digest,
            admin,
            closed: false,
        }
    }
}

impl Stream for SseEventStream {
    type Item = Result<Event, std::convert::Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.closed {
            return Poll::Ready(None);
        }
        loop {
            // 会话在本进程会话时钟里已失效（登出/撤销/闲置到期）：结束流并
            // 释放订阅，已排队的旧授权数据一律不再外发（06 §4）。
            if !this.auth.auth.is_live_session(&this.digest) {
                this.closed = true;
                return Poll::Ready(None);
            }
            let (result, rx) = ready!(this.pending.as_mut().poll(cx));
            this.pending = Box::pin(recv_frame(rx));
            match result {
                Ok(envelope) => {
                    if envelope.name.admin_only() && !this.admin {
                        continue;
                    }
                    return Poll::Ready(Some(Ok(sse_frame(envelope.name, &envelope.data))));
                }
                // 背压/缓冲区溢出：只提示 reset，不补发丢失的事件（06 §4）。
                Err(RecvError::Lagged(_)) => {
                    let reset = EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW);
                    return Poll::Ready(Some(Ok(sse_frame(reset.name, &reset.data))));
                }
                Err(RecvError::Closed) => {
                    this.closed = true;
                    return Poll::Ready(None);
                }
            }
        }
    }
}
