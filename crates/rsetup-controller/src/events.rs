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
//!
//! 受众语义（02 §7「逐事件过滤」、06 §48）：一条总线通道 **不等于** 一个受众。
//! 每个 [`EventEnvelope`] 必须携带 [`EventAudience`]（全局提示 / 仅管理员 /
//! 指定用户），总线本身**不做**受众路由；投递侧（[`crate::http_events`]）在
//! 每一帧发送之前用连接自身的权威身份视角做 [`EventAudience::visible_to`]
//! 严格匹配，不匹配即静默跳过。因此「谁能看到什么」的判定只有一个归属地
//! （投递侧），发布侧的职责是**如实标注受众**，两者都不许把事件当成全局广播。

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

/// 事件的**受众范围**：谁可以在自己的 SSE 连接上看到这条事件
/// （02 §7 `GET /events`「逐事件过滤」、06 §4「对象更新携带……可见字段」）。
///
/// 总线只有一条通道，但**通道共用不等于受众共用**：流侧在投递每一帧之前用
/// [`EventAudience::visible_to`] 与该连接的身份视角做严格匹配，不匹配即
/// **静默跳过**（fail-closed，绝不无差别广播）。
///
/// 这里刻意**不**提供「未指定受众」的第三种取值：受众是发布方的必填项，
/// 漏掉受众在类型上就不成立。对象级事件（设备/任务/授权投影）必须由发布方
/// 明确落到具体用户或管理员范围，只有**不携带任何对象事实**的全局提示
/// （`reset`）才允许 [`EventAudience::AnySession`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventAudience {
    /// 对任何已改密的合法会话都可公开：仅限不含对象 ID、字段或权限事实的
    /// 全局提示（五种命名事件里只有 `reset` 属于这一类）。
    AnySession,
    /// 仅管理员会话可见（06 §4「时间事件仅管理员」）。
    Admins,
    /// 仅目标用户可见：对象私有事件的受众限定（`users.id` 的 16 字节原生形状，
    /// 与 `Session::user.id` 同源比较，不经字符串往返）。
    User([u8; 16]),
}

/// 当前连接的**身份视角**：受众匹配只依赖这两个权威事实（用户 id 与
/// 管理员标志），二者都由流侧在握手期与每帧投递前从权威源复核。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventViewer {
    pub user_id: [u8; 16],
    pub is_admin: bool,
}

impl EventAudience {
    /// 该连接是否属于本事件的受众。**fail-closed**：非目标用户看到私有事件、
    /// 非管理员看到管理员事件一律 `false`；调用方必须丢弃事件而不是降级广播。
    pub fn visible_to(self, viewer: EventViewer) -> bool {
        match self {
            Self::AnySession => true,
            Self::Admins => viewer.is_admin,
            Self::User(user_id) => viewer.user_id == user_id,
        }
    }
}

/// 一次总线投递的载荷：命名事件 + `data:` 的 JSON 对象 + 受众范围。
///
/// `Clone` 是 `tokio::sync::broadcast` 的硬要求；载荷只允许出现对象 ID、
/// revision/epoch 与**已授权可见**的字段，禁止携带任何凭据、会话令牌或未脱敏
/// 的原始错误。`audience` 是投递前的**硬过滤**依据：总线不做受众路由，
/// 由每条连接在发送循环里逐事件自查（[`crate::http_events`]）。
#[derive(Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    pub name: EventName,
    pub data: Value,
    pub audience: EventAudience,
}

impl EventEnvelope {
    /// 通用构造：发布方自行保证 `data` 的形状与可见性合规，并**必须**给出受众。
    pub fn new(name: EventName, data: Value, audience: EventAudience) -> Self {
        Self {
            name,
            data,
            audience,
        }
    }

    /// `device.updated`：`{"kind":"device","id":<64hex>,"revision":<十进制字符串>}`。
    ///
    /// 受众由发布方给出：设备事件是对象级事实，只有确证「对任何合法会话都
    /// 可见」时才可传 [`EventAudience::AnySession`]，否则必须限定到目标用户或
    /// 管理员范围。
    pub fn device_updated(device_id: &str, revision: u64, audience: EventAudience) -> Self {
        Self::new(
            EventName::DeviceUpdated,
            json!({
                "kind": "device",
                "id": device_id,
                "revision": decimal_u64_json(revision),
            }),
            audience,
        )
    }

    /// `task.updated`：`{"kind":"task","id":<uuid>,"revision":<十进制字符串>}`。
    ///
    /// 受众限定规则同 [`EventEnvelope::device_updated`]。
    pub fn task_updated(task_id: &str, revision: u64, audience: EventAudience) -> Self {
        Self::new(
            EventName::TaskUpdated,
            json!({
                "kind": "task",
                "id": task_id,
                "revision": decimal_u64_json(revision),
            }),
            audience,
        )
    }

    /// `permissions.changed`：`{"kind":"permissions","id":<user uuid>,"epoch":<十进制字符串>}`。
    ///
    /// `epoch` 是授权 epoch（authz_epoch）：客户端据此判定旧授权投影失效，
    /// 清缓存并重新鉴权，而不是继续展示撤权前的数据（06 §4）。
    ///
    /// 授权投影是**用户私有**事实：受众由构造钉死为 [`EventAudience::User`]，
    /// 并且 `data.id` 与受众来自同一份字节，杜绝「载荷与受众不一致」。
    pub fn permissions_changed(user_id: [u8; 16], authz_epoch: u64) -> Self {
        Self::new(
            EventName::PermissionsChanged,
            json!({
                "kind": "permissions",
                "id": uuid::Uuid::from_bytes(user_id).to_string(),
                "epoch": decimal_u64_json(authz_epoch),
            }),
            EventAudience::User(user_id),
        )
    }

