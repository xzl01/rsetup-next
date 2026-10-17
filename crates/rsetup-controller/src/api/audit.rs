//! audit 资源管理路由骨架：网关契约与空 router（路由挂载属 Task 12）。

use std::sync::Arc;

use axum::Router;

use crate::auth::service::Session;
use crate::devices::repository::{AuditCursor, AuditFilter};
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone)]
pub struct AuditApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn AuditGateway>,
}

/// `AuditCursor` 是 Task 3 审计游标段定义的复合键（process_epoch/event_seq/id），
/// 不能仅 event_seq。
pub trait AuditGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        filter: AuditFilter,
        cursor: Option<AuditCursor>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET /api/v1/audit` 由 Task 12 注册。
pub fn audit_router(state: AuditApiState) -> Router<()> {
    Router::new().with_state(state)
}
