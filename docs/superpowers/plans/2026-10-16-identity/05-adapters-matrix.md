# Controller v1 Task4 Identity：生产管理适配器与真实网关调用矩阵

> **执行者：** 本文替代Identity execution Task13，顶层lib/main归独立集成Task0。本文尚未正式批准，不授权当前业务实现/测试/真库/提交/合并。

**Goal:** 八个dyn-safe Gateway接真实仓储/服务，26管理路由以动态业务spy覆盖拒绝零调用与授权阳性。

**Architecture:** 显式ManagementState，共享权威认证边界，仓储同事务重验当前actor，commit后typed通知。fixture、SQLx、真实同源分层。

**Tech Stack:** 既有Rust/Axum/Tokio/SQLx/serde/sha2/hex/uuid/rand/tower，零新增依赖。

**Spec:** [主Identity计划](../2026-10-16-controller-v1-four-task4-identity-execution.md)、[收敛设计](../../specs/2026-10-07-controller-v1-four-task4-design.md)、[01](../../specs/2026-09-23-controller-v1-01-identity-access.md)、[02](../../specs/2026-09-23-controller-v1-02-data-api.md)。

## Global Constraints

不改AppState本体/认证模型、不拥有顶层router；不复制gate、不返回常量生产probe；无真实源不挂载。新审计ADMIN namespace保留AUTH/CAS旧namespace。请求含nested source/scope严格未知字段拒绝；污染503/requested missing404。JSON≤1_048_576，写session20/s、读模板+session50/s。无secret读取、真实reboot、DROP DATABASE、push；真库由独立门禁逐引擎单case串行。编译错误/缺fixture/0用例不算RED。

## Files与接口

Create `crates/rsetup-controller/src/api/adapters.rs`、`tests/management_gateway_http.rs`、`tests/support/management_http_fixture.rs`；Modify `src/api/mod.rs`注册adapters及资源consumer/DTO、`src/authz/service.rs`分页来源、`src/devices/repository.rs`筛选/审批/同tx组替换结果。不改main/lib顶层。

Consumes主Task6八trait/状态、Task3/4真实repo、Task5服务；Produces `build_sqlx_management_state(db,auth,device_probe,recovery_probe,notification_sink)->ManagementState`，只由集成owner传真实源构造。

## 当前实际源码核对

真实Cookie名称为`rsc_session`，已read `src/http_auth.rs:993–1021`确认；fixture使用64字节小写hex值，不猜名称。AuthServiceApi九方法签名、Session字段、CsrfBinding及限流状态已按真实源码核对。26路由fixture全部synthetic，无凭据内容。

## 26路由动态网关fixture

- [ ] **Step 1：完整fixture与矩阵。** resources先可编译；未委托handler时合法请求因计数0失败；若handler已交付，矩阵记回归，不伪造过去RED。fixture只证明router→方法接线。

