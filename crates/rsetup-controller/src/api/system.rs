//! system 资源管理路由骨架：网关/恢复模式探针契约与空 router（路由挂载属 Task 12）。

use std::sync::Arc;

use axum::Router;

use crate::auth::service::Session;
use crate::http_auth::HttpAuthState;

use super::ManagementFuture;

#[derive(Clone, Copy, Debug)]
pub struct RecoveryView {
    pub ready: bool,
    pub mode: &'static str,
}

/// 恢复模式探针是 Runtime 真实控制器的可失败读取，不是静态字符串默认值：
/// `ready`/`mode` 必须来自 Runtime 当前启动/DB/恢复控制器真实快照，不可读时上抛错误
/// 由 handler 映射 503 `NOT_READY`。
pub trait RecoveryModeProbe: Send + Sync {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView>;
}

#[derive(Clone)]
pub struct SystemApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn SystemGateway>,
    pub recovery_probe: Arc<dyn RecoveryModeProbe>,
}

/// 即使只是统计也必须把 actor 送到仓储的当前管理员检查。
pub trait SystemGateway: Send + Sync {
    fn status<'a>(&'a self, actor: &'a Session) -> ManagementFuture<'a, serde_json::Value>;
}

/// 骨架：挂载 0 条路由。`GET /api/v1/system/status` 由 Task 12 注册。
pub fn system_router(state: SystemApiState) -> Router<()> {
    Router::new().with_state(state)
}
