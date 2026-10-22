//! 类型化分发：白名单查表 → DTO 结构校验 → 触达 source/journal，顺序不可调换。
//!
//! 本模块同时承载 Reboot 执行调度（[`RebootExecutor`]）与固定无参 OS 重启边界
//! （[`RebootOs`]）：`device.reboot.prepare` 走票据生成，`device.reboot.execute`
//! 走「载荷哈希去重 → fresh boot 校验 → 一次性票据消费 → accepted/attempted 可靠落盘
//! → 至多一次 OS 调用」的固定顺序。

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::actions::{ActionKind, lookup_action};
use crate::journal::{DedupOutcome, Fs, JournalError, JournalStore, TaskRecord, TaskState};
use crate::read_only::{PeerContext, ReadOnlySource};
use crate::tickets::{TicketBinding, TicketManager, TicketOrigin, TicketToken};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, BusinessPayloadKind, Codec,
    CodecError, DtoError, RebootExecute, RebootPrepare, RebootTicket, approved_codec_witness,
    canonical_reboot_payload_hash, decode_business, is_canonical_uuid_v4,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BusinessStatus {
    #[error("ok")]
    Success = 0,
    #[error("unsupported version")]
    UnsupportedVersion = 1001,
    #[error("unsupported action")]
    UnsupportedAction = 1002,
    #[error("invalid argument")]
    InvalidArgument = 1003,
    #[error("capability unavailable")]
    CapabilityUnavailable = 1004,
    #[error("device busy")]
    DeviceBusy = 1005,
    #[error("task not found")]
    TaskNotFound = 1006,
    #[error("task conflict")]
    TaskConflict = 1007,
    #[error("resource exhausted")]
    ResourceExhausted = 1008,
    #[error("internal error")]
    InternalError = 1009,
    #[error("execution token invalid")]
    ExecutionTokenInvalid = 1010,
}

/// codec 错误 → 业务 `status_code` 的唯一映射。
///
/// **穷尽匹配（无 `_` 兜底）**：`CodecError` 现有 10 个变体全部逐条列出，
/// 协议层新增变体时本函数必须编译失败并强制归类，不得静默落入 1003。
///
/// 三条界线按 03 §1 状态码表的稳定语义划分，不得互相冒充：
///
/// * **版本可协商** → 1001 [`BusinessStatus::UnsupportedVersion`]：
///   [`CodecError::UnsupportedSchemaVersion`] 表示对端声明了本部署不支持的
///   `schema_version`，中控据此触发版本/能力协商；
/// * **资源超限** → 1008 [`BusinessStatus::ResourceExhausted`]：
///   [`CodecError::LengthLimit`] 表示消息（含嵌套子消息）字节数越过 512 KiB
///   整包上限，与「这条请求的字段写错了」不是一回事；
/// * **部署 / 门禁不可用** → 1009 [`BusinessStatus::InternalError`]：
///   [`CodecError::CodecUnapproved`] **只**表示「本次没有可用的获批 codec 见证」
///   （`witness: None`，或传入 [`rsetup_protocol::wire::NoCodec`]），是本端部署
///   故障，**不是**对端版本问题。它不再冒充 1001：那会让中控误以为「换个版本
///   重试」就能通过，而真实原因是本端没有获批 codec，重试多少次结果都一样。
///   03 §1 允许该类内部故障落在 1009 或 1004，本仓取 1009：1004
///   [`BusinessStatus::CapabilityUnavailable`] 在本板端专指「该动作的执行能力未接线」
///   （如未注入 reboot 调度器），而 codec 门禁是全部分发动作的公共前置，不是单个动作的能力缺失。
///   该路径的 fail-closed 语义完全不变——见证守卫在触碰 `bytes` 与任何
///   source/journal/OS 之前失败关闭，也绝不自造编码器；
/// * **这条请求自身的编码缺陷** → 1003 [`BusinessStatus::InvalidArgument`]：
///   `Malformed` / `UnknownField` / `DuplicateField` / `MissingField` /
///   `InvalidUtf8` / `InvalidDto`，以及 [`CodecError::TooManyMetrics`]
///   （`metrics` 条目数超 schema 契约上限 `MAX_METRICS`：属集合规模的入参非法，
///   与 512 KiB 的**整包长度**上限分属两个约束，因此不归 1008）。
///
/// 所有分支都绝不回显 payload 内容与 codec 错误细节。
fn codec_error_status(error: CodecError) -> BusinessStatus {
    match error {
        CodecError::UnsupportedSchemaVersion(_) => BusinessStatus::UnsupportedVersion,
        CodecError::LengthLimit => BusinessStatus::ResourceExhausted,
        CodecError::CodecUnapproved(_) => BusinessStatus::InternalError,
        CodecError::TooManyMetrics
        | CodecError::Malformed(_)
        | CodecError::UnknownField(_)
        | CodecError::DuplicateField(_)
        | CodecError::MissingField(_)
        | CodecError::InvalidUtf8
        | CodecError::InvalidDto(_) => BusinessStatus::InvalidArgument,
    }
}

pub trait TaskJournalReader: Send + Sync {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &rsetup_protocol::wire::TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus>;
}

pub trait JournalEpochSource: Send + Sync {
    fn journal_epoch(&self) -> String;
}

