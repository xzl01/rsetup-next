//! SSE 事件读组件（完整 V1 目标的服务端切片；06 §4、02 §7）。
//!
//! 只提供**独立组件子 router** [`build_events_router`]，路径集合恰为
//! `GET /api/v1/events` 一条；顶层生产装配唯一归属
//! [`crate::production::build_production_router`]。
//!
//! 握手顺序与 `src/http_auth.rs`、`src/tasks/http.rs`、
//! [`crate::http_device_status`] 完全一致，**不自写** Host/Cookie/CSRF：
//! ①[`authenticate_read`]`(EndpointId::EventsRead)`（单值 Host 允许列表 →
//! 严格单值会话 Cookie → 进程内活会话预检 → DB 权威 authenticate → 读限速
//! 50/s；未登录恒 401 `AUTH_REQUIRED`）→ ②[`require_business_session`]
//! （临时密码会话一律 403 `PASSWORD_CHANGE_REQUIRED`，02 §7「已改密会话」）→
//! ③Origin 允许列表（02 §7「握手校验 Host/Origin 允许列表」：`Origin` 缺失即
//! 同源 GET，只由 Host 允许列表把关；携带时必须等于 `allowed_origin`，重复或
//! 非 ASCII 失败关闭 403）→ ④[`crate::http_auth::AuthServiceApi::session_authority`] 取**权威**
//! 基准快照（会话仍有效 + 当前 `authz_epoch` + 当前 `is_admin`；会话已失效
//! 401，权威源读不出 503，且门禁在订阅之前）→ ⑤订阅总线并进入 SSE 流。
//!
//! 响应标头：`Content-Type: text/event-stream`、`Cache-Control: no-cache`
//! （二者由 axum `Sse` 写入）与显式 `Connection: keep-alive`。诚实界定：
//! `Connection` 是 hop-by-hop 头，真实 HTTP/1.1 连接上的最终值由 hyper 依
//! 连接复用状态决定，本层只负责按契约把它写进响应。
//!
//! 流的生命周期（06 §4「发送前验证 session 与 authz_epoch，撤权关闭旧订阅，
//! 排队旧权限数据不得发送」）：
//!
//! 1. **逐帧权威复核是唯一的发送前置条件**。流里所有外发帧（含背压
//!    `reset`）都先进入 `queued` 槽，再触发一次
//!    [`crate::http_auth::AuthServiceApi::session_authority`] 权威读；核验通过后才有唯一一处
//!    `Poll::Ready(Some(frame))` 出口。会话在权威库里被撤销、用户被停用、
//!    进程换代、`authz_epoch` 与握手基准不等、`is_admin` 与握手基准不等、
//!    权威源读取失败，一律**立即关流并丢弃待发事件**（fail-closed：宁断不
//!    泄，客户端重连后按新授权重新握手）。因此在数据库里被降权/撤销的连接
//!    不可能继续享用握手期冻结的旧权限；
//! 2. **逐事件受众过滤**。总线的 `audience` 标注由发布方给出（02 §7「逐事件
//!    过滤」），本流用连接自己的权威身份视角做 [`crate::events::EventAudience::visible_to`]
//!    严格匹配，不属于本连接的受众**静默跳过**，不发送、不占水位。管理员专属
//!    事件名（[`EventName::admin_only`]）与受众是两道并联门禁，缺一不可；
//! 3. **每次 poll 都先跑内存会话时钟预检** [`crate::http_auth::AuthServiceApi::is_live_session`]：
//!    已失效（登出/撤销/闲置到期）即结束流并释放订阅，这条廉价路径让内存侧
//!    已知的失效不必等一次权威读；权威判定始终在 ①/复核侧，内存预检只是必要
//!    非充分条件；
//! 4. **周期性复核**（周期 [`EventsState::revalidate_interval`]，默认 5s）。
//!    长时间无事件的空闲连接同样必须被切断，否则「数据库里已注销的会话」会
//!    永远挂着订阅。两条活性界定（都被 `tests/events_sse.rs` 钉住）：
//!    - **周期从复核完成之后起算**：一次成功权威读之后 `tick` 被重置，「读耗时
//!      ≥ 周期」时不会同一条连接反复读 SQL（`②` 的 `tick.reset()`）；
//!    - **到期的周期不被无关事件推迟**：每轮 poll 都在消费新事件**之前**问一次
//!      `tick`，到期状态与「有待发帧」并联后才发起复核（`④`/`⑥`）。因此总线上
//!      持续涌入与本连接无关的事件时，复核的推迟上界是**一次 cooperative 强制
//!      让出**，与事件速率无关 —— 这不是「可接受的推迟」而是必须成立的前提：
//!      只在数据库里被撤销（内存会话时钟仍 live）的连接，切断它的唯一路径就是
//!      这条周期复核；
//! 5. 订阅者落后于总线容量（`Lagged`，即 06 §4 的「每连接最多 128 条待发，
//!    满时发送 reset」）时，把 `reset` 帧也走同一套核验后外发，并**继续**该
//!    订阅：客户端据此清空对象水位、全量 GET 重取；服务端不补发丢失事件、
//!    不做历史重放；
//! 6. 客户端断开 → axum 丢弃响应体 → 本流与其 `Drop` 一起释放广播订阅，
//!    不等下一次事件才退出；总线关闭（所有 `EventBus` 句柄析构）同样结束流。
//!
//! 成本界定（诚实）：每帧一次权威读 = 每帧至多两次 `SELECT`（会话行 +
//! `schema_meta` epoch），上限由事件速率与复核周期决定，不与板端采集争抢
//! （发布侧永不因慢订阅阻塞）；本切片**不**实现 `Last-Event-ID` 断档重放
//! （06 §4 明示其只用于断档识别、不保证重放）。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

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
use tokio::time::{Instant as TimerInstant, Interval, MissedTickBehavior, interval_at};

