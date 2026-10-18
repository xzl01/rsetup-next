//! 生产管理适配器（Identity Task 13b）：八个 dyn-safe Gateway trait 委托真实
//! SQLx 仓储 / AuthzService / DeviceService，`build_sqlx_management_state` 只构造
//! 并返回 `ManagementState`，不读取环境变量、不安装 cursor 密钥（密钥安装唯一归
//! 集成 Task 0 在 build router 前完成）。
//!
//! cursor 边界纪律（05 子计划）：各 list 方法返回的 `next_cursor` 一律是**内部
//! raw key 的 hex**（UUID 32hex / 设备公钥 64hex；audit 为 Task 12 复合键
//! 80hex = epoch32hex ‖ seq16hex ‖ id32hex，唯一编码定义见
//! `audit::format_audit_raw_cursor`），本模块绝不签发或解码签名 token；
//! 外部 signed 游标（64/96hex）只由 handler 的 HTTP 边界 helper
//! `decode_management_list_cursor` / `sign_management_list_page` 负责。

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::ControllerError;
use crate::DbPool;
use crate::auth::service::Session;
use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission, RoleRecord};
use crate::authz::service::AuthzService;
use crate::authz::sqlx_repo::{
    AuthzRepository, GroupRecord, SqlxAuthorizationSnapshotRepository, SqlxAuthzRepository,
};
use crate::devices::repository::{
    AuditCursor, AuditFilter, DeviceAdminView, DeviceRepository, SqlxDeviceRepository,
};
use crate::devices::service::{
    AdmissionChangeEvent, AdmissionChangeSink, DecisionResult, DeviceService,
    MAX_ADMISSION_REASON_BYTES,
};
use crate::http_auth::HttpAuthState;
use crate::model::{AdmissionState, ReviewDecision};

use super::ManagementState;
use super::approvals::ApprovalTarget;
use super::{ManagementFuture, approvals, audit, devices, grants, groups, roles, system, users};

/// 生产授权服务的唯一装配类型：快照读 + 管理写仓储同一 `SqlxAuthzRepository`。
pub type SqlxManagementAuthzService =
    AuthzService<SqlxAuthorizationSnapshotRepository, SqlxAuthzRepository>;

/// commit 后 typed 通知桥：`ManagementNotificationSink`（可失败、去敏）→
/// `AdmissionChangeSink`（unit error），失败只上抛信号不回滚已 commit 事务。
pub struct NotificationBridge(Arc<dyn approvals::ManagementNotificationSink>);

impl AdmissionChangeSink for NotificationBridge {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ()> {
        self.0.after_commit(event).map_err(|_| ())
    }
}

/// 八个 Gateway trait 的单一生产实现：授权读写走 `SqlxAuthzRepository`，
/// 临时密码编排走 `AuthzService`，设备/审批/审计/system 走 `SqlxDeviceRepository`
/// 与 `DeviceService`（唯一准入状态机），不引入第二套授权来源。
pub struct SqlxManagementGateway {
    repo: Arc<SqlxAuthzRepository>,
    devices: Arc<SqlxDeviceRepository>,
    authz: Arc<SqlxManagementAuthzService>,
    admission: Arc<DeviceService<DbPool, NotificationBridge>>,
}

// ---------------------------------------------------------------------------
// 纯投影函数（契约：revision/member_count 十进制字符串；admission_state 大写；
// review_decision 小写；id 规范小写 UUID / hex64）
// ---------------------------------------------------------------------------

fn page(items: Vec<Value>, next: Option<String>) -> Value {
    json!({"items": items, "next_cursor": next})
}

fn role_value(role: &RoleRecord) -> Value {
    let mut permissions: Vec<_> = role.permissions.iter().map(Permission::as_str).collect();
    permissions.sort_unstable();
    json!({
        "id": role.id.to_string(),
        "name": role.name,
        "builtin": role.builtin,
        "archived": role.archived,
        "revision": role.revision.to_string(),
        "permissions": permissions,
    })
}

fn group_value(group: &GroupRecord) -> Value {
    json!({
        "id": group.id.to_string(),
        "name": group.name,
        "archived": group.archived,
        "revision": group.revision.to_string(),
        "member_count": group.member_count.to_string(),
    })
}