/// 结构校验成功后的类型化响应；原始 wire 入口只有在 codec 解码成功后才可能产出它。
#[derive(Clone, Debug, PartialEq)]
pub enum BusinessResponse {
    Capabilities(rsetup_protocol::wire::Capabilities),
    DeviceStatus(rsetup_protocol::wire::DeviceStatus),
    ClockSample(rsetup_protocol::wire::ClockSample),
    TaskRecord(rsetup_protocol::wire::TaskRecord),
    /// `device.reboot.prepare` 成功签发的 30s 单调票据。
    RebootTicket(rsetup_protocol::wire::RebootTicket),
    /// 断流、无业务回包：执行结果不可知，交由后续 `device.task.get` 核实。
    /// 既不是成功也不是失败，绝不携带任何 `TaskRecord` 载荷。
    Indeterminate,
}

/// 已绑定连接代际的 Reboot 调度入口（对象安全缝，便于 `BoardAgent` 注入替身）。
///
/// `connection_id` 由传输层在每条 control 连接建立时给出：票据绑定连接代际，
/// 新连接必须换 `connection_id` 并重新注入，旧票据随之失效。
pub trait RebootDispatcher: Send + Sync {
    fn prepare(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootPrepare,
        now: Instant,
    ) -> Result<RebootTicket, BusinessStatus>;
    /// 实现方负责用 [`canonical_reboot_payload_hash`] 计算规范载荷哈希（producer 唯一入口），
    /// 绝不接受调用方自备的哈希。
    fn execute(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootExecute,
        now: Instant,
    ) -> ExecuteOutcome;
}

/// `BoardAgent` 内的 Reboot 调度绑定：调度器 + 本实例所属连接代际。
struct RebootBinding {
    dispatcher: Arc<dyn RebootDispatcher>,
    connection_id: String,
}

pub struct BoardAgent<R, J, E> {
    read_only: R,
    journal: J,
    epoch: E,
    reboot: Option<RebootBinding>,
}

impl<R: ReadOnlySource, J: TaskJournalReader, E: JournalEpochSource> BoardAgent<R, J, E> {
    pub fn new(read_only: R, journal: J, epoch: E) -> Self {
        Self {
            read_only,
            journal,
            epoch,
            reboot: None,
        }
    }
    /// 注入本实例所属连接代际的 Reboot 调度器；未注入时两个 reboot 动作恒
    /// `CapabilityUnavailable`（fail-closed，绝不假装成功）。
    pub fn with_reboot(
        mut self,
        dispatcher: Arc<dyn RebootDispatcher>,
        connection_id: impl Into<String>,
    ) -> Self {
        self.reboot = Some(RebootBinding {
            dispatcher,
            connection_id: connection_id.into(),
        });
        self
    }
    pub fn source(&self) -> &R {
        &self.read_only
    }
    pub fn journal_epoch(&self) -> String {
        self.epoch.journal_epoch()
    }

    /// 两个 reboot 动作的统一前置：未注入调度器即 `CapabilityUnavailable`。
    fn reboot_binding(&self) -> Result<&RebootBinding, BusinessStatus> {
        self.reboot
            .as_ref()
            .ok_or(BusinessStatus::CapabilityUnavailable)
    }

