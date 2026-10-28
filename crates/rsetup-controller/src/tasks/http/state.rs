use std::sync::Arc;

use axum::extract::FromRef;

use super::contract::TaskApi;
use crate::events::{EventBus, EventEnvelope, EventObjectAudience};
use crate::http_auth::HttpAuthState;
use crate::operations::{RecoveryOperation, RuntimeRecoveryController};

/// 任务路由 state：业务依赖仍只有 `api` 与 `auth` 两项（游标密钥由 `api` 的生产
/// 实现持有），`events` 是**可选**的业务事件发布面（06 §4 `task.updated`），
/// `recovery` 是**可选**的受控恢复模式真源句柄（05 §9 提交闸门）。
///
/// `events` 为 `None` 时所有写端点行为与接入事件总线之前完全一致（不发布、不
/// 报错、不阻塞）；发布失败在 [`EventBus::publish`] 的契约里即「无在线订阅者
/// 即丢弃」，因此任何 handler 都不得依据发布返回值改变响应。
///
/// `recovery` 为 `None` 表示本装配不提供受控恢复闸门（单测、嵌入式装配），提交
/// 行为与接入该闸门之前一致；生产装配必须注入与 transport 探针、调度循环**同一个**
/// `Arc<RuntimeRecoveryController>`，不得为闸门再造一套模式快照。
#[derive(Clone)]
pub struct HttpTasksState {
    pub api: Arc<dyn TaskApi>,
    pub auth: Arc<HttpAuthState>,
    /// 进程内事件总线句柄；`None` 表示本部署未开启 SSE 事件线（单测与嵌入式装配）。
    pub events: Option<EventBus>,
    /// 受控恢复模式唯一真源（05 §9）；`None` 表示本装配不启用提交闸门。
    pub recovery: Option<Arc<RuntimeRecoveryController>>,
}

impl HttpTasksState {
    /// 不接事件总线的构造（保持既有 `{api, auth}` 两字段装配语义的入口）。
    pub fn new(api: Arc<dyn TaskApi>, auth: Arc<HttpAuthState>) -> Self {
        Self {
            api,
            auth,
            events: None,
            recovery: None,
        }
    }

    /// 接上生产事件总线（与 SSE 读线 `EventsState.bus` 必须是同一通道句柄）。
    pub fn with_events(mut self, events: EventBus) -> Self {
        self.events = Some(events);
        self
    }

    /// 接上受控恢复模式唯一真源（05 §9）。必须与 transport 探针、`/system/status`、
    /// 调度循环用的是同一个 `Arc<RuntimeRecoveryController>`；本模块只读它的当前
    /// 模式，不改模式（进入/解除归管理员侧管理写线）。
    pub fn with_recovery(mut self, recovery: Arc<RuntimeRecoveryController>) -> Self {
        self.recovery = Some(recovery);
        self
    }

    /// 受控恢复模式提交闸门（05 §9「禁止变更任务调度/提交」）。
    ///
    /// 每条请求实时读控制器当前模式，不缓存模式快照：管理员解除受控模式后，下一条
    /// 提交即放行。`recovery = None`（未装配闸门）与正常模式同为「不阻止」；
    /// 只读端点、管理与只读核验类操作不经本闸门。
    pub(super) fn blocks_mutation_task(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|controller| !controller.permits(RecoveryOperation::MutationTask))
    }

    /// 任务写操作成功后的统一 `task.updated` 发布：
    ///
    /// 1. 先投给**任务创建者本人**（对象级受众 `User(creator_user_id)`，普通
    ///    用户由此看到自己任务的变化）；
    /// 2. 操作者不是管理员时再补投一份**管理员受众**——管理员不是「任何合法
    ///    会话」，也不是创建者的旁路，需要独立受众的一条事件（02 §7 逐事件过滤）；
    ///    管理员本人已收到第 1 条（其本人受众即覆盖其会话），不重复投递。
    ///
    /// `revision` 只作同一 `kind + id` 内的单调提示：调用方给不出精确新值时给
    /// **递增下界**（CAS 的 `expected_revision.saturating_add(1)`，饱和加以防客户端
    /// 传入 `u64::MAX` 时 panic 或回绕成倒退版本），不得给任意定值。
    pub(super) fn publish_task_update(
        &self,
        task_id: &str,
        revision: u64,
        creator_user_id: [u8; 16],
        is_admin_actor: bool,
    ) {
        let Some(bus) = self.events.as_ref() else {
            return;
        };
        bus.publish(EventEnvelope::task_updated(
            task_id,
            revision,
            EventObjectAudience::user(creator_user_id),
        ));
        if !is_admin_actor {
            bus.publish(EventEnvelope::task_updated(
                task_id,
                revision,
                EventObjectAudience::admins(),
            ));
        }
    }
}

impl FromRef<HttpTasksState> for Arc<HttpAuthState> {
    fn from_ref(state: &HttpTasksState) -> Self {
        Arc::clone(&state.auth)
    }
}

impl FromRef<HttpTasksState> for Arc<dyn TaskApi> {
    fn from_ref(state: &HttpTasksState) -> Self {
        Arc::clone(&state.api)
    }
}
