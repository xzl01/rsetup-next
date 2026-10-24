//! 生产会话的业务 handler：真实 `BusinessCodec` 入站解码 → `BoardAgent` 类型化分发
//! → 真实 typed payload 回编码，并把裁决映射到隧道驱动的 `SessionHandler` 契约。
//!
//! 职责边界（顺序不可调换，全部在触达任何 source/journal 之前完成前三步）：
//!
//! 1. **只有 `TYPE_REQUEST` 才是请求**：`TYPE_EVENT` / `TYPE_SYSTEM` / `TYPE_RESPONSE`
//!    一律不回业务包（§6.5：Event 禁止回包；System 由驱动拥有）。
//! 2. **`tunnel.` 保留前缀由协议层拥有**：`tunnel.ping` 探测与 kick/revoke 绝不被业务层
//!    按业务 payload 解码或代答（§6.7.2 的 pong 只能由驱动的未决表产出）。
//! 3. **业务只在控制流**：中控 `board_client` 固定用 Control 流下发六动作，数据流上的
//!    业务请求按入参非法（1003）拒绝，且不触达任何 source/journal。
//! 4. **身份只来自握手**：`PeerContext.authenticated_controller_key` 是本机在握手
//!    验签后绑定的中控公钥；packet 的 `metadata` 与 `payload` 里自称的任何身份
//!    （controller_key / device_sn / 任意字段）**完全不参与**判定，驱动也不从 packet
//!    自报字段推断设备。
//! 5. **失败回包形态固定**：非 0 `status_code` + **空 payload** + 固定去敏
//!    `error_message`（`BusinessStatus` 的固定文本，不含 payload/路径/密钥）。
//! 6. **`Indeterminate` 无业务回包**：执行结果不可知时既不能报成功也不能报 1009，
//!    只能让本会话立即中止（见 [`BoardSessionHandler`]），由后续 `device.task.get`
//!    核实；本层绝不伪造响应掩盖「结果未知」。中止意图**不因尚未 attach 而丢失**：
//!    [`SessionBinding`] 记住它并在句柄就位后同步补发（两种到达顺序皆可靠）。
//! 7. **板端主动事件只在会话打开时投递**：`device.reboot.execute` 裁决出成功的
//!    `TaskRecord` 后，同一份记录经 [`SessionBinding::send_event`] 以 `TYPE_EVENT`
//!    （控制流、动作 `device.task.result`、禁止回包）交给会话发送队列。事件是
//!    best-effort 通知：句柄尚未 attach、会话已关闭/排空或队列背压都只跳过并计数，
//!    **绝不改写已裁决的业务回包**，也不伪造「中控已收到」。
//!
//! 关于阻塞：`decide` 是同步的。journal 查询在 `JournalStore` 内是纯内存查表
//! （持久化只发生在写入路径），只读来源只读 `/proc` 的小文件；handler future 由驱动
//! 的 `JoinSet` 与并发预算托管，最坏情况占用一个 handler 槽位，不会饿死 probe
//! （probe 走独立小队列）。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BusinessMessage, BusinessPayload, TaskRecord, approved_codec_witness, encode_business,
};
use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::session::{
    HandlerFuture, Reply, RequestContext, RequestError, SessionHandle, SessionHandler,
};

use crate::actions::{REBOOT_EXECUTE_ACTION, TASK_RESULT_EVENT_ACTION, is_reserved};
use crate::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader,
};
use crate::read_only::{PeerContext, ReadOnlySource};

/// 优雅排空期限：§6.6 规定 GOAWAY 后至少 5s；生产接线只会更长，不会更短。
pub const DEFAULT_SESSION_DRAIN: Duration = Duration::from_secs(5);

/// 业务请求允许的流（§6.3 + 中控 `board_client` 固定 Control）。
pub const BUSINESS_STREAM: StreamKind = StreamKind::Control;

/// 数据流上收到业务请求时的固定去敏错误文本。
const DATA_STREAM_ERROR: &str = "business requests must use the control stream";
/// 内部故障的固定去敏错误文本。
const INTERNAL_ERROR: &str = "internal error";

