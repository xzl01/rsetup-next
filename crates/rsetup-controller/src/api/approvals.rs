//! approvals 资源管理路由骨架：网关/通知 sink 契约与空 router（路由挂载属 Task 11）。

use std::sync::Arc;

use axum::Router;

use crate::auth::service::Session;
use crate::devices::service::AdmissionChangeEvent;
use crate::error::ControllerError;
use crate::http_auth::HttpAuthState;
use crate::model::ReviewDecision;

use super::ManagementFuture;
use super::devices::DeviceLiveProbe;

#[derive(Clone, Copy, Debug)]
pub struct ApprovalTarget {
    pub device_id: [u8; 32],
    pub expected_revision: u64,
}

/// 只消费已 commit 的 typed `AdmissionChangeEvent`，不接受客户端伪造事件；
/// 与既有同步 `AdmissionChangeSink` 接线兼容的可失败边界。
pub trait ManagementNotificationSink: Send + Sync {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ControllerError>;
}

#[derive(Clone)]
pub struct ApprovalsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn ApprovalsGateway>,
    pub notification_sink: Arc<dyn ManagementNotificationSink>,
    /// 审批 `connection_state` 的唯一消费通道：与 devices 侧同源的真实探针实例，
    /// 不新造第二套探针类型、不用常量 offline 源或 `Option` 缺省。
    pub probe: Arc<dyn DeviceLiveProbe>,
}

pub trait ApprovalsGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        decision: Option<ReviewDecision>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn approve<'a>(
        &'a self,
        actor: &'a Session,
        targets: &'a [ApprovalTarget],
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reject<'a>(
        &'a self,
        actor: &'a Session,
        targets: &'a [ApprovalTarget],
        reason: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reopen<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn revoke<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
        reason: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reauthorize<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。审批 list/approve/reject/reopen/revoke/reauthorize
/// 由 Task 11 注册。
pub fn approvals_router(state: ApprovalsApiState) -> Router<()> {
    Router::new().with_state(state)
}
