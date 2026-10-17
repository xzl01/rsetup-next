//! users 资源管理路由骨架：网关契约与空 router（路由挂载属 Task 7）。

use std::sync::Arc;

use axum::Router;

use crate::auth::service::Session;
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone)]
pub struct UsersApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn UsersGateway>,
}

/// users 资源网关契约：所有方法显式借用 actor，返回已校验的 HTTP 响应投影。
/// `Value` 不是让 HTTP 传任意 SQL/任意操作的入口；生产 adapter 用固定投影函数构造。
pub trait UsersGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 16]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        username: &'a str,
        display_name: &'a str,
        is_admin: bool,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
        revision: u64,
        active: Option<bool>,
        is_admin: Option<bool>,
        display_name: Option<&'a str>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reset_password<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn effective_permissions<'a>(
        &'a self,
        actor: &'a Session,
        target: [u8; 16],
        device_id: Option<[u8; 32]>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET/POST /api/v1/users`、`PATCH /api/v1/users/{id}`、
/// `POST /api/v1/users/{id}/reset-password` 与
/// `GET /api/v1/users/{id}/effective-permissions` 由 Task 7 注册。
pub fn users_router(state: UsersApiState) -> Router<()> {
    Router::new().with_state(state)
}
