use std::collections::HashSet;
use std::sync::Arc;

use rand::RngCore;

use crate::ControllerError;
use crate::auth::password::PasswordHasher;
use crate::auth::service::Session;
use crate::authz::policy::{
    Permission, PermissionSourceEntry, eval_memory_permissions, is_current_admin,
    permission_sources,
};
use crate::authz::sqlx_repo::{
    AuthorizationSnapshot, AuthorizationSnapshotRepository, AuthzRepository, UserRecord,
};

/// 授权服务：读侧走内部快照层（DB 当前真相），写侧走管理写事务仓储。
pub struct AuthzService<S: AuthorizationSnapshotRepository, W: AuthzRepository> {
    pub(crate) snapshots: Arc<S>,
    pub(crate) repo: Arc<W>,
}

impl<S: AuthorizationSnapshotRepository, W: AuthzRepository> AuthzService<S, W> {
    pub fn new(snapshots: Arc<S>, repo: Arc<W>) -> Self {
        Self { snapshots, repo }
    }

    /// 有效权限集（C1：DB 当前快照读 → 纯求值；`Session.user` 字段只是认证身份，不参与授权）。
    pub async fn effective_permissions(
        &self,
        actor: &Session,
        device_id: [u8; 32],
    ) -> Result<HashSet<Permission>, ControllerError> {
        let snapshot = self
            .snapshots
            .load_snapshot(actor.user.id, &[device_id])
            .await?;
        Ok(snapshot_effective_permissions(&snapshot, device_id))
    }

    pub async fn require_permission(
        &self,
        actor: &Session,
        device_id: [u8; 32],
        required: Permission,
    ) -> Result<(), ControllerError> {
        if !self
            .effective_permissions(actor, device_id)
            .await?
            .contains(&required)
        {
            return Err(ControllerError::PermissionDenied);
        }
        Ok(())
    }

    /// 来源归因（`effective-permissions` 路由直接消费）：走 protected 管理读取
    /// `load_admin_target_snapshot(actor, target_user, &[device_id])`——同一读事务内先以 DB
    /// raw_bool 重验 `actor` 为当前 admin（HTTP 的 `require_admin_session` 只是前置，服务/仓储
    /// 权威检查不省），再按 `target_user` 加载目标快照；授权真相是目标用户的 DB 当前快照。
    pub async fn effective_permissions_with_sources(
        &self,
        actor: &Session,
        target_user: [u8; 16],
        device_id: [u8; 32],
    ) -> Result<Vec<PermissionSourceEntry>, ControllerError> {
        let snapshot = self
            .snapshots
            .load_admin_target_snapshot(actor, target_user, &[device_id])
            .await?;
        Ok(snapshot_permission_sources(&snapshot, device_id))
    }

    /// 管理员写编排：临时密码在进入仓储事务**前**生成并哈希（事务只拿 `hash` 串，只耗纳秒级 SQL）。
    pub async fn create_user(
        &self,
        actor: &Session,
        username: &str,
        display_name: &str,
        is_admin: bool,
    ) -> Result<(UserRecord, String), ControllerError> {
        let (plain, password_hash) = generate_temporary_password()?;
        let record = self
            .repo
            .create_user(actor, username, display_name, is_admin, &password_hash)
            .await?;
        Ok((record, plain))
    }

    pub async fn reset_user_password(
        &self,
        actor: &Session,
        user_id: [u8; 16],
        expected_revision: u64,
    ) -> Result<(UserRecord, String), ControllerError> {
        let (plain, password_hash) = generate_temporary_password()?;
        let record = self
            .repo
            .reset_user_password(actor, user_id, expected_revision, &password_hash)
            .await?;
        Ok((record, plain))
    }

