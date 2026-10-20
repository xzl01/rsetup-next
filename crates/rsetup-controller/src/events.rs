//! 运行时事件总线（完整 V1 SSE 目标的**发布侧**基础设施，06 §4 与 02 §7）。
//!
//! 本模块只负责「把一次真实发生的状态变化投给当前所有在线订阅者」，
//! 不是持久消息总线、不做历史重放、不与板端做双向 Ping/Pong（06 §4 U-03）：
//!
//! - 载体是 `tokio::sync::broadcast` 环形通道，容量固定
//!   [`EVENT_BUS_CAPACITY`] = 128（06 §4「每连接建议最多 128 条待发事件」）。
//!   broadcast 的语义与那条建议一致：**慢订阅者不会阻塞发送方**，也不会把
//!   背压传回板端采集；它落后超过容量时只会得到 `Lagged`，由 HTTP 侧
//!   （[`crate::http_events`]）翻译成 `event: reset` 提示客户端全量重取；
//! - 没有任何在线订阅者时发布即丢弃（事件只是「变化提示」，权威事实始终来自
//!   GET 快照），不缓存、不落库、不伪造「已送达」；
//! - 五种命名事件（[`EventName`]）与 `data:` JSON 载荷形状由本模块钉死，
//!   客户端对五种名称**分别**注册监听，不能只监听默认 `message`（06 §4）。
//!
//! revision 语义（06 §4）：只在**同一 kind + id** 内单调比较，不是事件流或
//! 分页列表的全局版本；十进制字符串输出沿用 `decimal_u64_json` 的既有契约，
//! 避免 JS 端丢精度。

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::model::decimal_u64_json;
use crate::time::evidence::TimeEvidence;

/// SSE 命名事件名：设备状态变化（06 §4）。
pub const EVENT_NAME_DEVICE_UPDATED: &str = "device.updated";
/// SSE 命名事件名：任务状态变化（06 §4）。
pub const EVENT_NAME_TASK_UPDATED: &str = "task.updated";
/// SSE 命名事件名：授权投影变化（06 §4）。
pub const EVENT_NAME_PERMISSIONS_CHANGED: &str = "permissions.changed";
/// SSE 命名事件名：中控时间证据变化（06 §4，仅管理员可见）。
pub const EVENT_NAME_SYSTEM_TIME_CHANGED: &str = "system.time.changed";
/// SSE 命名事件名：要求客户端清空水位并全量重取（06 §4）。
pub const EVENT_NAME_RESET: &str = "reset";

/// `reset` 载荷 `reason` 的固定稳定码：订阅端落后导致缓冲区溢出。
pub const RESET_REASON_BUFFER_OVERFLOW: &str = "buffer_overflow";

/// 总线环形缓冲容量（06 §4：每连接建议最多 128 条待发事件，满即 reset）。
///
/// tokio 的 broadcast 是**全通道共用**一个环形缓冲，而不是每连接一份独立
/// 队列；这里取 128 作为「一条慢连接最多把内存推高到 128 条」的上界，
/// 任何订阅者落后都只影响它自己（收到 `Lagged` → `reset`），发送方恒不阻塞。
pub const EVENT_BUS_CAPACITY: usize = 128;

/// 五种命名事件的稳定标识（06 §4 全集，含服务端生成的 `reset`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventName {
    DeviceUpdated,
    TaskUpdated,
    PermissionsChanged,
    SystemTimeChanged,
    /// 服务端在背压/代际失效时发出的重同步提示。
    Reset,
}

impl EventName {
    /// SSE `event:` 字段使用的线上名称（06 §4 逐字规定的五种名称）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeviceUpdated => EVENT_NAME_DEVICE_UPDATED,
            Self::TaskUpdated => EVENT_NAME_TASK_UPDATED,
            Self::PermissionsChanged => EVENT_NAME_PERMISSIONS_CHANGED,
            Self::SystemTimeChanged => EVENT_NAME_SYSTEM_TIME_CHANGED,
            Self::Reset => EVENT_NAME_RESET,
        }
    }

    /// 是否只对管理员会话可见（06 §4「时间事件仅管理员」）。
    pub const fn admin_only(self) -> bool {
        matches!(self, Self::SystemTimeChanged)
    }
}

