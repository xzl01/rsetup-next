//! 业务写操作 → `GET /api/v1/events` 业务事件读线的进程内端到端集成测试（06 §4、02 §7）。
//!
//! 与 `tests/events_sse.rs` 的分工：那个文件用**直接调用 `EventBus::publish`** 的
//! 方式钉住 SSE 读线本身（握手门禁、五种命名帧、背压 `reset`、权威复核关流）；本文件
//! 反过来钉住**发布侧**——真实业务路由（tasks 线 `POST /api/v1/tasks`、
//! `/api/v1/tasks/{id}/cancel`，management 线 `PATCH /api/v1/devices/{id}`、
//! `POST /api/v1/devices/{id}/reauthorize`、`POST /api/v1/grants`）在处理写请求时，是否真的
//! 向同一条总线投出了形状正确、受众正确的 `task.updated` / `device.updated` /
//! `permissions.changed`，并且这些帧确实出现在**对的**那条 SSE 连接上、
//! **没有**出现在别人的连接上。
//!
//! 同时钉住两条「**不该发**」的边界（审查反馈 Medium-1 / Medium-2）：
//! - `PATCH /api/v1/roles/{id}`、`PATCH /api/v1/groups/{id}`、`DELETE /api/v1/grants/{id}`
//!   的受影响用户集合在本层不可确定，一律**零发布**（不得给操作者管理员发关于其本人的
//!   虚假 `permissions.changed`）；
//! - `POST /api/v1/tasks/{id}/cancel` 返回零个取消目标（`cancelled: null`）时**零发布**。
//!
//! 证据边界（诚实界定）：
//! - 全程进程内：真实 axum router（events + tasks + management 三条线按
//!   `production::compose_component_routers` 的同一形状 merge）+ 真实
//!   `tokio::sync::broadcast` 总线 + `tower::ServiceExt::oneshot`，不经真实 socket；
//! - 认证是内存 `Directory` 桩（三个用户共用**一个** `Arc<HttpAuthState>`，与生产
//!   `build_production_router` 的 `Arc::ptr_eq` 约束同形），业务网关是内存 fake
//!   （`FakeTaskApi` / `FakeGateway`）。因此本文件**不**证明真实 SQLx 仓储在同事务里
//!   `bump_authz_epoch`、不证明真实库写入与事件的因果顺序，也不证明跨进程投递；
//! - 订阅侧的权威复核（`session_authority`）在本文件里恒返回与握手基准一致的值，
//!   即「连接存续」是前提而不是被审对象——失效关流由 `tests/events_sse.rs` 专门取证。
//!
//! 读体的有限终止：SSE 是长连接，本文件用「先建立订阅 → 再发起写请求 → 最后释放所有
//! `EventBus` 句柄」的顺序取证。顺序是**确定性**的而非时间性的：`events_handler` 在
//! 返回响应之前就已经 `bus.subscribe()`（订阅早于响应），因此只要 `oneshot` 握手返回
//! 200，后续发布必然落在该订阅者的缓冲区里；读体只在整个装配体（router 与测试侧句柄
//! 一起）析构、通道关闭后结束。全程没有 `sleep`，也没有靠超时猜时序。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use rsetup_controller::ControllerError;
use rsetup_controller::HttpAuthState;
use rsetup_controller::api::approvals::{
    ApprovalTarget, ApprovalsGateway, ManagementNotificationSink,
};
use rsetup_controller::api::audit::AuditGateway;
use rsetup_controller::api::devices::{
    DeviceConnectionState, DeviceLiveProbe, DeviceLiveView, DeviceStreamHealth, DevicesGateway,
};
use rsetup_controller::api::grants::GrantsGateway;
use rsetup_controller::api::groups::GroupsGateway;
use rsetup_controller::api::roles::RolesGateway;
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView, SystemGateway};
use rsetup_controller::api::users::UsersGateway;
use rsetup_controller::api::{ManagementFuture, ManagementState, build_management_router};
use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
use rsetup_controller::auth::session::token_digest;
use rsetup_controller::authz::policy::{GrantScope, GrantSource, Permission};
use rsetup_controller::devices::repository::{AuditCursor, AuditFilter, DeviceAdminView};
use rsetup_controller::devices::service::AdmissionChangeEvent;
use rsetup_controller::events::EventBus;
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding, SessionAuthority};
use rsetup_controller::http_events::{EventsState, build_events_router};
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::model::{AdmissionState, ReviewDecision};
use rsetup_controller::tasks::http::{
    CancelTaskRequest, CancelTaskResponse, ChildrenPage, HttpTasksState, ReleaseLockRequest,
    ReleaseLockResponse, TaskApi, TaskDetail, TaskFuture, TaskPage, TimeReviewTaskRequest,
    build_tasks_router,
};
use rsetup_controller::tasks::model::MainTaskState;
use rsetup_controller::tasks::preview::PreviewOutcome;
use rsetup_controller::tasks::service::SubmitOutcome;

// ---------------------------------------------------------------------------
// 常量：装配参数与三个身份
// ---------------------------------------------------------------------------

const HOST: &str = "sse-business-events.test";
const ORIGIN: &str = "http://sse-business-events.test";
/// 读体终止的硬超时护栏：装配体析构后三条流都应在毫秒级结束，超时即说明有句柄
/// 泄漏或流被卡住，绝不允许把测试挂到 CI 超时。
const READ_BUDGET: Duration = Duration::from_secs(5);
/// 本文件的订阅侧刻意**不做**周期复核（复核对象是业务事件本身，不是长连接鉴权），
/// 取一个远大于用例生命周期的周期，让时序只由「发布 + 通道关闭」驱动。
/// 周期复核自身由 `tests/events_sse.rs` 取证。
const NO_PERIODIC_REVALIDATE: Duration = Duration::from_secs(3600);

