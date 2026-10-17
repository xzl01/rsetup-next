//! 26 管理路由动态网关 fixture（Identity Task 13b，05 子计划 Step 1）。
//!
//! 只证明「router → 业务 Gateway 方法」的动态接线：`SpyGateway` 以
//! `Arc<AtomicUsize>` 计数八个 Gateway 的全部方法调用；认证/Host/CSRF/临时密码
//! 拒绝路径必须计数恒零。**不**证明生产 SQLx、真实数据源、真实 probe 或真库
//! 验收（那归 Task 14 受控门禁）。
//!
//! 真实 Cookie 名称 `rsc_session` 已按 `src/http_auth.rs` 核对；fixture 值全部
//! synthetic，无任何凭据内容。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
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
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding};
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::model::{AdmissionState, ReviewDecision};

pub const HOST: &str = "controller.test";
pub const ORIGIN: &str = "http://controller.test";
/// 64 字节小写 hex 的 synthetic 会话 cookie 值（真实 cookie 名 `rsc_session`）。
pub const COOKIE_RAW: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const CSRF: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

pub struct TestAuth {
    pub must_change: bool,
}

impl AuthServiceApi for TestAuth {
    fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        Box::pin(async move {
            if raw != COOKIE_RAW {
                return Err(ControllerError::InvalidArgument);
            }
            Ok(Session {
                user: IdentityUser {
                    id: *Uuid::from_u128(0x00000000000040008000000000000001).as_bytes(),
                    username: "fixture_admin".into(),
                    password_hash: "synthetic-unused".into(),
                    active: true,
                    is_admin: true,
                    must_change_password: self.must_change,
                    revision: 1,
                },
                digest: token_digest(COOKIE_RAW.as_bytes()),
            })
        })
    }
    fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        *digest == token_digest(COOKIE_RAW.as_bytes())
    }
    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        Box::pin(async { Ok(1) })
    }
    fn change_password<'a>(
        &'a self,
        _s: &'a Session,
        _c: &'a str,
        _n: &'a str,
    ) -> AuthFuture<'a, ()> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn list_sessions<'a>(
        &'a self,
        _s: &'a Session,
        _c: Option<&'a str>,
        _l: usize,
    ) -> AuthFuture<'a, SessionPage> {
        Box::pin(async { Err(ControllerError::InvalidArgument) })
    }
    fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
        Box::pin(async { Ok(0) })
    }
}

/// 业务网关 spy：八个 Gateway 的每个方法调用都递增同一计数，证明
/// router→方法接线与拒绝路径零业务调用。
pub struct SpyGateway {
    pub calls: Arc<AtomicUsize>,
}

macro_rules! value_method {
    ($name:ident($($arg:ident: $ty:ty),*) => $result:expr) => {
        fn $name<'a>(&'a self, actor: &'a Session, $($arg: $ty),*) -> ManagementFuture<'a, Value> {
            let _ = (actor, $(&$arg),*);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result = $result;
            Box::pin(async move { Ok(result) })
        }
    };
}

fn page() -> Value {
    json!({"items": [], "next_cursor": null})
}
fn user() -> Value {
    json!({"id":"00000000-0000-4000-8000-000000000001","username":"fixture_user","display_name":"fixture","active":true,"is_admin":false,"must_change_password":false,"revision":"1"})
}
fn group() -> Value {
    json!({"id":"00000000-0000-4000-8000-000000000001","name":"fixture","archived":false,"revision":"1","member_count":"1"})
}

