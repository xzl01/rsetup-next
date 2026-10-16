//! 类型化分发：白名单查表 → DTO 结构校验 → 触达 source/journal，顺序不可调换。

use crate::actions::{ActionKind, lookup_action};
use crate::read_only::{PeerContext, ReadOnlySource};
use rsetup_protocol::wire::{
    BusinessMessage, BusinessPayload, BusinessPayloadKind, Codec, DtoError, decode_business,
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
}

pub struct BoardAgent<R, J, E> {
    read_only: R,
    journal: J,
    epoch: E,
}

impl<R: ReadOnlySource, J: TaskJournalReader, E: JournalEpochSource> BoardAgent<R, J, E> {
    pub fn new(read_only: R, journal: J, epoch: E) -> Self {
        Self {
            read_only,
            journal,
            epoch,
        }
    }
    pub fn source(&self) -> &R {
        &self.read_only
    }
    pub fn journal_epoch(&self) -> String {
        self.epoch.journal_epoch()
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
            // prepare/execute 由 Task 4 的 RebootExecutor 承担；本切片明确拒绝，绝不假装成功
            (ActionKind::RebootPrepare, _) | (ActionKind::RebootExecute, _) => {
                Err(BusinessStatus::CapabilityUnavailable)
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
