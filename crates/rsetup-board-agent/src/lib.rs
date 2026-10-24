//! 板端代理：只读白名单分发、任务 journal、票据与无参 OS 重启边界，
//! 以及**生产主动出站连接**（`connect` / `session_handler` / `host_source`）。
//!
//! 本 crate 自身不包含任何密码学实现：握手、加密记录层与会话驱动都来自 `rsetup-tunnel`。
//! 网络能力只存在于 `connect` 的显式装配路径（生产 `--connect`）：配置必须来自
//! [`connect_config::BoardConnectConfig`] 的 **validated** 构造入口（显式 pin 列表，
//! 无 TOFU），身份密钥由隧道 `IdentityKey` 按 0600/owner/symlink 规则校验，
//! journal 走真实 `RealFs` 持久与恢复路径。
//!
//! 历史说法已更新：业务 codec 已按所有者授权启用，`--connect` 也会建立真实加密会话。
//! `--once` / `--watch` / `--loopback-test-server` 仍是**本地非生产**模式，
//! 只用于本机验收，不构成与中控互通的证据。

pub mod actions;
pub mod connect;
pub mod connect_config;
pub mod dispatch;
pub mod host_source;
pub mod journal;
pub mod read_only;
pub mod session_handler;
#[cfg(any(test, feature = "test-seam"))]
pub mod test_seam;
pub mod tickets;
pub mod tunnel_binding;

pub use actions::{
    ACTION_ALLOWLIST, ActionKind, ActionSpec, REBOOT_EXECUTE_ACTION, TASK_RESULT_EVENT_ACTION,
    allowed_action_names, is_reserved, lookup_action,
};
pub use dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, ExecuteOutcome, HostRebootOs, JournalEpochSource,
    RebootDispatcher, RebootExecutor, RebootOs, RecordingOs, TaskJournalReader,
};
pub use journal::{
    DedupOutcome, EvidenceMerge, Fs, JOURNAL_MAX_RECORDS, JOURNAL_SCHEMA_VERSION, JournalError,
    JournalRecovery, JournalStore, RealFs, TaskJournal, TaskRecord, TaskState,
};
pub use read_only::{PeerContext, ReadOnlyMutationGuard, ReadOnlySource};
pub use tickets::{
    TICKET_TOKEN_BYTES, TICKET_TTL_MS, TICKET_TTL_SECS, TicketBinding, TicketManager, TicketOrigin,
    TicketToken,
};
pub use tunnel_binding::{ApprovalGate, DualStreamBinding, StreamError, StreamState, StreamType};
