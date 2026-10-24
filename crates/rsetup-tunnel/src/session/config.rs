//! 会话驱动的预算与期限配置（protocol_spec.md §6.4/§6.6/§6.7 与计划 Task4 裁决）。
//!
//! 这里只描述**有界性**：业务期限、未决预算、业务队列深度与字节预算、handler
//! 并发预算、排空期限。探测窗口（15s）、连接级保活窗口、开流期限都来自协议常量
//! （见 [`super::probe::PROBE_WINDOW`]），不是可放大项，因此不在本结构里。
use crate::envelope::MAX_ENVELOPE_SIZE;
use crate::session::pending::{MAX_PENDING, PROBE_RESERVATION};
use crate::session::probe::PROBE_WINDOW;
use std::fmt;
use std::time::Duration;

/// 规范默认普通请求期限：5s（§6.4）。**不得**为绕过抖动而放大本默认值。
pub const DEFAULT_ORDINARY_TIMEOUT: Duration = Duration::from_secs(5);
/// 规范最小排空期限：GOAWAY 后至少 5s（§6.6）。可加长，不可缩短。
pub const MIN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 单条流业务队列上限（条目数）。
pub const MAX_BUSINESS_QUEUE: usize = 32;
/// handler future 并发预算上限（§6.7.2：接收端处理队列必须有限）。
pub const MAX_HANDLER_CONCURRENCY: usize = 16;
/// 默认每连接业务队列字节预算：4 MiB（整连接共享，非按设备预分配）。
pub const DEFAULT_BUSINESS_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// 字节预算上限：64 MiB，防把「每连接」预算写成隐式无限。
pub const MAX_BUSINESS_QUEUE_BYTES: usize = 64 * 1024 * 1024;
/// 默认事件队列深度（有界，队满丢弃并计数）。
pub const DEFAULT_EVENT_QUEUE: usize = 64;
/// 事件队列深度上限。
pub const MAX_EVENT_QUEUE: usize = 1024;
/// 默认生命周期回调队列深度。
pub const DEFAULT_NOTIFY_QUEUE: usize = 16;
/// 生命周期回调队列深度上限。
pub const MAX_NOTIFY_QUEUE: usize = 64;

/// HTTP/2 双流通用的窗口/帧参数（§6.1 建议 ≥1MiB；帧上限 16KiB 与 gRPC 分片一致）。
pub const H2_INITIAL_WINDOW: u32 = 1024 * 1024;
/// 连接级流控窗口。
pub const H2_INITIAL_CONNECTION_WINDOW: u32 = 1024 * 1024;
/// 单个 DATA 帧上限：16KiB。
pub const H2_MAX_FRAME_SIZE: u32 = 16 * 1024;
/// HEADERS 块上限：8KiB。
pub const H2_MAX_HEADER_LIST_SIZE: u32 = 8 * 1024;
/// 同连接并发流上限：2 条长流 + 顶替余量（计划 Task4：server 最大并发可为 4）。
pub const H2_MAX_CONCURRENT_STREAMS: u32 = 4;