    /// 唯一类型化入口：`action` 必须由调用方显式给出，顺序恒为
    /// 「白名单查表 → DTO 结构校验 → 触达 source/journal」。三个只读查询的 DTO 无法自证
    /// action，因此绝不允许由 DTO 变体反推 action。
    pub fn handle_typed_action(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &BusinessPayload,
    ) -> Result<BusinessResponse, BusinessStatus> {
        // 1) 先做动作判定：不在封闭表内的动作（含 device.task.result）在触碰任何 source/journal 之前被拒绝
        let spec = lookup_action(action).ok_or(BusinessStatus::UnsupportedAction)?;
        // 2) 结构校验：失败即拒绝，且未触达 I/O。错误按语义分流：
        //    仅 DtoError::UnsupportedVersion → UnsupportedVersion（1001，可协商的版本错误）；
        //    其余结构/字段/枚举/长度类校验失败（含空白 sub_task_id）→ InvalidArgument（1003）。
        //    该分流必须与 [`codec_error_status`] 对 `UnsupportedSchemaVersion` 的归类一致，
        //    否则同一条版本违规在 typed / wire 两条入口上会得到不同 status_code。
        payload.validate().map_err(|err| match err {
            DtoError::UnsupportedVersion(_) => BusinessStatus::UnsupportedVersion,
            _ => BusinessStatus::InvalidArgument,
        })?;

        match (spec.kind, payload) {
            // 只读动作接受三类请求载荷：Query / DeviceStatus / ClockSample（均已经过上方 payload.validate()）；
            // 载荷 kind 只决定校验面，响应恒按 action 路由由 source 提供，绝不按 DTO 变体反推 action
            (
                ActionKind::ReadOnly,
                BusinessPayload::Query(_)
                | BusinessPayload::DeviceStatus(_)
                | BusinessPayload::ClockSample(_),
            ) => match action {
                // TR-R8-2：出站响应返回前必须先执行结构校验，非法 producer DTO 绝不流出；
                // 校验失败固定映射 InternalError，本分支保持 OS 调用与持久写为 0。
                "device.capabilities.get" => {
                    let caps = self.read_only.capabilities();
                    caps.validate().map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::Capabilities(caps))
                }
                "device.status.get" => {
                    let status = self.read_only.status();
                    status
                        .validate()
                        .map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::DeviceStatus(status))
                }
                "device.clock.get" => {
                    let clock = self.read_only.clock();
                    clock
                        .validate()
                        .map_err(|_| BusinessStatus::InternalError)?;
                    Ok(BusinessResponse::ClockSample(clock))
                }
                _ => Err(BusinessStatus::InvalidArgument),
            },
            (ActionKind::ReadOnly, _) => Err(BusinessStatus::InvalidArgument),
            (ActionKind::JournalRead, BusinessPayload::TaskQuery(query)) => {
                if query.sub_task_id.trim().is_empty() {
                    return Err(BusinessStatus::InvalidArgument);
                }
                match self
                    .journal
                    .query(&peer.authenticated_controller_key, query)?
                {
                    Some(record) => {
                        // TR-R8-2：journal query 返回的 record 出站前同样执行结构校验，非法记录绝不流出。
                        record
                            .validate()
                            .map_err(|_| BusinessStatus::InternalError)?;
                        Ok(BusinessResponse::TaskRecord(record))
                    }
                    None => Err(BusinessStatus::TaskNotFound),
                }
            }
            (ActionKind::JournalRead, _) => Err(BusinessStatus::InvalidArgument),
            // prepare：结构校验通过后才触达调度器签发票据；未注入调度器 → CapabilityUnavailable。
            // 出站票据同样先做结构校验，非法 producer DTO 绝不流出。
            (ActionKind::RebootPrepare, BusinessPayload::RebootPrepare(request)) => {
                let binding = self.reboot_binding()?;
                let ticket = binding.dispatcher.prepare(
                    &peer.authenticated_controller_key,
                    &binding.connection_id,
                    request,
                    Instant::now(),
                )?;
                ticket
                    .validate()
                    .map_err(|_| BusinessStatus::InternalError)?;
                Ok(BusinessResponse::RebootTicket(ticket))
            }
            // execute：载荷哈希由本层用 canonical_reboot_payload_hash 现算（不接受调用方自备哈希）；
            // 三个持久态（Attempted/Accepted/OsErrorPersisted）一律 status=0 + 持久 TaskRecord，
            // Rejected → 非零 status，Indeterminate → 无业务回包。
            (ActionKind::RebootExecute, BusinessPayload::RebootExecute(request)) => {
                let binding = self.reboot_binding()?;
                let outcome = binding.dispatcher.execute(
                    &peer.authenticated_controller_key,
                    &binding.connection_id,
                    request,
                    Instant::now(),
                );
                match outcome {
                    ExecuteOutcome::Rejected(status) => Err(status),
                    ExecuteOutcome::Indeterminate => Ok(BusinessResponse::Indeterminate),
                    ExecuteOutcome::Attempted(record)
                    | ExecuteOutcome::Accepted(record)
                    | ExecuteOutcome::OsErrorPersisted(record) => {
                        let record = record.to_wire();
                        record
                            .validate()
                            .map_err(|_| BusinessStatus::InternalError)?;
                        Ok(BusinessResponse::TaskRecord(record))
                    }
                }
            }
            (ActionKind::RebootPrepare, _) | (ActionKind::RebootExecute, _) => {
                Err(BusinessStatus::InvalidArgument)
            }
        }
    }

    /// 原始字节入口：先查白名单与期望载荷类型，再解码，最后汇入同一条类型化分发。
    ///
    /// 白名单与 expected kind 的判定顺序不变：未授权动作、动作与载荷类型不匹配都在
    /// 触碰 codec 与任何 I/O 之前被拒绝。
    pub fn handle_wire(
        &self,
        codec: &dyn Codec,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus> {
        // 1) 白名单先行：未授权动作不消耗 codec，也不触碰任何 I/O
        let spec = lookup_action(action).ok_or(BusinessStatus::UnsupportedAction)?;
        // 2) 按 action 精确给出期望载荷类型；板端事件不在入站白名单
        let expected = match (spec.kind, action) {
            (ActionKind::ReadOnly, "device.capabilities.get") => BusinessPayloadKind::Query,
            (ActionKind::ReadOnly, "device.status.get") => BusinessPayloadKind::Query,
            (ActionKind::ReadOnly, "device.clock.get") => BusinessPayloadKind::Query,
            (ActionKind::JournalRead, "device.task.get") => BusinessPayloadKind::TaskQuery,
            (ActionKind::RebootPrepare, "device.reboot.prepare") => {
                BusinessPayloadKind::RebootPrepare
            }
            (ActionKind::RebootExecute, "device.reboot.execute") => {
                BusinessPayloadKind::RebootExecute
            }
            _ => return Err(BusinessStatus::InvalidArgument),
        };
        // 3) 业务 codec 已按项目所有者 2026-10-22 的授权启用（见 `approved_codec_witness`
        //    的授权性质说明：不是外部专家决议）。见证在本函数栈上取得，
        //    不引入任何全局可变状态；传入 `NoCodec` 等未获批实现时
        //    `CodecUnapproved` 仍按原语义在触碰 bytes 与任何 I/O 之前失败关闭，
        //    绝不自造编码器——它是本端部署故障，按 1009 InternalError 回包（见
        //    [`codec_error_status`]），不再冒充 1001 版本不匹配。
        let witness = approved_codec_witness();
        // 4) 解码错误按语义分流（见 [`codec_error_status`]）；所有路径都不回显 payload。
        let decoded = decode_business(codec, Some(&witness), expected, payload)
            .map_err(codec_error_status)?;
        self.handle_typed_action(peer, action, &decoded)
    }
}

// ---------------------------------------------------------------------------
// 固定无参 OS 重启边界
// ---------------------------------------------------------------------------

