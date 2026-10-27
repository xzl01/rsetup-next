use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission, RoleRecord};
use crate::error::ControllerError;

use super::model::{AuthorizationSnapshot, GroupRecord, RepoFuture, UserRecord};

pub trait AuthzRepository: Send + Sync {
    fn find_user<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
    ) -> RepoFuture<'a, Option<UserRecord>>;
    fn list_users<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 16]>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<UserRecord>, Option<[u8; 16]>)>;
    fn find_role<'a>(&'a self, actor: &'a Session, id: Uuid) -> RepoFuture<'a, Option<RoleRecord>>;
    fn list_roles<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<RoleRecord>, Option<Uuid>)>;
    fn find_grant<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GrantRecord>>;
    fn list_grants<'a>(
        &'a self,
        actor: &'a Session,
        user_id: Option<[u8; 16]>,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GrantRecord>, Option<Uuid>)>;
    fn list_grants_for_user<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
    ) -> RepoFuture<'a, Vec<GrantRecord>>;
    fn roles_by_id<'a>(
        &'a self,
        actor: &'a Session,
        ids: Vec<Uuid>,
    ) -> RepoFuture<'a, HashMap<Uuid, RoleRecord>>;
    fn device_groups_for_device<'a>(
        &'a self,
        actor: &'a Session,
        device_id: [u8; 32],
    ) -> RepoFuture<'a, HashSet<Uuid>>;
    fn find_group<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GroupRecord>>;
    fn list_groups<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GroupRecord>, Option<Uuid>)>;
    fn list_group_device_ids<'a>(
        &'a self,
        actor: &'a Session,
        group_id: Uuid,
    ) -> RepoFuture<'a, Vec<[u8; 32]>>;

    // ---- 管理写方法（Task 4）：统一事务骨架 guard → 行锁顺序 → CAS →
    // LastAdmin → 审计同事务插入 → commit；任一错误 drop 局部 tx 自动回滚。----
    fn create_user<'a>(
        &'a self,
        actor: &'a Session,
        username: &'a str,
        display_name: &'a str,
        is_admin: bool,
        next_hash: &'a str,
    ) -> RepoFuture<'a, UserRecord>;
    fn patch_user<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        expected_revision: u64,
        active: Option<bool>,
        is_admin: Option<bool>,
        display_name: Option<&'a str>,
    ) -> RepoFuture<'a, UserRecord>;
    fn reset_user_password<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        expected_revision: u64,
        next_hash: &'a str,
    ) -> RepoFuture<'a, UserRecord>;
    fn create_role<'a>(
        &'a self,
        actor: &'a Session,
        name: &'a str,
        permissions: &'a [Permission],
    ) -> RepoFuture<'a, RoleRecord>;
    fn patch_role<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
        name: Option<&'a str>,
        permissions: Option<&'a [Permission]>,
        archived: Option<bool>,
    ) -> RepoFuture<'a, RoleRecord>;
    fn create_grant<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        source: GrantSource,
        scope: GrantScope,
    ) -> RepoFuture<'a, GrantRecord>;
    fn delete_grant<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
    ) -> RepoFuture<'a, ()>;
    fn create_group<'a>(&'a self, actor: &'a Session, name: &'a str)
    -> RepoFuture<'a, GroupRecord>;
    fn patch_group<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
        name: Option<&'a str>,
        archived: Option<bool>,
    ) -> RepoFuture<'a, GroupRecord>;
}

pub trait AuthorizationSnapshotRepository: Send + Sync {
    fn load_snapshot<'a>(
        &'a self,
        actor_id: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot>;

    fn load_admin_target_snapshot<'a>(
        &'a self,
        caller: &'a Session,
        target_user: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot>;

    /// protected 管理来源页：同一受保护读事务内按 target 用户投影有效权限与来源归因。
    /// 可编译默认可失败关闭；未覆盖该方法的旧 fixture 不得因此得到成功空页。
    fn load_admin_target_page<'a>(
        &'a self,
        _caller: &'a Session,
        _target: [u8; 16],
        _device_id: Option<[u8; 32]>,
        _cursor: Option<[u8; 32]>,
        _limit: usize,
    ) -> RepoFuture<'a, serde_json::Value> {
        Box::pin(async { Err(ControllerError::PermissionDenied) })
    }
}
