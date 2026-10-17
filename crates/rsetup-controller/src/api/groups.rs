//! groups 资源管理路由骨架：网关契约与空 router（路由挂载属 Task 9）。

use std::sync::Arc;

use axum::Router;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone)]
pub struct GroupsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn GroupsGateway>,
}

pub trait GroupsGateway: Send + Sync {
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
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        name: Option<&'a str>,
        archived: Option<bool>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn replace_members<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        devices: &'a [[u8; 32]],
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET/POST /api/v1/groups`、`PATCH /api/v1/groups/{id}` 与
/// `PUT /api/v1/groups/{id}/members` 由 Task 9 注册。
pub fn groups_router(state: GroupsApiState) -> Router<()> {
    Router::new().with_state(state)
}