/// 固定无参重启边界：绝不接受 command / path / argv，杜绝任何命令注入面。
pub trait RebootOs: Send + Sync {
    fn reboot(&self) -> Result<(), std::io::Error>;
}

/// 生产实现：本切片绝不实现真实主机重启，`reboot()` 恒 `Unsupported`。
///
/// `call_count()` 的语义是「真实主机重启入口被实际触发的次数」：本实现永不触达该入口，
/// 因此自动化测试中恒为 0。它与 [`RecordingOs`] 的 fake 计数（证明 OS 边界被进入，可为 1）
/// 严格区分、互为证据，不得互相冒充。
pub struct HostRebootOs {
    calls: AtomicUsize,
}

impl HostRebootOs {
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Default for HostRebootOs {
    fn default() -> Self {
        Self::new()
    }
}

impl RebootOs for HostRebootOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        // 纯组件安全切片：本计划中绝不在此调用系统重启。
        // 即便被误调用也只返回明确错误，绝不会重启开发机；真实主机重启入口未被触达，
        // 因此 `call_count()` 在自动化测试中恒为 0。
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "real host reboot is not implemented in this component slice (unsupported)",
        ))
    }
}

/// 测试替身：只记录调用次数，绝不触发真实重启。
pub struct RecordingOs {
    calls: AtomicUsize,
}

impl RecordingOs {
    pub fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Default for RecordingOs {
    fn default() -> Self {
        Self::new()
    }
}

impl RebootOs for RecordingOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reboot 执行调度
// ---------------------------------------------------------------------------

/// `device.reboot.execute` 的执行结果；与业务 `status_code` 的映射关系（必须写进调用方注释）：
///
/// - `Attempted(record)` → `status_code = 0` + 持久 `TaskRecord`：**不代表 OS 调用成功**，
///   更不代表任务成功（03 §5：`attempted ≠ 成功`，真实重启证据需认证持久日志 / boot transition）；
/// - `Accepted(record)` → `status_code = 0` + 持久 `Accepted` 记录：accepted 已可靠落盘而
///   attempted 确未提交，绝不调用 OS，也绝不能用非零错误暗示「未接收」；
/// - `OsErrorPersisted(record)` → `status_code = 0` + 持久 `Failed(OS_ERROR)` 记录；
/// - `Rejected(status)` → 非零 `status_code`、payload 为空：仅允许在确证 accepted 未提交、
///   请求未被接受且尚未进入 OS 调用边界时；
/// - `Indeterminate` → **无业务回包**（断流），留待 `device.task.get` 核查。
#[derive(Clone, Debug, PartialEq)]
pub enum ExecuteOutcome {
    Attempted(TaskRecord),
    Accepted(TaskRecord),
    OsErrorPersisted(TaskRecord),
    Rejected(BusinessStatus),
    Indeterminate,
}

/// Reboot 执行调度器：票据、journal 与固定无参 OS 边界的唯一接线点。
///
/// 设备级占用状态机（TR-N5 / 正审 R-05 / 复审 fix-2）：同 agent 实例单设备串行互斥
/// （1005 DeviceBusy）。「是否占用、当前占用者集合、占用代际」合并为**同一个互斥锁**
/// （[`DeviceOccupationState`]）保护的状态机：占用标记与占用者身份之间不存在任何中间窗口，
/// 释放与交接必须在锁内以代际 CAS 复核「仍是我复核过的那份占用」后才生效。
///
/// 占用覆盖未决重启全生命周期：fresh execute 开始时申请；当且仅当确定未执行的拒绝
/// （DefinitelyNotCommitted / 票据无效 / boot 不匹配）或写入终态（Failed）才释放；
/// OS Ok 返回未决 Attempted 中间态时保持占用。
///
/// 恢复规则（TR-R6-3 / 复审 fix-2）：实例重启后经 [`RebootExecutor::new_recovering`] 重建时
/// 扫描持久 records，若存在任何 `state == Attempted` 且 `pre_boot_id == 当前 boot`（重启已发起
/// 但 boot transition 未观察到）的记录，恢复占用并把**全部**未决记录记为占用者集合（不得只追踪
/// 第一条）；全部为终态或已观察到 boot transition 则为空闲；扫描失败按占用 fail-closed（占用者
/// 未知，见 [`OccupationState::Unknown`]）。
///
/// 占用解除复核（正审 fix-1 / 复审 fix-2）：新任务被占用阻断前，复核占用集合中**每一条**任务
/// 在持久日志中是否都已进入终态（`Failed` / `Succeeded`），或设备 boot 是否已变化（boot
/// transition）；成立才以代际 CAS 把占用原子交接给新任务。复核结论跨锁失效（代际被并发改写）
/// 时一律放弃，绝不依据过期的终态结论清除并发线程刚取得的新占用。
pub struct RebootExecutor<O: RebootOs, F: Fs> {
    journal: Arc<JournalStore<F>>,
    os: O,
    tickets: TicketManager,
    boot_id: Box<dyn Fn() -> String + Send + Sync>,
    agent_instance_id: String,
    /// 设备占用状态机：`generation` 每次变更递增，释放 / 交接必须携带复核时看到的代际做 CAS。
    occupation: Mutex<DeviceOccupationState>,
}

/// 设备占用状态机：占用标记、占用者身份与代际号必须一起变更，因此整体置于同一互斥锁内。
#[derive(Clone, Debug)]
struct DeviceOccupationState {
    /// 占用代际：每次占用状态变更（占用 / 释放 / 交接）递增，作为 CAS 见证。
    generation: u64,
    state: OccupationState,
}

/// 占用状态：空闲 / 被具体任务集合占用 / 占用状态未知（恢复扫描失败 fail-closed）。
#[derive(Clone, Debug)]
enum OccupationState {
    /// 设备空闲，可被新任务原子占用。
    Free,
    /// 被 [`Occupation`] 记录的全部任务占用。
    Occupied(Occupation),
    /// 占用状态未知（恢复扫描失败 fail-closed）：与 `Occupied` 同样排他，但无占用者身份可
    /// 逐条复核，解除时回退到再次扫描持久日志。
    Unknown,
}

/// 占用者身份见证：持有占用的**全部**任务（恢复场景可能同时存在多条同 boot 未决
/// `Attempted`），外加占用发生时的 `boot_id`，用于复核占用是否已失效。
#[derive(Clone, Debug)]
struct Occupation {
    boot_id: String,
    tasks: Vec<OccupiedTask>,
}

/// 单个占用任务的身份：与 journal 去重键一致（`controller_key` + `sub_task_id`）。
#[derive(Clone, Debug)]
struct OccupiedTask {
    controller_key: [u8; 32],
    sub_task_id: String,
}

impl DeviceOccupationState {
    fn new(state: OccupationState) -> Self {
        Self {
            generation: 0,
            state,
        }
    }
}

/// fresh execute 占用身份：单个任务、当前 boot。
fn fresh_occupation(controller_key: &[u8; 32], sub_task_id: &str, boot_id: &str) -> Occupation {
    Occupation {
        boot_id: boot_id.to_string(),
        tasks: vec![OccupiedTask {
            controller_key: *controller_key,
            sub_task_id: sub_task_id.to_string(),
        }],
    }
}

impl<O: RebootOs, F: Fs> RebootExecutor<O, F> {
    /// fresh journal 构造：占用状态机初始化为 `Free`（无未决任务场景）。
    pub fn new(
        journal: Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
    ) -> Self {
        Self::build(
            journal,
            os,
            tickets,
            boot_id,
            agent_instance_id,
            OccupationState::Free,
        )
    }

