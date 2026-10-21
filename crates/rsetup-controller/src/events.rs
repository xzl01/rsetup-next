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
//! 每一帧发送之前用连接自身的权威身份视角做 [`EventEnvelope::visible_to`]
//! 严格匹配，不匹配即静默跳过。因此「谁能看到什么」的判定只有一个归属地
//! （投递侧），发布侧的职责是**如实标注受众**，两者都不许把事件当成全局广播。
//!
//! 「如实标注」在这里不是约定而是**类型约束**（06 §4 受众隔离、02 §7 逐事件
//! 过滤的构造级保证）：
//!
//! 1. [`EventEnvelope`] 的字段是私有的，模块外**不存在**任意 `(name, data,
//!    audience)` 组合的构造点，也不存在事后改写受众的入口——「Alice 的授权
//!    投影被标成 Bob 的受众」这种对象在本 crate 的对外 API 上不可书写；
//! 2. 对象级事件（`device.updated`/`task.updated`）的受众参数是
//!    [`EventObjectAudience`]，它的变体里**没有**「任何会话」：对象事实要么
//!    限定管理员范围，要么限定某一个用户，不接受「广播给所有连接」；
//! 3. `permissions.changed` 的 `data.id` 与受众由**同一份字节**生成，
//!    `system.time.changed` 钉死仅管理员，只有不携带任何对象事实的 `reset`
//!    携带 [`EventAudience::AnySession`]；
//! 4. 作为纵深防御，投递判定 [`EventEnvelope::visible_to`] 会重新核对名称、
//!    受众与载荷的一致性（`permissions.changed` 的载荷 `id` 必须就是受众本人；
//!    非 `reset` 事件哪怕带着 `AnySession` 也一律不可见），任一不一致即
//!    **失败关闭**。第 1 条保证模块内构造不出这种对象，第 4 条保证即便将来
//!    新增构造点写错，也只会静默丢弃而不会漏给他人。

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

    /// 该名称的事件是否**允许**全局受众（任何合法会话）。
    ///
    /// 全集里只有 `reset` 不携带任何对象事实（只有一个稳定原因码），因此只有
    /// 它可以全局；其余四种都携带对象 ID、revision/epoch 或权限事实，必须
    /// 限定受众。这是 [`EventEnvelope::visible_to`] 的失败关闭依据之一。
    pub const fn allows_global_audience(self) -> bool {
        matches!(self, Self::Reset)
    }
}

/// 事件的**受众范围**：谁可以在自己的 SSE 连接上看到这条事件
/// （02 §7 `GET /events`「逐事件过滤」、06 §4「对象更新携带……可见字段」）。
///
/// 总线只有一条通道，但**通道共用不等于受众共用**：流侧在投递每一帧之前用
/// [`EventEnvelope::visible_to`] 与该连接的身份视角做严格匹配，不匹配即
/// **静默跳过**（fail-closed，绝不无差别广播）。
///
/// 这个类型是**投递侧的判定结果表示**，不是发布方的自由填写项：它的构造点全部
/// 在 [`EventEnvelope`] 的类型化构造里（模块头第 1/2/3 条），其中
/// [`EventAudience::AnySession`] 只能由 [`EventEnvelope::reset`] 产生。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventAudience {
    /// 对任何已改密的合法会话都可公开：仅限不含对象 ID、字段或权限事实的
    /// 全局提示（五种命名事件里只有 `reset` 属于这一类，见
    /// [`EventName::allows_global_audience`]）。
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

/// **对象级**事件可选的受众：要么限定某一个用户，要么限定管理员范围。
///
/// 这里刻意**没有**「任何合法会话」这一档，也刻意不提供从任意
/// [`EventAudience`] 转换的入口：`device.updated`/`task.updated`/
/// `permissions.changed` 携带对象 ID 与 revision/epoch，是对象级事实，
/// 「未明确授权即广播」在类型上不成立（02 §7「逐事件过滤」）。发布方要表达
/// 「这条对象事件谁都没资格看」不存在合法写法，只能不发。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventObjectAudience {
    /// 仅管理员会话（用于「这条对象事实只投递给管理员范围」的发布方）。
    Admins,
    /// 仅该用户本人的会话。
    User([u8; 16]),
}