fn grant_value(grant: &GrantRecord) -> Value {
    let source = match &grant.source {
        GrantSource::Role(id) => json!({"kind": "role", "role_id": id.to_string()}),
        GrantSource::Direct(permissions) => {
            let mut values: Vec<_> = permissions.iter().map(Permission::as_str).collect();
            values.sort_unstable();
            json!({"kind": "direct", "permissions": values})
        }
    };
    let scope = match &grant.scope {
        GrantScope::All => json!({"kind": "all"}),
        GrantScope::Group(id) => json!({"kind": "group", "group_id": id.to_string()}),
        GrantScope::Device(id) => json!({"kind": "device", "device_id": hex::encode(id)}),
    };
    json!({
        "id": grant.id.to_string(),
        "user_id": Uuid::from_bytes(grant.user_id).to_string(),
        "source": source,
        "scope": scope,
        "revision": grant.revision.to_string(),
    })
}

fn admission_text(state: AdmissionState) -> &'static str {
    match state {
        AdmissionState::Pending => "PENDING",
        AdmissionState::Approved => "APPROVED",
        AdmissionState::Revoked => "REVOKED",
    }
}

fn decision_text(decision: ReviewDecision) -> &'static str {
    match decision {
        ReviewDecision::None => "none",
        ReviewDecision::Approved => "approved",
        ReviewDecision::Denied => "denied",
        ReviewDecision::Revoked => "revoked",
    }
}

fn device_value(device: &DeviceAdminView) -> Value {
    json!({
        "device_id": hex::encode(device.public_key),
        "display_name": device.display_name,
        "descriptor": device.descriptor,
        "admission_state": admission_text(device.admission_state),
        "review_decision": decision_text(device.review_decision),
        "revision": device.revision.to_string(),
    })
}

fn decision_value(result: DecisionResult, reopen: bool) -> Value {
    if reopen {
        json!({
            "decision": decision_text(result.snapshot.review_decision),
            "requires_device_reset": result.requires_device_reset,
        })
    } else {
        json!({
            "state": admission_text(result.snapshot.admission_state),
            "requires_device_reset": result.requires_device_reset,
        })
    }
}

/// 批量目标上限（spec 02 §3：单请求 1..=1024），与 HTTP 入口门禁
/// `api::approvals::MAX_BATCH_TARGETS` 同值同源；后者是 approvals 模块私有常量
/// 无法跨模块引用，此处为同值命名定义，任何一侧调整上限必须同步另一侧。
const MAX_BATCH_TARGETS: usize = 1024;

/// 审批批量项结果：单项失败收集为 `ok:false` + 去敏错误码，不中断其余项。
/// 错误码与 `api::map_error` 的 02 信封逐变体一一对齐（显式穷尽匹配，新增
/// `ControllerError` 变体将编译失败，强制在此补映射）；`LastAdmin` 与任务类
/// 冲突在准入状态机上不可达，映射仅保持与 `map_error` 的强一致（见报告偏差说明）。
fn batch_item(id: [u8; 32], result: Result<DecisionResult, ControllerError>) -> Value {
    match result {
        Ok(result) => json!({
            "device_id": hex::encode(id),
            "ok": true,
            "revision": result.snapshot.revision.to_string(),
        }),
        Err(error) => {
            let code = match error {
                ControllerError::InvalidArgument => "INVALID_ARGUMENT",
                ControllerError::PermissionDenied => "PERMISSION_DENIED",
                ControllerError::NotFound => "NOT_FOUND",
                ControllerError::RevisionConflict => "REVISION_CONFLICT",
                ControllerError::LastAdmin => "LAST_ADMIN",
                ControllerError::InvalidState => "INVALID_STATE",
                ControllerError::TaskIdempotencyConflict => "IDEMPOTENCY_CONFLICT",
                ControllerError::TaskPreviewInvalidState => "INVALID_STATE",
                ControllerError::ResourceExhausted => "RESOURCE_EXHAUSTED",
                ControllerError::Database(_) | ControllerError::Config(_) => "STORAGE_UNAVAILABLE",
                ControllerError::SchemaNotReady { .. } | ControllerError::Crypto => "NOT_READY",
            };
            json!({"device_id": hex::encode(id), "ok": false, "error": {"code": code}})
        }
    }
}

// ---------------------------------------------------------------------------
// 八个 Gateway impl（委托真实 repo/service/admission）
// ---------------------------------------------------------------------------