/// 普通用户 Alice：任务写操作的**发起者**，也是权限变更的**目标用户**。
const ALICE: [u8; 16] = [0xa1u8; 16];
/// 普通用户 Bob：全程旁观者，任何一条业务事件都不属于他。
const BOB: [u8; 16] = [0xb0u8; 16];
/// 管理员：management 线写操作的发起者，同时是 `task.updated` 的管理员受众成员。
const ADMIN: [u8; 16] = [0xadu8; 16];

/// 三个身份的全集（装配 CSRF、建立订阅时共用，漏一个即编译期/断言期暴露）。
const USERS: [([u8; 16], &str, bool); 3] = [
    (ALICE, "alice", false),
    (BOB, "bob", false),
    (ADMIN, "admin", true),
];

/// 设备公钥（64 位小写 hex 的 32 字节形状，与 `parse_device_id_path` 契约一致）。
const DEVICE_ID: [u8; 32] = [0x5au8; 32];
/// fake 服务层返回的主任务 id（事件载荷与 HTTP 响应必须指向同一个它）。
const TASK_ID: Uuid = Uuid::from_u128(0x2b3c4d5e6f7048a19b2c3d4e5f607182);
/// fake 任务创建返回的幂等键（canonical UUID v4 形状）。
const IDEMPOTENCY_KEY: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
/// roles/groups/grants 三条写线共用的资源 id（canonical 小写 UUID；fake 网关不校验值，
/// 本文件取证的是「有没有发布」，不是网关的业务判定）。
const MANAGED_ID: Uuid = Uuid::from_u128(0x2468ace02468ace01357bdf11357bdf1);

/// 会话令牌：64 位小写 hex（`session_cookie_value` 的唯一合法形状），逐用户不同。
fn token_for(user_id: [u8; 16]) -> String {
    let base = Uuid::from_bytes(user_id).simple().to_string();
    format!("{base}{base}")
}

/// 每个用户一枚独立的 CSRF 绑定值（写请求的 `x-csrf-token`）。
fn csrf_for(user_id: [u8; 16]) -> String {
    format!("csrf-{}-bound", &token_for(user_id)[..16])
}

fn device_id_hex() -> String {
    DEVICE_ID.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// 认证桩：一个 Directory 同时服务三条 SSE 连接与全部写请求
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct DirectoryEntry {
    user_id: [u8; 16],
    name: &'static str,
    is_admin: bool,
}

/// 内存身份 + 授权 epoch 源。三个用户共用**一个**实例（因此也共用一个
/// `Arc<HttpAuthState>`），这与生产「所有条线共享同一认证上下文」的装配约束同形，
/// 也让「换个 cookie 就是换个受众」成为唯一变量。
struct Directory {
    entries: HashMap<[u8; 32], DirectoryEntry>,
    /// 全局 `authz_epoch` 的唯一真相源：`permissions.changed` 的载荷 epoch 必须来自
    /// 这里（`publish_permissions_changed` 经 `AuthServiceApi::authz_epoch` 读取）。
    epoch: Arc<AtomicU64>,
}

impl Directory {
    fn new(epoch: u64) -> Self {
        let entries = USERS
            .into_iter()
            .map(|(user_id, name, is_admin)| {
                (
                    token_digest(token_for(user_id).as_bytes()),
                    DirectoryEntry {
                        user_id,
                        name,
                        is_admin,
                    },
                )
            })
            .collect();
        Self {
            entries,
            epoch: Arc::new(AtomicU64::new(epoch)),
        }
    }

    fn entry(&self, digest: &[u8; 32]) -> Option<DirectoryEntry> {
        self.entries.get(digest).copied()
    }
}

impl AuthServiceApi for Directory {
    fn login<'a>(&'a self, _username: &'a str, _password: &'a str) -> AuthFuture<'a, Login> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        let digest = token_digest(raw.as_bytes());
        let entry = self.entry(&digest);
        Box::pin(async move {
            let entry = entry.ok_or(ControllerError::InvalidArgument)?;
            Ok(Session {
                user: IdentityUser {
                    id: entry.user_id,
                    username: entry.name.to_string(),
                    password_hash: "synthetic-unused".into(),
                    active: true,
                    is_admin: entry.is_admin,
                    must_change_password: false,
                    revision: 1,
                },
                digest,
            })
        })
    }
    fn change_password<'a>(
        &'a self,
        _session: &'a Session,
        _current: &'a str,
        _new_password: &'a str,
    ) -> AuthFuture<'a, ()> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn logout<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        let epoch = Arc::clone(&self.epoch);
        Box::pin(async move { Ok(epoch.load(Ordering::SeqCst)) })
    }
    fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        self.entries.contains_key(digest)
    }
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        // 本文件的界定：权威侧结论与内存桩同构且 epoch 恒定（除非用例显式 bump），
        // 使「连接存续」成为前提；会话撤销/降权/epoch 漂移导致的关流由
        // `tests/events_sse.rs` 专门取证。
        let entry = self.entry(digest);
        let authz_epoch = self.epoch.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(SessionAuthority {
                session_live: entry.is_some(),
                is_admin: entry.is_some_and(|entry| entry.is_admin),
                authz_epoch,
            })
        })
    }
    fn list_sessions<'a>(
        &'a self,
        _session: &'a Session,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> AuthFuture<'a, SessionPage> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn revoke_by_alias<'a>(&'a self, _session: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    fn revoke_others<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, u64> {
        Box::pin(async { Ok(0) })
    }
}

// ---------------------------------------------------------------------------
// 业务网关 fake：只被本文件的写路径触达，并如实记录每一次业务调用
// ---------------------------------------------------------------------------