/// 会话驱动配置。`Default` 即规范默认值；所有字段都有硬上/下限，
/// 越界一律 [`SessionError::InvalidConfig`]，不允许“配错就静默放宽”。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// 普通请求期限：从**入队那一刻**起算（排队、写阻塞都不暂停）。
    /// 默认 5s；上限取探测窗口 15s，禁止把业务期限配成比 probe 窗口还长。
    pub ordinary_timeout: Duration,
    /// 本端该连接的未决总预算（含两条流本端 probe），<=1024。
    /// 业务实际可用 `max_pending - PROBE_RESERVATION`，即默认 1022。
    pub max_pending: usize,
    /// 单条流业务队列深度（条目数），1..=32；队满直接 `Busy`。
    pub business_queue: usize,
    /// 整连接业务队列字节预算（两条流共享），单位字节。
    pub business_queue_bytes: usize,
    /// handler future 并发预算，1..=16；超出直接回忙而不排队。
    pub handler_concurrency: usize,
    /// GOAWAY 后至少维持的排空期限，>=5s。
    pub drain_timeout: Duration,
    /// 事件回调队列深度，1..=1024；队满丢弃事件并计数，绝不阻塞 reader。
    pub event_queue: usize,
    /// 生命周期回调（on_ready/on_system/on_closed）队列深度，1..=64。
    /// 与事件队列分开：事件洪峰不能把 ready/system 通知挤出回调通道。
    pub notify_queue: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ordinary_timeout: DEFAULT_ORDINARY_TIMEOUT,
            max_pending: MAX_PENDING,
            business_queue: MAX_BUSINESS_QUEUE,
            business_queue_bytes: DEFAULT_BUSINESS_QUEUE_BYTES,
            handler_concurrency: MAX_HANDLER_CONCURRENCY,
            drain_timeout: MIN_DRAIN_TIMEOUT,
            event_queue: DEFAULT_EVENT_QUEUE,
            notify_queue: DEFAULT_NOTIFY_QUEUE,
        }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 只暴露预算数字，不含任何凭据/地址。
        f.debug_struct("SessionConfig")
            .field("ordinary_timeout", &self.ordinary_timeout)
            .field("max_pending", &self.max_pending)
            .field("business_queue", &self.business_queue)
            .field("business_queue_bytes", &self.business_queue_bytes)
            .field("handler_concurrency", &self.handler_concurrency)
            .field("drain_timeout", &self.drain_timeout)
            .field("event_queue", &self.event_queue)
            .field("notify_queue", &self.notify_queue)
            .finish()
    }
}

/// 单个包计入字节预算时附加的固定开销（信封 + gRPC 帧头 + 通道常驻成本）。
pub const PACKET_OVERHEAD_BYTES: usize = 1024;

/// 会话入口错误：配置越界，或 h2 连接在建立阶段即失败。
///
/// 固定文案：不携带底层错误文本、不含对端可控内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("session config rejected: {0}")]
    InvalidConfig(&'static str),
    #[error("h2 session could not be established")]
    Transport,
}

impl SessionConfig {
    /// 一个业务包占用的预算字节数：payload + 固定开销。
    pub(crate) fn queued_bytes(payload_len: usize) -> usize {
        payload_len.saturating_add(PACKET_OVERHEAD_BYTES)
    }

    /// 校验全部预算边界。任何一条不满足都拒绝启动会话，
    /// 而不是内部 clamp 后再跑——静默放宽会让两侧对同一连接得出不同预算结论。
    pub fn validate(&self) -> Result<(), SessionError> {
        let invalid = |reason| Err(SessionError::InvalidConfig(reason));
        if self.ordinary_timeout.is_zero() || self.ordinary_timeout > PROBE_WINDOW {
            return invalid("ordinary_timeout must be in (0, 15s]");
        }
        if !(PROBE_RESERVATION..=MAX_PENDING).contains(&self.max_pending) {
            return invalid("max_pending must cover probe reservation and stay <=1024");
        }
        if self.business_queue == 0 || self.business_queue > MAX_BUSINESS_QUEUE {
            return invalid("business_queue must be in 1..=32");
        }
        if self.business_queue_bytes < MAX_ENVELOPE_SIZE
            || self.business_queue_bytes > MAX_BUSINESS_QUEUE_BYTES
        {
            return invalid("business_queue_bytes must hold one max envelope and stay bounded");
        }
        if self.handler_concurrency == 0 || self.handler_concurrency > MAX_HANDLER_CONCURRENCY {
            return invalid("handler_concurrency must be in 1..=16");
        }
        if self.drain_timeout < MIN_DRAIN_TIMEOUT {
            return invalid("drain_timeout must be at least 5s");
        }
        if self.event_queue == 0 || self.event_queue > MAX_EVENT_QUEUE {
            return invalid("event_queue must be in 1..=1024");
        }
        if self.notify_queue == 0 || self.notify_queue > MAX_NOTIFY_QUEUE {
            return invalid("notify_queue must be in 1..=64");
        }
        Ok(())
    }
}