    /// TR-R6-3 恢复构造器：按恢复规则初始化占用状态机（扫描失败按占用 fail-closed）。
    ///
    /// 扫描持久 records：若存在 `state == Attempted` 且 `pre_boot_id == 当前 boot` 的未决任务，
    /// 则恢复占用，并把**全部**未决任务记为占用者集合（复审 fix-2：不得只追踪第一条，否则
    /// 其中一条进入终态就会误解除仍被其他未决任务持有的占用）；全部为终态或已观察到 boot
    /// transition 则为空闲。扫描出错（含 uncertain 隔离态 / 毒锁 / 解码失败）fail-closed 视为
    /// 占用且占用者未知（`Unknown`，复核时回退到再次扫描）。
    pub fn new_recovering(
        journal: Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
    ) -> Self {
        let current_boot = (boot_id)();
        let initial = match journal.scan_pending_attempted(&current_boot) {
            Ok(pending) if pending.is_empty() => OccupationState::Free,
            Ok(pending) => OccupationState::Occupied(Occupation {
                boot_id: current_boot,
                tasks: pending
                    .iter()
                    .map(|(controller_key, sub_task_id)| OccupiedTask {
                        controller_key: *controller_key,
                        sub_task_id: sub_task_id.clone(),
                    })
                    .collect(),
            }),
            // 扫描失败 fail-closed：视为占用，占用者未知（复核时回退到再次扫描）
            Err(_) => OccupationState::Unknown,
        };
        Self::build(journal, os, tickets, boot_id, agent_instance_id, initial)
    }

    fn build(
        journal: Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
        initial: OccupationState,
    ) -> Self {
        Self {
            journal,
            os,
            tickets,
            boot_id,
            agent_instance_id: agent_instance_id.into(),
            occupation: Mutex::new(DeviceOccupationState::new(initial)),
        }
    }

    pub fn os(&self) -> &O {
        &self.os
    }

    pub fn tickets(&self) -> &TicketManager {
        &self.tickets
    }

    /// 设备级占用见证（未决重启生命周期）；锁中毒按占用 fail-closed。
    pub fn device_occupied(&self) -> bool {
        match self.occupation.lock() {
            Ok(guard) => !matches!(guard.state, OccupationState::Free),
            Err(_) => true,
        }
    }

    /// 当前占用者集合中首个 `sub_task_id`（测试见证：空闲或占用者未知时返回 `None`）。
    pub fn occupying_sub_task(&self) -> Option<String> {
        self.occupying_sub_tasks().first().cloned()
    }

