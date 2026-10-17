//! devices 资源管理路由骨架：网关/实时探针契约与空 router（路由挂载属 Task 10）。

use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::Permission;
use crate::devices::repository::DeviceAdminView;
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

/// 仅 HTTP 视图，不创建第二套 wire DTO。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceConnectionState {
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStreamHealth {
    Starting,
    Healthy,
    Degraded,
    Offline,
}

#[derive(Clone, Debug)]
pub struct DeviceLiveView {
    pub connection_state: DeviceConnectionState,
    pub control_health: DeviceStreamHealth,
    pub data_health: DeviceStreamHealth,
    /// Transport 真实 typed 缓存适配器按唯一共享 DTO 做 02 语义投影；未知样本为 `None`。
    /// `Value` 不是原始 wire 入口，也不是未经验证的任意设备 JSON。
    pub capabilities: Option<serde_json::Value>,
}

/// 实时视图探针：同一次调用取得连接状态与双流健康，读取失败必须失败上抛（503），
/// 不得降级为 offline/空能力 200。
pub trait DeviceLiveProbe: Send + Sync {
    fn snapshot<'a>(&'a self, id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView>;
}

#[derive(Clone)]
pub struct DevicesApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn DevicesGateway>,
    pub live_probe: Arc<dyn DeviceLiveProbe>,
}

/// `DeviceAdminView`/`HashSet` 来自 repository/std，列表筛选不得静默忽略。
pub trait DevicesGateway: Send + Sync {
    fn list_devices_visible<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 32]>,
        limit: usize,
        group_id: Option<Uuid>,
        search: Option<&'a str>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn find_device<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
    ) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
        display_name: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET /api/v1/devices`、`GET /api/v1/devices/{id}` 与
/// `PATCH /api/v1/devices/{id}` 由 Task 10 注册。
pub fn devices_router(state: DevicesApiState) -> Router<()> {
    Router::new().with_state(state)
}