/// 记录「哪个业务方法被触达」的唯一手段：既证明路由确实打到了网关（而不是在
/// 门禁处被静默吞掉），也证明**没有**别的方法被顺手调用。
#[derive(Default)]
struct CallLog(Mutex<Vec<&'static str>>);

impl CallLog {
    fn record(&self, name: &'static str) {
        self.0.lock().unwrap().push(name);
    }
    fn calls(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

fn empty_page() -> Value {
    json!({ "items": [], "next_cursor": null })
}

/// `TaskApi` 的内存实现。
#[derive(Default)]
struct FakeTaskApi {
    log: CallLog,
    /// 置位后 `cancel` 返回「本事务未取消任何目标」的投影（`cancelled: null`）。
    cancel_empty: AtomicBool,
}

impl FakeTaskApi {
    /// 已被触达的业务方法序列（正/负例都靠它取证）。
    fn calls(&self) -> Vec<&'static str> {
        self.log.calls()
    }

    /// 把 `cancel` 切到零目标结果：HTTP 契约下这等价于「没有任何目标进入取消结果」。
    fn set_cancel_empty(&self, value: bool) {
        self.cancel_empty.store(value, Ordering::SeqCst);
    }
}

/// 未被本文件用例触达的方法一律 `NotFound`：若将来有路径意外走到这里，
/// 得到的是显式 404 + 调用记录，而不是伪造的成功数据。
fn not_reached() -> ControllerError {
    ControllerError::NotFound
}

impl TaskApi for FakeTaskApi {
    fn preview<'a>(
        &'a self,
        _actor: &'a Session,
        _device_ids: &'a [[u8; 32]],
        _group_ids: &'a [Uuid],
    ) -> TaskFuture<'a, PreviewOutcome> {
        self.log.record("tasks.preview");
        Box::pin(async { Err(not_reached()) })
    }
    fn submit<'a>(
        &'a self,
        _actor: &'a Session,
        _preview_token: &'a str,
        _idempotency_key: &'a str,
    ) -> TaskFuture<'a, SubmitOutcome> {
        self.log.record("tasks.submit");
        Box::pin(async move {
            Ok(SubmitOutcome::Created {
                main_task_id: TASK_ID,
            })
        })
    }
    fn list_tasks<'a>(
        &'a self,
        _actor: &'a Session,
        _state: Option<MainTaskState>,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> TaskFuture<'a, TaskPage> {
        self.log.record("tasks.list");
        Box::pin(async { Err(not_reached()) })
    }
    fn task_detail<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
    ) -> TaskFuture<'a, TaskDetail> {
        self.log.record("tasks.detail");
        Box::pin(async { Err(not_reached()) })
    }
    fn task_children<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _cursor: Option<&'a str>,
        _limit: usize,
    ) -> TaskFuture<'a, ChildrenPage> {
        self.log.record("tasks.children");
        Box::pin(async { Err(not_reached()) })
    }
    fn cancel<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _request: CancelTaskRequest,
    ) -> TaskFuture<'a, CancelTaskResponse> {
        self.log.record("tasks.cancel");
        let empty = self.cancel_empty.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(CancelTaskResponse {
                accepted: true,
                cancelled: if empty { None } else { Some(vec![TASK_ID]) },
            })
        })
    }
    fn time_review<'a>(
        &'a self,
        _actor: &'a Session,
        _task_id: Uuid,
        _request: TimeReviewTaskRequest,
    ) -> TaskFuture<'a, TaskDetail> {
        self.log.record("tasks.time_review");
        Box::pin(async { Err(not_reached()) })
    }
    fn release_lock<'a>(
        &'a self,
        _actor: &'a Session,
        _subtask_id: Uuid,
        _request: ReleaseLockRequest,
    ) -> TaskFuture<'a, ReleaseLockResponse> {
        self.log.record("tasks.release_lock");
        Box::pin(async { Err(not_reached()) })
    }
}

/// management 线 8 个网关 + 3 个 probe/sink 的内存实现（一个对象实现全部 trait，
/// 与 `tests/support/management_http_fixture.rs` 同形）。
#[derive(Default)]
struct FakeGateway {
    log: CallLog,
    /// 置位后 `devices.patch` 返回 CAS 冲突：用于「写失败零发布」的负例。
    fail_device_patch: AtomicBool,
}

impl FakeGateway {
    /// 已被触达的业务方法序列（含被门禁挡下时的「应当为空」）。
    fn calls(&self) -> Vec<&'static str> {
        self.log.calls()
    }
}

macro_rules! value_method {
    ($log:literal $name:ident($($arg:ident : $ty:ty),*) => $result:expr) => {
        fn $name<'a>(&'a self, _actor: &'a Session, $($arg: $ty),*) -> ManagementFuture<'a, Value> {
            self.log.record($log);
            let result = $result;
            Box::pin(async move { Ok(result) })
        }
    };
}

impl UsersGateway for FakeGateway {
    value_method!("users.list" list(_cursor: Option<[u8; 16]>, _limit: usize) => empty_page());
    value_method!("users.create" create(_username: &'a str, _display_name: &'a str, _is_admin: bool) => empty_page());
    value_method!("users.patch" patch(_id: [u8; 16], _revision: u64, _active: Option<bool>, _is_admin: Option<bool>, _display_name: Option<&'a str>) => empty_page());
    value_method!("users.reset_password" reset_password(_id: [u8; 16], _revision: u64) => empty_page());
    value_method!("users.effective_permissions" effective_permissions(_target: [u8; 16], _device_id: Option<[u8; 32]>, _cursor: Option<[u8; 32]>, _limit: usize) => empty_page());
}

impl RolesGateway for FakeGateway {
    value_method!("roles.list" list(_cursor: Option<Uuid>, _limit: usize) => empty_page());
    value_method!("roles.create" create(_name: &'a str, _permissions: &'a [Permission]) => empty_page());
    value_method!("roles.patch" patch(_id: Uuid, _revision: u64, _name: Option<&'a str>, _permissions: Option<&'a [Permission]>, _archived: Option<bool>) => empty_page());
}

impl GrantsGateway for FakeGateway {
    value_method!("grants.list" list(_user_id: Option<[u8; 16]>, _cursor: Option<Uuid>, _limit: usize) => empty_page());
    fn create<'a>(
        &'a self,
        _actor: &'a Session,
        user_id: [u8; 16],
        _source: GrantSource,
        _scope: GrantScope,
    ) -> ManagementFuture<'a, Value> {
        self.log.record("grants.create");
        Box::pin(async move {
            Ok(json!({
                "id": "00000000-0000-4000-8000-000000000009",
                "revision": "1",
                "user_id": Uuid::from_bytes(user_id).to_string(),
                "source": {"kind": "direct", "permissions": ["device.read"]},
                "scope": {"kind": "all"},
            }))
        })
    }
    fn delete<'a>(
        &'a self,
        _actor: &'a Session,
        _id: Uuid,
        _revision: u64,
    ) -> ManagementFuture<'a, ()> {
        self.log.record("grants.delete");
        Box::pin(async { Ok(()) })
    }
}

