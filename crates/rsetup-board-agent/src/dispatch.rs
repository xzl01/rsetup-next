//! 类型化分发：白名单查表 → DTO 结构校验 → 触达 source/journal，顺序不可调换。
//!
//! 本模块同时承载 Reboot 执行调度（[`RebootExecutor`]）与固定无参 OS 重启边界
//! （[`RebootOs`]）：`device.reboot.prepare` 走票据生成，`device.reboot.execute`
//! 走「载荷哈希去重 → fresh boot 校验 → 一次性票据消费 → accepted/attempted 可靠落盘
//! → 至多一次 OS 调用」的固定顺序。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crate::actions::{ActionKind, lookup_action};
use crate::journal::{DedupOutcome, Fs, JournalError, JournalStore, TaskRecord, TaskState};
use crate::read_only::{PeerContext, ReadOnlySource};
use crate::tickets::{TicketBinding, TicketManager, TicketOrigin, TicketToken};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, BusinessPayloadKind, Codec,
    DtoError, RebootExecute, RebootPrepare, RebootTicket, canonical_reboot_payload_hash,
    decode_business, is_canonical_uuid_v4,
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

/// 结构校验成功后的类型化响应；原始 wire 只有在获批 codec 就位后才可能产出。
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
        // 2) 结构校验：失败即拒绝，且未触达 I/O。错误按映射表分类（先匹配先赢）：
        //    仅 schema 版本/能力不匹配 → UnsupportedVersion；
        //    结构/字段/枚举/长度类校验失败（含空白 sub_task_id）→ InvalidArgument
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

    /// 原始字节入口：先查白名单，再 fail-closed，最后才允许解码。
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
        // 3) 无获批 codec 见证：fail-closed，绝不解析、绝不自造编码器
        let decoded = decode_business(codec, None, expected, payload)
            .map_err(|_| BusinessStatus::UnsupportedVersion)?;
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
/// 设备级原子占用标记（TR-N5 / 正审 R-05）：同 agent 实例单设备串行互斥（1005 DeviceBusy）。
/// 占用覆盖未决重启全生命周期：fresh execute 开始时申请；当且仅当确定未执行的拒绝
/// （DefinitelyNotCommitted / 票据无效 / boot 不匹配）或写入终态（Failed）才释放；
/// OS Ok 返回未决 Attempted 中间态时保持占用。
pub struct RebootExecutor<O: RebootOs, F: Fs> {
    journal: Arc<JournalStore<F>>,
    os: O,
    tickets: TicketManager,
    boot_id: Box<dyn Fn() -> String + Send + Sync>,
    agent_instance_id: String,
    device_occupied: AtomicBool,
}

impl<O: RebootOs, F: Fs> RebootExecutor<O, F> {
    pub fn new(
        journal: Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
    ) -> Self {
        Self {
            journal,
            os,
            tickets,
            boot_id,
            agent_instance_id: agent_instance_id.into(),
            device_occupied: AtomicBool::new(false),
        }
    }

    pub fn os(&self) -> &O {
        &self.os
    }

    pub fn tickets(&self) -> &TicketManager {
        &self.tickets
    }

    /// 设备级占用见证（未决重启生命周期）。
    pub fn device_occupied(&self) -> bool {
        self.device_occupied.load(Ordering::SeqCst)
    }

    /// 释放设备级占用（仅在确定未执行拒绝或写入终态后调用）。
    fn release_device(&self) {
        self.device_occupied.store(false, Ordering::SeqCst);
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
        if self.device_occupied.swap(true, Ordering::SeqCst) {
            return ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy);
        }
        let current_boot = (self.boot_id)();
        if current_boot != request.expected_boot_id {
            // 确定未执行：释放占用
            self.release_device();
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
            self.release_device();
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
                self.release_device();
                return persisted_outcome(record);
            }
            // 请求确未被接受且 OS 未进边界：安全返回非零错误
            Err(JournalError::DefinitelyNotCommitted) => {
                self.release_device();
                return ExecuteOutcome::Rejected(BusinessStatus::InternalError);
            }
            Err(JournalError::Conflict) => {
                self.release_device();
                return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict);
            }
            Err(JournalError::StorageFull) => {
                self.release_device();
                return ExecuteOutcome::Rejected(BusinessStatus::ResourceExhausted);
            }
            // insert_accepted 提交结果不明（CommitUnknown，OS 边界前）：断流待查、绝不调用 OS；
            // store 已进入 uncertain 隔离态，保持占用直至实例丢弃
            Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
            Err(_) => {
                self.release_device();
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
                self.release_device();
                return ExecuteOutcome::Accepted(accepted);
            }
            // mark_attempted 提交结果不明（仍为 OS 边界前）：断流、绝不调用 OS，交由 task.get 核查
            Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
            Err(_) => {
                self.release_device();
                return ExecuteOutcome::Indeterminate;
            }
        };

        // 5) 至多一次 OS 调用边界（仅 `attempted` 可靠落盘之后）
        match self.os.reboot() {
            // OS Ok 仅表示「重启已发起」：任务仍是未决 Attempted 中间态（attempted ≠ 成功），
            // 占用必须覆盖未决生命周期，此处绝不释放 device_occupied。
            Ok(()) => ExecuteOutcome::Attempted(attempted),
            Err(_) => match self.journal.finish(
                controller_key,
                &request.sub_task_id,
                TaskState::Failed,
                Some("OS_ERROR"),
            ) {
                // OS 明确报错且 failed/OS_ERROR 可靠落盘：以 status=0 回复真实 failed
                Ok(record) => {
                    self.release_device();
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