    /// 当前占用者集合的全部 `sub_task_id`（测试见证：恢复场景可能有多条同 boot 未决记录）。
    pub fn occupying_sub_tasks(&self) -> Vec<String> {
        match self.occupation.lock() {
            Ok(guard) => match &guard.state {
                OccupationState::Occupied(occupation) => occupation
                    .tasks
                    .iter()
                    .map(|task| task.sub_task_id.clone())
                    .collect(),
                _ => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    /// 空闲时原子占用（同一临界区内写入占用者与代际），返回本次占用的代际；已占用返回 `None`。
    fn try_acquire(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        boot_id: &str,
    ) -> Option<u64> {
        let mut guard = self.occupation.lock().ok()?;
        if !matches!(guard.state, OccupationState::Free) {
            return None;
        }
        guard.generation = guard.generation.wrapping_add(1);
        guard.state =
            OccupationState::Occupied(fresh_occupation(controller_key, sub_task_id, boot_id));
        Some(guard.generation)
    }

    /// 代际 CAS 释放：仅当占用状态自 `expected_generation` 起**未被任何线程改写**时才释放。
    /// 返回 false 表示占用已被并发线程交接 / 改写，本次绝不清除他人的新占用。
    fn try_release(&self, expected_generation: u64) -> bool {
        let Ok(mut guard) = self.occupation.lock() else {
            return false;
        };
        if guard.generation != expected_generation {
            return false;
        }
        guard.generation = guard.generation.wrapping_add(1);
        guard.state = OccupationState::Free;
        true
    }

    /// 代际 CAS 交接：仅当代际仍为复核时看到的 `expected_generation` 时，把占用原子换给新任务，
    /// 返回交接后的新代际；代际已变（并发线程已交接）则放弃并返回 `None`。
    fn try_handover(
        &self,
        expected_generation: u64,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        boot_id: &str,
    ) -> Option<u64> {
        let Ok(mut guard) = self.occupation.lock() else {
            return None;
        };
        if guard.generation != expected_generation {
            return None;
        }
        guard.generation = guard.generation.wrapping_add(1);
        guard.state =
            OccupationState::Occupied(fresh_occupation(controller_key, sub_task_id, boot_id));
        Some(guard.generation)
    }

    /// 占用失效复核 + 原子交接（正审 fix-1 / 复审 fix-2）。
    ///
    /// 次序恒为：锁内快照（占用状态 + 代际）→ 锁外持久日志复核（不持锁做 I/O）→ 代际 CAS 交接。
    /// CAS 失败说明复核结论已过期（并发线程已完成交接），本次一律放弃并返回 `None`，
    /// 严禁任何跨锁后无条件释放的行为。
    fn try_handover_stale_occupation(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        current_boot: &str,
    ) -> Option<u64> {
        let snapshot = match self.occupation.lock() {
            Ok(guard) => (*guard).clone(),
            Err(_) => return None,
        };
        if !self.occupation_is_stale(&snapshot.state, current_boot) {
            return None;
        }
        self.try_handover(
            snapshot.generation,
            controller_key,
            sub_task_id,
            current_boot,
        )
    }

    /// 占用是否已可安全解除（任一成立即失效）：
    ///
    /// 1. `Occupied`：占用之后设备 boot 已变化（boot transition 终结未决重启）；或占用集合中
    ///    **每一条**任务在持久日志里都已进入终态（`Failed` / `Succeeded`）。只要还有一条未决 /
    ///    缺失 / 读取失败就 fail-closed 保持占用（复审 fix-2：多条未决不得只看第一条）；
    /// 2. `Unknown`：回退到再次扫描持久日志，确认当前 boot 已无未决 `Attempted` 才失效；
    /// 3. `Free`：本就无占用（复核窗口内已被他人释放）。
    fn occupation_is_stale(&self, state: &OccupationState, current_boot: &str) -> bool {
        match state {
            OccupationState::Free => true,
            OccupationState::Unknown => match self.journal.scan_pending_attempted(current_boot) {
                Ok(pending) => pending.is_empty(),
                Err(_) => false,
            },
            OccupationState::Occupied(occupation) => {
                if occupation.boot_id != current_boot {
                    // 占用之后设备 boot 已变化（发生 boot transition）：未决重启已被 boot
                    // 代际更替终结，安全释放
                    return true;
                }
                !occupation.tasks.is_empty()
                    && occupation.tasks.iter().all(|task| {
                        match self.journal.get(&task.controller_key, &task.sub_task_id) {
                            // 同 boot：仅当该条持久记录已进入终态（Failed / Succeeded）才算失效
                            Ok(Some(record)) => record.state.is_terminal(),
                            // 记录缺失或读取失败：无事实证明终态，fail-closed 保持占用
                            _ => false,
                        }
                    })
            }
        }
    }

    /// `device.reboot.prepare`：形状校验 → fresh boot 校验 → 签发票据。
    /// boot 不一致绝不发票据（03 §4），本方法不触达文件系统、不触达 OS。
    pub fn prepare(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootPrepare,
        now: Instant,
    ) -> Result<RebootTicket, BusinessStatus> {
        if request.schema_version != BUSINESS_SCHEMA_VERSION {
            return Err(BusinessStatus::UnsupportedVersion);
        }
        if !is_canonical_uuid_v4(&request.sub_task_id)
            || !is_canonical_uuid_v4(&request.expected_boot_id)
        {
            return Err(BusinessStatus::InvalidArgument);
        }
        if (self.boot_id)() != request.expected_boot_id {
            return Err(BusinessStatus::InvalidArgument);
        }
        let binding = TicketBinding {
            controller_key: *controller_key,
            sub_task_id: request.sub_task_id.clone(),
            expected_boot_id: request.expected_boot_id.clone(),
            connection_id: connection_id.to_string(),
            agent_instance_id: self.agent_instance_id.clone(),
        };
        Ok(self.tickets.prepare(binding, now).1)
    }

    /// `device.reboot.execute`（顺序不可调换）：
    /// 形状校验 → journal dedup → 占用申请 → fresh boot 校验 → 一次性票据消费
    /// → accepted 可靠落盘 → attempted 可靠落盘 → 至多一次 OS 调用。
    ///
    /// `payload_hash` 必须由 [`canonical_reboot_payload_hash`] 产出（规范载荷哈希，
    /// 排除 execution token），本方法绝不接受调用方自备的任意哈希语义。
    pub fn execute(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootExecute,
        payload_hash: &[u8; 32],
        now: Instant,
    ) -> ExecuteOutcome {
        // 0) 形状校验（不触达任何 I/O）：先拒绝不合法请求，合法请求继续走去重。
        if request.schema_version != BUSINESS_SCHEMA_VERSION {
            return ExecuteOutcome::Rejected(BusinessStatus::UnsupportedVersion);
        }
        if !is_canonical_uuid_v4(&request.sub_task_id)
            || !is_canonical_uuid_v4(&request.expected_boot_id)
        {
            return ExecuteOutcome::Rejected(BusinessStatus::InvalidArgument);
        }

        // 1) journal dedup 前置（键 = 已认证 controller_key + sub_task_id）：
        //    已有同 ID 持久记录时直接回已有记录，不要求当前 boot 匹配，也不要求旧票据仍有效
        //    （板端对重复请求健壮 ≠ 中控获准重发：中控每 process 至多一次 execute）。
        match self.journal.get(controller_key, &request.sub_task_id) {
            // dedup get 是 execute 的第一次 I/O：Err → Indeterminate，无业务回包、绝不调用 OS
            Err(_) => return ExecuteOutcome::Indeterminate,
            Ok(Some(existing)) => {
                if existing.pre_boot_id != request.expected_boot_id {
                    return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict);
                }
                // 载荷哈希比对经 `insert_accepted` 的去重语义完成（journal 未导出哈希只读访问器）：
                // 同哈希 → `ReusedExisting` 且零写盘；不同哈希 → `Conflict`。
                // 读失败（uncertain 隔离态 / 毒锁 / 解码失败）保持不确定性 → Indeterminate，
                // 无任何事实证明载荷不同，绝不伪造确定性 TaskConflict。
                match self.journal.insert_accepted(
                    controller_key,
                    &request.sub_task_id,
                    payload_hash,
                    &existing.pre_boot_id,
                ) {
                    Ok((record, _)) => return persisted_outcome(record),
                    Err(JournalError::Conflict) => {
                        return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict);
                    }
                    Err(_) => return ExecuteOutcome::Indeterminate,
                }
            }
            Ok(None) => {}
        }

        // 2) 仅首次（无持久记录）：设备级独占占用 → fresh boot 校验 → 一次性票据消费。
        //    此刻尚未产生任何持久写；bootChanged 之后的合法旧请求已在第 1 步 dedup 返回，
        //    不会到达本校验，不得误判 ExecutionTokenInvalid。
        let current_boot = (self.boot_id)();
        // 占用申请与（必要时）失效复核交接都在占用状态机内以代际 CAS 完成，返回本次占用的代际；
        // 后续所有释放都必须携带该代际，绝不无条件清除并发线程取得的新占用（复审 fix-2）。
        let occupation_generation =
            match self.try_acquire(controller_key, &request.sub_task_id, &current_boot) {
                Some(generation) => generation,
                // 已占用：复核占用是否已安全失效（占用集合全部进入终态，或占用后发生 boot
                // transition）；已失效则原子交接给本任务，否则 1005 DeviceBusy。
                None => {
                    match self.try_handover_stale_occupation(
                        controller_key,
                        &request.sub_task_id,
                        &current_boot,
                    ) {
                        Some(generation) => generation,
                        None => return ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy),
                    }
                }
            };
        if current_boot != request.expected_boot_id {
            // 确定未执行：释放占用
            self.try_release(occupation_generation);
            return ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid);
        }
        if let Err(status) = self.tickets.consume(
            controller_key,
            TicketOrigin {
                connection_id,
                agent_instance_id: &self.agent_instance_id,
            },
            &request.sub_task_id,
            &request.expected_boot_id,
            TicketToken(request.execution_token),
            now,
        ) {
            self.try_release(occupation_generation);
            return ExecuteOutcome::Rejected(status);
        }