impl GroupsGateway for FakeGateway {
    value_method!("groups.list" list(_cursor: Option<Uuid>, _limit: usize) => empty_page());
    value_method!("groups.create" create(_name: &'a str) => empty_page());
    value_method!("groups.patch" patch(_id: Uuid, _revision: u64, _name: Option<&'a str>, _archived: Option<bool>) => empty_page());
    value_method!("groups.replace_members" replace_members(_id: Uuid, _revision: u64, _devices: &'a [[u8; 32]]) => empty_page());
}

impl DevicesGateway for FakeGateway {
    value_method!("devices.list" list_devices_visible(_cursor: Option<[u8; 32]>, _limit: usize, _group_id: Option<Uuid>, _search: Option<&'a str>) => empty_page());
    fn find_device<'a>(
        &'a self,
        _actor: &'a Session,
        id: [u8; 32],
    ) -> ManagementFuture<'a, Option<(DeviceAdminView, std::collections::HashSet<Permission>)>>
    {
        self.log.record("devices.find");
        Box::pin(async move {
            Ok(Some((
                DeviceAdminView {
                    public_key: id,
                    display_name: "fixture".into(),
                    admission_state: AdmissionState::Approved,
                    review_decision: ReviewDecision::Approved,
                    revision: 1,
                    descriptor: None,
                    archived: false,
                },
                std::collections::HashSet::from([Permission::DeviceRead]),
            )))
        })
    }
    fn patch<'a>(
        &'a self,
        _actor: &'a Session,
        id: [u8; 32],
        _revision: u64,
        display_name: &'a str,
    ) -> ManagementFuture<'a, Value> {
        self.log.record("devices.patch");
        let conflict = self.fail_device_patch.load(Ordering::SeqCst);
        let name = display_name.to_string();
        Box::pin(async move {
            if conflict {
                return Err(ControllerError::RevisionConflict);
            }
            // 回显新 revision（"2"）：事件载荷的 revision 必须以它为准，
            // 而不是 handler 手里的 CAS 入参 + 1。
            Ok(json!({
                "device_id": hex::encode(id),
                "display_name": name,
                "revision": "2",
            }))
        })
    }
}

impl ApprovalsGateway for FakeGateway {
    value_method!("approvals.list" list(_decision: Option<ReviewDecision>, _cursor: Option<[u8; 32]>, _limit: usize) => empty_page());
    value_method!("approvals.approve" approve(_targets: &'a [ApprovalTarget]) => json!({"results": []}));
    value_method!("approvals.reject" reject(_targets: &'a [ApprovalTarget], _reason: &'a str) => json!({"results": []}));
    value_method!("approvals.reopen" reopen(_id: [u8; 32], _revision: u64) => json!({"decision": "none", "requires_device_reset": true}));
    value_method!("approvals.revoke" revoke(_id: [u8; 32], _revision: u64, _reason: &'a str) => json!({"state": "REVOKED", "requires_device_reset": true}));
    value_method!("approvals.reauthorize" reauthorize(_id: [u8; 32], _revision: u64) => json!({"state": "APPROVED", "requires_device_reset": true}));
}

impl AuditGateway for FakeGateway {
    value_method!("audit.list" list(_filter: AuditFilter, _cursor: Option<AuditCursor>, _limit: usize) => empty_page());
}

impl SystemGateway for FakeGateway {
    value_method!("system.status" status() => json!({"ready": true, "mode": "normal"}));
}

/// 实时探针 / 恢复模式探针 / commit 后通知槽：本文件的用例都不触达它们，
/// 但 `ManagementState` 要求显式实例，不给常量兜底。
struct FakeProbe;

impl DeviceLiveProbe for FakeProbe {
    fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
        Box::pin(async {
            Ok(DeviceLiveView {
                connection_state: DeviceConnectionState::Offline,
                control_health: DeviceStreamHealth::Offline,
                data_health: DeviceStreamHealth::Offline,
                capabilities: None,
            })
        })
    }
}

impl RecoveryModeProbe for FakeProbe {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Ok(RecoveryView {
                ready: true,
                mode: "normal",
            })
        })
    }
}

impl ManagementNotificationSink for FakeProbe {
    fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 装配：一条总线 + 一份认证上下文 + merge 后的单条 router
// ---------------------------------------------------------------------------

/// 一次用例的完整装配体。
///
/// **`Drop` 即关流**：router 里三条线共 3 个 `EventBus` 句柄、本结构另持 1 个，
/// 全部随 `World` 析构 → 唯一 `broadcast` 通道关闭 → 三条 SSE 流读到 `Closed`
/// 自然结束。因此「先握手订阅、后发起写请求、最后 drop 装配体读体」是确定的
/// 因果顺序，不需要任何 sleep。
struct World {
    router: Router<()>,
    bus: EventBus,
    epoch: Arc<AtomicU64>,
    api: Arc<FakeTaskApi>,
    gateway: Arc<FakeGateway>,
}

impl World {
    /// 以 `epoch` 为初始 `authz_epoch` 装配一套进程内控制器（events + tasks +
    /// management 三条线共用同一 `Arc<HttpAuthState>` 与同一条 `EventBus`）。
    fn new(epoch: u64) -> Self {
        let bus = EventBus::new();
        let directory = Arc::new(Directory::new(epoch));
        let epoch = Arc::clone(&directory.epoch);
        let auth = Arc::new(HttpAuthState::new(
            directory,
            HttpSecurityConfig::from_values(Some(HOST), Some(ORIGIN), None).unwrap(),
        ));
        for (user_id, _, _) in USERS {
            auth.csrf.lock().unwrap().insert(
                token_digest(token_for(user_id).as_bytes()),
                CsrfBinding {
                    token: csrf_for(user_id),
                    issued_at: Instant::now(),
                },
            );
        }
        let api = Arc::new(FakeTaskApi::default());
        let gateway = Arc::new(FakeGateway::default());
        let probe = Arc::new(FakeProbe);
        let tasks = build_tasks_router(HttpTasksState {
            api: api.clone(),
            auth: Arc::clone(&auth),
            events: Some(bus.clone()),
        });
        let management = build_management_router(ManagementState {
            auth: Arc::clone(&auth),
            users: gateway.clone(),
            roles: gateway.clone(),
            grants: gateway.clone(),
            groups: gateway.clone(),
            devices: gateway.clone(),
            approvals: gateway.clone(),
            audit: gateway.clone(),
            system: gateway.clone(),
            device_probe: probe.clone(),
            recovery_probe: probe.clone(),
            notification_sink: probe,
            events: Some(bus.clone()),
            time_source: None,
        });
        let events = build_events_router(EventsState {
            auth,
            bus: bus.clone(),
            revalidate_interval: NO_PERIODIC_REVALIDATE,
        });
        Self {
            router: events.merge(tasks).merge(management),
            bus,
            epoch,
            api,
            gateway,
        }
    }

    /// 显式把授权 epoch 推进到 `value`：模拟「仓储在同一次写事务里
    /// `bump_authz_epoch`」之后的权威值。
    fn set_epoch(&self, value: u64) {
        self.epoch.store(value, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// 请求构造与读体收齐
// ---------------------------------------------------------------------------

/// `GET /api/v1/events`：与浏览器同源 GET 同形（带会话 Cookie，不带 `Origin`）。
fn events_request(user_id: [u8; 16]) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/v1/events")
        .header(header::HOST, HeaderValue::from_static(HOST))
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", token_for(user_id))).unwrap(),
        )
        .body(Body::empty())
        .unwrap()
}

/// 写请求：Host + 同源 Origin + 会话 Cookie + CSRF 绑定值 + JSON body，
/// 即 `authenticate_write` 要求的全部输入。
fn write_request(
    method: &str,
    uri: &str,
    user_id: [u8; 16],
    body: Value,
    idempotency_key: Option<&str>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, HeaderValue::from_static(HOST))
        .header(header::ORIGIN, HeaderValue::from_static(ORIGIN))
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )
        .header(
            header::COOKIE,
            HeaderValue::from_str(&format!("rsc_session={}", token_for(user_id))).unwrap(),
        )
        .header(
            "x-csrf-token",
            HeaderValue::from_str(&csrf_for(user_id)).unwrap(),
        );
    if let Some(key) = idempotency_key {
        builder = builder.header("idempotency-key", HeaderValue::from_str(key).unwrap());
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

/// 建立一条 SSE 订阅：握手必须是 200 + `text/event-stream`（`Content-Type` 由 axum
/// `Sse` 写入）。返回响应体未读的响应——此刻服务端**已经**持有本连接的广播订阅。
async fn subscribe(world: &World, user_id: [u8; 16]) -> axum::response::Response {
    let response = world
        .router
        .clone()
        .oneshot(events_request(user_id))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "SSE 握手必须 200");
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
        "握手响应必须是事件流"
    );
    response
}

/// 发起一次业务写请求并读干响应体（响应体必须可读尽，SSE 响应不走这里）。
async fn call(world: &World, request: Request<Body>) -> (StatusCode, Value) {
    let response = world.router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

/// 三条已建立的订阅（Alice / Bob / Admin）。
struct Subscribers {
    alice: axum::response::Response,
    bob: axum::response::Response,
    admin: axum::response::Response,
}

impl Subscribers {
    /// 逐个握手，并断言总线上确实挂着 3 个订阅（防止「一条都没订上」的空断言：
    /// 若订阅数为 0，后面所有「Bob 没收到」都会假阳性通过）。
    async fn open(world: &World) -> Self {
        let alice = subscribe(world, ALICE).await;
        let bob = subscribe(world, BOB).await;
        let admin = subscribe(world, ADMIN).await;
        assert_eq!(
            world.bus.subscriber_count(),
            3,
            "三条连接必须订在同一条总线上，否则受众隔离无从取证"
        );
        Self { alice, bob, admin }
    }

    /// 释放装配体（→ 通道关闭 → 三条流结束）后收齐三个方向的帧。
    async fn collect(self, world: World) -> Streams {
        drop(world);
        Streams {
            alice: drain_frames(self.alice).await,
            bob: drain_frames(self.bob).await,
            admin: drain_frames(self.admin).await,
        }
    }
}

async fn drain_frames(response: axum::response::Response) -> Vec<Frame> {
    let bytes = tokio::time::timeout(READ_BUDGET, to_bytes(response.into_body(), 1 << 20))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "SSE 体必须在 {READ_BUDGET:?} 内随总线关闭而结束；读不出来即说明仍有 \
                 EventBus 句柄存活"
            )
        })
        .unwrap();
    parse_frames(&bytes)
}

/// 一帧 SSE：`event:` 与 `data:` 字段（`data:` 解析成 JSON 值）。
#[derive(Debug, PartialEq)]
struct Frame {
    event: Option<String>,
    data: Option<Value>,
}

/// 把响应体字节切成帧序列；跳过 `:` 开头的注释帧（keep-alive 心跳）。
fn parse_frames(bytes: &[u8]) -> Vec<Frame> {
    let text = String::from_utf8(bytes.to_vec()).expect("SSE 帧恒为 UTF-8");
    let mut frames = Vec::new();
    for block in text.split("\n\n") {
        let trimmed = block.trim_matches(|c: char| c == '\r' || c == '\n');
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }
        let mut event = None;
        let mut data = None;
        for line in trimmed.lines() {
            if let Some(value) = line.strip_prefix("event: ") {
                event = Some(value.to_string());
            } else if let Some(value) = line.strip_prefix("data: ") {
                data = Some(
                    serde_json::from_str(value)
                        .unwrap_or_else(|e| panic!("data: 字段必须是 JSON：{value:?}（{e}）")),
                );
            }
        }
        frames.push(Frame { event, data });
    }
    frames
}

/// 三个方向的帧集合。
struct Streams {
    alice: Vec<Frame>,
    bob: Vec<Frame>,
    admin: Vec<Frame>,
}

impl Streams {
    fn alice(&self) -> &[Frame] {
        &self.alice
    }
    fn bob(&self) -> &[Frame] {
        &self.bob
    }
    fn admin(&self) -> &[Frame] {
        &self.admin
    }
}

/// 断言某个方向**一帧都没收到**（受众隔离的负向证据）。
fn assert_nothing(who: &str, frames: &[Frame]) {
    assert!(
        frames.is_empty(),
        "{who} 不该收到任何事件帧，实际收到 {frames:?}"
    );
}

/// 断言某个方向**恰好一条**指定名称的事件帧，并返回其 `data:` 载荷。
fn only_event<'a>(who: &str, frames: &'a [Frame], name: &str) -> &'a Value {
    let [frame] = frames else {
        panic!("{who} 应当恰好收到 1 帧 {name}，实际收到 {frames:?}");
    };
    assert_eq!(frame.event.as_deref(), Some(name), "{who} 收到的事件名不符");
    frame
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 帧必须携带 data: JSON，实际 {frame:?}"))
}

