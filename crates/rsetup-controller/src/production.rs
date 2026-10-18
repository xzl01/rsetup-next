//! 生产路由装配（集成验收 Task 0a 纯拓扑合并 + Task 0b typed 生产装配）。
//!
//! 本模块是中控五线（auth / management / tasks / status / assets）**唯一**的顶层
//! 装配点：
//! - [`compose_component_routers`] 只做纯拓扑合并，不额外注册探活路径，路径集合
//!   归各条线所有，组件合并顺序无业务语义；
//! - [`build_production_router`] 接收各条线已构造好的 typed 状态，先强制校验五线
//!   共享**同一个** `Arc<HttpAuthState>`（同一 session/CSRF/限流上下文），再合并。
//!
//! 指针一致性是硬门禁：任一条线持有不同的 `HttpAuthState` 实例，就意味着两条线
//! 各自维护会话预过滤、CSRF 绑定表与限速桶——登录态、CSRF 与限速在不同端点之间
//! 不再互相可见。这种情况一律 `Config` 失败关闭，绝不静默装配出多认证上下文的
//! 进程。

use std::sync::Arc;

use axum::Router;

use crate::ControllerError;
use crate::api::{ManagementState, build_management_router};
use crate::http_auth::{self, AppState};
use crate::http_device_status::{DeviceStatusState, build_device_status_router};
use crate::tasks::http::{HttpTasksState, build_tasks_router};

/// 生产顶层 router 的全部 typed 输入：五线状态各自由所属条线的生产适配器构造，
/// 本模块不构造任何默认源、测试网关或占位池。
pub struct ProductionInputs {
    /// Identity 认证线（含 `/healthz`、`/readyz` 探活）。
    pub auth: AppState,
    /// Identity 管理线（26 条管理路由）。
    pub management: ManagementState,
    /// Runtime 任务线（8 条任务路由）。
    pub tasks: HttpTasksState,
    /// 设备状态读线（1 条状态路由）。
    pub status: DeviceStatusState,
    /// Web 嵌入静态资源线。
    pub web: Router<()>,
}

/// 唯一生产装配入口：校验共享认证上下文指针一致后合并五线子 router。
///
/// 不一致时返回去敏固定 `Config`（不回显任何指针/地址值），进程应在触达监听套接字
/// 之前失败关闭。
pub fn build_production_router(inputs: ProductionInputs) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
    {
        return Err(ControllerError::Config(
            "production auth context mismatch".into(),
        ));
    }
    Ok(compose_component_routers(
        http_auth::build_http_router(inputs.auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status),
        inputs.web,
    ))
}

/// 把 auth / management / tasks / status / web 五个组件子 router 合并为顶层路由。
pub fn compose_component_routers(
    auth: Router<()>,
    management: Router<()>,
    tasks: Router<()>,
    status: Router<()>,
    web: Router<()>,
) -> Router<()> {
    auth.merge(management).merge(tasks).merge(status).merge(web)
}