macro_rules! method {
    ($name:ident($($arg:ident: $ty:ty),*) ; $this:ident ; $body:block) => {
        fn $name<'a>(&'a self, $($arg: $ty),*) -> ManagementFuture<'a, Value> {
            let $this = self;
            Box::pin(async move $body)
        }
    };
}

impl users::UsersGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, cursor: Option<[u8; 16]>, limit: usize); g; {
        // 仓储内部键 Option<[u8;16]> 由 handler 经 HTTP 边界 helper
        // `decode_management_list_cursor`（UUID profile）校验解析后传入；返回的上一页末 ID
        // 由 handler 出口经 `sign_management_list_page` 签发为外部 signed next_cursor，
        // gateway 返回仅为 raw 内部分页键（32hex），管理端点不得签发/接受裸内部键。
        let (rows, next) = g.repo.list_users(actor, cursor, limit).await?;
        Ok(page(
            rows.iter()
                .map(crate::http_api::user_admin_public)
                .collect(),
            next.map(hex::encode),
        ))
    });
    method!(create(actor: &'a Session, username: &'a str, display_name: &'a str, is_admin: bool); g; {
        let (user, plain) = g.authz.create_user(actor, username, display_name, is_admin).await?;
        Ok(json!({
            "user": crate::http_api::user_admin_public(&user),
            "temporary_password": plain,
        }))
    });
    method!(patch(actor: &'a Session, id: [u8; 16], revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&'a str>); g; {
        let user = g
            .repo
            .patch_user(actor, id, revision, active, is_admin, display_name)
            .await?;
        Ok(crate::http_api::user_admin_public(&user))
    });
    method!(reset_password(actor: &'a Session, id: [u8; 16], revision: u64); g; {
        let (user, plain) = g.authz.reset_user_password(actor, id, revision).await?;
        Ok(json!({
            "user": crate::http_api::user_admin_public(&user),
            "temporary_password": plain,
        }))
    });
    method!(effective_permissions(actor: &'a Session, target: [u8; 16], device_id: Option<[u8; 32]>, cursor: Option<[u8; 32]>, limit: usize); g; {
        // Task 13a 分页来源：单次 protected 同连接 RR 事务内 target 投影；
        // 返回的 next_cursor 为内部 raw 设备公钥 hex64，出口签发归 HTTP 边界 helper。
        g.authz
            .effective_permissions_page(actor, target, device_id, cursor, limit)
            .await
    });
}

impl roles::RolesGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, cursor: Option<Uuid>, limit: usize); g; {
        // 内部键/出口签发纪律同 users::list（UUID profile）。
        let (rows, next) = g.repo.list_roles(actor, cursor, limit).await?;
        Ok(page(
            rows.iter().map(role_value).collect(),
            next.map(|id| hex::encode(id.as_bytes())),
        ))
    });
    method!(create(actor: &'a Session, name: &'a str, permissions: &'a [Permission]); g; {
        Ok(role_value(&g.repo.create_role(actor, name, permissions).await?))
    });
    method!(patch(actor: &'a Session, id: Uuid, revision: u64, name: Option<&'a str>, permissions: Option<&'a [Permission]>, archived: Option<bool>); g; {
        Ok(role_value(
            &g.repo
                .patch_role(actor, id, revision, name, permissions, archived)
                .await?,
        ))
    });
}

impl grants::GrantsGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, user_id: Option<[u8; 16]>, cursor: Option<Uuid>, limit: usize); g; {
        // 内部键/出口签发纪律同上（UUID profile，入参绑定 user_id 筛选上下文）。
        let (rows, next) = g.repo.list_grants(actor, user_id, cursor, limit).await?;
        Ok(page(
            rows.iter().map(grant_value).collect(),
            next.map(|id| hex::encode(id.as_bytes())),
        ))
    });
    method!(create(actor: &'a Session, user_id: [u8; 16], source: GrantSource, scope: GrantScope); g; {
        Ok(grant_value(
            &g.repo.create_grant(actor, user_id, source, scope).await?,
        ))
    });
    fn delete<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
    ) -> ManagementFuture<'a, ()> {
        Box::pin(async move { self.repo.delete_grant(actor, id, revision).await })
    }
}