impl UsersGateway for SpyGateway {
    value_method!(list(cursor: Option<[u8; 16]>, limit: usize) => page());
    value_method!(create(username: &'a str, display_name: &'a str, is_admin: bool) =>
        json!({"user":user(),"temporary_password":"synthetic-only-not-real"}));
    value_method!(patch(id: [u8; 16], revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&'a str>) => user());
    value_method!(reset_password(id: [u8; 16], revision: u64) =>
        json!({"user":user(),"temporary_password":"synthetic-only-not-real"}));
    value_method!(effective_permissions(target: [u8; 16], device_id: Option<[u8; 32]>, cursor: Option<[u8; 32]>, limit: usize) => page());
}
impl RolesGateway for SpyGateway {
    value_method!(list(cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(name: &'a str, permissions: &'a [Permission]) =>
        json!({"id":"00000000-0000-4000-8000-000000000001","name":"fixture","builtin":false,"archived":false,"revision":"1","permissions":["device.read"]}));
    value_method!(patch(id: Uuid, revision: u64, name: Option<&'a str>, permissions: Option<&'a [Permission]>, archived: Option<bool>) =>
        json!({"id":id.to_string(),"revision":"2","name":"fixture","builtin":false,"archived":false,"permissions":["device.read"]}));
}
impl GrantsGateway for SpyGateway {
    value_method!(list(user_id: Option<[u8; 16]>, cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(user_id: [u8; 16], source: GrantSource, scope: GrantScope) =>
        json!({"id":"00000000-0000-4000-8000-000000000001","revision":"1","user_id":"00000000-0000-4000-8000-000000000001","source":{"kind":"direct","permissions":["device.read"]},"scope":{"kind":"all"}}));
    fn delete<'a>(
        &'a self,
        _actor: &'a Session,
        _id: Uuid,
        _revision: u64,
    ) -> ManagementFuture<'a, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}
impl GroupsGateway for SpyGateway {
    value_method!(list(cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(name: &'a str) => group());
    value_method!(patch(id: Uuid, revision: u64, name: Option<&'a str>, archived: Option<bool>) => group());
    value_method!(replace_members(id: Uuid, revision: u64, devices: &'a [[u8; 32]]) => group());
}
impl DevicesGateway for SpyGateway {
    value_method!(list_devices_visible(cursor: Option<[u8; 32]>, limit: usize, group_id: Option<Uuid>, search: Option<&'a str>) => page());
    fn find_device<'a>(
        &'a self,
        _actor: &'a Session,
        id: [u8; 32],
    ) -> ManagementFuture<'a, Option<(DeviceAdminView, std::collections::HashSet<Permission>)>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
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
    value_method!(patch(id: [u8; 32], revision: u64, display_name: &'a str) =>
        json!({"device_id":hex::encode(id),"display_name":display_name,"revision":"2"}));
}
impl ApprovalsGateway for SpyGateway {
    value_method!(list(decision: Option<ReviewDecision>, cursor: Option<[u8; 32]>, limit: usize) => page());
    value_method!(approve(targets: &'a [ApprovalTarget]) => json!({"results":[]}));
    value_method!(reject(targets: &'a [ApprovalTarget], reason: &'a str) => json!({"results":[]}));
    value_method!(reopen(id: [u8; 32], revision: u64) =>
        json!({"decision":"none","requires_device_reset":true}));
    value_method!(revoke(id: [u8; 32], revision: u64, reason: &'a str) =>
        json!({"state":"REVOKED","requires_device_reset":true}));
    value_method!(reauthorize(id: [u8; 32], revision: u64) =>
        json!({"state":"APPROVED","requires_device_reset":true}));
}
impl AuditGateway for SpyGateway {
    value_method!(list(filter: AuditFilter, cursor: Option<AuditCursor>, limit: usize) => page());
}
impl SystemGateway for SpyGateway {
    value_method!(status() =>
        json!({"ready":false,"version":"synthetic","schema_version":4,"users":"1","devices":"1","roles":"1","grants":"1","groups":"1","recovery_mode":"normal"}));
}

/// 单一测试 probe：设备实时视图、恢复模式与 commit 后通知的真实形状 synthetic 源。
struct TestProbe;
impl DeviceLiveProbe for TestProbe {
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
impl RecoveryModeProbe for TestProbe {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Ok(RecoveryView {
                ready: false,
                mode: "normal",
            })
        })
    }
}
impl ManagementNotificationSink for TestProbe {
    fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
        Ok(())
    }
}

pub fn fixture(must_change: bool) -> (Router<()>, Arc<AtomicUsize>) {
    // 测试进程内一次性注入合成 cursor 密钥 `SYNTHETIC_SECRET = [7u8; 32]`：仅测试
    // fixture 允许，保证 6 个 list 端点在测试进程内不因全局 OnceLock 未装返回 503，
    // 使 26 路由阳性测试正常通过并签发合法游标。生产 handler 严禁硬编码默认密钥
    // 或降级（生产密钥唯一来源是集成 Task 0 读取环境变量后安装）。
    static INIT_TEST_KEY: std::sync::Once = std::sync::Once::new();
    INIT_TEST_KEY.call_once(|| {
        rsetup_controller::api::install_management_cursor_secret(&[7u8; 32])
            .expect("synthetic cursor key installation must succeed");
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let spy = Arc::new(SpyGateway {
        calls: calls.clone(),
    });
    let auth = Arc::new(HttpAuthState::new(
        Arc::new(TestAuth { must_change }),
        HttpSecurityConfig {
            allowed_hosts: vec![HOST.into()],
            allowed_origin: Some(ORIGIN.into()),
            trust_tls: false,
        },
    ));
    auth.csrf.lock().unwrap().insert(
        token_digest(COOKIE_RAW.as_bytes()),
        CsrfBinding {
            token: CSRF.into(),
            issued_at: Instant::now(),
        },
    );
    let state = ManagementState {
        auth,
        users: spy.clone(),
        roles: spy.clone(),
        grants: spy.clone(),
        groups: spy.clone(),
        devices: spy.clone(),
        approvals: spy.clone(),
        audit: spy.clone(),
        system: spy,
        device_probe: Arc::new(TestProbe),
        recovery_probe: Arc::new(TestProbe),
        notification_sink: Arc::new(TestProbe),
    };
    (build_management_router(state), calls)
}

pub struct Case {
    pub method: &'static str,
    pub path: String,
    pub body: Value,
}

/// 恰 26 条管理路由矩阵（spec 02 §4 路由表全集）。
pub fn cases() -> Vec<Case> {
    let id = "00000000-0000-4000-8000-000000000001";
    let device = format!("{:064x}", 1);
    let target =
        json!({"targets":[{"device_id":device.clone(),"expected_revision":"1"}],"confirm":true});
    let mut reject = target.clone();
    reject["reason"] = json!("synthetic reason");
    vec![
        Case {
            method: "GET",
            path: "/api/v1/users".into(),
            body: json!({}),
        },
        Case {
            method: "POST",
            path: "/api/v1/users".into(),
            body: json!({"username":"fixture_user","display_name":"fixture","is_admin":false}),
        },
        Case {
            method: "PATCH",
            path: format!("/api/v1/users/{id}"),
            body: json!({"expected_revision":"1","display_name":"updated"}),
        },
        Case {
            method: "POST",
            path: format!("/api/v1/users/{id}/reset-password"),
            body: json!({"expected_revision":"1"}),
        },
        Case {
            method: "GET",
            path: format!("/api/v1/users/{id}/effective-permissions"),
            body: json!({}),
        },
        Case {
            method: "GET",
            path: "/api/v1/roles".into(),
            body: json!({}),
        },
        Case {
            method: "POST",
            path: "/api/v1/roles".into(),
            body: json!({"name":"fixture","permissions":["device.read"]}),
        },
        Case {
            method: "PATCH",
            path: format!("/api/v1/roles/{id}"),
            body: json!({"expected_revision":"1","name":"updated"}),
        },
        Case {
            method: "GET",
            path: "/api/v1/grants".into(),
            body: json!({}),
        },
        Case {
            method: "POST",
            path: "/api/v1/grants".into(),
            body: json!({"user_id":id,"source":{"kind":"direct","permissions":["device.read"]},"scope":{"kind":"all"}}),
        },
        Case {
            method: "DELETE",
            path: format!("/api/v1/grants/{id}"),
            body: json!({"expected_revision":"1"}),
        },
        Case {
            method: "GET",
            path: "/api/v1/groups".into(),
            body: json!({}),
        },
        Case {
            method: "POST",
            path: "/api/v1/groups".into(),
            body: json!({"name":"fixture"}),
        },
        Case {
            method: "PATCH",
            path: format!("/api/v1/groups/{id}"),
            body: json!({"expected_revision":"1","name":"updated"}),
        },
        Case {
            method: "PUT",
            path: format!("/api/v1/groups/{id}/members"),
            body: json!({"expected_revision":"1","device_ids":[device.clone()]}),
        },
        Case {
            method: "GET",
            path: "/api/v1/devices".into(),
            body: json!({}),
        },
        Case {
            method: "GET",
            path: format!("/api/v1/devices/{device}"),
            body: json!({}),
        },
        Case {
            method: "PATCH",
            path: format!("/api/v1/devices/{device}"),
            body: json!({"expected_revision":"1","display_name":"updated"}),
        },
        Case {
            method: "GET",
            path: "/api/v1/approvals".into(),
            body: json!({}),
        },
        Case {
            method: "POST",
            path: "/api/v1/approvals/approve".into(),
            body: target,
        },
        Case {
            method: "POST",
            path: "/api/v1/approvals/reject".into(),
            body: reject,
        },
        Case {
            method: "POST",
            path: format!("/api/v1/approvals/{device}/reopen"),
            body: json!({"expected_revision":"1","confirm":true}),
        },
        Case {
            method: "POST",
            path: format!("/api/v1/devices/{device}/revoke"),
            body: json!({"expected_revision":"1","reason":"synthetic reason","confirm":true}),
        },
        Case {
            method: "POST",
            path: format!("/api/v1/devices/{device}/reauthorize"),
            body: json!({"expected_revision":"1","confirm":true}),
        },
        Case {
            method: "GET",
            path: "/api/v1/audit".into(),
            body: json!({}),
        },
        Case {
            method: "GET",
            path: "/api/v1/system/status".into(),
            body: json!({}),
        },
    ]
}

pub fn request(
    case: &Case,
    authenticated: bool,
    with_host: bool,
    valid_csrf: bool,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(case.method)
        .uri(case.path.as_str())
        .header("origin", ORIGIN)
        .header("content-type", "application/json");
    if with_host {
        builder = builder.header("host", HOST);
    }
    if authenticated {
        builder = builder.header("cookie", format!("rsc_session={COOKIE_RAW}"));
    }
    builder = builder.header("x-csrf-token", if valid_csrf { CSRF } else { "invalid" });
    builder.body(Body::from(case.body.to_string())).unwrap()
}