```rust
// tests/support/management_http_fixture.rs
use std::{collections::HashSet, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Instant};
use axum::{Router, body::Body, http::Request};
use serde_json::{Value, json};
use uuid::Uuid;
use rsetup_controller::{ControllerError, HttpAuthState};
use rsetup_controller::auth::{service::{IdentityUser, Login, Session, SessionPage}, session::token_digest};
use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding};
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::api::{ManagementState, ManagementFuture, build_management_router};
use rsetup_controller::api::users::UsersGateway;
use rsetup_controller::api::roles::RolesGateway;
use rsetup_controller::api::grants::GrantsGateway;
use rsetup_controller::api::groups::GroupsGateway;
use rsetup_controller::api::devices::{DevicesGateway, DeviceLiveProbe, DeviceLiveView, DeviceConnectionState, DeviceStreamHealth};
use rsetup_controller::api::approvals::{ApprovalsGateway, ApprovalTarget, ManagementNotificationSink};
use rsetup_controller::api::audit::AuditGateway;
use rsetup_controller::api::system::{SystemGateway, RecoveryModeProbe, RecoveryView};
use rsetup_controller::authz::policy::{Permission, GrantSource, GrantScope};
use rsetup_controller::devices::repository::{DeviceAdminView, AuditCursor, AuditFilter};
use rsetup_controller::model::{AdmissionState, ReviewDecision};

pub const HOST: &str = "controller.test";
pub const ORIGIN: &str = "http://controller.test";
pub const COOKIE_RAW: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const CSRF: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

pub struct TestAuth { pub must_change: bool }
impl AuthServiceApi for TestAuth {
    fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> { Box::pin(async { Err(ControllerError::InvalidArgument) }) }
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        Box::pin(async move {
            if raw != COOKIE_RAW { return Err(ControllerError::InvalidArgument); }
            Ok(Session { user: IdentityUser {
                id: *Uuid::from_u128(0x00000000000040008000000000000001).as_bytes(),
                username: "fixture_admin".into(), password_hash: "synthetic-unused".into(),
                active: true, is_admin: true, must_change_password: self.must_change, revision: 1,
            }, digest: token_digest(COOKIE_RAW.as_bytes()) })
        })
    }
    fn is_live_session(&self, digest: &[u8; 32]) -> bool { *digest == token_digest(COOKIE_RAW.as_bytes()) }
    fn authz_epoch(&self) -> AuthFuture<'_, u64> { Box::pin(async { Ok(1) }) }
    fn change_password<'a>(&'a self, _s: &'a Session, _c: &'a str, _n: &'a str) -> AuthFuture<'a, ()> { Box::pin(async { Err(ControllerError::InvalidArgument) }) }
    fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn list_sessions<'a>(&'a self, _s: &'a Session, _c: Option<&'a str>, _l: usize) -> AuthFuture<'a, SessionPage> { Box::pin(async { Err(ControllerError::InvalidArgument) }) }
    fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> { Box::pin(async { Ok(false) }) }
    fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> { Box::pin(async { Ok(0) }) }
}

pub struct SpyGateway { pub calls: Arc<AtomicUsize> }
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
fn page() -> Value { json!({"items": [], "next_cursor": null}) }
fn user() -> Value { json!({"id":"00000000-0000-4000-8000-000000000001","username":"fixture_user","display_name":"fixture","active":true,"is_admin":false,"must_change_password":false,"revision":"1"}) }
fn group() -> Value { json!({"id":"00000000-0000-4000-8000-000000000001","name":"fixture","archived":false,"revision":"1","member_count":"1"}) }
impl UsersGateway for SpyGateway {
    value_method!(list(cursor: Option<[u8;16]>, limit: usize) => page());
    value_method!(create(username: &'a str, display_name: &'a str, is_admin: bool) => json!({"user":user(),"temporary_password":"synthetic-only-not-real"}));
    value_method!(patch(id: [u8;16], revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&'a str>) => user());
    value_method!(reset_password(id: [u8;16], revision: u64) => json!({"user":user(),"temporary_password":"synthetic-only-not-real"}));
    value_method!(effective_permissions(target: [u8;16], device_id: Option<[u8;32]>, cursor: Option<[u8;32]>, limit: usize) => page());
}
impl RolesGateway for SpyGateway {
    value_method!(list(cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(name: &'a str, permissions: &'a [Permission]) => json!({"id":"00000000-0000-4000-8000-000000000001","name":"fixture","builtin":false,"archived":false,"revision":"1","permissions":["device.read"]}));
    value_method!(patch(id: Uuid, revision: u64, name: Option<&'a str>, permissions: Option<&'a [Permission]>, archived: Option<bool>) => json!({"id":id.to_string(),"revision":"2","name":"fixture","builtin":false,"archived":false,"permissions":["device.read"]}));
}
impl GrantsGateway for SpyGateway {
    value_method!(list(user_id: Option<[u8;16]>, cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(user_id: [u8;16], source: GrantSource, scope: GrantScope) => json!({"id":"00000000-0000-4000-8000-000000000001","revision":"1","user_id":"00000000-0000-4000-8000-000000000001","source":{"kind":"direct","permissions":["device.read"]},"scope":{"kind":"all"}}));
    fn delete<'a>(&'a self, _actor: &'a Session, _id: Uuid, _revision: u64) -> ManagementFuture<'a, ()> { self.calls.fetch_add(1, Ordering::SeqCst); Box::pin(async { Ok(()) }) }
}
impl GroupsGateway for SpyGateway {
    value_method!(list(cursor: Option<Uuid>, limit: usize) => page());
    value_method!(create(name: &'a str) => group());
    value_method!(patch(id: Uuid, revision: u64, name: Option<&'a str>, archived: Option<bool>) => group());
    value_method!(replace_members(id: Uuid, revision: u64, devices: &'a [[u8;32]]) => group());
}
impl DevicesGateway for SpyGateway {
    value_method!(list_devices_visible(cursor: Option<[u8;32]>, limit: usize, group_id: Option<Uuid>, search: Option<&'a str>) => page());
    fn find_device<'a>(&'a self, _actor: &'a Session, id: [u8;32]) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(Some((DeviceAdminView { public_key:id, display_name:"fixture".into(), admission_state:AdmissionState::Approved, review_decision:ReviewDecision::Approved, revision:1, descriptor:None, archived:false }, HashSet::from([Permission::DeviceRead])))) })
    }
    value_method!(patch(id: [u8;32], revision: u64, display_name: &'a str) => json!({"device_id":hex::encode(id),"display_name":display_name,"revision":"2"}));
}
impl ApprovalsGateway for SpyGateway {
    value_method!(list(decision: Option<ReviewDecision>, cursor: Option<[u8;32]>, limit: usize) => page());
    value_method!(approve(targets: &'a [ApprovalTarget]) => json!({"results":[]}));
    value_method!(reject(targets: &'a [ApprovalTarget], reason: &'a str) => json!({"results":[]}));
    value_method!(reopen(id: [u8;32], revision: u64) => json!({"decision":"none","requires_device_reset":true}));
    value_method!(revoke(id: [u8;32], revision: u64, reason: &'a str) => json!({"state":"REVOKED","requires_device_reset":true}));
    value_method!(reauthorize(id: [u8;32], revision: u64) => json!({"state":"APPROVED","requires_device_reset":true}));
}
impl AuditGateway for SpyGateway { value_method!(list(filter: AuditFilter, cursor: Option<AuditCursor>, limit: usize) => page()); }
impl SystemGateway for SpyGateway { value_method!(status() => json!({"ready":false,"version":"synthetic","schema_version":4,"users":"1","devices":"1","roles":"1","grants":"1","groups":"1","recovery_mode":"normal"})); }
struct TestProbe;
impl DeviceLiveProbe for TestProbe { fn snapshot<'a>(&'a self, _id: &'a [u8;32]) -> ManagementFuture<'a, DeviceLiveView> { Box::pin(async { Ok(DeviceLiveView { connection_state:DeviceConnectionState::Offline, control_health:DeviceStreamHealth::Offline, data_health:DeviceStreamHealth::Offline, capabilities:None }) }) } }
impl RecoveryModeProbe for TestProbe { fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> { Box::pin(async { Ok(RecoveryView{ready:false,mode:"normal"}) }) } }
impl ManagementNotificationSink for TestProbe { fn after_commit(&self, _event: &rsetup_controller::devices::service::AdmissionChangeEvent) -> Result<(), ControllerError> { Ok(()) } }

pub fn fixture(must_change: bool) -> (Router<()>, Arc<AtomicUsize>) {
    // 集成测试环境前置注入测试合成密钥 `SYNTHETIC_SECRET = [7u8; 32]`，保证 6 个 list 端点在测试进程内不会因全局 OnceLock 未装返回 503，使 26 路由阳性测试正常通过并签发合法游标。生产 handler 严禁硬编码默认密钥或降级。
    static INIT_TEST_KEY: std::sync::Once = std::sync::Once::new();
    INIT_TEST_KEY.call_once(|| {
        rsetup_controller::api::install_management_cursor_secret(&[7u8; 32])
            .expect("synthetic cursor key installation must succeed");
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let spy = Arc::new(SpyGateway { calls:calls.clone() });
    let auth = Arc::new(HttpAuthState::new(Arc::new(TestAuth { must_change }), HttpSecurityConfig { allowed_hosts:vec![HOST.into()], allowed_origin:Some(ORIGIN.into()), trust_tls:false }));
    auth.csrf.lock().unwrap().insert(token_digest(COOKIE_RAW.as_bytes()), CsrfBinding { token:CSRF.into(), issued_at:Instant::now() });
    let state = ManagementState { auth, users:spy.clone(), roles:spy.clone(), grants:spy.clone(), groups:spy.clone(), devices:spy.clone(), approvals:spy.clone(), audit:spy.clone(), system:spy, device_probe:Arc::new(TestProbe), recovery_probe:Arc::new(TestProbe), notification_sink:Arc::new(TestProbe) };
    (build_management_router(state), calls)
}

pub struct Case { pub method: &'static str, pub path: String, pub body: Value }
pub fn cases() -> Vec<Case> {
    let id = "00000000-0000-4000-8000-000000000001";
    let device = format!("{:064x}", 1);
    let target = json!({"targets":[{"device_id":device.clone(),"expected_revision":"1"}],"confirm":true});
    let mut reject = target.clone(); reject["reason"] = json!("synthetic reason");
    vec![
        Case{method:"GET",path:"/api/v1/users".into(),body:json!({})},
        Case{method:"POST",path:"/api/v1/users".into(),body:json!({"username":"fixture_user","display_name":"fixture","is_admin":false})},
        Case{method:"PATCH",path:format!("/api/v1/users/{id}"),body:json!({"expected_revision":"1","display_name":"updated"})},
        Case{method:"POST",path:format!("/api/v1/users/{id}/reset-password"),body:json!({"expected_revision":"1"})},
        Case{method:"GET",path:format!("/api/v1/users/{id}/effective-permissions"),body:json!({})},
        Case{method:"GET",path:"/api/v1/roles".into(),body:json!({})},
        Case{method:"POST",path:"/api/v1/roles".into(),body:json!({"name":"fixture","permissions":["device.read"]})},
        Case{method:"PATCH",path:format!("/api/v1/roles/{id}"),body:json!({"expected_revision":"1","name":"updated"})},
        Case{method:"GET",path:"/api/v1/grants".into(),body:json!({})},
        Case{method:"POST",path:"/api/v1/grants".into(),body:json!({"user_id":id,"source":{"kind":"direct","permissions":["device.read"]},"scope":{"kind":"all"}})},
        Case{method:"DELETE",path:format!("/api/v1/grants/{id}"),body:json!({"expected_revision":"1"})},
        Case{method:"GET",path:"/api/v1/groups".into(),body:json!({})},
        Case{method:"POST",path:"/api/v1/groups".into(),body:json!({"name":"fixture"})},
        Case{method:"PATCH",path:format!("/api/v1/groups/{id}"),body:json!({"expected_revision":"1","name":"updated"})},
        Case{method:"PUT",path:format!("/api/v1/groups/{id}/members"),body:json!({"expected_revision":"1","device_ids":[device.clone()]})},
        Case{method:"GET",path:"/api/v1/devices".into(),body:json!({})},
        Case{method:"GET",path:format!("/api/v1/devices/{device}"),body:json!({})},
        Case{method:"PATCH",path:format!("/api/v1/devices/{device}"),body:json!({"expected_revision":"1","display_name":"updated"})},
        Case{method:"GET",path:"/api/v1/approvals".into(),body:json!({})},
        Case{method:"POST",path:"/api/v1/approvals/approve".into(),body:target},
        Case{method:"POST",path:"/api/v1/approvals/reject".into(),body:reject},
        Case{method:"POST",path:format!("/api/v1/approvals/{device}/reopen"),body:json!({"expected_revision":"1","confirm":true})},
        Case{method:"POST",path:format!("/api/v1/devices/{device}/revoke"),body:json!({"expected_revision":"1","reason":"synthetic reason","confirm":true})},
        Case{method:"POST",path:format!("/api/v1/devices/{device}/reauthorize"),body:json!({"expected_revision":"1","confirm":true})},
        Case{method:"GET",path:"/api/v1/audit".into(),body:json!({})},
        Case{method:"GET",path:"/api/v1/system/status".into(),body:json!({})},
    ]
}

pub fn request(case: &Case, authenticated: bool, with_host: bool, valid_csrf: bool) -> Request<Body> {
    let mut builder = Request::builder().method(case.method).uri(case.path.as_str()).header("origin", ORIGIN).header("content-type","application/json");
    if with_host { builder = builder.header("host", HOST); }
    if authenticated { builder = builder.header("cookie", format!("rsc_session={COOKIE_RAW}")); }
    builder = builder.header("x-csrf-token", if valid_csrf { CSRF } else { "invalid" });
    builder.body(Body::from(case.body.to_string())).unwrap()
}
```