        // 3) accepted 必须可靠落盘（pre_boot_id 在此确定）
        let accepted = match self.journal.insert_accepted(
            controller_key,
            &request.sub_task_id,
            payload_hash,
            &current_boot,
        ) {
            Ok((record, DedupOutcome::Inserted)) => record,
            // 竞态窗口：dedup 查询时未见记录、并发请求已先持久化 → 只回已有持久记录，不 mark、无第二 OS
            Ok((record, DedupOutcome::ReusedExisting)) => {
                self.try_release(occupation_generation);
                return persisted_outcome(record);
            }
            // 请求确未被接受且 OS 未进边界：安全返回非零错误
            Err(JournalError::DefinitelyNotCommitted) => {
                self.try_release(occupation_generation);
                return ExecuteOutcome::Rejected(BusinessStatus::InternalError);
            }
            Err(JournalError::Conflict) => {
                self.try_release(occupation_generation);
                return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict);
            }
            Err(JournalError::StorageFull) => {
                self.try_release(occupation_generation);
                return ExecuteOutcome::Rejected(BusinessStatus::ResourceExhausted);
            }
            // insert_accepted 提交结果不明（CommitUnknown，OS 边界前）：断流待查、绝不调用 OS；
            // store 已进入 uncertain 隔离态，保持占用直至实例丢弃
            Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
            Err(_) => {
                self.try_release(occupation_generation);
                return ExecuteOutcome::Indeterminate;
            }
        };

