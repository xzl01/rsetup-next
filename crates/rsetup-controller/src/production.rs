//! 生产路由装配（Task 0a 纯拓扑合并 + Task 0b typed 生产装配 + 指标线六线合并）。
//!
//! 本模块是中控七线（auth / management / tasks / status / events / metrics / web）
//! **唯一**的顶层装配点：
//! - [`compose_component_routers`] 只做纯拓扑合并，不额外注册探活路径，路径集合
//!   归各条线所有，组件合并顺序无业务语义；
//! - [`build_production_router`] 接收各条线已构造好的 typed 状态，先强制校验原四线
//!   共享**同一个** `Arc<HttpAuthState>`，再合并。指标线**没有** `HttpAuthState`，
//!   不参与也无法违反该校验（I3：独立认证边界）。
//!
//! 指针一致性是硬门禁（保持原样，只覆盖原四线）。指标线的连接池唯一性由**结构性
//! 收缩 + 契约 + 静态门禁**保证：`MetricsLineInputs` 里既没有 `DbPool` 也没有汇总源，
//! 两者都只能在装配函数内部由 `inputs.auth.db` 派生（I2）。诚实界定：这是装配约定
//! 加上 §7.6 第 9 条的源码级回归与 §7.8 的 grep 静态门禁共同执法的**架构契约**，
//! 不是"编译期不可能违反"的绝对结论。

use std::sync::Arc;

use axum::Router;

use crate::ControllerError;
use crate::api::{ManagementState, build_management_router};
use crate::http_auth::{self, AppState};
use crate::http_device_status::{DeviceStatusState, build_device_status_router};
use crate::http_events::{EventsState, build_events_router};
use crate::metrics::collect::{
    MAX_CONCURRENT_SCRAPE, MetricsRuntime, MetricsSnapshotSource, SqlxMetricsSnapshotSource,
};
use crate::metrics::config::MetricsConfig;
use crate::metrics::http::{MetricsState, build_metrics_router};
use crate::tasks::http::{HttpTasksState, build_tasks_router};

/// 指标线的**全部**外部装配输入：配置与运行时锚点。
///
/// 字段集合**只有这两项**（§7.6 第 9 条用写全字段的构造点把它钉死）：汇总源与池
/// 读数都在 `build_production_router` 内部由 `inputs.auth.db` 派生，外部既递不进
/// `DbPool`，也递不进自建的汇总源。
pub struct MetricsLineInputs {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
}

/// 生产顶层 router 的全部 typed 输入：七线状态各由所属条线的生产适配器构造，
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
    /// SSE 事件读线（1 条 `GET /api/v1/events` 路由 + 事件总线订阅面）。
    ///
    /// 其 `bus` 与生产进程内唯一的 [`crate::events::EventBus`] 是同一通道句柄；各条线的发布方
    /// 通过同一个句柄投递命名事件（本切片尚无业务发布方，事件流因此真实地
    /// 只有心跳，不伪造任何事件）。
    pub events: EventsState,
    /// Prometheus 指标线（1 条路由，独立 Basic 认证边界）。
    pub metrics: MetricsLineInputs,
    /// Web 嵌入静态资源线。
    pub web: Router<()>,
}

/// 唯一生产装配入口：校验共享认证上下文指针一致后合并七线子 router。
///
/// 真实汇总源只能在本函数内部由 `auth.db` 构造（I2）；不一致时返回去敏固定
/// `Config`（不回显任何指针/地址值），进程应在触达监听套接字之前失败关闭。
pub fn build_production_router(inputs: ProductionInputs) -> Result<Router<()>, ControllerError> {
    build_router(inputs, None)
}

/// 仅供 `crates/rsetup-controller/tests/` 的装配测试注入合成汇总源。
/// `#[doc(hidden)]` 且 MUST NOT 被 `main.rs` 或任何生产路径调用（§7.8 第 6 条 grep
/// 门禁）。它接收的是 `Arc<dyn MetricsSnapshotSource>`：类型里没有 `DbPool` 只是
/// **收缩违规面**，MUST NOT 据此宣称"该缝在类型上绝不可能引入第二池"。无论走哪个
/// 入口，`MetricsState.pool` 恒为 `inputs.auth.db.clone()`。
#[doc(hidden)]
pub fn build_production_router_with_metrics_source(
    inputs: ProductionInputs,
    snapshot: Arc<dyn MetricsSnapshotSource>,
) -> Result<Router<()>, ControllerError> {
    build_router(inputs, Some(snapshot))
}

/// 唯一实现：`snapshot_override` 只允许测试注入，`None` 即真实源。
fn build_router(
    inputs: ProductionInputs,
    snapshot_override: Option<Arc<dyn MetricsSnapshotSource>>,
) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.events.auth)
    {
        return Err(ControllerError::Config(
            "production auth context mismatch".into(),
        ));
    }
    // 连接池唯一派生点：指标线的池与汇总源都只能来自 inputs.auth.db（I2）。
    let snapshot = snapshot_override
        .unwrap_or_else(|| Arc::new(SqlxMetricsSnapshotSource::new(inputs.auth.db.clone())));
    let metrics_state = MetricsState {
        config: inputs.metrics.config,
        runtime: inputs.metrics.runtime,
        snapshot,
        pool: inputs.auth.db.clone(),
        guard: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SCRAPE)),
    };
    let metrics = build_metrics_router(metrics_state);
    let auth = inputs.auth; // 在指标线借用 db 之后再移交给认证线。
    Ok(compose_component_routers(
        http_auth::build_http_router(auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status),
        build_events_router(inputs.events),
        metrics,
        inputs.web,
    ))
}

/// 把 auth / management / tasks / status / events / metrics / web 七个组件子 router
/// 合并为顶层路由。`metrics` 固定排在 `events` 之后、`web` 之前（web 含 SPA 兜底，
/// 必须最后合并）。
pub fn compose_component_routers(
    auth: Router<()>,
    management: Router<()>,
    tasks: Router<()>,
    status: Router<()>,
    events: Router<()>,
    metrics: Router<()>,
    web: Router<()>,
) -> Router<()> {
    auth.merge(management)
        .merge(tasks)
        .merge(status)
        .merge(events)
        .merge(metrics)
        .merge(web)
}