    /// 管理来源页编排：单次 protected 同连接 RR 事务内完成 target 投影（不逐设备另开事务）。
    /// `limit` 合法域 `1..=200`，域外 `InvalidArgument`（HTTP 前置一次、仓储权威再一次）。
    pub async fn effective_permissions_page(
        &self,
        caller: &Session,
        target: [u8; 16],
        device_id: Option<[u8; 32]>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> Result<serde_json::Value, ControllerError> {
        if !(1..=200).contains(&limit) {
            return Err(ControllerError::InvalidArgument);
        }
        self.snapshots
            .load_admin_target_page(caller, target, device_id, cursor, limit)
            .await
    }
}

/// 快照生命周期 + 设备存在性统一求值：actor `!active || must_change_password`，或目标设备
/// missing/archived → 空集（fail-closed；admin 未改密同样 0 权限）。其余交给 `eval_memory_permissions`。
pub fn snapshot_effective_permissions(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> HashSet<Permission> {
    if !snapshot.actor.active
        || snapshot.actor.must_change_password
        || !snapshot
            .devices
            .get(&device_id)
            .is_some_and(|device| !device.archived)
    {
        return HashSet::new();
    }
    eval_memory_permissions(
        &snapshot.actor,
        device_id,
        &snapshot.grants,
        &snapshot.roles,
        &snapshot
            .device_groups
            .get(&device_id)
            .cloned()
            .unwrap_or_default(),
    )
}

/// 来源归因：先按同一生命周期规则求 effective，空集直接空 vec（target inactive/must-change
/// 及 missing/archived 设备无论是否 admin 都无来源）；其余按同快照 effective 集合归因，
/// 保留全部合法来源，其 permission 集合恰等于 effective。
pub fn snapshot_permission_sources(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> Vec<PermissionSourceEntry> {
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.is_empty() {
        return Vec::new();
    }
    let mut entries = permission_sources(
        snapshot.actor.id,
        device_id,
        &snapshot.grants,
        &snapshot.roles,
        &snapshot
            .device_groups
            .get(&device_id)
            .cloned()
            .unwrap_or_default(),
        is_current_admin(&snapshot.actor),
    );
    entries.retain(|entry| effective.contains(&entry.permission));
    entries
}

/// 事务外 CSPRNG 临时密码：32 字节 OsRng 熵 → 64 字符小写 hex → Argon2id 哈希。
/// 明文只返回调用方一次，永不落库、永不写入脱敏审计。
pub fn generate_temporary_password() -> Result<(String, String), ControllerError> {
    let mut entropy = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut entropy)
        .map_err(|_| ControllerError::Crypto)?;
    let raw = hex::encode(entropy);
    let hash = PasswordHasher::new().hash(&raw)?;
    Ok((raw, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use uuid::Uuid;

    use crate::auth::password::PasswordHasher;
    use crate::auth::service::{IdentityUser, Session};
    use crate::authz::policy::{
        GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, SnapshotActor,
    };
    use crate::authz::sqlx_repo::{
        AuthorizationSnapshot, AuthorizationSnapshotRepository, AuthzRepository, GroupRecord,
        RepoFuture, UserRecord,
    };

    // ── 测试 fixture：两个桩仓储（完整定义；本步测试的读路径只走 StubSnapshotRepo）──

    /// 固定快照桩：`load_snapshot` 断言调用方只读本人快照（自隔离边界），返回预置快照。
    struct StubSnapshotRepo {
        snapshot: AuthorizationSnapshot,
    }
    impl AuthorizationSnapshotRepository for StubSnapshotRepo {
        fn load_snapshot<'a>(
            &'a self,
            actor_id: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                assert_eq!(actor_id, self.snapshot.actor.id, "自隔离：只读本人快照");
                Ok(self.snapshot.clone())
            })
        }

        /// protected 管理读取桩：当前 fixture 只覆盖 ordinary effective 场景，这里明确 `PermissionDenied`
        /// 拒绝管理分支（非未定义/unreachable 占位）；三类 caller 边界由下方 SourceSnapshotRepo 覆盖。
        fn load_admin_target_snapshot<'a>(
            &'a self,
            _caller: &'a Session,
            _target_user: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
    }

    /// 管理写仓储桩：本步测试的读路径不触碰（完整定义、全方法 unreachable、不留 undefined fn）。
    struct StubAuthzRepo;
    impl AuthzRepository for StubAuthzRepo {
        fn find_user<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 16],
        ) -> RepoFuture<'a, Option<UserRecord>> {
            Box::pin(async { unreachable!("桩：读路径不触写仓储") })
        }
        fn list_users<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<[u8; 16]>,
            _limit: usize,
        ) -> RepoFuture<'a, (Vec<UserRecord>, Option<[u8; 16]>)> {
            Box::pin(async { unreachable!() })
        }
        fn find_role<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
        ) -> RepoFuture<'a, Option<RoleRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_roles<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<Uuid>,
            _limit: usize,
        ) -> RepoFuture<'a, (Vec<RoleRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn find_grant<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
        ) -> RepoFuture<'a, Option<GrantRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_grants<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: Option<[u8; 16]>,
            _cursor: Option<Uuid>,
            _limit: usize,
        ) -> RepoFuture<'a, (Vec<GrantRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn list_grants_for_user<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: [u8; 16],
        ) -> RepoFuture<'a, Vec<GrantRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn roles_by_id<'a>(
            &'a self,
            _actor: &'a Session,
            _ids: Vec<Uuid>,
        ) -> RepoFuture<'a, HashMap<Uuid, RoleRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn device_groups_for_device<'a>(
            &'a self,
            _actor: &'a Session,
            _device_id: [u8; 32],
        ) -> RepoFuture<'a, HashSet<Uuid>> {
            Box::pin(async { unreachable!() })
        }
        fn find_group<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
        ) -> RepoFuture<'a, Option<GroupRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_groups<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<Uuid>,
            _limit: usize,
        ) -> RepoFuture<'a, (Vec<GroupRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn list_group_device_ids<'a>(
            &'a self,
            _actor: &'a Session,
            _group_id: Uuid,
        ) -> RepoFuture<'a, Vec<[u8; 32]>> {
            Box::pin(async { unreachable!() })
        }
        // Task 4 写方法（本步测试同样不触碰）
        fn create_user<'a>(
            &'a self,
            _actor: &'a Session,
            _username: &'a str,
            _display_name: &'a str,
            _is_admin: bool,
            _next_hash: &'a str,
        ) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_user<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: [u8; 16],
            _expected_revision: u64,
            _active: Option<bool>,
            _is_admin: Option<bool>,
            _display_name: Option<&'a str>,
        ) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn reset_user_password<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: [u8; 16],
            _expected_revision: u64,
            _next_hash: &'a str,
        ) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn create_role<'a>(
            &'a self,
            _actor: &'a Session,
            _name: &'a str,
            _permissions: &'a [Permission],
        ) -> RepoFuture<'a, RoleRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_role<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _expected_revision: u64,
            _name: Option<&'a str>,
            _permissions: Option<&'a [Permission]>,
            _archived: Option<bool>,
        ) -> RepoFuture<'a, RoleRecord> {
            Box::pin(async { unreachable!() })
        }
        fn create_grant<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: [u8; 16],
            _source: GrantSource,
            _scope: GrantScope,
        ) -> RepoFuture<'a, GrantRecord> {
            Box::pin(async { unreachable!() })
        }
        fn delete_grant<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _expected_revision: u64,
        ) -> RepoFuture<'a, ()> {
            Box::pin(async { unreachable!() })
        }
        fn create_group<'a>(
            &'a self,
            _actor: &'a Session,
            _name: &'a str,
        ) -> RepoFuture<'a, GroupRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_group<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _expected_revision: u64,
            _name: Option<&'a str>,
            _archived: Option<bool>,
        ) -> RepoFuture<'a, GroupRecord> {
            Box::pin(async { unreachable!() })
        }
    }

    fn source_device_fixture(
        id: [u8; 32],
        archived: bool,
    ) -> crate::devices::repository::DeviceAdminView {
        crate::devices::repository::DeviceAdminView {
            public_key: id,
            display_name: "synthetic-device".into(),
            admission_state: crate::model::AdmissionState::Approved,
            review_decision: crate::model::ReviewDecision::Approved,
            revision: 1,
            descriptor: None,
            archived,
        }
    }

    #[test]
    fn temporary_password_is_at_least_32_bytes_hex_and_hashed_with_argon2id() {
        let (raw, hash) = generate_temporary_password().expect("entropy available");
        assert_eq!(raw.len(), 64, "32 bytes hex = 64 chars");
        assert!(
            raw.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert!(PasswordHasher::new().verify(&raw, &hash).unwrap());
    }

    /// C1 关键行为：Session 声称 admin，但 DB 当前 `must_change_password` 才是权威（fail-closed → 0 权限）。
    #[tokio::test]
    async fn must_change_password_user_has_zero_effective_permissions() {
        let user = IdentityUser {
            id: [1u8; 16],
            username: "u".into(),
            password_hash: "x".into(),
            active: true,
            is_admin: true,
            must_change_password: false,
            revision: 1, // Session 陈旧数据声称"未需改密"
        };
        let session = Session {
            user,
            digest: [0u8; 32],
        };
        // DB 当前快照（桩的固定返回）：must_change_password = true。
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: true,
                must_change_password: true,
            },
            authz_epoch: 1,
            grants: vec![],
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices: HashMap::new(),
        };
        let service = AuthzService::new(
            Arc::new(StubSnapshotRepo { snapshot }),
            Arc::new(StubAuthzRepo),
        );
        // Session 说"admin 且未需改密"，DB 当前"必须改密" → 0 权限。
        let perms = service
            .effective_permissions(&session, [0u8; 32])
            .await
            .expect("读成功");
        assert!(
            perms.is_empty(),
            "must_change_password 的 admin 必须 fail-closed 到 0 权限"
        );
    }

    /// 普通用户：快照 group 范围授权只覆盖自身组成员（不跨设备）；组外设备 0 权限。
    #[tokio::test]
    async fn regular_user_effective_permissions_come_from_snapshot_and_never_cross_devices() {
        let user = IdentityUser {
            id: [1u8; 16],
            username: "u".into(),
            password_hash: "x".into(),
            active: true,
            is_admin: false,
            must_change_password: false,
            revision: 1,
        };
        let session = Session {
            user,
            digest: [0u8; 32],
        };
        let group = Uuid::from_bytes([0x67; 16]);
        let grant = GrantRecord {
            id: Uuid::from_bytes([0x0a; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::Group(group),
            revision: 1,
        };
        let mut device_groups = HashMap::new();
        let mut g = HashSet::new();
        g.insert(group);
        device_groups.insert([0xa1; 32], g); // 仅设备 A 属于 G
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: false,
                must_change_password: false,
            },
            authz_epoch: 1,
            grants: vec![grant],
            roles: HashMap::new(),
            device_groups,
            devices: HashMap::from([
                ([0xa1; 32], source_device_fixture([0xa1; 32], false)),
                ([0xa2; 32], source_device_fixture([0xa2; 32], false)),
            ]),
        };
        let service = AuthzService::new(
            Arc::new(StubSnapshotRepo { snapshot }),
            Arc::new(StubAuthzRepo),
        );
        // 设备 A 属于 G → 有权限。
        assert_eq!(
            service
                .effective_permissions(&session, [0xa1; 32])
                .await
                .expect("读成功"),
            HashSet::from([Permission::DeviceRead])
        );
        // 设备 B 不在 G → 0 权限（同一快照，B 的自身 group 集为空）。
        assert!(
            service
                .effective_permissions(&session, [0xa2; 32])
                .await
                .expect("读成功")
                .is_empty()
        );
    }

    // 验证caller边界的service fixture；合成现存设备保持跨用户成功正例，
    // 不证明真实SQL隔离/HTTP接线；target生命周期另由下方10场景纯测试覆盖。
    struct SourceSnapshotRepo {
        current_caller: SnapshotActor, // 独立于 Session 的旧 flags，模拟当前 DB caller。
        target_is_admin: bool,
        ordinary_calls: std::sync::atomic::AtomicUsize,
        protected_calls: std::sync::atomic::AtomicUsize,
    }
    impl SourceSnapshotRepo {
        fn new(current_caller: SnapshotActor, target_is_admin: bool) -> Self {
            Self {
                current_caller,
                target_is_admin,
                ordinary_calls: std::sync::atomic::AtomicUsize::new(0),
                protected_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn fresh_target_snapshot(&self) -> AuthorizationSnapshot {
            AuthorizationSnapshot {
                actor: SnapshotActor {
                    id: [2u8; 16],
                    active: true,
                    is_admin: self.target_is_admin,
                    must_change_password: false,
                },
                authz_epoch: 1,
                grants: vec![GrantRecord {
                    id: Uuid::from_bytes([0x0a; 16]),
                    user_id: [2u8; 16],
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                roles: HashMap::new(),
                device_groups: HashMap::new(),
                devices: HashMap::from([([0xa1; 32], source_device_fixture([0xa1; 32], false))]),
            } // 每次新建，不要求 AuthorizationSnapshot: Clone。
        }
        fn assert_protected_only(&self) {
            use std::sync::atomic::Ordering::SeqCst;
            assert_eq!(self.protected_calls.load(SeqCst), 1);
            assert_eq!(self.ordinary_calls.load(SeqCst), 0);
        }
    }
    impl AuthorizationSnapshotRepository for SourceSnapshotRepo {
        fn load_snapshot<'a>(
            &'a self,
            _actor_id: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                self.ordinary_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self.fresh_target_snapshot())
            })
        }
        fn load_admin_target_snapshot<'a>(
            &'a self,
            caller: &'a Session,
            target_user: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                self.protected_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if caller.user.id != self.current_caller.id
                    || !crate::authz::policy::is_current_admin(&self.current_caller)
                    || target_user != [2u8; 16]
                {
                    return Err(ControllerError::PermissionDenied);
                }
                Ok(self.fresh_target_snapshot())
            })
        }
    }
    fn source_caller_session(is_admin: bool) -> Session {
        Session {
            user: IdentityUser {
                id: [1u8; 16],
                username: "caller".into(),
                password_hash: "fixture-only".into(),
                active: true,
                is_admin,
                must_change_password: false,
                revision: 1,
            },
            digest: [9u8; 32],
        }
    }

    #[tokio::test]
    async fn stale_admin_sources_are_rejected_by_current_caller() {
        let session = source_caller_session(true); // 旧 Session 仍声称有效 admin。
        for (active, is_admin, must_change_password) in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let repo = Arc::new(SourceSnapshotRepo::new(
                SnapshotActor {
                    id: [1u8; 16],
                    active,
                    is_admin,
                    must_change_password,
                },
                false,
            ));
            let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
            let result = service
                .effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32])
                .await;
            assert!(matches!(result, Err(ControllerError::PermissionDenied)));
            repo.assert_protected_only(); // 每 case protected=1 / ordinary=0；for 不增加用例数。
        }
    }

    #[tokio::test]
    async fn admin_target_does_not_authorize_nonadmin_caller() {
        let session = source_caller_session(false);
        let repo = Arc::new(SourceSnapshotRepo::new(
            SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: false,
                must_change_password: false,
            },
            true,
        )); // target admin 不给 caller 授权。
        let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
        let result = service
            .effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32])
            .await;
        assert!(matches!(result, Err(ControllerError::PermissionDenied)));
        repo.assert_protected_only(); // 直接调用 service，不声称普通用户可穿过 HTTP gate。
    }

    #[tokio::test]
    async fn current_admin_can_read_other_user_sources() {
        let session = source_caller_session(true);
        let repo = Arc::new(SourceSnapshotRepo::new(
            SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: true,
                must_change_password: false,
            },
            false,
        )); // target 为不同的普通用户，只有 Direct DeviceRead。
        let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
        let entries = service
            .effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32])
            .await
            .expect("当前 admin 可以读取其他用户来源；恒拒绝不可假绿");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].permission, Permission::DeviceRead);
        assert!(
            matches!(&entries[0].source, crate::authz::policy::PermissionSourceKind::Direct(id)
                if *id == Uuid::from_bytes([0x0a; 16]))
        );
        repo.assert_protected_only();
    }

    /// Task 13a：`load_admin_target_page` 的 trait 默认可编译实现必须**失败关闭**——
    /// 未覆盖该方法的旧 fixture 绝不能因此得到成功空页；`limit` 合法域也在 service 处校验。
    #[tokio::test]
    async fn effective_permissions_page_fails_closed_on_default_repo_and_limit() {
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: true,
                must_change_password: false,
            },
            authz_epoch: 1,
            grants: vec![],
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices: HashMap::new(),
        };
        // StubSnapshotRepo 不覆盖 `load_admin_target_page` → 只能走 trait 默认实现。
        let service = AuthzService::new(
            Arc::new(StubSnapshotRepo { snapshot }),
            Arc::new(StubAuthzRepo),
        );
        let caller = source_caller_session(true);
        for limit in [0, 201] {
            assert!(
                matches!(
                    service
                        .effective_permissions_page(&caller, [2u8; 16], None, None, limit)
                        .await,
                    Err(ControllerError::InvalidArgument)
                ),
                "limit {limit} 必须 InvalidArgument"
            );
        }
        let page = service
            .effective_permissions_page(&caller, [2u8; 16], None, None, 50)
            .await;
        assert!(
            matches!(page, Err(ControllerError::PermissionDenied)),
            "默认实现必须失败关闭，不得出现成功空页：{page:?}"
        );
    }

    #[test]
    fn sources_permission_set_equals_effective_for_target_lifecycle() {
        let device_id = [0xa1; 32];
        for (active, must_change, is_admin, exists, archived) in [
            (true, false, false, true, false), // direct成功阳性，禁止恒空假绿
            (true, false, true, true, false),  // admin全集阳性
            (false, false, false, true, false),
            (false, false, true, true, false),
            (true, true, false, true, false),
            (true, true, true, true, false),
            (true, false, false, false, false),
            (true, false, true, false, false),
            (true, false, false, true, true),
            (true, false, true, true, true),
        ] {
            let mut devices = HashMap::new();
            if exists {
                devices.insert(device_id, source_device_fixture(device_id, archived));
            }
            let snapshot = AuthorizationSnapshot {
                actor: SnapshotActor {
                    id: [2; 16],
                    active,
                    is_admin,
                    must_change_password: must_change,
                },
                authz_epoch: 1,
                grants: vec![GrantRecord {
                    id: Uuid::new_v4(),
                    user_id: [2; 16],
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                roles: HashMap::new(),
                device_groups: HashMap::new(),
                devices,
            };
            let effective = snapshot_effective_permissions(&snapshot, device_id);
            let sources = snapshot_permission_sources(&snapshot, device_id);
            let source_set: HashSet<_> = sources.iter().map(|entry| entry.permission).collect();
            assert_eq!(source_set, effective);
            let valid = active && !must_change && exists && !archived;
            if valid && is_admin {
                assert_eq!(
                    effective,
                    HashSet::from(crate::authz::policy::ALL_DEVICE_PERMISSIONS)
                );
            } else if valid {
                assert_eq!(effective, HashSet::from([Permission::DeviceRead]));
            } else {
                assert!(effective.is_empty());
                assert!(sources.is_empty());
            }
        }
    }
}