/// 业务分发注入点：`BoardAgent<R, J, E>` 的 blanket 实现使用真实 [`BusinessCodec`]。
///
/// 之所以再抽一层，是为了让 handler 的入站裁决可以用替身 dispatcher 做端到端断言，
/// 同时生产路径仍然必须经过 `BoardAgent::handle_wire` 的「白名单 → codec 解码 →
/// DTO 校验 → 触达 source/journal」固定顺序。
pub trait BusinessDispatch: Send + Sync {
    fn handle_business(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus>;
}

impl<R, J, E> BusinessDispatch for BoardAgent<R, J, E>
where
    R: ReadOnlySource,
    J: TaskJournalReader,
    E: JournalEpochSource,
{
    fn handle_business(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus> {
        self.handle_wire(&BusinessCodec, peer, action, payload)
    }
}

/// 一次业务回包：`status_code`、固定 `error_message` 与 typed payload 编码结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusinessReply {
    pub status_code: i32,
    pub error_message: String,
    pub payload: Vec<u8>,
}

/// 不回包的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoReplyReason {
    /// 不是请求（Event / System / Response）。
    NotARequest,
    /// `tunnel.` 保留前缀动作，由协议驱动拥有。
    ReservedAction,
    /// 执行结果不可知：必须中止会话，不能伪造回包。
    Indeterminate,
}

impl NoReplyReason {
    /// 该原因映射到「会话已不可信」时的固定错误文本（仅用于协议层兜底路径）。
    fn label(self) -> &'static str {
        match self {
            Self::NotARequest => "packet is not a request",
            Self::ReservedAction => "reserved protocol action",
            Self::Indeterminate => INTERNAL_ERROR,
        }
    }
}

/// 入站裁决：回包，或不回包（并说明原因）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusinessDecision {
    Reply(BusinessReply),
    NoReply(NoReplyReason),
}

impl BusinessDecision {
    /// 成功回包的快捷构造。
    fn ok(payload: Vec<u8>) -> Self {
        Self::Reply(BusinessReply {
            status_code: 0,
            error_message: String::new(),
            payload,
        })
    }
    /// 失败回包：非 0 status + 空 payload + 固定错误文本。
    fn failed(status: BusinessStatus) -> Self {
        Self::Reply(BusinessReply {
            status_code: status as i32,
            error_message: status.to_string(),
            payload: Vec::new(),
        })
    }
    /// 带固定错误文本的入参非法回包（数据流业务、协议层兜底）。
    fn invalid_argument(error_message: &str) -> Self {
        Self::Reply(BusinessReply {
            status_code: BusinessStatus::InvalidArgument as i32,
            error_message: error_message.to_string(),
            payload: Vec::new(),
        })
    }
    /// 内部故障回包（协议层兜底；绝不伪造成功）。
    fn internal(reason: NoReplyReason) -> Self {
        Self::Reply(BusinessReply {
            status_code: BusinessStatus::InternalError as i32,
            error_message: reason.label().to_string(),
            payload: Vec::new(),
        })
    }
    /// 取回包（不回包时 `None`）。
    pub fn reply(&self) -> Option<&BusinessReply> {
        match self {
            Self::Reply(reply) => Some(reply),
            Self::NoReply(_) => None,
        }
    }
}

/// 板端主动出站事件帧（§6.5 模式 3：`TYPE_EVENT`，独立 trace、不建未决项、不等响应）。
///
/// 首版只有 `device.task.result` 一种：固定走控制流（03 规格 B-01：业务请求/响应与
/// `device.task.result` 事件均在 OpenControl），载荷是同一份 `TaskRecord` DTO。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEvent {
    pub stream: StreamKind,
    pub action: String,
    pub payload: Vec<u8>,
}

/// 把已裁决的 `TaskRecord` 组装为 `device.task.result` 事件帧。
///
/// 出站 DTO 先自检（非法记录绝不外流），再复用与回包完全相同的编码路径，
/// 保证中控在「响应」与「事件」两处看到的是同一份字节。
pub fn task_result_event(record: &TaskRecord) -> Result<PendingEvent, BusinessStatus> {
    record
        .validate()
        .map_err(|_| BusinessStatus::InternalError)?;
    Ok(PendingEvent {
        stream: BUSINESS_STREAM,
        action: TASK_RESULT_EVENT_ACTION.to_string(),
        payload: encode_response(&BusinessResponse::TaskRecord(record.clone()))?,
    })
}