```rust
// tests/management_gateway_http.rs
#[path = "support/management_http_fixture.rs"] mod fixture;
use axum::{body::to_bytes, http::StatusCode};
use std::sync::atomic::Ordering;
use tower::ServiceExt;

#[tokio::test]
async fn all_26_missing_sessions_return_401_and_zero_business_calls() {
    assert_eq!(fixture::cases().len(),26);
    for case in fixture::cases() {
        let (router,calls)=fixture::fixture(false);
        let response=router.oneshot(fixture::request(&case,false,true,true)).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED,"{} {}",case.method,case.path);
        let bytes=to_bytes(response.into_body(),1_048_576).await.unwrap();
        let body:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"],"AUTH_REQUIRED");
        assert_eq!(calls.load(Ordering::SeqCst),0);
    }
}
#[tokio::test]
async fn all_26_allowed_requests_really_reach_their_business_gateway() {
    for case in fixture::cases() {
        let (router,calls)=fixture::fixture(false);
        let response=router.oneshot(fixture::request(&case,true,true,true)).await.unwrap();
        assert!(response.status().is_success(),"{} {}",case.method,case.path);
        assert!(calls.load(Ordering::SeqCst)>0,"方法spy必须增长，不得恒零");
    }
}
#[tokio::test]
async fn host_and_temporary_password_and_write_csrf_reject_before_gateway() {
    for case in fixture::cases() {
        for mode in [0,1,2] {
            if mode==2 && case.method=="GET" { continue; }
            let (router,calls)=fixture::fixture(mode==1);
            let response=router.oneshot(fixture::request(&case,true,mode!=0,mode!=2)).await.unwrap();
            assert_eq!(response.status(),StatusCode::FORBIDDEN);
            let bytes=to_bytes(response.into_body(),1_048_576).await.unwrap();
            let body:serde_json::Value=serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"],if mode==1 {"PASSWORD_CHANGE_REQUIRED"} else {"CSRF_INVALID"});
            assert_eq!(calls.load(Ordering::SeqCst),0);
        }
    }
}
```

