//! 内存双流绑定与审批门（纯组件切片）。
//!
//! 本模块用真实 [`BoardAgent`] + 真实 [`JournalStore`] + mpsc 通道在内存内装配 control/data
//! 双流，包含所有者授权启用的业务 codec（[`BusinessCodec`]）；
//! 仍不含真实网络 socket、不含真实加密实现、不含握手实现。
//! 本内存夹具的 [`ApprovalGate`] 默认关闭 `network_approved`，不在此组件内创建网络连接。
//! 生产出站加密通道由 `connect` / `session_handler` / `--connect` 路径单独装配；
//! 此夹具的默认值不是对生产隧道实现或其授权状态的结论。

use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::BusinessPayload;
use tokio::sync::mpsc;

use crate::actions::lookup_action;
use crate::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, RebootExecutor, RebootOs,
    TaskJournalReader,
};
use crate::journal::Fs;
use crate::read_only::{PeerContext, ReadOnlySource};

/// 本内存组件的 codec / network 行为门；不是生产网络连接的全局授权凭证。
///
/// 历史组件基线（2026-10-22，保留默认值以覆盖夹具 fail-closed 行为）：
/// * `codec_approved` 默认 **true**：业务 codec 已按项目所有者 2026-10-22 的授权
///   （原话「直接实现并启用生产 codec」）启用。**这是所有者授权，不是外部专家书面决议**，
///   也不构成对 03 规格「待审阅」状态的批准；跨库固定测试向量、两进程互操作、
///   库选型三类证据仍待补齐。
/// * `network_approved` 默认仍为 **false**：本内存夹具不发起网络 I/O，继续 fail-closed。
///   后续获授权的生产 socket、v3 握手与加密 h2 会话走 `connect` 路径，不依赖本夹具开门。
///
/// `evidence_path` 指向所有者授权记录；它只留档「谁在什么范围内授权」这一事实，
/// 隧道安全协议的外部专家决议仍留在
/// `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md`，二者性质不同。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalGate {
    pub codec_approved: bool,
    pub network_approved: bool,
    pub evidence_path: &'static str,
}

impl Default for ApprovalGate {
    fn default() -> Self {
        Self {
            codec_approved: true,
            network_approved: false,
            evidence_path: "docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md",
        }
    }
}

impl ApprovalGate {
    /// 测试中显式构造；默认值为 codec 已授权启用、network 仍未批准，
    /// 需要验证 codec 门关闭的 fail-closed 行为时用本构造函数显式传 `false`。
    pub fn new(codec_approved: bool, network_approved: bool, evidence_path: &'static str) -> Self {
        Self {
            codec_approved,
            network_approved,
            evidence_path,
        }
    }

    /// codec 门关闭（`codec_approved == false`）时：任何原始字节编解码路径都不得进入。
    pub fn require_codec(&self) -> Result<(), StreamError> {
        if self.codec_approved {
            Ok(())
        } else {
            Err(StreamError::CodecApprovalRequired)
        }
    }