/// 一次入站裁决的完整产出：业务回包（或不回包）**加上**随后应补发的板端主动事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adjudication {
    pub decision: BusinessDecision,
    /// 事件补发队列（当前只可能来自 `device.reboot.execute` 的成功 `TaskRecord`）。
    pub events: Vec<PendingEvent>,
}

/// 事件投递失败的原因。两条都不排队、不重放、不重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EventDeliveryError {
    /// 会话句柄尚未就位（本 connection 还没 attach），本次直接跳过。
    #[error("session handle is not attached yet")]
    NotAttached,
    /// 会话已关闭 / 排空 / 背压：事件是 best-effort 通知，权威事实由 `device.task.get` 核实。
    #[error("event delivery rejected by the session")]
    Session(#[from] RequestError),
}

/// 业务 handler：绑定「一次会话」的身份（握手验签公钥 + 本连接代际）。
///
/// `connection_id` 必须是**本次 connection** 新生成的值：票据与 reboot 调度按它绑定，
/// 旧连接的票据在新 connection 上必然失效。
pub struct BusinessHandler {
    dispatch: Arc<dyn BusinessDispatch>,
    peer: PeerContext,
    connection_id: String,
}

impl BusinessHandler {
    pub fn new(
        dispatch: Arc<dyn BusinessDispatch>,
        authenticated_controller_key: [u8; 32],
        connection_id: impl Into<String>,
    ) -> Self {
        Self {
            dispatch,
            peer: PeerContext {
                authenticated_controller_key,
            },
            connection_id: connection_id.into(),
        }
    }

    /// 身份只可能来自握手结果。
    pub fn peer_context(&self) -> PeerContext {
        self.peer.clone()
    }
    pub fn connection_id(&self) -> &str {
        &self.connection_id
    }

    /// 纯函数式裁决（只产出回包部分）：完整产出见 [`Self::adjudicate`]。
    pub fn decide(&self, stream: StreamKind, packet: &TunnelPacket) -> BusinessDecision {
        self.adjudicate(stream, packet).decision
    }

    /// 纯函数式裁决：不做任何 I/O 之外的副作用，可被驱动与测试同一入口驱动。
    ///
    /// 产出 = 业务回包（或「不回包」）+ 应在会话打开后补发的板端主动事件。事件只在
    /// **`device.reboot.execute` 且裁决产出成功 `TaskRecord`** 时产生（accepted /
    /// attempted / failed / unknown 都是持久记录，中控都需要主动通知）；
    /// `Rejected`（非零 status、空 payload）与 `Indeterminate`（无回包）都不产生事件。
    pub fn adjudicate(&self, stream: StreamKind, packet: &TunnelPacket) -> Adjudication {
        // 1) 只有请求需要回包；Event / System / Response 绝不进入业务分发。
        if packet.packet_type != PacketType::Request {
            return Adjudication {
                decision: BusinessDecision::NoReply(NoReplyReason::NotARequest),
                events: Vec::new(),
            };
        }
        // 2) tunnel. 保留前缀属于协议层（ping / kick / revoke），业务层一律代答=否。
        if is_reserved(&packet.action) {
            return Adjudication {
                decision: BusinessDecision::NoReply(NoReplyReason::ReservedAction),
                events: Vec::new(),
            };
        }
        // 3) 业务只接受控制流；数据流业务在触达任何 I/O 之前拒绝。
        if stream != BUSINESS_STREAM {
            return Adjudication {
                decision: BusinessDecision::invalid_argument(DATA_STREAM_ERROR),
                events: Vec::new(),
            };
        }
        // 4) 真实 codec 解码 + 白名单 + DTO 校验 + source/journal 触达（顺序由分发层保证）。
        match self
            .dispatch
            .handle_business(&self.peer, &packet.action, &packet.payload)
        {
            // 结果不可知：无业务回包，由会话层 abort，也绝不补发结果事件。
            Ok(BusinessResponse::Indeterminate) => Adjudication {
                decision: BusinessDecision::NoReply(NoReplyReason::Indeterminate),
                events: Vec::new(),
            },
            Ok(response) => {
                // execute 产出持久 TaskRecord → 补发 device.task.result；
                // 事件组装失败（DTO 自检 / 编码）只丢事件，绝不把已成功的回包改成错误。
                let events = match &response {
                    BusinessResponse::TaskRecord(record)
                        if packet.action == REBOOT_EXECUTE_ACTION =>
                    {
                        task_result_event(record).into_iter().collect()
                    }
                    _ => Vec::new(),
                };
                let decision = match encode_response(&response) {
                    Ok(payload) => BusinessDecision::ok(payload),
                    // 出站 DTO 已在 BoardAgent 内自检通过，这里失败只能是本地编码故障。
                    Err(status) => BusinessDecision::failed(status),
                };
                Adjudication { decision, events }
            }
            Err(status) => Adjudication {
                decision: BusinessDecision::failed(status),
                events: Vec::new(),
            },
        }
    }
}

