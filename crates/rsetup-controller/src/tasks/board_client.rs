//! 共享类型化 `BoardClient` 契约（04 Task 5，dyn-safe 形式）。
//!
//! 6 方法签名逐字传播自四线接口所有权清单 §3 之「6 方法待审统一目标契约」：
//! 全部返回 Box 化 [`BoardFuture`]，**不用** `async fn`/RPIT/泛型方法，
//! `Arc<dyn BoardClient>` 因此对象安全（见 [`assert_board_client_dyn_safe`] 的
//! 编译证明）。设备键恒为 `device: &[u8; 32]`（设备公钥字节）；`DeviceIdentity`
//! 只作内部调度/恢复上下文，**不可替代** trait 键参数。
//!
//! 契约要点（与所有权清单 §3 一致）：
//! - 6 方法不可增 `task_query(&TaskQuery)`/`prepare_request()`（二者为 Transport
//!   待审扩展，不属本契约）；
//! - [`BoardClientError`] 为独立 typed 枚举，**不**泛携 `ControllerError`/driver
//!   字符串；`Rejected.reason_code` 严格按 03/04 稳定词表校验，不存原 driver 串；
//! - `execute` 消费 `ticket` 与 `request` 双引用，实现层发送前须核对
//!   `ticket.sub_task_id == request.sub_task_id`；
//! - `task_record` 返回 `Option<TaskRecord>`：`Ok(None)` 即 TASK_NOT_FOUND(1006)，
//!   表示查询时未找到记录，**不证明** `execute` 未执行，也不单独推进状态。
//!
//! 集成装配时由调度器/集成 `TaskApi` adapter 以 `Arc<dyn BoardClient>` 持有与
//! 注入；`HttpTasksState` 保持 `{api, auth}` 两个字段（不新增 `BoardClient`
//! 字段），经 `TaskApi` 间接消费。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rsetup_protocol::wire::{
    Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket, TaskRecord,
};
use uuid::Uuid;

/// 板端调用的 dyn-safe future 别名：`Send + 'a` 保证可跨任务搬运与 `tokio::spawn`。
pub type BoardFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BoardClientError>> + Send + 'a>>;

/// 板端任务客户端的 6 方法统一目标契约（dyn-safe）。
///
/// 读方法（`capabilities`/`status`/`clock`/`task_record`）无任何发送语义；
/// 发送方法（`prepare`/`execute`）只允许出现在意图先行的 dispatch 链路，
/// 恢复路径（`recover_process`）**只**消费 `task_record`。
pub trait BoardClient: Send + Sync {
    /// 读取板端能力快照（动作支持、持久 journal、状态预算与 journal epoch）。
    fn capabilities<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities>;
    /// 读取板端状态快照（boot/agent 代际、采样序列与时钟质量）。
    fn status<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus>;
    /// 读取一次时钟样本（TimeGate 证据源）。
    fn clock<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample>;
    /// 派发链路②：换取重启票据（保存 `boot_id` 与 `valid_for_ms`）。
    fn prepare<'a>(
        &'a self,
        device: &'a [u8; 32],
        sub_task_id: Uuid,
        expected_boot_id: &'a str,
    ) -> BoardFuture<'a, RebootTicket>;
    /// 派发链路⑤：意图 `COMMIT` 成功后**至多一次**发送执行请求。
    ///
    /// 实现层发送前必须核对 `ticket.sub_task_id == request.sub_task_id`，
    /// 不匹配不发并记 `ProtocolMismatch`。
    fn execute<'a>(
        &'a self,
        device: &'a [u8; 32],
        ticket: &'a RebootTicket,
        request: &'a RebootExecute,
    ) -> BoardFuture<'a, TaskRecord>;
    /// 只读查询板端子任务记录：`Ok(None)` 即 TASK_NOT_FOUND(1006)，仅表示查询时
    /// 未找到记录，**不证明** `execute` 未执行，也不单独推进状态。
    fn task_record<'a>(
        &'a self,
        device: &'a [u8; 32],
        sub_task_id: Uuid,
    ) -> BoardFuture<'a, Option<TaskRecord>>;
}

/// 板端调用的独立 typed 错误枚举。
///
/// 不泛携 `ControllerError`/driver 字符串（实际 `ControllerError` 仅派生
/// `Debug`/`thiserror`，本枚举不照抄 Transport 草案的 `Clone`/`PartialEq`/`Eq`
/// 与 `Controller(#[from] ControllerError)` 变体，也不引入 `DiagCode`）；
/// `Rejected.reason_code` 只存 03/04 稳定词表值。
#[derive(Debug)]
pub enum BoardClientError {
    /// 设备控制流不在线/不可达。
    DeviceOffline,
    /// 板端显式拒绝：HTTP/业务状态码 + 稳定词表 `reason_code`（可选）。
    Rejected {
        status: i32,
        reason_code: Option<String>,
    },
    /// 线协议/契约不一致（如 `ticket.sub_task_id != request.sub_task_id`）。
    ProtocolMismatch,
    /// 板端能力不支持或持久 journal 不可用。
    CapabilityUnavailable,
    /// 设备忙（锁不属于本次操作）。
    DeviceBusy,
    /// 板端资源不足（并发/配额）。
    ResourceExhausted,
    /// 调用超时：结果未知，**绝不**据此判 `failed`、**绝不**重发。
    Timeout,
    /// 传输层错误：结果未知，同 `Timeout` 口径。
    Transport,
}

/// 对象安全编译证明：本函数**可编译**即证 `Arc<dyn BoardClient>` 对象安全
/// （各方法均返回 Box 化 [`BoardFuture`]，无泛型方法/`async fn`/RPIT）。
///
/// 非运行时入口：不期望被调用，futures 构造后立即丢弃；保留它是为了把
/// dyn-safe 约束钉进类型检查，防止后续改动（如误加 `async fn`）悄悄破坏
/// `Arc<dyn BoardClient>` 的持有与注入路径。
pub fn assert_board_client_dyn_safe(
    client: Arc<dyn BoardClient>,
    device: &[u8; 32],
    subtask_id: Uuid,
    ticket: &RebootTicket,
    request: &RebootExecute,
) {
    let _: BoardFuture<'_, Capabilities> = client.capabilities(device);
    let _: BoardFuture<'_, DeviceStatus> = client.status(device);
    let _: BoardFuture<'_, ClockSample> = client.clock(device);
    let _: BoardFuture<'_, RebootTicket> = client.prepare(device, subtask_id, "");
    let _: BoardFuture<'_, TaskRecord> = client.execute(device, ticket, request);
    let _: BoardFuture<'_, Option<TaskRecord>> = client.task_record(device, subtask_id);
}
