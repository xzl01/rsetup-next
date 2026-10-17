//! 生产路由装配（集成验收 Task 0a：纯拓扑合并）。
//!
//! 本模块只负责把各条线已经构造好的组件子 router 合并成唯一的生产顶层 router：
//! 不额外注册探活路径，路径集合归各条线所有，组件合并顺序无业务语义。
//! typed 生产输入（`ProductionInputs`/`build_production_router`）属 Task 0b，
//! 在上游真实源就绪前不在本模块出现。

use axum::Router;

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