/// `BusinessResponse` → typed payload 编码。
///
/// `Indeterminate` **没有可编码形态**：返回 `InternalError`，调用方必须走
/// 「无回包」路径，不得把它编码成 status=0 的空 payload。
pub fn encode_response(response: &BusinessResponse) -> Result<Vec<u8>, BusinessStatus> {
    let payload = match response {
        BusinessResponse::Capabilities(message) => BusinessPayload::Capabilities(message.clone()),
        BusinessResponse::DeviceStatus(message) => BusinessPayload::DeviceStatus(message.clone()),
        BusinessResponse::ClockSample(message) => BusinessPayload::ClockSample(message.clone()),
        BusinessResponse::TaskRecord(message) => BusinessPayload::TaskRecord(message.clone()),
        BusinessResponse::RebootTicket(message) => BusinessPayload::RebootTicket(message.clone()),
        BusinessResponse::Indeterminate => return Err(BusinessStatus::InternalError),
    };
    let witness = approved_codec_witness();
    encode_business(&BusinessCodec, Some(&witness), &payload)
        // 出站 DTO 已通过自检，编码失败属本地部署故障 → 1009。
        .map_err(|_| BusinessStatus::InternalError)
}

/// 会话句柄的一次性绑定槽 + **已请求中止的记忆**：
/// `Arc<dyn SessionHandler>` 必须先于 `SessionHandle` 存在（`start_client_session` 的
/// 签名决定），而 `Indeterminate` 又需要反过来 abort。
///
/// 因此存在一个真实的窗口：`start_client_session` 已在别的 runtime 线程上开始服务
/// 请求，驱动/业务 handler 可能先做出 `Indeterminate` 裁决，而外层还没把句柄放进来。
/// 槽必须**记住**这次中止意图，并在 attach 之后同步补发——中止意图静默丢失等于
/// 「结果不可知的会话继续在线」，正是本层要禁止的事。
///
/// 同步语义（标准的两变量握手，两侧都是「先 `SeqCst` 写自己的一方、再 `SeqCst` 读对方」）：
/// * [`Self::abort`]：`SeqCst` 置意图位 → `SeqCst` 读就位位；
/// * [`Self::attach`]：`SeqCst` 置就位位 → `SeqCst` 读意图位。
///
/// 于是并发交错里至少有一方看得见对方：要么 abort 看见了「已就位」并直接中止，要么
/// attach 看见了意图位并在句柄已就位后补发。**两个位都必须是 `SeqCst`**：只有
/// store-buffering（Dekker）双方都是全序操作时，「两边都读不到对方」才被内存模型禁止；
/// 一侧退化成 release/acquire 就让这个结论不成立（弱内存序平台上是真缺陷，不是洁癖）。
/// 就位位与句柄之间是同步边（同一原子的 store/load 配对），因此看见「已就位」就一定能
/// 读到那个句柄。这里没有任何互斥量，`SessionHandle::abort` 只在锁外被调用，
/// 不存在「持锁回调外部代码再重入本槽」的死锁面。
#[derive(Clone, Default)]
pub struct SessionBinding(Arc<BindingSlot>);

#[derive(Default)]
struct BindingSlot {
    /// 本 connection 的会话句柄：一次性，绝不更换。
    handle: OnceLock<SessionHandle>,
    /// 句柄是否已就位（与 `abort_requested` 构成 `SeqCst` 握手对）。
    attached: AtomicBool,
    /// 已请求中止（可能早于 attach）。置位后不再复位。
    abort_requested: AtomicBool,
}