        // 4) attempted 必须可靠落盘，之后才允许进入 OS 边界
        let attempted = match self
            .journal
            .mark_attempted(controller_key, &request.sub_task_id)
        {
            Ok(record) => record,
            Err(JournalError::DefinitelyNotCommitted) => {
                // accepted 已可靠写、attempted 确未提交：绝不调用 OS！
                // 必须以 status=0 回复最后可靠持久的 accepted，不能返回非零错误暗示未接收
                self.try_release(occupation_generation);
                return ExecuteOutcome::Accepted(accepted);
            }
            // mark_attempted 提交结果不明（仍为 OS 边界前）：断流、绝不调用 OS，交由 task.get 核查
            Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
            Err(_) => {
                self.try_release(occupation_generation);
                return ExecuteOutcome::Indeterminate;
            }
        };

        // 5) 至多一次 OS 调用边界（仅 `attempted` 可靠落盘之后）
        match self.os.reboot() {
            // OS Ok 仅表示「重启已发起」：任务仍是未决 Attempted 中间态（attempted ≠ 成功），
            // 占用必须覆盖未决生命周期，此处绝不释放设备占用。
            Ok(()) => ExecuteOutcome::Attempted(attempted),
            Err(_) => match self.journal.finish(
                controller_key,
                &request.sub_task_id,
                TaskState::Failed,
                Some("OS_ERROR"),
            ) {
                // OS 明确报错且 failed/OS_ERROR 可靠落盘：以 status=0 回复真实 failed
                Ok(record) => {
                    self.try_release(occupation_generation);
                    ExecuteOutcome::OsErrorPersisted(record)
                }
                // finish 确未提交或结果不明（OS 边界后）：OS 已调用，不返回旧 attempted、
                // 不伪造新 failed/unknown、不非零暗示未执行 → 断流；未决记录仍排他，绝不释放占用
                Err(_) => ExecuteOutcome::Indeterminate,
            },
        }
    }
}

/// 已可靠落盘的持久记录 → 执行结果：状态只决定回包语义标签，三者都是 status=0 的持久回复。
fn persisted_outcome(record: TaskRecord) -> ExecuteOutcome {
    match record.state {
        TaskState::Accepted => ExecuteOutcome::Accepted(record),
        TaskState::Failed => ExecuteOutcome::OsErrorPersisted(record),
        TaskState::Attempted | TaskState::Succeeded | TaskState::Unknown => {
            ExecuteOutcome::Attempted(record)
        }
    }
}

impl<O: RebootOs, F: Fs> RebootDispatcher for RebootExecutor<O, F> {
    fn prepare(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootPrepare,
        now: Instant,
    ) -> Result<RebootTicket, BusinessStatus> {
        RebootExecutor::prepare(self, controller_key, connection_id, request, now)
    }

    fn execute(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &RebootExecute,
        now: Instant,
    ) -> ExecuteOutcome {
        let payload_hash = canonical_reboot_payload_hash(request);
        RebootExecutor::execute(
            self,
            controller_key,
            connection_id,
            request,
            &payload_hash,
            now,
        )
    }
}

#[cfg(test)]
mod codec_error_mapping_tests {
    //! [`codec_error_status`] 的逐变体锁定：四档归类一旦回退（尤其 1009 重新冒充 1001），
    //! 本模块直接失败。端到端见证见 `tests/wire_codec.rs`；`TooManyMetrics` 因入站白名单
    //! 没有 `expected = DeviceStatus` 的动作而无法经 `handle_wire` 触达，只在此锁定。
    use super::{BusinessStatus, codec_error_status};
    use rsetup_protocol::wire::CodecError;

    #[test]
    fn codec_error_status_maps_every_variant_to_its_class() {
        // 1001：可协商的版本错误
        assert_eq!(
            codec_error_status(CodecError::UnsupportedSchemaVersion(2)),
            BusinessStatus::UnsupportedVersion
        );
        // 1008：整包长度越过 512 KiB
        assert_eq!(
            codec_error_status(CodecError::LengthLimit),
            BusinessStatus::ResourceExhausted
        );
        // 1009：本端部署/门禁不可用，不是对端版本问题
        assert_eq!(
            codec_error_status(CodecError::CodecUnapproved("no approved business codec")),
            BusinessStatus::InternalError
        );
        // 1003：这条请求自身的编码/契约缺陷（含集合条目数超限）
        let own_defect: &[CodecError] = &[
            CodecError::Malformed("truncated"),
            CodecError::UnknownField(9),
            CodecError::DuplicateField(1),
            CodecError::MissingField("schema_version"),
            CodecError::InvalidUtf8,
            CodecError::TooManyMetrics,
            CodecError::InvalidDto("invalid_uuid"),
        ];
        for error in own_defect {
            assert_eq!(
                codec_error_status(error.clone()),
                BusinessStatus::InvalidArgument,
                "{error:?}"
            );
        }
    }

    #[test]
    fn the_four_classes_are_distinct_wire_codes() {
        // 四档必须是四个互不相同的 wire 码：任何两者合并都会让中控丢失分流能力。
        let codes = [
            BusinessStatus::UnsupportedVersion as i32,
            BusinessStatus::InvalidArgument as i32,
            BusinessStatus::ResourceExhausted as i32,
            BusinessStatus::InternalError as i32,
        ];
        assert_eq!(codes, [1001, 1003, 1008, 1009]);
        let unique: std::collections::BTreeSet<i32> = codes.iter().copied().collect();
        assert_eq!(unique.len(), codes.len(), "四档不得互相合并");
    }
}