// ---------------------------------------------------------------------------
// 场景一：任务写操作 → `task.updated`（创建者本人 + 管理员受众，旁观者零帧）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_created_by_regular_user_reaches_owner_and_admins_but_not_bystander() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/tasks",
            ALICE,
            json!({"preview_token": "cd", "confirm": true}),
            Some(IDEMPOTENCY_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "任务创建应 202，实际 {body}");
    assert_eq!(body["data"]["task_id"], json!(TASK_ID.to_string()));
    assert_eq!(body["data"]["accepted"], json!(true));
    // 路由确实打到了业务网关，且只打了这一个方法。
    assert_eq!(world.api.calls(), vec!["tasks.submit"]);

    let streams = subscribers.collect(world).await;
    let expected = json!({
        "kind": "task",
        "id": TASK_ID.to_string(),
        "revision": "1",
    });

    // 创建者本人：看到自己任务的变化。
    assert_eq!(
        only_event("Alice", streams.alice(), "task.updated"),
        &expected
    );
    // 管理员：以独立的管理员受众看到同一条对象事实（不是「任何合法会话」）。
    assert_eq!(
        only_event("Admin", streams.admin(), "task.updated"),
        &expected
    );
    // 旁观者：同一个对象事件对另一个普通用户完全不可见。
    assert_nothing("Bob", streams.bob());
}