impl groups::GroupsGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, cursor: Option<Uuid>, limit: usize); g; {
        // 内部键/出口签发纪律同上（UUID profile）。
        let (rows, next) = g.repo.list_groups(actor, cursor, limit).await?;
        Ok(page(
            rows.iter().map(group_value).collect(),
            next.map(|id| hex::encode(id.as_bytes())),
        ))
    });
    method!(create(actor: &'a Session, name: &'a str); g; {
        Ok(group_value(&g.repo.create_group(actor, name).await?))
    });
    method!(patch(actor: &'a Session, id: Uuid, revision: u64, name: Option<&'a str>, archived: Option<bool>); g; {
        Ok(group_value(
            &g.repo.patch_group(actor, id, revision, name, archived).await?,
        ))
    });
    method!(replace_members(actor: &'a Session, id: Uuid, revision: u64, devices: &'a [[u8; 32]]); g; {
        // 同事务组替换结果（Task 4 唯一真源）：返回替换后的 GroupRecord。
        Ok(group_value(
            &g.devices
                .replace_group_members(actor, id, revision, devices)
                .await?,
        ))
    });
}

impl devices::DevicesGateway for SqlxManagementGateway {
    method!(list_devices_visible(actor: &'a Session, cursor: Option<[u8; 32]>, limit: usize, group_id: Option<Uuid>, search: Option<&'a str>); g; {
        // 内部键/出口签发纪律同上（设备公钥 profile，入参绑定 group_id 与 search 筛选）。
        let (rows, next) = g
            .devices
            .device_page_filtered(actor, cursor, limit, group_id, search)
            .await?;
        Ok(page(
            rows.iter()
                .map(|(view, permissions)| {
                    crate::authz::policy::minimum_device_projection(
                        view.public_key,
                        &view.display_name,
                        permissions,
                    )
                })
                .collect(),
            next.map(hex::encode),
        ))
    });
    fn find_device<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
    ) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
        Box::pin(async move { self.devices.find_device(actor, id).await })
    }
    method!(patch(actor: &'a Session, id: [u8; 32], revision: u64, display_name: &'a str); g; {
        Ok(device_value(
            &g.devices
                .patch_device_display_name(actor, id, revision, display_name)
                .await?,
        ))
    });
}

impl approvals::ApprovalsGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, decision: Option<ReviewDecision>, cursor: Option<[u8; 32]>, limit: usize); g; {
        // 审批持久真相页（Task 13a）：next_cursor 为内部 raw 公钥 hex64，出口签发归 HTTP 层。
        g.devices.approval_page(actor, decision, cursor, limit).await
    });
    method!(approve(actor: &'a Session, targets: &'a [ApprovalTarget]); g; {
        if targets.is_empty() || targets.len() > MAX_BATCH_TARGETS {
            return Err(ControllerError::InvalidArgument);
        }
        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            results.push(batch_item(
                target.device_id,
                g.admission
                    .approve(actor, target.device_id, target.expected_revision)
                    .await,
            ));
        }
        Ok(json!({"results": results}))
    });
    method!(reject(actor: &'a Session, targets: &'a [ApprovalTarget], reason: &'a str); g; {
        if targets.is_empty()
            || targets.len() > MAX_BATCH_TARGETS
            || reason.trim().is_empty()
            || reason.len() > MAX_ADMISSION_REASON_BYTES
            || reason.chars().any(char::is_control)
        {
            return Err(ControllerError::InvalidArgument);
        }
        let mut results = Vec::with_capacity(targets.len());
        for target in targets {
            results.push(batch_item(
                target.device_id,
                g.admission
                    .reject(actor, target.device_id, target.expected_revision, reason)
                    .await,
            ));
        }
        Ok(json!({"results": results}))
    });
    method!(reopen(actor: &'a Session, id: [u8; 32], revision: u64); g; {
        Ok(decision_value(
            g.admission.reopen(actor, id, revision).await?,
            true,
        ))
    });
    method!(revoke(actor: &'a Session, id: [u8; 32], revision: u64, reason: &'a str); g; {
        Ok(decision_value(
            g.admission.revoke(actor, id, revision, reason).await?,
            false,
        ))
    });
    method!(reauthorize(actor: &'a Session, id: [u8; 32], revision: u64); g; {
        Ok(decision_value(
            g.admission.reauthorize(actor, id, revision).await?,
            false,
        ))
    });
}

