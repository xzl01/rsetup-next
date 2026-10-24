//! 会话层：trace 去重、健康探测、HTTP/2 长流编解码与双流会话驱动。
//!
//! 依赖方向自下而上：`types`/`pending`/`probe`/`streams` 是无状态或小状态构件，
//! `driver` 才把它们组装成拥有任务的会话。`handle` 是对外契约（句柄、handler
//! trait、状态与关闭原因），`config` 是预算与期限的唯一来源。
pub mod config;
pub mod driver;
pub mod handle;
pub mod pending;
pub mod probe;
pub mod streams;
pub mod types;

pub use config::{SessionConfig, SessionError};
pub use driver::{start_client_session, start_server_session};
pub use handle::{
    HandlerFuture, RequestContext, SessionCloseReason, SessionHandle, SessionHandler,
    SessionOutcome, SessionState, SystemSignal,
};
pub use pending::{
    MAX_PENDING, Matched, PROBE_RESERVATION, PendingKind, PendingTable, RequestKey, next_trace_id,
};
pub use probe::{PROBE_WINDOW, ProbeDecision, ProbeRequest, ProbeState};
pub use streams::{
    CONTROL_PATH, DATA_PATH, StreamError, StreamReceiver, StreamSender, accept_server_stream,
    open_client_stream,
};
pub use types::{Reply, RequestError, StreamEpoch};