- [ ] **Step 2：确认RED。** 设计根命令`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller --test management_gateway_http`恰好3方法。目标合法阳性在未委托时counter=0/404失败；认证前置失败不是此目标RED。已有handler则记回归，不编造过去RED。
- [ ] **Step 3 GREEN：** 共享gate后调用对应trait，固定投影包装ok_response，delete→deleted:true，详情单snapshot及probe错误503，system传actor并读probe；nested请求strict DTO。重跑3方法和资源旧测试、fmt/clippy。
- [ ] **Step 4：证据边界。** 仅router→方法动态计数，尚不证明生产SQLx/真实源/同源验收。

## 真实生产适配器

Create adapters.rs；代码消费者只使用主计划的唯一repo/service/trait，不另造授权来源。list投影cursor先为内部raw key，handler经HTTP边界helper包装next_cursor并严格解码入参；不得把raw cursor直接发HTTP。该统一签名cursor函数对即主计划Task 6小节的`encode_signed_cursor`/`decode_signed_cursor`（内部唯一真源，本文件代码不得裸调），按`cursor_kind`双profile签发外部signed游标（64hex覆盖users/roles/grants/groups，96hex覆盖devices/approvals）；生产owner=独立集成Task 0：启动时读取`CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`并install到api/mod.rs的OnceLock，Task 13只构造adapters/state不加载密钥，不得扩ManagementState/ApiState；handler只能调用HTTP边界helper `decode_management_list_cursor`/`sign_management_list_page`完成入参解码与出口签发；不得在子计划内另造第二套cursor实现。gateway各list方法返回的`next.map(hex::encode)`仅为内部raw key（UUID 32hex=16B，设备公钥64hex=32B），不得直接外传；HTTP出口一律为signed外部64/96hex，无raw泄漏。

