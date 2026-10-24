//! 板端 transport 线：已验签设备准入（`admission`）、六方法 RPC 适配（`board_client`）、
//! 已认证连接管理（`manager`）与生产监听装配（`listener`）。
//!
//! 本模块的存在不代表隧道已交付：真实监听、双进程与真实库验收见
//! `docs/testing/tunnel-controller-manager-report.md`。
pub mod admission;
pub mod board_client;
pub mod limits;
pub mod listener;
pub mod manager;

pub use listener::{
    ACTION_TASK_RESULT, BridgeStats, ListenerError, ListenerStats, RejectNoTaskResultSink,
    STATUS_UNSUPPORTED_ACTION, SessionConnection, SqlxTaskResultSink, TaskResultSink,
    TaskResultSinkStats, TransportConfig, TransportEnv, TunnelSessionHandler,
    run_controller_listener, run_controller_listener_with_sink, run_transport_listener,
    run_transport_listener_with_sink, run_transport_listener_with_stats,
};
pub use manager::{
    AdmissionTruth, CallError, CancelSignal, CancellableAdmissionPolicy, ConnectionState,
    DeviceConnection, HalfOpenPermit, ManagerConfig, NewSession, PendingTicket, RegisterError,
    RegisterOutcome, RevokeOutcome, SampleOutcome, SystemNotice, TaskResultContext,
    TransportManager,
};