use crate::error::ControllerError;
use crate::events::{
    EventBus, EventEnvelope, EventName, EventViewer, RESET_REASON_BUFFER_OVERFLOW,
};
use crate::http_auth::{EndpointId, HttpAuthState, SessionAuthority};
use crate::http_gate::{
    auth_required, authenticate_read, backend_error, get_single_origin, origin_host_invalid,
};
use crate::tasks::http::require_business_session;

/// 空闲连接的权威复核周期缺省值：与 `KeepAlive` 的 15s 心跳同数量级、
/// 明显更快，使「数据库里被撤销/降权」在一轮复核内切断连接，而不是等
/// 下一条事件才暴露。装配方可按部署形态收紧。
pub const DEFAULT_REVALIDATE_INTERVAL: Duration = Duration::from_secs(5);

/// `poll_next` 里一次权威复核的周期下限：装配方给出 0 时也不许让
/// `tokio::time::interval_at` 直接 panic。
const MIN_REVALIDATE_INTERVAL: Duration = Duration::from_millis(1);

/// SSE 事件读路由的显式状态：共享认证上下文 + 事件总线 + 复核周期。
///
/// `auth` 必须与 auth/management/tasks/status 线来自**同一个**
/// `Arc<HttpAuthState>`（共享 session 预过滤、CSRF 绑定表与限速桶），由
/// `build_production_router` 以 `Arc::ptr_eq` 强制校验；`bus` 是同一通道
/// 的克隆句柄（[`EventBus::clone`] 不新建通道）。
#[derive(Clone)]
pub struct EventsState {
    pub auth: Arc<HttpAuthState>,
    pub bus: EventBus,
    /// 长连接的权威复核周期（见模块头第 4 条）。生产用
    /// [`DEFAULT_REVALIDATE_INTERVAL`]，测试可缩短以做确定性时序断言。
    pub revalidate_interval: Duration,
}