```rust
// src/api/adapters.rs
use std::{collections::HashSet, sync::Arc};
use serde_json::{Value, json};
use uuid::Uuid;
use crate::{DbPool, ControllerError};
use crate::auth::service::Session;
use crate::http_auth::HttpAuthState;
use crate::authz::{policy::{Permission, GrantSource, GrantScope, GrantRecord, RoleRecord},
    sqlx_repo::{SqlxAuthzRepository, SqlxAuthorizationSnapshotRepository, AuthzRepository, GroupRecord}};
use crate::authz::service::AuthzService;
use crate::devices::{repository::{SqlxDeviceRepository, DeviceRepository, DeviceAdminView, AuditFilter, AuditCursor},
    service::{DeviceService, AdmissionChangeEvent, AdmissionChangeSink, DecisionResult}};
use crate::model::ReviewDecision;
use super::{ManagementFuture, ManagementState};
use super::{users::UsersGateway, roles::RolesGateway, grants::GrantsGateway, groups::GroupsGateway,
    devices::{DevicesGateway, DeviceLiveProbe}, approvals::{ApprovalsGateway, ApprovalTarget, ManagementNotificationSink},
    audit::AuditGateway, system::{SystemGateway, RecoveryModeProbe, RecoveryView}};

pub type SqlxManagementAuthzService = AuthzService<SqlxAuthorizationSnapshotRepository, SqlxAuthzRepository>;
pub struct NotificationBridge(Arc<dyn ManagementNotificationSink>);
impl AdmissionChangeSink for NotificationBridge {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ()> {
        self.0.after_commit(event).map_err(|_| ())
    }
}

pub struct SqlxManagementGateway {
    repo: Arc<SqlxAuthzRepository>,
    devices: Arc<SqlxDeviceRepository>,
    authz: Arc<SqlxManagementAuthzService>,
    admission: Arc<DeviceService<DbPool, NotificationBridge>>,
}
fn page(items: Vec<Value>, next: Option<String>) -> Value { json!({"items":items,"next_cursor":next}) }
fn role_value(role: &RoleRecord) -> Value {
    let mut permissions:Vec<_>=role.permissions.iter().map(Permission::as_str).collect();
    permissions.sort_unstable();
    json!({"id":role.id.to_string(),"name":role.name,"builtin":role.builtin,"archived":role.archived,"revision":role.revision.to_string(),"permissions":permissions})
}
fn group_value(group: &GroupRecord) -> Value {
    json!({"id":group.id.to_string(),"name":group.name,"archived":group.archived,"revision":group.revision.to_string(),"member_count":group.member_count.to_string()})
}
fn grant_value(grant: &GrantRecord) -> Value {
    let source=match &grant.source {
        GrantSource::Role(id)=>json!({"kind":"role","role_id":id.to_string()}),
        GrantSource::Direct(permissions)=>{
            let mut values:Vec<_>=permissions.iter().map(Permission::as_str).collect();values.sort_unstable();
            json!({"kind":"direct","permissions":values})
        }
    };
    let scope=match &grant.scope {
        GrantScope::All=>json!({"kind":"all"}),
        GrantScope::Group(id)=>json!({"kind":"group","group_id":id.to_string()}),
        GrantScope::Device(id)=>json!({"kind":"device","device_id":hex::encode(id)}),
    };
    json!({"id":grant.id.to_string(),"user_id":Uuid::from_bytes(grant.user_id).to_string(),"source":source,"scope":scope,"revision":grant.revision.to_string()})
}
fn admission_text(state: crate::model::AdmissionState) -> &'static str {
    match state { crate::model::AdmissionState::Pending=>"PENDING", crate::model::AdmissionState::Approved=>"APPROVED", crate::model::AdmissionState::Revoked=>"REVOKED" }
}
fn decision_text(decision: ReviewDecision) -> &'static str {
    match decision { ReviewDecision::None=>"none", ReviewDecision::Approved=>"approved", ReviewDecision::Denied=>"denied", ReviewDecision::Revoked=>"revoked" }
}
fn device_value(device: &DeviceAdminView) -> Value {
    json!({"device_id":hex::encode(device.public_key),"display_name":device.display_name,"descriptor":device.descriptor,
        "admission_state":admission_text(device.admission_state),"review_decision":decision_text(device.review_decision),"revision":device.revision.to_string()})
}
fn decision_value(result: DecisionResult, reopen: bool) -> Value {
    if reopen {
        json!({"decision":decision_text(result.snapshot.review_decision),"requires_device_reset":result.requires_device_reset})
    } else {
        json!({"state":admission_text(result.snapshot.admission_state),"requires_device_reset":result.requires_device_reset})
    }
}
fn batch_item(id: [u8;32], result: Result<DecisionResult,ControllerError>) -> Value {
    match result {
        Ok(result)=>json!({"device_id":hex::encode(id),"ok":true,"revision":result.snapshot.revision.to_string()}),
        Err(error)=>{
            let code=match error {
                ControllerError::RevisionConflict=>"REVISION_CONFLICT",
                ControllerError::InvalidState=>"INVALID_STATE",
                ControllerError::PermissionDenied=>"PERMISSION_DENIED",
                ControllerError::NotFound=>"NOT_FOUND",
                ControllerError::InvalidArgument=>"INVALID_ARGUMENT",
                ControllerError::ResourceExhausted=>"RESOURCE_EXHAUSTED",
                ControllerError::Database(_)|ControllerError::Config(_)=>"STORAGE_UNAVAILABLE",
                _=>"NOT_READY",
            };
            json!({"device_id":hex::encode(id),"ok":false,"error":{"code":code}})
        }
    }
}
macro_rules! method {
    ($name:ident($($arg:ident: $ty:ty),*) ; $this:ident ; $body:block) => {
        fn $name<'a>(&'a self, $($arg:$ty),*) -> ManagementFuture<'a,Value> {
            let $this=self;
            Box::pin(async move $body)
        }
    }
}
impl UsersGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,cursor:Option<[u8;16]>,limit:usize);g;{
        // 仓储内部键 Option<[u8;16]> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（UUID profile）校验解析后传入；
        // 返回的上一页末 ID 由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），管理端点不得签发/接受裸内部键。
        let (rows,next)=g.repo.list_users(actor,cursor,limit).await?;
        Ok(page(rows.iter().map(crate::http_api::user_admin_public).collect(),next.map(hex::encode)))
    });
    method!(create(actor:&'a Session,username:&'a str,display_name:&'a str,is_admin:bool);g;{
        let (user,plain)=g.authz.create_user(actor,username,display_name,is_admin).await?;
        Ok(json!({"user":crate::http_api::user_admin_public(&user),"temporary_password":plain}))
    });
    method!(patch(actor:&'a Session,id:[u8;16],revision:u64,active:Option<bool>,is_admin:Option<bool>,display_name:Option<&'a str>);g;{
        Ok(crate::http_api::user_admin_public(&g.repo.patch_user(actor,id,revision,active,is_admin,display_name).await?))
    });
    method!(reset_password(actor:&'a Session,id:[u8;16],revision:u64);g;{
        let (user,plain)=g.authz.reset_user_password(actor,id,revision).await?;
        Ok(json!({"user":crate::http_api::user_admin_public(&user),"temporary_password":plain}))
    });
    method!(effective_permissions(actor:&'a Session,target:[u8;16],device_id:Option<[u8;32]>,cursor:Option<[u8;32]>,limit:usize);g;{
        g.authz.effective_permissions_page(actor,target,device_id,cursor,limit).await
    });
}
impl RolesGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,cursor:Option<Uuid>,limit:usize);g;{
        // 仓储内部键 Option<Uuid> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（UUID profile）校验解析后传入；返回的上一页末 ID 由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），端点不直接外传裸键。
        let (rows,next)=g.repo.list_roles(actor,cursor,limit).await?;
        Ok(page(rows.iter().map(role_value).collect(),next.map(|id|hex::encode(id.as_bytes()))))
    });
    method!(create(actor:&'a Session,name:&'a str,permissions:&'a [Permission]);g;{Ok(role_value(&g.repo.create_role(actor,name,permissions).await?))});
    method!(patch(actor:&'a Session,id:Uuid,revision:u64,name:Option<&'a str>,permissions:Option<&'a [Permission]>,archived:Option<bool>);g;{Ok(role_value(&g.repo.patch_role(actor,id,revision,name,permissions,archived).await?))});
}
impl GrantsGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,user_id:Option<[u8;16]>,cursor:Option<Uuid>,limit:usize);g;{
        // 仓储内部键 Option<Uuid> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（UUID profile，入参绑定 user_id 等 filter 上下文）校验解析后传入；返回的上一页末 ID 由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），端点不直接外传裸键。
        let (rows,next)=g.repo.list_grants(actor,user_id,cursor,limit).await?;
        Ok(page(rows.iter().map(grant_value).collect(),next.map(|id|hex::encode(id.as_bytes()))))
    });
    method!(create(actor:&'a Session,user_id:[u8;16],source:GrantSource,scope:GrantScope);g;{Ok(grant_value(&g.repo.create_grant(actor,user_id,source,scope).await?))});
    fn delete<'a>(&'a self,actor:&'a Session,id:Uuid,revision:u64)->ManagementFuture<'a,()>{Box::pin(async move{self.repo.delete_grant(actor,id,revision).await})}
}
impl GroupsGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,cursor:Option<Uuid>,limit:usize);g;{
        // 仓储内部键 Option<Uuid> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（UUID profile）校验解析后传入；返回的上一页末 ID 由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），端点不直接外传裸键。
        let (rows,next)=g.repo.list_groups(actor,cursor,limit).await?;
        Ok(page(rows.iter().map(group_value).collect(),next.map(|id|hex::encode(id.as_bytes()))))
    });
    method!(create(actor:&'a Session,name:&'a str);g;{Ok(group_value(&g.repo.create_group(actor,name).await?))});
    method!(patch(actor:&'a Session,id:Uuid,revision:u64,name:Option<&'a str>,archived:Option<bool>);g;{Ok(group_value(&g.repo.patch_group(actor,id,revision,name,archived).await?))});
    method!(replace_members(actor:&'a Session,id:Uuid,revision:u64,devices:&'a [[u8;32]]);g;{Ok(group_value(&g.devices.replace_group_members(actor,id,revision,devices).await?))});
}
impl DevicesGateway for SqlxManagementGateway {
    method!(list_devices_visible(actor:&'a Session,cursor:Option<[u8;32]>,limit:usize,group_id:Option<Uuid>,search:Option<&'a str>);g;{
        // 仓储内部键 Option<[u8;32]> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（设备公钥 profile，入参绑定 group_id 与 search 等 filter 上下文）校验解析后传入；返回的上一页末设备公钥由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），端点不直接外传裸公钥。
        let (rows,next)=g.devices.device_page_filtered(actor,cursor,limit,group_id,search).await?;
        Ok(page(rows.iter().map(|(view,permissions)|crate::authz::policy::minimum_device_projection(view.public_key,&view.display_name,permissions)).collect(),next.map(hex::encode)))
    });
    fn find_device<'a>(&'a self,actor:&'a Session,id:[u8;32])->ManagementFuture<'a,Option<(DeviceAdminView,HashSet<Permission>)>>{Box::pin(async move{self.devices.find_device(actor,id).await})}
    method!(patch(actor:&'a Session,id:[u8;32],revision:u64,display_name:&'a str);g;{Ok(device_value(&g.devices.patch_device_display_name(actor,id,revision,display_name).await?))});
}
impl ApprovalsGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,decision:Option<ReviewDecision>,cursor:Option<[u8;32]>,limit:usize);g;{
        // 仓储内部键 Option<[u8;32]> 由 handler 经 HTTP 边界 helper decode_management_list_cursor（设备公钥 profile，入参绑定 decision 等 filter 上下文）校验解析后传入；返回的上一页末设备公钥由 handler 出口经 sign_management_list_page（内部统一签名）签发为 next_cursor（gateway 返回仅为 raw 内部分页键），端点不直接外传裸键。
        g.devices.approval_page(actor,decision,cursor,limit).await
    });
    method!(approve(actor:&'a Session,targets:&'a [ApprovalTarget]);g;{
        if targets.is_empty()||targets.len()>1024{return Err(ControllerError::InvalidArgument);}
        let mut results=Vec::with_capacity(targets.len());
        for target in targets{results.push(batch_item(target.device_id,g.admission.approve(actor,target.device_id,target.expected_revision).await));}
        Ok(json!({"results":results}))
    });
    method!(reject(actor:&'a Session,targets:&'a [ApprovalTarget],reason:&'a str);g;{
        if targets.is_empty()||targets.len()>1024||reason.trim().is_empty()||reason.len()>1024||reason.chars().any(char::is_control){return Err(ControllerError::InvalidArgument);}
        let mut results=Vec::with_capacity(targets.len());
        for target in targets{results.push(batch_item(target.device_id,g.admission.reject(actor,target.device_id,target.expected_revision,reason).await));}
        Ok(json!({"results":results}))
    });
    method!(reopen(actor:&'a Session,id:[u8;32],revision:u64);g;{Ok(decision_value(g.admission.reopen(actor,id,revision).await?,true))});
    method!(revoke(actor:&'a Session,id:[u8;32],revision:u64,reason:&'a str);g;{Ok(decision_value(g.admission.revoke(actor,id,revision,reason).await?,false))});
    method!(reauthorize(actor:&'a Session,id:[u8;32],revision:u64);g;{Ok(decision_value(g.admission.reauthorize(actor,id,revision).await?,false))});
}
impl AuditGateway for SqlxManagementGateway {
    method!(list(actor:&'a Session,filter:AuditFilter,cursor:Option<AuditCursor>,limit:usize);g;{
        let (rows,next)=g.devices.audit_page(actor,filter,cursor,limit).await?;
        let items=rows.into_iter().map(|row|json!({"id":row.id.to_string(),"actor_kind":row.actor_kind,"actor_user_id":row.actor_user_id.map(|id|Uuid::from_bytes(id).to_string()),"event_type":row.event_type,"target_kind":row.target_kind,"target_id":row.target_id,"params_redacted":row.params_redacted,"outcome":row.outcome,"process_epoch":row.process_epoch.to_string(),"event_seq":row.event_seq.to_string(),"time_evidence":row.created_evidence})).collect();
        // audit handler使用自己的复合cursor签名；raw三元键编码只在内部传递。
        let raw=next.map(|next|format!("{}{:016x}{}",hex::encode(next.process_epoch.as_bytes()),next.event_seq,hex::encode(next.id.as_bytes())));
        Ok(page(items,raw))
    });
}
impl SystemGateway for SqlxManagementGateway {
    method!(status(actor:&'a Session);g;{
        let counts=g.devices.system_counts(actor).await?;
        Ok(json!({"schema_version":counts.schema_version,"version":env!("CARGO_PKG_VERSION"),"users":counts.users.to_string(),"devices":counts.devices.to_string(),"roles":counts.roles.to_string(),"grants":counts.grants.to_string(),"groups":counts.groups.to_string()}))
    });
}

pub fn build_sqlx_management_state(db:DbPool,auth:Arc<HttpAuthState>,device_probe:Arc<dyn DeviceLiveProbe>,recovery_probe:Arc<dyn RecoveryModeProbe>,notification_sink:Arc<dyn ManagementNotificationSink>)->ManagementState {
    let repo=Arc::new(SqlxAuthzRepository::new(db.clone()));
    let snapshots=Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone()));
    let devices=Arc::new(SqlxDeviceRepository::new(db.clone()));
    let gateway=Arc::new(SqlxManagementGateway { repo:repo.clone(),devices,
        authz:Arc::new(AuthzService::new(snapshots,repo)),
        admission:Arc::new(DeviceService::new(db,NotificationBridge(notification_sink.clone()))) });
    // 跨线衔接：审批connection_state的唯一消费通路为主计划ApprovalsApiState.probe（主稿唯一读取点，约L6128），经build_management_router装配注入；本网关不持有probe，避免双probe与每项两次消费，不使用allow压警。审批读取消费该真实probe，失败返回503（对齐主计划L5716/L5956契约）。
    ManagementState {auth,users:gateway.clone(),roles:gateway.clone(),grants:gateway.clone(),groups:gateway.clone(),devices:gateway.clone(),approvals:gateway.clone(),audit:gateway.clone(),system:gateway,device_probe,recovery_probe,notification_sink}
}
```