impl SessionBinding {
    pub fn new() -> Self {
        Self(Arc::new(BindingSlot {
            handle: OnceLock::new(),
            attached: AtomicBool::new(false),
            abort_requested: AtomicBool::new(false),
        }))
    }
    /// 只接受第一次绑定（重连必须换 handler 实例与新 connection_id；返回 `false`
    /// 表示这个槽已经属于另一条 connection，调用方必须自行 abort 新句柄并停止）。
    pub fn attach(&self, handle: SessionHandle) -> bool {
        if self.0.handle.set(handle).is_err() {
            return false;
        }
        // 先发布「已就位」，再读中止意图（顺序不可调换，见类型文档）。
        self.0.attached.store(true, Ordering::SeqCst);
        if self.0.abort_requested.load(Ordering::SeqCst)
            && let Some(handle) = self.0.handle.get()
        {
            handle.abort();
        }
        true
    }
    pub fn is_attached(&self) -> bool {
        self.0.attached.load(Ordering::SeqCst)
    }
    /// 是否已请求过中止（含尚未 attach 的情况）。让调用方能区分
    /// 「还没人要求停」与「停的要求已经记下、等 attach 补发」。
    pub fn abort_requested(&self) -> bool {
        self.0.abort_requested.load(Ordering::SeqCst)
    }
    /// 中止本会话：已绑定则立即生效；尚未绑定则**记注意图**，由 [`Self::attach`] 补发。
    /// 幂等，且绝不静默丢弃。
    pub fn abort(&self) {
        // 先置意图位，再读就位位（顺序不可调换，见类型文档）。
        self.0.abort_requested.store(true, Ordering::SeqCst);
        if self.0.attached.load(Ordering::SeqCst)
            && let Some(handle) = self.0.handle.get()
        {
            handle.abort();
        }
    }
    /// 在**会话打开**时投递一个板端主动事件（`TYPE_EVENT`，控制流）。
    ///
    /// 事件不像中止意图那样需要「记忆补发」：句柄尚未就位、会话已关闭或已排空、
    /// 队列背压都只返回错误并跳过本次通知——任务结果的权威事实始终可由
    /// `device.task.get` 核实，本层不伪造「事件已达」。成功也只表示已交给本端发送队列。
    pub async fn send_event(&self, event: &PendingEvent) -> Result<(), EventDeliveryError> {
        if !self.is_attached() {
            return Err(EventDeliveryError::NotAttached);
        }
        let Some(handle) = self.0.handle.get() else {
            return Err(EventDeliveryError::NotAttached);
        };
        handle
            .send_event(event.stream, &event.action, event.payload.clone())
            .await
            .map_err(EventDeliveryError::from)
    }
    /// 当前绑定的会话句柄（仅在本 connection 已 attach 后可见）。
    pub fn handle(&self) -> Option<&SessionHandle> {
        if self.is_attached() {
            self.0.handle.get()
        } else {
            None
        }
    }
}

/// 把 [`BusinessHandler`] 升格为隧道驱动的 `SessionHandler`。
pub struct BoardSessionHandler {
    core: Arc<BusinessHandler>,
    binding: SessionBinding,
    /// 已交给会话发送队列的板端主动事件数。
    events_sent: Arc<AtomicU64>,
    /// 因会话未就位 / 已关闭 / 背压而跳过的事件数（不重试、不补发）。
    events_skipped: Arc<AtomicU64>,
}

impl BoardSessionHandler {
    pub fn new(core: Arc<BusinessHandler>) -> Arc<Self> {
        Arc::new(Self {
            core,
            binding: SessionBinding::new(),
            events_sent: Arc::new(AtomicU64::new(0)),
            events_skipped: Arc::new(AtomicU64::new(0)),
        })
    }
    /// 会话启动后由连接层 `attach`；`Indeterminate` 只能经它 abort。
    pub fn binding(&self) -> SessionBinding {
        self.binding.clone()
    }
    pub fn core(&self) -> &Arc<BusinessHandler> {
        &self.core
    }
    /// 已投递的主动事件数（见证：`device.task.result` 确实被交出去了）。
    pub fn events_sent(&self) -> u64 {
        self.events_sent.load(Ordering::SeqCst)
    }
    /// 被跳过的事件数（会话未打开或未接受该事件）。
    pub fn events_skipped(&self) -> u64 {
        self.events_skipped.load(Ordering::SeqCst)
    }
}