/// 一次总线投递的载荷：命名事件 + `data:` 的 JSON 对象。
///
/// `Clone` 是 `tokio::sync::broadcast` 的硬要求；载荷只允许出现对象 ID、
/// revision/epoch 与**已授权可见**的字段，禁止携带任何凭据、会话令牌或未脱敏
/// 的原始错误（可见性判定属于发布方职责，见模块头）。
#[derive(Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    pub name: EventName,
    pub data: Value,
}

impl EventEnvelope {
    /// 通用构造：发布方自行保证 `data` 的形状与可见性合规。
    pub fn new(name: EventName, data: Value) -> Self {
        Self { name, data }
    }

    /// `device.updated`：`{"kind":"device","id":<64hex>,"revision":<十进制字符串>}`。
    pub fn device_updated(device_id: &str, revision: u64) -> Self {
        Self::new(
            EventName::DeviceUpdated,
            json!({
                "kind": "device",
                "id": device_id,
                "revision": decimal_u64_json(revision),
            }),
        )
    }

    /// `task.updated`：`{"kind":"task","id":<uuid>,"revision":<十进制字符串>}`。
    pub fn task_updated(task_id: &str, revision: u64) -> Self {
        Self::new(
            EventName::TaskUpdated,
            json!({
                "kind": "task",
                "id": task_id,
                "revision": decimal_u64_json(revision),
            }),
        )
    }

    /// `permissions.changed`：`{"kind":"permissions","id":<user uuid>,"epoch":<十进制字符串>}`。
    ///
    /// `epoch` 是授权 epoch（authz_epoch）：客户端据此判定旧授权投影失效，
    /// 清缓存并重新鉴权，而不是继续展示撤权前的数据（06 §4）。
    pub fn permissions_changed(user_id: &str, authz_epoch: u64) -> Self {
        Self::new(
            EventName::PermissionsChanged,
            json!({
                "kind": "permissions",
                "id": user_id,
                "epoch": decimal_u64_json(authz_epoch),
            }),
        )
    }

    /// `system.time.changed`：`{"kind":"system_time","evidence":<TimeEvidence>}`。
    ///
    /// 载荷就是既有的 `TimeEvidence` 对象（02 §7 `/system/time` 同源形状），
    /// 不新增第二套时间字段；仅管理员订阅者可见（[`EventName::admin_only`]）。
    pub fn system_time_changed(evidence: &TimeEvidence) -> Self {
        Self::new(
            EventName::SystemTimeChanged,
            json!({
                "kind": "system_time",
                "evidence": serde_json::to_value(evidence)
                    .unwrap_or(Value::Null),
            }),
        )
    }

    /// `reset`：`{"kind":"reset","reason":<稳定去敏原因码>}`。
    pub fn reset(reason: &str) -> Self {
        Self::new(
            EventName::Reset,
            json!({ "kind": "reset", "reason": reason }),
        )
    }
}