system handler完整计划代码（system.rs，替换Task12泛泛挂载说明；这是资源handler，不是顶层builder）：

```rust
use axum::{extract::State,http::HeaderMap,response::Response,routing::get,Router};
use crate::http_auth::EndpointId;
use crate::http_gate::{authenticate_read,require_admin_session};
use crate::http_api::ok_response;

pub fn system_router(state:SystemApiState)->Router<()> {
    Router::new().route("/api/v1/system/status",get(status)).with_state(state)
}
async fn status(State(state):State<SystemApiState>,headers:HeaderMap)->Response {
    let actor=match authenticate_read(&state.auth,&headers,EndpointId::SystemStatusRead).await{Ok(actor)=>actor,Err(response)=>return response};
    if let Err(response)=require_admin_session(&actor){return response;}
    let counts=match state.gateway.status(&actor).await{Ok(counts)=>counts,Err(error)=>return super::map_error(error)};
    let recovery=match state.recovery_probe.current_mode().await{Ok(recovery)=>recovery,Err(_)=>return crate::http_api::err_response(axum::http::StatusCode::SERVICE_UNAVAILABLE,"NOT_READY","system.not_ready",serde_json::json!({}))};
    let Some(data)=counts.as_object() else{return super::map_error(crate::ControllerError::Config("invalid system projection".into()));};
    ok_response(serde_json::json!({"ready":recovery.ready,"recovery_mode":recovery.mode,
        "schema_version":data.get("schema_version"),"version":data.get("version"),
        "counts":{"users":data.get("users"),"devices":data.get("devices"),"roles":data.get("roles"),"grants":data.get("grants"),"groups":data.get("groups")}}))
}
```

资源status counts十进制字符串，对齐Counter公共规则，旧Task12 is_number断言必须改is_string并按synthetic期望精确比较；不因一个有用fixture就说真实ready/恢复已验。

必须同步主Task3/4/5新增方法完整定义：effective_permissions_page、device_page_filtered、approval_page及replace_group_members返回GroupRecord；不能留这些方法未定义即运行RED。主代理在本子计划收敛完成前会补入相应实现与测试。