    /// 未获批网络 I/O：任何 socket 建立路径都不得进入。
    pub fn require_network_io(&self) -> Result<(), StreamError> {
        if self.network_approved {
            Ok(())
        } else {
            Err(StreamError::NetworkApprovalRequired)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("codec approval gate is closed")]
    CodecApprovalRequired,
    #[error("network io approval gate is closed")]
    NetworkApprovalRequired,
    #[error("data stream cannot open before the control stream")]
    ControlStreamRequired,
    #[error("action is not allowed on this stream")]
    ActionNotAllowedOnStream,
    #[error("response trace id mismatch")]
    ResponseMismatch,
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("agent rejected the request: {0}")]
    AgentRejected(BusinessStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamType {
    Control,
    Data,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    NotOpened,
    Open,
    Closed,
}

/// `TYPE_SYSTEM` 帧动作：仅 Control 流。
const SYSTEM_ACTION_KICK: &str = "tunnel.kick";
const SYSTEM_ACTION_REVOKE: &str = "tunnel.revoke";

/// 出站通道容量：本切片不消费，仅维持 channel 生命周期。
const OUTBOUND_CAPACITY: usize = 32;

/// 内存双流绑定：用真实 [`BoardAgent`] + 真实 [`JournalStore`] + mpsc 通道装配，
/// 不含任何真实网络 socket、不含任何加密实现。
pub struct DualStreamBinding<R, J, E, O, F>
where
    R: ReadOnlySource,
    J: TaskJournalReader,
    E: JournalEpochSource,
    O: RebootOs,
    F: Fs,
{
    agent: BoardAgent<R, J, E>,
    executor: RebootExecutor<O, F>,
    control: StreamState,
    data: StreamState,
    // 获批网络切片前保留该接收端以维持 channel 生命周期；当前不声称已有生产消费方。
    _outbound: mpsc::Receiver<BusinessPayload>,
    gate: ApprovalGate,
}

impl<R, J, E, O, F> DualStreamBinding<R, J, E, O, F>
where
    R: ReadOnlySource,
    J: TaskJournalReader,
    E: JournalEpochSource,
    O: RebootOs,
    F: Fs,
{
    pub fn new(
        agent: BoardAgent<R, J, E>,
        executor: RebootExecutor<O, F>,
        gate: ApprovalGate,
    ) -> (Self, mpsc::Sender<BusinessPayload>) {
        let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let binding = Self {
            agent,
            executor,
            control: StreamState::NotOpened,
            data: StreamState::NotOpened,
            _outbound: rx,
            gate,
        };
        (binding, tx)
    }

    /// 必须先于任何 Data 操作与业务请求调用（03 §1 / 现行规范 §6.6 顺序契约）。
    pub fn open_control(&mut self) -> Result<(), StreamError> {
        self.control = StreamState::Open;
        Ok(())
    }

    pub fn close_control(&mut self) {
        self.control = StreamState::Closed;
    }

    /// 未先开 Control 时返回 [`StreamError::ControlStreamRequired`]，绝不静默放行。
    pub fn open_data(&mut self) -> Result<(), StreamError> {
        if self.control != StreamState::Open {
            // 绝不静默放行：不允许 Data-first 的乱序开流
            return Err(StreamError::ControlStreamRequired);
        }
        self.data = StreamState::Open;
        Ok(())
    }

    pub fn stream_state(&self, stream: StreamType) -> StreamState {
        match stream {
            StreamType::Control => self.control,
            StreamType::Data => self.data,
        }
    }

    pub fn gate(&self) -> &ApprovalGate {
        &self.gate
    }

    pub fn os(&self) -> &O {
        self.executor.os()
    }

    /// 私有统一流状态判定函数：typed 与 wire 双入口共用，避免守卫分叉。
    fn ensure_business_stream(&self, stream: StreamType, action: &str) -> Result<(), StreamError> {
        // 守卫 1: Control 必须处于 Open 状态（NotOpened 或 Closed 均拒绝），
        // 拒绝覆盖 source/journal/业务 action 的全部访问，且不触碰下层。
        if self.control != StreamState::Open {
            return Err(StreamError::ControlStreamRequired);
        }
        // 守卫 2: 规格 03 §1 业务请求/响应均在 Control（OpenControl）流；
        // Data 流只参与传输层应用探测。业务目录内 action 若落在 Data 流直接拒绝。
        if stream == StreamType::Data && lookup_action(action).is_some() {
            return Err(StreamError::ActionNotAllowedOnStream);
        }
        Ok(())
    }

    /// 统一守卫：Control 未开流（NotOpened/Closed）恒返回 [`StreamError::ControlStreamRequired`]。
    pub fn handle_typed(
        &self,
        stream: StreamType,
        peer: &PeerContext,
        action: &str,
        payload: &BusinessPayload,
    ) -> Result<BusinessResponse, StreamError> {
        self.ensure_business_stream(stream, action)?;
        self.agent
            .handle_typed_action(peer, action, payload)
            .map_err(StreamError::AgentRejected)
    }

    /// 统一守卫：与 [`Self::handle_typed`] 共享判定函数。
    ///
    /// 判定顺序固定为 gate → stream → agent，不得劣化：
    /// 1. [`ApprovalGate::require_codec`]：codec 门关闭时恒返回 [`StreamError::CodecApprovalRequired`]，
    ///    在触碰流状态与任何下层之前失败关闭；
    /// 2. [`Self::ensure_business_stream`]：Control 未开流与 Data 流业务 action 仍先行拒绝；
    /// 3. [`BoardAgent::handle_wire`]：内部仍先查动作白名单与期望载荷类型，再解码。
    ///
    /// 本入口使用所有者授权启用的 [`BusinessCodec`]（授权性质见
    /// [`rsetup_protocol::wire::approved_codec_witness`]）；本模块不引入任何新的 codec 实现，
    /// 也未因此放开网络/加密/握手门禁。
    pub fn handle_wire(
        &self,
        stream: StreamType,
        peer: &PeerContext,
        action: &str,
        bytes: &[u8],
    ) -> Result<BusinessResponse, StreamError> {
        self.gate.require_codec()?;
        self.ensure_business_stream(stream, action)?;
        self.agent
            .handle_wire(&BusinessCodec, peer, action, bytes)
            .map_err(StreamError::AgentRejected)
    }

    /// `TYPE_SYSTEM`（`tunnel.kick` / `tunnel.revoke`）只允许在 Control 流。
    pub fn handle_system(&self, stream: StreamType, action: &str) -> Result<(), StreamError> {
        // 系统帧只属于 Control 流：Data 流与 Control 未开（NotOpened/Closed）一律拒绝。
        if stream != StreamType::Control || self.control != StreamState::Open {
            return Err(StreamError::ControlStreamRequired);
        }
        match action {
            SYSTEM_ACTION_KICK | SYSTEM_ACTION_REVOKE => Ok(()),
            _ => Err(StreamError::ActionNotAllowedOnStream),
        }
    }

    /// 真实网络入口：在审批门关闭时恒返回 [`StreamError::NetworkApprovalRequired`]，绝不建立连接。
    pub async fn bind_real_network(&self, _addr: &str) -> Result<(), StreamError> {
        self.gate.require_network_io()?;
        // 即便门被打开，本计划也不实现真实监听/握手；交由获批的传输计划实现。
        Err(StreamError::NetworkApprovalRequired)
    }

    /// 板端主动事件（`device.task.result`）：必须先在 Control 流打开后发送。
    pub fn emit_task_result(
        &self,
        record: &crate::journal::TaskRecord,
    ) -> Result<BusinessPayload, StreamError> {
        if self.control != StreamState::Open {
            return Err(StreamError::ControlStreamRequired);
        }
        Ok(BusinessPayload::TaskRecord(record.to_wire()))
    }

    /// 与生产补发路径**同一形状**的事件帧：Control 未开流一律拒绝，载荷编码复用
    /// [`crate::session_handler::task_result_event`]（与业务回包同一条编码路径）。
    pub fn emit_task_result_event(
        &self,
        record: &crate::journal::TaskRecord,
    ) -> Result<crate::session_handler::PendingEvent, StreamError> {
        // 先过未开流的门（与 emit_task_result 完全一致），再编码事件帧。
        self.emit_task_result(record)?;
        crate::session_handler::task_result_event(&record.to_wire())
            .map_err(|status| StreamError::Serialization(status.to_string()))
    }
}