/// 事件总线句柄：一个 `broadcast::Sender` 的克隆语义包装。
///
/// 生产装配只在 `production` 里构造一次，随后按 `Clone`（内部 `Sender::clone`，
/// 同一通道）分发给各条线的发布方与 HTTP 订阅侧；`Clone` 不创建第二通道。
#[derive(Clone, Debug)]
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// 以 [`EVENT_BUS_CAPACITY`] 容量建通道（生产唯一推荐入口）。
    pub fn new() -> Self {
        Self::with_capacity(EVENT_BUS_CAPACITY)
    }

    /// 显式容量入口：仅装配与测试需要非默认容量时使用（0 视为 1，
    /// `tokio::sync::broadcast` 不接受 0 容量）。
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// 广播一个事件，返回实际收到的订阅者数量。
    ///
    /// **永不阻塞、永不上抛**：无订阅者（`broadcast` 报 `SendError`）即丢弃并返回 0，
    /// 这是「事件只是变化提示」的契约内行为，不是失败，也不得改成排队重放。
    pub fn publish(&self, event: EventEnvelope) -> usize {
        self.sender.send(event).unwrap_or(0)
    }

    /// 新建一个订阅（从「现在之后」开始，不回放历史）。
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    /// 当前活跃订阅数（仅用于可观测性与测试；不作为投递保证）。
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn fixture_evidence() -> TimeEvidence {
        TimeEvidence {
            system_wall_utc: Utc.with_ymd_and_hms(2026, 10, 16, 8, 0, 0).unwrap(),
            reference_utc: Utc.with_ymd_and_hms(2026, 10, 16, 8, 0, 0).unwrap(),
            quality: crate::time::evidence::TimeQuality::NtpValid,
            clock_epoch: Uuid::new_v4(),
            source: Some("ntp.example".into()),
            sample_age_ms: Some(1_500),
            offset_ms: Some(-5),
            uncertainty_ms: Some(15),
        }
    }

    #[test]
    fn five_event_names_are_the_contract_set() {
        assert_eq!(EventName::DeviceUpdated.as_str(), "device.updated");
        assert_eq!(EventName::TaskUpdated.as_str(), "task.updated");
        assert_eq!(
            EventName::PermissionsChanged.as_str(),
            "permissions.changed"
        );
        assert_eq!(EventName::SystemTimeChanged.as_str(), "system.time.changed");
        assert_eq!(EventName::Reset.as_str(), "reset");
        // 06 §4：时间事件仅管理员，其余四种对任何已改密会话可见。
        assert!(EventName::SystemTimeChanged.admin_only());
        assert!(!EventName::DeviceUpdated.admin_only());
        assert!(!EventName::TaskUpdated.admin_only());
        assert!(!EventName::PermissionsChanged.admin_only());
        assert!(!EventName::Reset.admin_only());
    }

    #[test]
    fn payloads_carry_kind_id_and_decimal_revision() {
        let device = EventEnvelope::device_updated(&"ab".repeat(32), 12);
        assert_eq!(device.name, EventName::DeviceUpdated);
        assert_eq!(device.data["kind"], json!("device"));
        assert_eq!(device.data["id"], json!("ab".repeat(32)));
        assert_eq!(device.data["revision"], json!("12"));

        let task = EventEnvelope::task_updated("0e7f1c2e-0000-4000-8000-000000000001", 7);
        assert_eq!(task.data["kind"], json!("task"));
        assert_eq!(task.data["revision"], json!("7"));

        let perms =
            EventEnvelope::permissions_changed("0e7f1c2e-0000-4000-8000-000000000002", u64::MAX);
        assert_eq!(perms.data["kind"], json!("permissions"));
        // 十进制字符串，不经 JS number，u64 上界不丢精度。
        assert_eq!(perms.data["epoch"], json!(u64::MAX.to_string()));

        let time = EventEnvelope::system_time_changed(&fixture_evidence());
        assert_eq!(time.data["kind"], json!("system_time"));
        assert_eq!(time.data["evidence"]["quality"], json!("ntp_valid"));
        assert_eq!(time.data["evidence"]["sample_age_ms"], json!("1500"));

        let reset = EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW);
        assert_eq!(reset.name, EventName::Reset);
        assert_eq!(reset.data["reason"], json!("buffer_overflow"));
    }

    #[test]
    fn publish_without_subscribers_is_dropped_not_buffered() {
        let bus = EventBus::new();
        assert_eq!(bus.subscriber_count(), 0);
        assert_eq!(
            bus.publish(EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW)),
            0,
            "无订阅者必须直接丢弃并返回 0，不得累积"
        );
    }

    #[test]
    fn subscribers_from_now_on_receive_broadcast_events() {
        let bus = EventBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);
        let delivered = bus.publish(EventEnvelope::task_updated("t-1", 1));
        assert_eq!(delivered, 2);
        assert_eq!(first.try_recv().unwrap().name, EventName::TaskUpdated);
        assert_eq!(second.try_recv().unwrap().name, EventName::TaskUpdated);
        // 订阅只收「之后」的事件，不回放历史。
        assert_eq!(bus.publish(EventEnvelope::task_updated("t-2", 2)), 2);
    }

    #[test]
    fn lagged_slow_subscriber_never_blocks_the_publisher() {
        // 容量 2：慢订阅者落后后，发送方仍然立即成功，落后方得到 Lagged（→ HTTP 侧 reset）。
        let bus = EventBus::with_capacity(2);
        let mut slow = bus.subscribe();
        for revision in 0..5 {
            assert_eq!(
                bus.publish(EventEnvelope::task_updated("t", revision)),
                1,
                "慢订阅者绝不得阻塞或拒绝发送方"
            );
        }
        assert!(matches!(
            slow.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(_))
        ));
    }

    #[test]
    fn cloning_the_bus_shares_one_channel() {
        let bus = EventBus::new();
        let clone = bus.clone();
        let mut rx = clone.subscribe();
        assert_eq!(bus.subscriber_count(), 1);
        assert_eq!(
            bus.publish(EventEnvelope::device_updated(&"cd".repeat(32), 3)),
            1
        );
        assert_eq!(rx.try_recv().unwrap().name, EventName::DeviceUpdated);
    }
}