impl EventObjectAudience {
    /// 限定给某一个用户。
    pub const fn user(user_id: [u8; 16]) -> Self {
        Self::User(user_id)
    }

    /// 限定给管理员范围。
    pub const fn admins() -> Self {
        Self::Admins
    }

    /// 投影成投递侧的受众判定值。私有：模块外拿不到「第三个受众」的入口。
    const fn to_event_audience(self) -> EventAudience {
        match self {
            Self::Admins => EventAudience::Admins,
            Self::User(user_id) => EventAudience::User(user_id),
        }
    }
}

impl EventAudience {
    /// 该受众是否覆盖这个连接。**fail-closed**：非目标用户命中私有受众、
    /// 非管理员命中管理员受众一律 `false`；调用方必须丢弃事件而不是降级广播。
    ///
    /// 这是**受众本身**的判定，不含名称/载荷一致性——发布方要用的判定入口是
    /// [`EventEnvelope::visible_to`]（并联门禁），不要只调这个函数。
    pub fn visible_to(self, viewer: EventViewer) -> bool {
        match self {
            Self::AnySession => true,
            Self::Admins => viewer.is_admin,
            Self::User(user_id) => viewer.user_id == user_id,
        }
    }
}

/// 一次总线投递的载荷：命名事件 + `data:` JSON 对象 + 受众范围。
///
/// `Clone` 是 `tokio::sync::broadcast` 的硬要求；载荷只允许出现对象 ID、
/// revision/epoch 与**已授权可见**的字段，禁止携带任何凭据、会话令牌或未脱敏
/// 的原始错误。`audience` 是投递前的**硬过滤**依据：总线不做受众路由，
/// 由每条连接在发送循环里逐事件自查（[`crate::http_events`]）。
///
/// 字段私有 + 只有类型化构造（见模块头）：模块外既构造不出「名称/载荷/受众
/// 互相矛盾」的信封，也改写不了已构造信封的受众。
#[derive(Clone, Debug, PartialEq)]
pub struct EventEnvelope {
    name: EventName,
    data: Value,
    audience: EventAudience,
}

impl EventEnvelope {
    /// 唯一的落库构造点，**私有**：把「名称 ↔ 载荷形状 ↔ 受众」三者的配对
    /// 封闭在本模块的类型化构造里（[`EventEnvelope::device_updated`] 等），
    /// 不对外提供任意组合的通用构造。
    fn assembled(name: EventName, data: Value, audience: EventAudience) -> Self {
        Self {
            name,
            data,
            audience,
        }
    }

    /// 命名事件（SSE `event:` 字段取自 [`EventName::as_str`]）。
    pub fn name(&self) -> EventName {
        self.name
    }

    /// `data:` 的 JSON 载荷（只读借用：投递侧不得改写后转发）。
    pub fn data(&self) -> &Value {
        &self.data
    }

    /// 该事件的受众（只读）。
    pub fn audience(&self) -> EventAudience {
        self.audience
    }

    /// 这条事件对该连接是否可投递：受众匹配 **且** 名称门禁 **且** 名称/受众/
    /// 载荷三者一致，任一不满足都返回 `false`（静默跳过，不发送、不占水位）。
    ///
    /// 三道门禁是**并联**的（缺一条都可能被某一侧的疏漏绕过）：
    /// 1. [`EventName::admin_only`]：管理员专属事件名，非管理员一律不可见；
    /// 2. 受众是否覆盖该连接（[`EventAudience::visible_to`]）；
    /// 3. 一致性：非 `reset` 事件哪怕带 `AnySession` 也不可见；
    ///    `permissions.changed` 的载荷 `id` 必须就是受众本人（否则宁可丢弃，
    ///    也不把「写着 Alice、发给 Bob」的帧发到线上）。
    pub fn visible_to(&self, viewer: EventViewer) -> bool {
        if self.name.admin_only() && !viewer.is_admin {
            return false;
        }
        if !self.audience_matches_payload() {
            return false;
        }
        self.audience.visible_to(viewer)
    }

