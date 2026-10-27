use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use uuid::Uuid;

use crate::authz::policy::{GrantRecord, RoleRecord, SnapshotActor};
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;

pub type RepoFuture<'a, T> =
    Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub id: [u8; 16],
    pub username: String,
    pub display_name: String,
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupRecord {
    pub id: Uuid,
    pub name: String,
    pub archived: bool,
    pub revision: u64,
    pub member_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantColumns {
    pub source_kind: &'static str,
    pub role_id: Option<Uuid>,
    pub permissions_json: Option<String>,
    pub scope_kind: &'static str,
    pub scope_group_id: Option<Uuid>,
    pub scope_device_id: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationSnapshot {
    pub actor: SnapshotActor,
    pub authz_epoch: u64,
    pub grants: Vec<GrantRecord>,
    pub roles: HashMap<Uuid, RoleRecord>,
    pub device_groups: HashMap<[u8; 32], HashSet<Uuid>>,
    pub devices: HashMap<[u8; 32], DeviceAdminView>,
}