impl EventsState {
    /// 生产装配便捷入口：复核周期取 [`DEFAULT_REVALIDATE_INTERVAL`]。
    pub fn new(auth: Arc<HttpAuthState>, bus: EventBus) -> Self {
        Self {
            auth,
            bus,
            revalidate_interval: DEFAULT_REVALIDATE_INTERVAL,
        }
    }
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
    // 握手期就取**权威**基准快照：会话有效性、`authz_epoch`、`is_admin` 三项
    // 都取权威侧此刻的值（而不是 `authenticate` 返回行的缓存值），之后每帧
    // 复核都与这个基准比对。权威源读不出即 503（宁断不泄），会话已失效即
    // 401；两者都发生在订阅之前，不给失效连接挂订阅。
    let authority = match state.auth.auth.session_authority(&actor.digest).await {
        Ok(authority) => authority,
        Err(error) => return backend_error(&error),
    };
    if !authority.session_live {
        return auth_required();
    }
    let stream = SseEventStream::new(
        state.bus.subscribe(),
        Arc::clone(&state.auth),
        EventViewer {
            user_id: actor.user.id,
            is_admin: authority.is_admin,
        },
        authority.authz_epoch,
        actor.digest,
        state.revalidate_interval,
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

/// 常驻在流里的 boxed 权威复核 future（见 `SseEventStream::verify` 字段）。
type PendingVerify =
    Pin<Box<dyn Future<Output = Result<SessionAuthority, ControllerError>> + Send>>;

/// `recv()` future 与 `Receiver` 成对搬运：`Receiver` 不能被 `&mut` 借用进
/// `'static` future，因此每次完成后把同一个 `Receiver` 连同结果一起取回，
/// 立刻重建下一个 future（等价于 `tokio-stream` 的 `BroadcastStream`，
/// 但不必为此引入 `tokio-stream`/`tokio-util` 两个新依赖）。
///
/// **一次只取一条**（刻意不连读）：06 §4 的契约是「每个事件一帧」，测试侧
/// 也按帧序断言；在 future 里连读会把积压合并成一帧（已实测：五条命名事件
/// 只剩最后一条，甚至因随后的 `Closed` 而一帧不发）。接收支路的活性不靠
/// 连读，而靠 `poll_next` 每轮**既**问一次 `tick`（`④`）**又**取一条事件
/// （`⑤`）——两条支路不再互斥，到期的周期在 `⑥` 与待发帧并联消费。
async fn recv_frame(mut rx: Receiver<EventEnvelope>) -> RecvOutcome {
    let result = rx.recv().await;
    (result, rx)
}

/// 单条 SSE 连接的订阅流。
///
/// `Item = Result<Event, std::convert::Infallible>`：本流自身不产生错误，
/// 唯一的外发失败（会话失效/降权/epoch 变更/权威源故障/总线关闭）表现为
/// **正常结束**而不是错误帧——EventSource 侧的重连会重新握手取新授权。
///
/// 发送不变式（读代码时按这两条自查即可）：
/// - 唯一的 `Poll::Ready(Some(..))` 出口在「权威复核成功之后」；
/// - 任何 `queued` 里的事件在复核失败/受众不符时只能被丢弃，不能被发送。
struct SseEventStream {
    auth: Arc<HttpAuthState>,
    /// 会话摘要：只用于复核，不含任何凭据（原始令牌从不留在流里）。
    digest: [u8; 32],
    /// 本连接的身份视角；`is_admin` 每次权威复核后按权威值刷新。
    viewer: EventViewer,
    /// 握手期基准 `authz_epoch`：不等即视为授权投影已变更，关流。
    epoch: u64,
    pending: PendingRecv,
    /// 进行中的权威复核；非 `None` 期间一帧都不许外发。
    verify: Option<PendingVerify>,
    /// 已通过廉价受众过滤、等待权威复核后才可外发的事件。
    queued: Option<EventEnvelope>,
    tick: Box<Interval>,
    closed: bool,
}

impl SseEventStream {
    fn new(
        rx: Receiver<EventEnvelope>,
        auth: Arc<HttpAuthState>,
        viewer: EventViewer,
        epoch: u64,
        digest: [u8; 32],
        revalidate_interval: Duration,
    ) -> Self {
        let revalidate_after = revalidate_interval.max(MIN_REVALIDATE_INTERVAL);
        let mut tick = Box::new(interval_at(
            TimerInstant::now() + revalidate_after,
            revalidate_after,
        ));
        // 长时间被 poll 不到（例如内核缓冲背压）后不补跑一串复核，只按
        // 「最近一次之后重新计时」推进，避免唤醒风暴放大成 DB 抖动。
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            auth,
            digest,
            viewer,
            epoch,
            pending: Box::pin(recv_frame(rx)),
            verify: None,
            queued: None,
            tick,
            closed: false,
        }
    }

    /// 廉价受众过滤（02 §7「逐事件过滤」）：判定入口只有一个——
    /// [`EventEnvelope::visible_to`]（管理员事件名门禁 + 受众覆盖 + 名称/受众/
    /// 载荷一致性，三者并联）。任一不满足即静默跳过，连权威读都不必发起。
    ///
    /// 不在这里各写一遍 `admin_only`/`audience` 的读法：字段已是只读访问器，
    /// 判定与构造配对同源，流侧再抄一份只会多出一个可能抄错的副本。
    fn may_view(&self, envelope: &EventEnvelope) -> bool {
        envelope.visible_to(self.viewer)
    }

    /// 权威快照是否仍然**完全**支持这条连接继续服务：
    /// 会话仍有效 + `authz_epoch` 与握手基准相等 + 管理员标志与握手基准相等。
    /// 任何一项不等都关流：连接的受众视野是在握手期权限下算出来的，权限
    /// 一旦移动（升或降）旧视野就不再可信，重连后按新授权重新握手。
    fn accepted(&self, authority: &SessionAuthority) -> bool {
        authority.session_live
            && authority.authz_epoch == self.epoch
            && authority.is_admin == self.viewer.is_admin
    }

    /// 发起一次权威复核。`async move` 块同时拥有 `Arc` 与 digest，内部对
    /// `session_authority` 的借用不逃出这个块，因此可安全装箱成 `'static`。
    fn start_verify(&mut self) {
        let auth = Arc::clone(&self.auth);
        let digest = self.digest;
        self.verify = Some(Box::pin(async move {
            auth.auth.session_authority(&digest).await
        }));
    }

    /// 结束流：丢弃待发事件（旧授权数据不得外发）并释放订阅。
    fn close(&mut self) -> Poll<Option<Result<Event, std::convert::Infallible>>> {
        self.closed = true;
        self.queued = None;
        self.verify = None;
        Poll::Ready(None)
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
            // ① 进程会话时钟预检（登出/撤销/闲置到期）：廉价且每轮必查，
            //    内存侧已知的失效不必再花一次权威读。
            if !this.auth.auth.is_live_session(&this.digest) {
                return this.close();
            }
            // ② 权威复核在飞：先把它跑完。`verify` 为 `Pending` 时整个 poll
            //    挂起，`queued` 保持不动（一帧都不许外发）。
            let in_flight = this.verify.as_mut().map(|verify| verify.as_mut().poll(cx));
            if let Some(outcome) = in_flight {
                match ready!(outcome) {
                    Ok(authority) => {
                        this.verify = None;
                        if !this.accepted(&authority) {
                            // 会话撤销/停用/换代、authz_epoch 变更、管理员
                            // 标志变更：立即关流并丢弃待发事件。
                            return this.close();
                        }
                        this.viewer.is_admin = authority.is_admin;
                        // 周期复核重新计时：本连接**刚刚**被权威证明仍然有效，
                        // 下一次周期复核必须从这次读**完成之后**起算。否则
                        // 「一次权威读的耗时 ≥ 复核周期」时，`tick` 在被 poll 的
                        // 那一刻又已到期，周期复核会一轮接一轮地抢先发起权威读，
                        // 把接收支路（排队事件、总线关闭）永久饿死（活性）。
                        this.tick.as_mut().reset();
                        if let Some(envelope) = this.queued.take() {
                            // 复核通过后再核一次受众（纵深防御）：不匹配即丢弃。
                            if this.may_view(&envelope) {
                                return Poll::Ready(Some(Ok(sse_frame(
                                    envelope.name(),
                                    envelope.data(),
                                ))));
                            }
                        }
                        continue;
                    }
                    // 权威源读取失败：无法证明「仍然有效」即等同于无效。
                    Err(_) => return this.close(),
                }
            }
            // ③ 有待发帧且尚无复核在飞 → 立即发起权威复核（唯一的发送前置）。
            if this.queued.is_some() {
                this.start_verify();
                continue;
            }
            // ④ 周期复核到点了吗？**先问周期，再取事件，但两者都不落空**：
            //    这里刻意只「问」不 `continue`。`Interval::poll_tick` 返回 Ready
            //    的同时就把下一次到期时刻推后一个周期，若在 `⑤` 之前抢先
            //    `continue`，到期的 tick 就被换成「一次立刻发起的权威复核」，慢
            //    权威读期间接收支路再也轮不到 poll（排队事件永不送达、总线关闭
            //    永不被发现，被审过的活性缺陷）；反过来，若把 `poll_tick` 整个挪
            //    到 `⑤` 之后（本文件曾经的样子），`⑤` 的 `continue` 会让 `tick`
            //    永不被 poll —— 持续涌入与本连接无关的事件即可把周期复核无限期
            //    推迟下去，数据库里已撤销的会话在洪流期间一直挂着订阅。折中是把
            //    到期状态存进 `periodic_due`，在 `⑥` 与「有待发帧」并联消费：每轮
            //    poll 都给到期的 tick 一次机会（洪流的 `continue` 发生在 `⑤`，而
            //    下一轮 `④` 仍然先问 `tick`），到期的那次 tick 又必定换来一次权威
            //    复核，不会凭空消失。
            let periodic_due = this.tick.as_mut().poll_tick(cx).is_ready();
            // ⑤ 事件支路：一次只取一条（`recv_frame` 每次 poll 至多返回一个），
            //    从根上杜绝「一次 poll 排入多条、多帧合并成一帧」。`queued` 非空
            //    时 `③` 已经把控制流截走，本支路的赋值因此不可能覆盖掉尚未发送
            //    的待发帧；取到事件后也**不**在此处 `continue` 之前丢掉 `④` 的
            //    到期状态（见 `⑥`）。
            let mut took_event = false;
            if let Poll::Ready((result, rx)) = this.pending.as_mut().poll(cx) {
                // 取走一条之后必须换上新 `pending`，否则已取走的 `&mut Receiver`
                // 借用无法结束（`poll` 的入参是 `&mut Receiver`，借用同时结束）。
                this.pending = Box::pin(recv_frame(rx));
                match result {
                    Ok(envelope) => {
                        // 受众不是权限证明：属于别人的事件在这里被**静默跳过**
                        // （不发送、不占水位、不关流），但受众过滤绝不意味着可以
                        // 放松权威复核 —— 发送资格只由 `②` 决定。
                        if this.may_view(&envelope) {
                            this.queued = Some(envelope);
                        }
                    }
                    // 背压/缓冲区溢出：reset 帧同样只有权威复核通过才外发，之后
                    // 订阅继续（不补发丢失的事件，06 §4）。
                    Err(RecvError::Lagged(_)) => {
                        this.queued = Some(EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW));
                    }
                    Err(RecvError::Closed) => {
                        return this.close();
                    }
                }
                took_event = true;
            }
            // ⑥ 唯一的发送前置：有待发帧（含 `reset`）**或**周期复核到期，就立刻
            //    发起一次权威复核；出口只在 `②`。两条支路并联在这里是刻意的：
            //    - 有待发帧 → 发送前必须过一次权威复核（宁断不泄）；
            //    - 周期到期 → 本轮就发起复核，空闲连接、以及总线上持续涌入与本
            //      连接无关事件的连接，都靠这一支被切断。无关事件每轮至多让 `⑤`
            //      多取一条，抢不走 `④` 每轮对 `tick` 的那一次 poll；cooperative
            //      预算耗尽时 `poll_tick` 与 `recv` 一起 `Pending`，强制让出之后
            //      预算刷新、`④` 仍然第一个被 poll，因此推迟的上界是**一次让出**，
            //      与事件速率无关。
            if this.queued.is_some() || periodic_due {
                this.start_verify();
                continue;
            }
            if took_event {
                // 取到的是与本连接无关的帧：静默跳过，继续消费下一条。
                continue;
            }
            // 两条支路都挂了唤醒（`pending` 的广播唤醒 / `tick` 的定时器），
            // 交回 `Pending`；任何一条就绪都会重新驱动本流。
            return Poll::Pending;
        }
    }
}
