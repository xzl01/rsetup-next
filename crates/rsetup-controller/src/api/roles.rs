//! roles 资源管理路由骨架：网关契约与空 router（路由挂载属 Task 8）。

use std::sync::Arc;

use axum::Router;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::Permission;
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone)]
pub struct RolesApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn RolesGateway>,
}

pub trait RolesGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        name: &'a str,
        permissions: &'a [Permission],
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        name: Option<&'a str>,
        permissions: Option<&'a [Permission]>,
        archived: Option<bool>,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET/POST /api/v1/roles` 与 `PATCH /api/v1/roles/{id}`
/// 由 Task 8 注册。
pub fn roles_router(state: RolesApiState) -> Router<()> {
    Router::new().with_state(state)
}