    /// `system.time.changed`：`{"kind":"system_time","evidence":<TimeEvidence>}`。
    ///
    /// 载荷就是既有的 `TimeEvidence` 对象（02 §7 `/system/time` 同源形状），
    /// 不新增第二套时间字段；受众由构造钉死为 [`EventAudience::Admins`]
    /// （06 §4「时间事件仅管理员」，与 [`EventName::admin_only`] 构成双重门禁）。
    pub fn system_time_changed(evidence: &TimeEvidence) -> Self {
        Self::new(
            EventName::SystemTimeChanged,
            json!({
                "kind": "system_time",
                "evidence": serde_json::to_value(evidence)
                    .unwrap_or(Value::Null),
            }),
            EventAudience::Admins,
        )
    }

    /// `reset`：`{"kind":"reset","reason":<稳定去敏原因码>}`。
    ///
    /// 只含一个稳定原因码、不含任何对象事实，因此受众是「任何合法会话」。
    pub fn reset(reason: &str) -> Self {
        Self::new(
            EventName::Reset,
            json!({ "kind": "reset", "reason": reason }),
            EventAudience::AnySession,
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
        let alice = [7u8; 16];
        let device =
            EventEnvelope::device_updated(&"ab".repeat(32), 12, EventAudience::User(alice));
        assert_eq!(device.name, EventName::DeviceUpdated);
        assert_eq!(device.audience, EventAudience::User(alice));
        assert_eq!(device.data["kind"], json!("device"));
        assert_eq!(device.data["id"], json!("ab".repeat(32)));
        assert_eq!(device.data["revision"], json!("12"));

        let task = EventEnvelope::task_updated(
            "0e7f1c2e-0000-4000-8000-000000000001",
            7,
            EventAudience::Admins,
        );
        assert_eq!(task.data["kind"], json!("task"));
        assert_eq!(task.data["revision"], json!("7"));
        assert_eq!(task.audience, EventAudience::Admins);

        let perms = EventEnvelope::permissions_changed(alice, u64::MAX);
        assert_eq!(perms.data["kind"], json!("permissions"));
        // 载荷 id 就是受众本人：canonical 小写 UUID。
        assert_eq!(
            perms.data["id"],
            json!(uuid::Uuid::from_bytes(alice).to_string())
        );
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
    fn object_events_pin_their_audience_at_construction() {
        let alice = [7u8; 16];
        // 授权投影事件的受众由构造钉死为本人，发布方无法把它发成全局事件。
        assert_eq!(
            EventEnvelope::permissions_changed(alice, 1).audience,
            EventAudience::User(alice)
        );
        // 时间事件的受众钉死为管理员，与 EventName::admin_only 构成双重门禁。
        assert_eq!(
            EventEnvelope::system_time_changed(&fixture_evidence()).audience,
            EventAudience::Admins
        );
        // 唯一免受众构造是 reset：它不含任何对象事实。
        assert_eq!(
            EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW).audience,
            EventAudience::AnySession
        );
    }

    #[test]
    fn audience_match_is_fail_closed_per_viewer() {
        let alice = [7u8; 16];
        let bob = [8u8; 16];
        let alice_viewer = EventViewer {
            user_id: alice,
            is_admin: false,
        };
        let admin_viewer = EventViewer {
            user_id: bob,
            is_admin: true,
        };
        // 私有事件只对目标用户可见：连管理员的他人也必须拒收（对象级 ACL
        // 不因「是管理员」而自动放开）。
        assert!(EventAudience::User(alice).visible_to(alice_viewer));
        assert!(!EventAudience::User(alice).visible_to(EventViewer {
            user_id: bob,
            is_admin: false,
        }));
        assert!(!EventAudience::User(alice).visible_to(admin_viewer));
        // 管理员事件只对管理员可见。
        assert!(EventAudience::Admins.visible_to(admin_viewer));
        assert!(!EventAudience::Admins.visible_to(alice_viewer));
        // 全局提示对任何合法会话可见（连接本身已通过握手认证）。
        assert!(EventAudience::AnySession.visible_to(alice_viewer));
        assert!(EventAudience::AnySession.visible_to(admin_viewer));
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
        let delivered = bus.publish(EventEnvelope::task_updated(
            "t-1",
            1,
            EventAudience::AnySession,
        ));
        assert_eq!(delivered, 2);
        assert_eq!(first.try_recv().unwrap().name, EventName::TaskUpdated);
        assert_eq!(second.try_recv().unwrap().name, EventName::TaskUpdated);
        // 订阅只收「之后」的事件，不回放历史。
        assert_eq!(
            bus.publish(EventEnvelope::task_updated(
                "t-2",
                2,
                EventAudience::AnySession
            )),
            2
        );
    }

    #[test]
    fn lagged_slow_subscriber_never_blocks_the_publisher() {
        // 容量 2：慢订阅者落后后，发送方仍然立即成功，落后方得到 Lagged（→ HTTP 侧 reset）。
        let bus = EventBus::with_capacity(2);
        let mut slow = bus.subscribe();
        for revision in 0..5 {
            assert_eq!(
                bus.publish(EventEnvelope::task_updated(
                    "t",
                    revision,
                    EventAudience::AnySession
                )),
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
            bus.publish(EventEnvelope::device_updated(
                &"cd".repeat(32),
                3,
                EventAudience::AnySession
            )),
            1
        );
        assert_eq!(rx.try_recv().unwrap().name, EventName::DeviceUpdated);
    }
}
