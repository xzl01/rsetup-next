//! grants 资源管理路由骨架：网关契约与空 router（路由挂载属 Task 8）。

use std::sync::Arc;

use axum::Router;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{GrantScope, GrantSource};
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone)]
pub struct GrantsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn GrantsGateway>,
}

/// `GrantSource`/`GrantScope` 来自唯一 policy 定义，不另造权限点。
pub trait GrantsGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        user_id: Option<[u8; 16]>,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        source: GrantSource,
        scope: GrantScope,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn delete<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
    ) -> ManagementFuture<'a, ()>;
}

/// 骨架：挂载 0 条路由。`GET/POST /api/v1/grants` 与 `DELETE /api/v1/grants/{id}`
/// 由 Task 8 注册。
pub fn grants_router(state: GrantsApiState) -> Router<()> {
    Router::new().with_state(state)
}