    /// 名称、受众与载荷是否自洽（见 [`EventEnvelope::visible_to`] 第 3 条）。
    fn audience_matches_payload(&self) -> bool {
        match self.audience {
            // 全局提示只允许 `reset`：其余四种都携带对象事实，出现这种配对
            // 说明构造点写错了，失败关闭。
            EventAudience::AnySession => self.name.allows_global_audience(),
            // 管理员受众采用**严格白名单**：只有 `device.updated`、
            // `task.updated`、`system.time.changed` 三种事件才允许挂在管理员
            // 受众上。`PermissionsChanged` 是**用户私有事实**（06 §4），只有
            // 目标用户本人受众（`EventAudience::User(user_id)`）才与它自洽，
            // 管理员受众绝不能接收 `PermissionsChanged`——否则「Alice 的授权
            // 投影被错标成管理员受众」会让管理员旁观者看到别人的私有授权变化；
            // `Reset` 不携带任何对象事实，只允许全局受众（`AnySession`），带
            // 管理员受众同样不自洽。
            EventAudience::Admins => matches!(
                self.name,
                EventName::DeviceUpdated | EventName::TaskUpdated | EventName::SystemTimeChanged
            ),
            // 授权投影事件的载荷 `id` 必须就是受众本人：受众与载荷 id 来自同一
            // 份字节，写错即整条丢弃（宁可不发也不串人）。
            EventAudience::User(user_id) => match self.name {
                EventName::PermissionsChanged => {
                    let expected = uuid::Uuid::from_bytes(user_id).to_string();
                    self.data
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id == expected)
                }
                // 设备/任务的载荷 id 是**对象** id，与受众本人没有等式可依；
                // 受众本身就是授权判定，由构造点保证不是全局。
                EventName::DeviceUpdated | EventName::TaskUpdated => {
                    !matches!(self.name, EventName::Reset)
                }
                // 时间事件钉死管理员受众，带用户受众即不自洽；`reset` 不带对象
                // 事实，出现用户受众同样不自洽。
                EventName::SystemTimeChanged | EventName::Reset => false,
            },
        }
    }

    /// `device.updated`：`{"kind":"device","id":<64hex>,"revision":<十进制字符串>}`。
    ///
    /// 受众是**对象级**受众（[`EventObjectAudience`]）：类型上不存在
    /// 「任何合法会话」这一档，设备事实要么发给管理员范围、要么发给某一个用户。
    pub fn device_updated(device_id: &str, revision: u64, audience: EventObjectAudience) -> Self {
        Self::assembled(
            EventName::DeviceUpdated,
            json!({
                "kind": "device",
                "id": device_id,
                "revision": decimal_u64_json(revision),
            }),
            audience.to_event_audience(),
        )
    }

    /// `task.updated`：`{"kind":"task","id":<uuid>,"revision":<十进制字符串>}`。
    ///
    /// 受众限定规则同 [`EventEnvelope::device_updated`]。
    pub fn task_updated(task_id: &str, revision: u64, audience: EventObjectAudience) -> Self {
        Self::assembled(
            EventName::TaskUpdated,
            json!({
                "kind": "task",
                "id": task_id,
                "revision": decimal_u64_json(revision),
            }),
            audience.to_event_audience(),
        )
    }

    /// `permissions.changed`：`{"kind":"permissions","id":<user uuid>,"epoch":<十进制字符串>}`。
    ///
    /// `epoch` 是授权 epoch（authz_epoch）：客户端据此判定旧授权投影失效，
    /// 清缓存并重新鉴权，而不是继续展示撤权前的数据（06 §4）。
    ///
    /// 授权投影是**用户私有**事实：受众不接受调用方给出，由本构造用
    /// `user_id` 这一份字节**同时**生成载荷 `id` 与 [`EventAudience::User`]，
    /// 两者不可能分别被改写（`permissions_changed(ALICE, n)` 在 API 上
    /// 不存在「受众写成 BOB」的写法）。
    pub fn permissions_changed(user_id: [u8; 16], authz_epoch: u64) -> Self {
        Self::assembled(
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
    /// 不新增第二套时间字段；受众不接受调用方给出，由本构造钉死为
    /// [`EventAudience::Admins`]（06 §4「时间事件仅管理员」，与
    /// [`EventName::admin_only`] 构成双重门禁）。
    pub fn system_time_changed(evidence: &TimeEvidence) -> Self {
        Self::assembled(
            EventName::SystemTimeChanged,
            json!({
                "kind": "system_time",
                "evidence": serde_json::to_value(evidence).unwrap_or(Value::Null),
            }),
            EventAudience::Admins,
        )
    }

    /// `reset`：`{"kind":"reset","reason":<稳定去敏原因码>}`。
    ///
    /// 只含一个稳定原因码、不含任何对象事实，因此它是唯一允许「任何合法会话」
    /// 受众的事件（见 [`EventName::allows_global_audience`]）。
    pub fn reset(reason: &str) -> Self {
        Self::assembled(
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
    ///
    /// 受众不是这里的运行期判断点：非法受众组合在类型上构造不出来（见
    /// [`EventEnvelope`]），而任何漏网的不一致都由投递侧
    /// [`EventEnvelope::visible_to`] 失败关闭，绝不进入任何用户的流。
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
    fn only_reset_may_carry_a_global_audience() {
        // 全局受众的白名单是单一事实来源：五种名称里只有 `reset`。
        assert!(EventName::Reset.allows_global_audience());
        assert!(!EventName::DeviceUpdated.allows_global_audience());
        assert!(!EventName::TaskUpdated.allows_global_audience());
        assert!(!EventName::PermissionsChanged.allows_global_audience());
        assert!(!EventName::SystemTimeChanged.allows_global_audience());
    }

    #[test]
    fn payloads_carry_kind_id_and_decimal_revision() {
        let alice = [7u8; 16];
        let device =
            EventEnvelope::device_updated(&"ab".repeat(32), 12, EventObjectAudience::user(alice));
        assert_eq!(device.name(), EventName::DeviceUpdated);
        assert_eq!(device.audience(), EventAudience::User(alice));
        assert_eq!(device.data()["kind"], json!("device"));
        assert_eq!(device.data()["id"], json!("ab".repeat(32)));
        assert_eq!(device.data()["revision"], json!("12"));

        let task = EventEnvelope::task_updated(
            "0e7f1c2e-0000-4000-8000-000000000001",
            7,
            EventObjectAudience::admins(),
        );
        assert_eq!(task.data()["kind"], json!("task"));
        assert_eq!(task.data()["revision"], json!("7"));
        assert_eq!(task.audience(), EventAudience::Admins);

        let perms = EventEnvelope::permissions_changed(alice, u64::MAX);
        assert_eq!(perms.data()["kind"], json!("permissions"));
        // 载荷 id 就是受众本人：canonical 小写 UUID。
        assert_eq!(
            perms.data()["id"],
            json!(uuid::Uuid::from_bytes(alice).to_string())
        );
        // 十进制字符串，不经 JS number，u64 上界不丢精度。
        assert_eq!(perms.data()["epoch"], json!(u64::MAX.to_string()));

        let time = EventEnvelope::system_time_changed(&fixture_evidence());
        assert_eq!(time.data()["kind"], json!("system_time"));
        assert_eq!(time.data()["evidence"]["quality"], json!("ntp_valid"));
        assert_eq!(time.data()["evidence"]["sample_age_ms"], json!("1500"));

        let reset = EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW);
        assert_eq!(reset.name(), EventName::Reset);
        assert_eq!(reset.data()["reason"], json!("buffer_overflow"));
    }

    #[test]
    fn object_events_pin_their_audience_at_construction() {
        let alice = [7u8; 16];
        // 授权投影事件的受众由构造钉死为本人，发布方无法把它发成全局事件。
        assert_eq!(
            EventEnvelope::permissions_changed(alice, 1).audience(),
            EventAudience::User(alice)
        );
        // 时间事件的受众钉死为管理员，与 EventName::admin_only 构成双重门禁。
        assert_eq!(
            EventEnvelope::system_time_changed(&fixture_evidence()).audience(),
            EventAudience::Admins
        );
        // 唯一免受众构造是 reset：它不含任何对象事实。
        assert_eq!(
            EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW).audience(),
            EventAudience::AnySession
        );
    }

    #[test]
    fn object_audience_has_no_global_variant_so_object_events_are_never_broadcast() {
        // 对象级受众的两个取值都必须投影成**非全局**受众：类型里没有
        // 「任何合法会话」，所以设备/任务事件在任何构造下都不可能全局可见。
        let alice = [7u8; 16];
        for audience in [
            EventObjectAudience::admins(),
            EventObjectAudience::user(alice),
        ] {
            let device = EventEnvelope::device_updated(&"ab".repeat(32), 1, audience);
            let task = EventEnvelope::task_updated("t-1", 1, audience);
            assert_ne!(device.audience(), EventAudience::AnySession);
            assert_ne!(task.audience(), EventAudience::AnySession);
        }
        // 反向证据：连「任何合法会话」这个受众值本身，对普通连接也只允许
        // `reset` 出现在流上（见 forged_pairing_* 与 visible_to）。
        let viewer = EventViewer {
            user_id: alice,
            is_admin: false,
        };
        assert!(EventEnvelope::reset(RESET_REASON_BUFFER_OVERFLOW).visible_to(viewer));
    }

    #[test]
    fn permissions_payload_and_audience_share_one_source() {
        // 载荷 id 与受众由同一份字节生成：二者永远指同一个人。
        let alice = [7u8; 16];
        let envelope = EventEnvelope::permissions_changed(alice, 12);
        let EventAudience::User(audience) = envelope.audience() else {
            panic!(
                "授权投影事件的受众必须是用户本人：{:?}",
                envelope.audience()
            );
        };
        assert_eq!(audience, alice);
        assert_eq!(
            envelope.data()["id"]
                .as_str()
                .expect("载荷 id 必须是字符串"),
            uuid::Uuid::from_bytes(audience).to_string()
        );
        // 一致性自洽：这条事件对 alice 可见、对 bob 不可见。
        assert!(envelope.visible_to(EventViewer {
            user_id: alice,
            is_admin: false
        }));
        assert!(!envelope.visible_to(EventViewer {
            user_id: [8u8; 16],
            is_admin: false
        }));
    }

    #[test]
    fn forged_pairings_are_never_deliverable() {
        // 模块**内**用私有构造点伪造「名称/载荷/受众互相矛盾」的信封：证明
        // 即便将来新增构造点写错配对，投递判定也只会被静默丢弃，不会漏给任何人
        // （本模块外根本写不出这些对象，见 EventEnvelope 的私有字段）。
        let alice = [7u8; 16];
        let bob = [8u8; 16];
        let alice_viewer = EventViewer {
            user_id: alice,
            is_admin: false,
        };
        let bob_viewer = EventViewer {
            user_id: bob,
            is_admin: false,
        };
        let admin_viewer = EventViewer {
            user_id: bob,
            is_admin: true,
        };

        // 1) 载荷写着 alice、受众却是 bob：两个人都必须收不到。
        let mismatched = EventEnvelope {
            name: EventName::PermissionsChanged,
            data: json!({
                "kind": "permissions",
                "id": uuid::Uuid::from_bytes(alice).to_string(),
                "epoch": "12",
            }),
            audience: EventAudience::User(bob),
        };
        assert!(
            !mismatched.visible_to(bob_viewer),
            "载荷/受众不一致不得发送"
        );
        assert!(!mismatched.visible_to(alice_viewer));
        assert!(!mismatched.visible_to(admin_viewer));

        // 2) 对象事件带全局受众：三个视角都不可见（不是「广播放行」）。
        for name in [
            EventName::DeviceUpdated,
            EventName::TaskUpdated,
            EventName::PermissionsChanged,
        ] {
            let forged = EventEnvelope {
                name,
                data: json!({"kind": "forged", "id": "obj-1", "revision": "1"}),
                audience: EventAudience::AnySession,
            };
            assert!(
                !forged.visible_to(alice_viewer) && !forged.visible_to(admin_viewer),
                "{name:?} 携带全局受众必须失败关闭"
            );
        }

        // 3) 时间事件带用户受众（绕开 admin_only 的写法）：同样不可见。
        let forged_time = EventEnvelope {
            name: EventName::SystemTimeChanged,
            data: json!({"kind": "system_time"}),
            audience: EventAudience::User(bob),
        };
        assert!(!forged_time.visible_to(bob_viewer));
        assert!(!forged_time.visible_to(admin_viewer));

        // 4) 授权投影是用户私有事实：哪怕被错标成管理员受众（`PermissionsChanged
        //    + EventAudience::Admins`），管理员受众的严格白名单也必须把它失败
        //    关闭——Alice 的授权投影绝不能被管理员旁观者看到，本人及其他普通
        //    用户同样收不到（宁可不发也不串人、不旁落）。
        let forged_perms_to_admins = EventEnvelope {
            name: EventName::PermissionsChanged,
            data: json!({
                "kind": "permissions",
                "id": uuid::Uuid::from_bytes(alice).to_string(),
                "epoch": "12",
            }),
            audience: EventAudience::Admins,
        };
        assert!(
            !forged_perms_to_admins.visible_to(alice_viewer),
            "授权投影即便被标成管理员受众也不得对本人可见（管理员受众不含 PermissionsChanged）"
        );
        assert!(
            !forged_perms_to_admins.visible_to(bob_viewer),
            "授权投影即便被标成管理员受众也不得对他人可见"
        );
        assert!(
            !forged_perms_to_admins.visible_to(admin_viewer),
            "授权投影即便被标成管理员受众也绝不能被管理员旁观者看到"
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
    fn envelope_visible_to_pins_the_admin_only_name_gate() {
        let alice = [7u8; 16];
        let admin_viewer = EventViewer {
            user_id: alice,
            is_admin: true,
        };
        // 管理员范围的对象事件：管理员可见，普通用户不可见（受众与名称两道门禁
        // 都不构成旁路）。
        let admins_only =
            EventEnvelope::device_updated(&"cd".repeat(32), 1, EventObjectAudience::admins());
        assert!(admins_only.visible_to(admin_viewer));
        assert!(!admins_only.visible_to(EventViewer {
            user_id: alice,
            is_admin: false
        }));
        // 时间事件由构造钉死管理员受众：管理员视角可见、普通用户不可见。
        let time = EventEnvelope::system_time_changed(&fixture_evidence());
        assert!(time.visible_to(admin_viewer));
        assert!(!time.visible_to(EventViewer {
            user_id: alice,
            is_admin: false
        }));
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
            EventObjectAudience::user([7u8; 16]),
        ));
        assert_eq!(delivered, 2);
        assert_eq!(first.try_recv().unwrap().name(), EventName::TaskUpdated);
        assert_eq!(second.try_recv().unwrap().name(), EventName::TaskUpdated);
        // 订阅只收「之后」的事件，不回放历史。
        assert_eq!(
            bus.publish(EventEnvelope::task_updated(
                "t-2",
                2,
                EventObjectAudience::user([7u8; 16])
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
                    EventObjectAudience::user([7u8; 16])
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
                EventObjectAudience::user([7u8; 16])
            )),
            1
        );
        assert_eq!(rx.try_recv().unwrap().name(), EventName::DeviceUpdated);
    }
}