impl audit::AuditGateway for SqlxManagementGateway {
    method!(list(actor: &'a Session, filter: AuditFilter, cursor: Option<AuditCursor>, limit: usize); g; {
        let (rows, next) = g.devices.audit_page(actor, filter, cursor, limit).await?;
        let items = rows
            .into_iter()
            .map(|row| {
                json!({
                    "id": row.id.to_string(),
                    "actor_kind": row.actor_kind,
                    "actor_user_id": row.actor_user_id.map(|id| Uuid::from_bytes(id).to_string()),
                    "event_type": row.event_type,
                    "target_kind": row.target_kind,
                    "target_id": row.target_id,
                    "params_redacted": row.params_redacted,
                    "outcome": row.outcome,
                    "process_epoch": row.process_epoch.to_string(),
                    "event_seq": row.event_seq.to_string(),
                    "time_evidence": row.created_evidence,
                })
            })
            .collect();
        // audit handler 使用自己的复合 cursor 签名（Task 12）；raw 三元键编码复用
        // `audit::format_audit_raw_cursor` 唯一编码定义，只在内部传递。
        let raw = next.as_ref().map(audit::format_audit_raw_cursor);
        Ok(page(items, raw))
    });
}

impl system::SystemGateway for SqlxManagementGateway {
    method!(status(actor: &'a Session); g; {
        // Task 12 定稿的内部投影契约：**扁平**形状，schema_version 为 JSON number
        // （i32 域），五个计数为十进制字符串；计数若为数字会被 handler fail-closed 503。
        // `version` 由 handler 覆盖为 `env!("CARGO_PKG_VERSION")`，adapter 仍应提供。
        let counts = g.devices.system_counts(actor).await?;
        Ok(json!({
            "schema_version": counts.schema_version,
            "version": env!("CARGO_PKG_VERSION"),
            "users": counts.users.to_string(),
            "devices": counts.devices.to_string(),
            "roles": counts.roles.to_string(),
            "grants": counts.grants.to_string(),
            "groups": counts.groups.to_string(),
        }))
    });
}

