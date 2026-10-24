//! 六方法类型化BoardClient与已认证设备会话RPC的业务codec适配。
//!
//! 不建立socket、不重试变更、不伪造在线状态。生产连接管理器必须实现DeviceRpc并
//! 仅按已批准、公钥绑定的当前会话路由；当前模块单测不代表该管理器已接线。
use crate::tasks::board_client::{BoardClient, BoardClientError, BoardFuture};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, BusinessPayloadKind, Capabilities,
    ClockSample, DeviceStatus, Query, RebootExecute, RebootPrepare, RebootTicket, TaskQuery,
    TaskRecord, approved_codec_witness, decode_business, encode_business,
};
use std::fmt;
use std::sync::Arc;
use uuid::Uuid;

/// 返回值已由session验证同trace/action/流/代际；错误消息不从peer原文传播。
pub struct RpcReply {
    pub status_code: i32,
    pub payload: Vec<u8>,
}
impl fmt::Debug for RpcReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcReply")
            .field("status_code", &self.status_code)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}
/// 每次调用只发送一次；超时/断线结果未知，禁止由实现者自动重放。
/// 所有业务动作固定Control流，public_key必须来自已验签并经SQL批准的连接。
pub trait DeviceRpc: Send + Sync {
    fn request<'a>(
        &'a self,
        device: &'a [u8; 32],
        action: &'static str,
        payload: Vec<u8>,
    ) -> BoardFuture<'a, RpcReply>;
}
pub struct TransportBoardClient {
    rpc: Arc<dyn DeviceRpc>,
}
impl TransportBoardClient {
    pub fn new(rpc: Arc<dyn DeviceRpc>) -> Self {
        Self { rpc }
    }
    async fn raw(
        &self,
        device: &[u8; 32],
        action: &'static str,
        payload: BusinessPayload,
    ) -> Result<RpcReply, BoardClientError> {
        let witness = approved_codec_witness();
        let wire = encode_business(&BusinessCodec, Some(&witness), &payload)
            .map_err(|_| BoardClientError::ProtocolMismatch)?;
        self.rpc.request(device, action, wire).await
    }
    async fn call(
        &self,
        device: &[u8; 32],
        action: &'static str,
        request: BusinessPayload,
        expected: BusinessPayloadKind,
    ) -> Result<BusinessPayload, BoardClientError> {
        let response = self.raw(device, action, request).await?;
        check_status(response.status_code)?;
        decode(response.payload, expected)
    }
}
fn query() -> BusinessPayload {
    BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    })
}
fn decode(
    bytes: Vec<u8>,
    expected: BusinessPayloadKind,
) -> Result<BusinessPayload, BoardClientError> {
    let witness = approved_codec_witness();
    decode_business(&BusinessCodec, Some(&witness), expected, &bytes)
        .map_err(|_| BoardClientError::ProtocolMismatch)
}
fn check_status(code: i32) -> Result<(), BoardClientError> {
    match code {
        0 => Ok(()),
        1004 => Err(BoardClientError::CapabilityUnavailable),
        1005 => Err(BoardClientError::DeviceBusy),
        1008 => Err(BoardClientError::ResourceExhausted),
        1001..=1010 => Err(BoardClientError::Rejected {
            status: code,
            reason_code: None,
        }),
        _ => Err(BoardClientError::ProtocolMismatch),
    }
}
impl BoardClient for TransportBoardClient {
    fn capabilities<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities> {
        Box::pin(async move {
            match self
                .call(
                    device,
                    "device.capabilities.get",
                    query(),
                    BusinessPayloadKind::Capabilities,
                )
                .await?
            {
                BusinessPayload::Capabilities(value) => Ok(value),
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
    fn status<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus> {
        Box::pin(async move {
            match self
                .call(
                    device,
                    "device.status.get",
                    query(),
                    BusinessPayloadKind::DeviceStatus,
                )
                .await?
            {
                BusinessPayload::DeviceStatus(value) => Ok(value),
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
    fn clock<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample> {
        Box::pin(async move {
            match self
                .call(
                    device,
                    "device.clock.get",
                    query(),
                    BusinessPayloadKind::ClockSample,
                )
                .await?
            {
                BusinessPayload::ClockSample(value) => Ok(value),
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
    fn prepare<'a>(
        &'a self,
        device: &'a [u8; 32],
        sub_task_id: Uuid,
        expected_boot_id: &'a str,
    ) -> BoardFuture<'a, RebootTicket> {
        Box::pin(async move {
            let sub_task_id = sub_task_id.to_string();
            let request = BusinessPayload::RebootPrepare(RebootPrepare {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: sub_task_id.clone(),
                expected_boot_id: expected_boot_id.into(),
            });
            match self
                .call(
                    device,
                    "device.reboot.prepare",
                    request,
                    BusinessPayloadKind::RebootTicket,
                )
                .await?
            {
                BusinessPayload::RebootTicket(value)
                    if value.sub_task_id == sub_task_id && value.boot_id == expected_boot_id =>
                {
                    Ok(value)
                }
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
    fn execute<'a>(
        &'a self,
        device: &'a [u8; 32],
        ticket: &'a RebootTicket,
        request: &'a RebootExecute,
    ) -> BoardFuture<'a, TaskRecord> {
        Box::pin(async move {
            if ticket.validate().is_err()
                || request.validate().is_err()
                || ticket.sub_task_id != request.sub_task_id
                || ticket.boot_id != request.expected_boot_id
                || ticket.execution_token != request.execution_token
            {
                return Err(BoardClientError::ProtocolMismatch);
            }
            match self
                .call(
                    device,
                    "device.reboot.execute",
                    BusinessPayload::RebootExecute(request.clone()),
                    BusinessPayloadKind::TaskRecord,
                )
                .await?
            {
                BusinessPayload::TaskRecord(value) if value.sub_task_id == request.sub_task_id => {
                    Ok(value)
                }
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
    fn task_record<'a>(
        &'a self,
        device: &'a [u8; 32],
        sub_task_id: Uuid,
    ) -> BoardFuture<'a, Option<TaskRecord>> {
        Box::pin(async move {
            let sub_task_id = sub_task_id.to_string();
            let response = self
                .raw(
                    device,
                    "device.task.get",
                    BusinessPayload::TaskQuery(TaskQuery {
                        schema_version: BUSINESS_SCHEMA_VERSION,
                        sub_task_id: sub_task_id.clone(),
                    }),
                )
                .await?;
            if response.status_code == 1006 {
                return Ok(None);
            }
            check_status(response.status_code)?;
            match decode(response.payload, BusinessPayloadKind::TaskRecord)? {
                BusinessPayload::TaskRecord(value) if value.sub_task_id == sub_task_id => {
                    Ok(Some(value))
                }
                _ => Err(BoardClientError::ProtocolMismatch),
            }
        })
    }
}