#[tokio::test]
async fn task_cancel_publishes_cas_lower_bound_revision_to_owner_and_admins() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "POST",
            &format!("/api/v1/tasks/{TASK_ID}/cancel"),
            ALICE,
            json!({"expected_revision": "3", "confirm": true}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "取消应 200，实际 {body}");
    assert_eq!(world.api.calls(), vec!["tasks.cancel"]);

    let streams = subscribers.collect(world).await;

    // revision 是 `expected_revision + 1` 的**单调下界**（06 §4：同 kind+id 内比较）。
    let alice = only_event("Alice", streams.alice(), "task.updated");
    assert_eq!(alice["kind"], json!("task"));
    assert_eq!(alice["id"], json!(TASK_ID.to_string()));
    assert_eq!(alice["revision"], json!("4"));
    assert!(
        alice["revision"].is_string(),
        "revision 必须是十进制字符串，避免 JS 端丢精度：{alice}"
    );
    assert_eq!(
        only_event("Admin", streams.admin(), "task.updated"),
        alice,
        "管理员受众看到的载荷必须与创建者一致"
    );
    assert_nothing("Bob", streams.bob());
}

/// 零目标取消零发布（审查反馈 Medium-2）：`cancelled: null` 是「本事务没有取消任何
/// 目标」的 HTTP 契约投影（`project_cancel_result` 绝不为 `Some([])`），主任务状态未变，
/// 不得用 `task.updated` 打扰客户端。反向证据用一次真实取消（200 + 1 个目标）钉住
/// 「0 帧是未发布，而不是订阅失效」。
#[tokio::test]
async fn task_cancel_without_cancelled_targets_publishes_no_task_updated() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;
    let cancel_uri = format!("/api/v1/tasks/{TASK_ID}/cancel");

    world.api.set_cancel_empty(true);
    let (status, body) = call(
        &world,
        write_request(
            "POST",
            &cancel_uri,
            ALICE,
            // 极限 revision：饱和下界既不得 panic，也不得回绕成 0。
            json!({"expected_revision": "18446744073709551615", "confirm": true}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "零目标取消仍应 200，实际 {body}");
    assert_eq!(body["data"]["cancelled"], json!(null));
    assert_eq!(world.api.calls(), vec!["tasks.cancel"]);

    // 反向证据：同一条订阅在确实取消了目标时必须收得到。
    world.api.set_cancel_empty(false);
    let (status, _) = call(
        &world,
        write_request(
            "POST",
            &cancel_uri,
            ALICE,
            json!({"expected_revision": "3", "confirm": true}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let streams = subscribers.collect(world).await;
    // 两条连接各只收到**第二次**那一条：revision 是 3 + 1 的单调下界。
    assert_eq!(
        only_event("Alice", streams.alice(), "task.updated"),
        &json!({"kind": "task", "id": TASK_ID.to_string(), "revision": "4"})
    );
    assert_nothing("Bob", streams.bob());
    let admin = streams.admin();
    assert_eq!(admin.len(), 1, "管理员只应收到第二次取消的一条事件帧");
    assert_eq!(admin[0].event.as_deref(), Some("task.updated"));
}

#[tokio::test]
async fn task_created_by_admin_publishes_once_to_its_own_audience_only() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/tasks",
            ADMIN,
            json!({"preview_token": "cd", "confirm": true}),
            Some(IDEMPOTENCY_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "任务创建应 202，实际 {body}");
    assert_eq!(world.api.calls(), vec!["tasks.submit"]);

    let streams = subscribers.collect(world).await;
    let expected = json!({
        "kind": "task",
        "id": TASK_ID.to_string(),
        "revision": "1",
    });
    // 管理员本人发起的写：其本人受众已覆盖其会话，不得重复投递第二份。
    assert_eq!(
        only_event("Admin", streams.admin(), "task.updated"),
        &expected
    );
    // 普通用户既不是创建者也不是管理员受众，一帧都不许有。
    assert_nothing("Alice", streams.alice());
    assert_nothing("Bob", streams.bob());
}

// ---------------------------------------------------------------------------
// 场景二：设备写操作 → `device.updated`（受众恒管理员范围）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn device_patch_by_admin_reaches_admins_only() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "PATCH",
            &format!("/api/v1/devices/{}", device_id_hex()),
            ADMIN,
            json!({"expected_revision": "1", "display_name": "renamed-by-admin"}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin 改名应 200，实际 {body}");
    assert_eq!(world.gateway.calls(), vec!["devices.patch"]);

    let streams = subscribers.collect(world).await;

    // 载荷 revision 取网关回显的新值 "2"（而不是 CAS 入参 + 1 的兜底）。
    assert_eq!(
        only_event("Admin", streams.admin(), "device.updated"),
        &json!({
            "kind": "device",
            "id": device_id_hex(),
            "revision": "2",
        })
    );
    // 设备对象事实的受众是管理员范围：两个普通用户一帧都不该有。
    assert_nothing("Alice", streams.alice());
    assert_nothing("Bob", streams.bob());
}

#[tokio::test]
async fn device_reauthorize_by_admin_publishes_device_updated_to_admins_only() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "POST",
            &format!("/api/v1/devices/{}/reauthorize", device_id_hex()),
            ADMIN,
            json!({"expected_revision": "5", "confirm": true}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重新授权应 200，实际 {body}");
    assert_eq!(body["data"]["state"], json!("APPROVED"));
    assert_eq!(world.gateway.calls(), vec!["approvals.reauthorize"]);

    let streams = subscribers.collect(world).await;
    // approvals 线同样发布 `device.updated`，revision 取 `expected_revision + 1`。
    assert_eq!(
        only_event("Admin", streams.admin(), "device.updated"),
        &json!({
            "kind": "device",
            "id": device_id_hex(),
            "revision": "6",
        })
    );
    assert_nothing("Alice", streams.alice());
    assert_nothing("Bob", streams.bob());
}

#[tokio::test]
async fn denied_or_conflicting_device_writes_publish_no_device_updated() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;
    let uri = format!("/api/v1/devices/{}", device_id_hex());

    // 1) 普通用户无权改名：403 且在网关之前就被拒。
    let (status, body) = call(
        &world,
        write_request(
            "PATCH",
            &uri,
            ALICE,
            json!({"expected_revision": "1", "display_name": "not-allowed"}),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "普通用户改名应 403，实际 {body}"
    );
    assert_eq!(body["error"]["code"], json!("PERMISSION_DENIED"));
    assert!(
        world.gateway.calls().is_empty(),
        "门禁拒绝路径不得触达业务网关：{:?}",
        world.gateway.calls()
    );

    // 2) CAS 冲突：写失败即没有状态变化，零发布。
    world
        .gateway
        .fail_device_patch
        .store(true, Ordering::SeqCst);
    let (status, body) = call(
        &world,
        write_request(
            "PATCH",
            &uri,
            ADMIN,
            json!({"expected_revision": "1", "display_name": "renamed"}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "CAS 冲突应 409，实际 {body}");
    assert_eq!(body["error"]["code"], json!("REVISION_CONFLICT"));
    assert_eq!(world.gateway.calls(), vec!["devices.patch"]);

    let streams = subscribers.collect(world).await;
    assert_nothing("Admin", streams.admin());
    assert_nothing("Alice", streams.alice());
    assert_nothing("Bob", streams.bob());
}

// ---------------------------------------------------------------------------
// 场景三：授权写操作 → `permissions.changed`（受众恒目标用户本人）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grant_created_for_target_user_reaches_only_that_user_with_current_epoch() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/grants",
            ADMIN,
            json!({
                "user_id": Uuid::from_bytes(ALICE).to_string(),
                "source": {"kind": "direct", "permissions": ["device.read"]},
                "scope": {"kind": "all"},
            }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "创建授权应 200，实际 {body}");
    assert_eq!(world.gateway.calls(), vec!["grants.create"]);

    let streams = subscribers.collect(world).await;

    // 目标用户本人：载荷 id 与受众同源，epoch 是权威 `authz_epoch` 的十进制字符串。
    assert_eq!(
        only_event("Alice", streams.alice(), "permissions.changed"),
        &json!({
            "kind": "permissions",
            "id": Uuid::from_bytes(ALICE).to_string(),
            "epoch": "7",
        })
    );
    // 另一个普通用户看不到别人的授权投影……
    assert_nothing("Bob", streams.bob());
    // 管理员同样看不到：`permissions.changed` 是用户私有事实，管理员受众与它
    // 不自洽（06 §4，`EventEnvelope::audience_matches_payload` 的白名单）。
    assert_nothing("Admin", streams.admin());
}

/// `permissions.changed` 的 `epoch` 必须是**发布时刻**的权威 `authz_epoch`。
///
/// 装配期把 epoch 定成 7，随后（在任何订阅与任何写请求之前）推进到 41：载荷里出现
/// `41` 即证明它由 `publish_permissions_changed` 经 `AuthServiceApi::authz_epoch`
/// 现读，既不是装配期快照也不是硬编码；再配合上一条用例（装配期 7 → 载荷 `7`），
/// 「常量」与「装配期缓存」两种实现都被钉死。
///
/// epoch 只在订阅**之前**推进：连接存续期间的 epoch 漂移会让服务端按 06 §4 关流
/// （那正是 `tests/events_sse.rs` 取证的失效面），会把本用例的前提变成被审对象。
#[tokio::test]
async fn permissions_changed_epoch_is_read_at_publish_time() {
    let world = World::new(7);
    world.set_epoch(41);

    let subscribers = Subscribers::open(&world).await;
    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/grants",
            ADMIN,
            json!({
                "user_id": Uuid::from_bytes(BOB).to_string(),
                "source": {"kind": "direct", "permissions": ["device.task.read"]},
                "scope": {"kind": "all"},
            }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "创建授权应 200，实际 {body}");
    assert_eq!(world.gateway.calls(), vec!["grants.create"]);

    let streams = subscribers.collect(world).await;
    // 载荷 id 指向 Bob（目标用户），epoch 是推进后的 41，且为十进制字符串。
    let data = only_event("Bob", streams.bob(), "permissions.changed");
    assert_eq!(data["id"], json!(Uuid::from_bytes(BOB).to_string()));
    assert_eq!(
        data["epoch"],
        json!("41"),
        "载荷 epoch 必须是发布时刻的权威值"
    );
    assert!(
        data["epoch"].is_string(),
        "epoch 必须是十进制字符串：{data}"
    );
    // 目标之外的两个连接（含发起变更的管理员）一帧都不许有。
    assert_nothing("Alice", streams.alice());
    assert_nothing("Admin", streams.admin());
}

#[tokio::test]
async fn failed_grant_write_publishes_no_permissions_changed() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    // 目标用户 id 不是 canonical UUID → 400，在网关之前就被拒。
    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/grants",
            ADMIN,
            json!({
                "user_id": "not-a-uuid",
                "source": {"kind": "direct", "permissions": ["device.read"]},
                "scope": {"kind": "all"},
            }),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "非法 user_id 应 400，实际 {body}"
    );
    assert_eq!(body["error"]["code"], json!("INVALID_ARGUMENT"));
    assert!(
        world.gateway.calls().is_empty(),
        "参数拒绝路径不得触达业务网关：{:?}",
        world.gateway.calls()
    );

    let streams = subscribers.collect(world).await;
    assert_nothing("Alice", streams.alice());
    assert_nothing("Bob", streams.bob());
    assert_nothing("Admin", streams.admin());
}

// ---------------------------------------------------------------------------
// 场景四：受影响用户不可确定的授权写操作 → 一律零发布（审查反馈 Medium-1）
// ---------------------------------------------------------------------------

/// `PATCH /api/v1/roles/{id}`、`PATCH /api/v1/groups/{id}`、`DELETE /api/v1/grants/{id}`
/// 的受影响用户集合在本层都不可确定（角色持有者集合、引用该组的授权可见范围、网关不回显
/// 授权归属人），因此**不得**向操作者管理员发一条关于其本人的 `permissions.changed`：
/// 那是指向错人的虚假提示。撤权安全性由全局 `authz_epoch` 递增 + SSE 读线每帧的权威复核
/// 关流保证（06 §4）。
///
/// 正对照用同一条总线上的一次 `POST /api/v1/grants`（目标 Bob）钉住「三条连接都订阅成功、
/// 总线也在投递」，因此上面的零帧结论是「未发布」而不是「订阅失效」。
#[tokio::test]
async fn ambiguous_affected_user_writes_publish_no_permissions_changed() {
    let world = World::new(7);
    let subscribers = Subscribers::open(&world).await;

    // 1) 改角色权限集：受影响的是该角色的全部持有者。
    let (status, body) = call(
        &world,
        write_request(
            "PATCH",
            &format!("/api/v1/roles/{MANAGED_ID}"),
            ADMIN,
            json!({"expected_revision": "5", "permissions": ["device.read"]}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "改角色应 200，实际 {body}");

    // 2) 归档组：受影响的是引用该组的授权可见范围内的用户。
    let (status, body) = call(
        &world,
        write_request(
            "PATCH",
            &format!("/api/v1/groups/{MANAGED_ID}"),
            ADMIN,
            json!({"expected_revision": "2", "archived": true}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "归档组应 200，实际 {body}");

    // 3) 删授权：网关不回显归属人。
    let (status, body) = call(
        &world,
        write_request(
            "DELETE",
            &format!("/api/v1/grants/{MANAGED_ID}"),
            ADMIN,
            json!({"expected_revision": "1"}),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "删授权应 200，实际 {body}");
    assert_eq!(
        world.gateway.calls(),
        vec!["roles.patch", "groups.patch", "grants.delete"],
        "三条写线都必须确实打到网关（否则零发布是空断言）"
    );

    // 正对照：目标用户明确的授权创建，仍然精准投给 Bob 本人。
    let (status, body) = call(
        &world,
        write_request(
            "POST",
            "/api/v1/grants",
            ADMIN,
            json!({
                "user_id": Uuid::from_bytes(BOB).to_string(),
                "source": {"kind": "direct", "permissions": ["device.read"]},
                "scope": {"kind": "all"},
            }),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "创建授权应 200，实际 {body}");

    let streams = subscribers.collect(world).await;
    assert_eq!(
        only_event("Bob", streams.bob(), "permissions.changed"),
        &json!({
            "kind": "permissions",
            "id": Uuid::from_bytes(BOB).to_string(),
            "epoch": "7",
        })
    );
    // 操作者管理员：三次由他发起的授权写操作，一帧都不该是他的。
    assert_nothing("Admin", streams.admin());
    assert_nothing("Alice", streams.alice());
}