impl SessionHandler for BoardSessionHandler {
    fn on_request(&self, ctx: RequestContext) -> HandlerFuture {
        // 裁决同步完成（无网络等待），future 只负责按裁决回包或挂起等 abort。
        let adjudication = self.core.adjudicate(ctx.stream, &ctx.packet);
        let Adjudication { decision, events } = adjudication;
        let binding = self.binding.clone();
        let events_sent = Arc::clone(&self.events_sent);
        let events_skipped = Arc::clone(&self.events_skipped);
        Box::pin(async move {
            // 事件先于回包交给同一条控制流的发送队列：两者是各自独立的帧
            // （事件独立 trace、禁止回包；回包同 trace、同 action），中控按 sub_task_id
            // 关联，不依赖先后顺序。
            for event in &events {
                match binding.send_event(event).await {
                    Ok(()) => {
                        events_sent.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(_) => {
                        events_skipped.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            match decision {
                BusinessDecision::Reply(reply) => Reply {
                    status_code: reply.status_code,
                    error_message: reply.error_message,
                    // Vec<u8> → Bytes 由字段类型推导完成。
                    payload: reply.payload.into(),
                },
                BusinessDecision::NoReply(reason) => match reason {
                    // 结果不可知：主动中止当前会话，本 future 不再产出任何响应
                    // （owner 收到 abort 后结清全部未决与任务，不留下伪 1009 的余地）。
                    NoReplyReason::Indeterminate => {
                        binding.abort();
                        std::future::pending::<Reply>().await
                    }
                    // 协议层兜底（驱动本不应把这些包路由给业务）：
                    // 回固定 1009 + 空 payload，不伪造成功，也绝不占住 handler 槽位。
                    NoReplyReason::NotARequest | NoReplyReason::ReservedAction => Reply {
                        status_code: BusinessDecision::internal(reason)
                            .reply()
                            .map(|reply| reply.status_code)
                            .unwrap_or(BusinessStatus::InternalError as i32),
                        error_message: reason.label().to_string(),
                        payload: Vec::new().into(),
                    },
                },
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_reply_reasons_have_fixed_safe_labels() {
        for reason in [
            NoReplyReason::NotARequest,
            NoReplyReason::ReservedAction,
            NoReplyReason::Indeterminate,
        ] {
            let label = reason.label();
            assert!(label.is_ascii());
            assert!(label.len() <= 64);
        }
    }

    #[test]
    fn binding_is_single_shot_and_keeps_an_early_abort() {
        let binding = SessionBinding::new();
        assert!(!binding.is_attached());
        assert!(!binding.abort_requested());
        // 未 attach 时 abort 不 panic，但**必须留下可观察的意图**：静默 no-op 会让
        // 「Indeterminate 要求停止」在 attach 窗口里凭空消失。
        binding.abort();
        binding.abort();
        assert!(binding.abort_requested(), "早到的中止意图必须被记住");
        assert!(!binding.is_attached());
        // 真正的「attach 后补发中止」与「第二次 attach 被拒」需要真实 SessionHandle，
        // 由 tests/session_abort.rs 用真实 SecureIo + h2 会话端到端断言。
    }

    /// 句柄未就位时事件既不排队也不重放：立即返回 `NotAttached`，
    /// 由调用方计入跳过（端到端的「已 attach 才发得出」见 tests/task_result_event.rs）。
    #[tokio::test]
    async fn unattached_binding_refuses_events_without_queueing() {
        let binding = SessionBinding::new();
        let event = PendingEvent {
            stream: BUSINESS_STREAM,
            action: TASK_RESULT_EVENT_ACTION.to_string(),
            payload: vec![1, 2, 3],
        };
        assert_eq!(
            binding.send_event(&event).await,
            Err(EventDeliveryError::NotAttached)
        );
        assert!(binding.handle().is_none());
    }
}