/// 生产装配唯一入口（05 子计划 Produces 契约）：只构造并返回 `ManagementState`，
/// **不读取环境变量、不安装 cursor 密钥**（密钥由集成 Task 0 在 build router 前
/// 安装）；probe 与通知 sink 由集成 owner 显式传入真实源，本函数不造常量默认。
///
/// 跨线衔接：审批 connection_state 的唯一消费通路是 `ApprovalsApiState.probe`
/// （`build_management_router` 装配注入），本网关不持有 probe，避免双 probe。
pub fn build_sqlx_management_state(
    db: DbPool,
    auth: Arc<HttpAuthState>,
    device_probe: Arc<dyn devices::DeviceLiveProbe>,
    recovery_probe: Arc<dyn system::RecoveryModeProbe>,
    notification_sink: Arc<dyn approvals::ManagementNotificationSink>,
) -> ManagementState {
    let repo = Arc::new(SqlxAuthzRepository::new(db.clone()));
    let snapshots = Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone()));
    let devices = Arc::new(SqlxDeviceRepository::new(db.clone()));
    let gateway = Arc::new(SqlxManagementGateway {
        repo: repo.clone(),
        devices,
        authz: Arc::new(AuthzService::new(snapshots, repo)),
        admission: Arc::new(DeviceService::new(
            db,
            NotificationBridge(notification_sink.clone()),
        )),
    });
    ManagementState {
        auth,
        users: gateway.clone(),
        roles: gateway.clone(),
        grants: gateway.clone(),
        groups: gateway.clone(),
        devices: gateway.clone(),
        approvals: gateway.clone(),
        audit: gateway.clone(),
        system: gateway,
        device_probe,
        recovery_probe,
        notification_sink,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AdmissionSnapshot;

    /// canonical v4 形状 UUID（version nibble=4、variant=10xx）。
    fn v4_uuid(fill: u8) -> Uuid {
        let mut bytes = [fill; 16];
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Uuid::from_bytes(bytes)
    }

    fn fixture_result(
        state: AdmissionState,
        decision: ReviewDecision,
        revision: u64,
    ) -> DecisionResult {
        DecisionResult {
            snapshot: AdmissionSnapshot {
                admission_state: state,
                review_decision: decision,
                revision,
            },
            requires_device_reset: true,
            notification_queued: true,
        }
    }

    #[test]
    fn admission_text_maps_three_states_to_uppercase() {
        assert_eq!(admission_text(AdmissionState::Pending), "PENDING");
        assert_eq!(admission_text(AdmissionState::Approved), "APPROVED");
        assert_eq!(admission_text(AdmissionState::Revoked), "REVOKED");
    }

    #[test]
    fn decision_text_maps_four_states_to_lowercase() {
        assert_eq!(decision_text(ReviewDecision::None), "none");
        assert_eq!(decision_text(ReviewDecision::Approved), "approved");
        assert_eq!(decision_text(ReviewDecision::Denied), "denied");
        assert_eq!(decision_text(ReviewDecision::Revoked), "revoked");
    }

    #[test]
    fn page_shape_keeps_items_and_next_cursor() {
        let with_next = page(vec![json!({"a": 1})], Some("raw".into()));
        assert_eq!(with_next["items"][0]["a"], 1);
        assert_eq!(with_next["next_cursor"], "raw");
        let without = page(Vec::new(), None);
        assert!(without["items"].is_array());
        assert!(without["next_cursor"].is_null());
    }

    #[test]
    fn role_value_uses_decimal_revision_and_sorted_permissions() {
        let id = v4_uuid(0x11);
        let role = RoleRecord {
            id,
            name: "ops".into(),
            builtin: false,
            archived: false,
            revision: 12,
            permissions: vec![Permission::DeviceReboot, Permission::DeviceRead],
        };
        let value = role_value(&role);
        assert_eq!(value["id"], id.to_string());
        assert_eq!(value["name"], "ops");
        assert_eq!(value["builtin"], false);
        assert_eq!(value["archived"], false);
        assert_eq!(value["revision"], "12");
        assert_eq!(
            value["permissions"],
            json!(["device.read", "device.reboot"])
        );
    }

    #[test]
    fn group_value_uses_decimal_revision_and_member_count() {
        let id = v4_uuid(0x22);
        let group = GroupRecord {
            id,
            name: "lab".into(),
            archived: true,
            revision: 3,
            member_count: 42,
        };
        let value = group_value(&group);
        assert_eq!(value["id"], id.to_string());
        assert_eq!(value["name"], "lab");
        assert_eq!(value["archived"], true);
        assert_eq!(value["revision"], "3");
        assert_eq!(value["member_count"], "42");
    }

    #[test]
    fn grant_value_projects_role_and_direct_sources_and_three_scopes() {
        let user_id = v4_uuid(0x33);
        let role_id = v4_uuid(0x44);
        let group_id = v4_uuid(0x55);
        let grant = GrantRecord {
            id: v4_uuid(0x66),
            user_id: *user_id.as_bytes(),
            source: GrantSource::Role(role_id),
            scope: GrantScope::All,
            revision: 2,
        };
        let value = grant_value(&grant);
        assert_eq!(value["user_id"], user_id.to_string());
        assert_eq!(value["revision"], "2");
        assert_eq!(
            value["source"],
            json!({"kind": "role", "role_id": role_id.to_string()})
        );
        assert_eq!(value["scope"], json!({"kind": "all"}));

        let direct = GrantRecord {
            id: v4_uuid(0x77),
            user_id: *user_id.as_bytes(),
            source: GrantSource::Direct(vec![Permission::DeviceReboot, Permission::DeviceRead]),
            scope: GrantScope::Group(group_id),
            revision: 1,
        };
        let value = grant_value(&direct);
        assert_eq!(
            value["source"],
            json!({"kind": "direct", "permissions": ["device.read", "device.reboot"]})
        );
        assert_eq!(
            value["scope"],
            json!({"kind": "group", "group_id": group_id.to_string()})
        );

        let device = GrantRecord {
            id: v4_uuid(0x88),
            user_id: *user_id.as_bytes(),
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::Device([0xab; 32]),
            revision: 1,
        };
        let value = grant_value(&device);
        assert_eq!(
            value["scope"],
            json!({"kind": "device", "device_id": hex::encode([0xab; 32])})
        );
    }

    #[test]
    fn device_value_uses_hex_id_uppercase_state_lowercase_decision() {
        let view = DeviceAdminView {
            public_key: [0xab; 32],
            display_name: "dev".into(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::None,
            revision: 5,
            descriptor: Some(json!({"model": "x"})),
            archived: false,
        };
        let value = device_value(&view);
        assert_eq!(value["device_id"], hex::encode([0xab; 32]));
        assert_eq!(value["display_name"], "dev");
        assert_eq!(value["admission_state"], "APPROVED");
        assert_eq!(value["review_decision"], "none");
        assert_eq!(value["revision"], "5");
        assert_eq!(value["descriptor"], json!({"model": "x"}));
    }

    #[test]
    fn decision_value_reopen_uses_decision_state_otherwise_state() {
        let result = fixture_result(AdmissionState::Approved, ReviewDecision::None, 4);
        let reopened = decision_value(result, true);
        assert_eq!(reopened["decision"], "none");
        assert_eq!(reopened["requires_device_reset"], true);
        let revoked = decision_value(
            fixture_result(AdmissionState::Revoked, ReviewDecision::Revoked, 9),
            false,
        );
        assert_eq!(revoked["state"], "REVOKED");
        assert_eq!(revoked["requires_device_reset"], true);
    }

    #[test]
    fn batch_item_success_uses_hex_id_and_decimal_revision() {
        let item = batch_item(
            [0x11; 32],
            Ok(fixture_result(
                AdmissionState::Approved,
                ReviewDecision::Approved,
                4,
            )),
        );
        assert_eq!(item["device_id"], hex::encode([0x11; 32]));
        assert_eq!(item["ok"], true);
        assert_eq!(item["revision"], "4");
    }

    #[tokio::test]
    async fn batch_item_error_codes_agree_with_map_error_envelope() {
        // 显式穷尽：`ControllerError` 全部 13 个变体逐一断言与 `map_error` 同码，
        // 新增变体未补映射时上方穷尽匹配先编译失败，此处兜底逐变体语义一致。
        let cases: Vec<(fn() -> ControllerError, &str)> = vec![
            (|| ControllerError::InvalidArgument, "INVALID_ARGUMENT"),
            (|| ControllerError::PermissionDenied, "PERMISSION_DENIED"),
            (|| ControllerError::NotFound, "NOT_FOUND"),
            (|| ControllerError::RevisionConflict, "REVISION_CONFLICT"),
            (|| ControllerError::LastAdmin, "LAST_ADMIN"),
            (|| ControllerError::InvalidState, "INVALID_STATE"),
            (
                || ControllerError::TaskIdempotencyConflict,
                "IDEMPOTENCY_CONFLICT",
            ),
            (|| ControllerError::TaskPreviewInvalidState, "INVALID_STATE"),
            (|| ControllerError::ResourceExhausted, "RESOURCE_EXHAUSTED"),
            (
                || ControllerError::Config("synthetic".into()),
                "STORAGE_UNAVAILABLE",
            ),
            (
                || ControllerError::Database(sqlx::Error::RowNotFound),
                "STORAGE_UNAVAILABLE",
            ),
            (
                || ControllerError::SchemaNotReady {
                    found: None,
                    required: 1,
                },
                "NOT_READY",
            ),
            (|| ControllerError::Crypto, "NOT_READY"),
        ];
        assert_eq!(cases.len(), 13, "必须逐变体覆盖 ControllerError 全集");
        for (make, code) in cases {
            let item = batch_item([0x22; 32], Err(make()));
            assert_eq!(item["ok"], false, "case {code}");
            assert_eq!(item["device_id"], hex::encode([0x22; 32]));
            assert_eq!(item["error"]["code"], code);
            let response = super::super::map_error(make());
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("error body");
            let envelope: Value = serde_json::from_slice(&bytes).expect("error envelope");
            assert_eq!(envelope["error"]["code"], code, "map_error {code}");
        }
    }

    struct OkSink;
    struct FailSink;

    impl approvals::ManagementNotificationSink for OkSink {
        fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
            Ok(())
        }
    }

    impl approvals::ManagementNotificationSink for FailSink {
        fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
            Err(ControllerError::Config("synthetic sink failure".into()))
        }
    }

    #[test]
    fn notification_bridge_maps_sink_result_to_unit_error_channel() {
        let event = AdmissionChangeEvent {
            public_key: [1u8; 32],
            previous_state: AdmissionState::Pending,
            snapshot: AdmissionSnapshot {
                admission_state: AdmissionState::Approved,
                review_decision: ReviewDecision::Approved,
                revision: 2,
            },
        };
        let ok = NotificationBridge(Arc::new(OkSink));
        assert!(ok.after_commit(&event).is_ok());
        let fail = NotificationBridge(Arc::new(FailSink));
        assert!(fail.after_commit(&event).is_err());
    }
}
