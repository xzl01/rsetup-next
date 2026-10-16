use std::collections::HashSet;
use std::pin::Pin;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{GrantRecord, Permission, RoleRecord, eval_memory_permissions};
use crate::authz::sqlx_repo::{
    AuthorizationSnapshot, acquire_authorization_read_connection, load_actor_authorization_in_tx,
    load_device_snapshot_in_tx, load_snapshot_in_tx,
};
use crate::db::DbPool;
use crate::error::ControllerError;
use crate::model::{AdmissionState, ReviewDecision};

pub type RepoFuture<'a, T> =
    Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditCursor {
    pub process_epoch: Uuid,
    pub event_seq: u64,
    pub id: Uuid,
}

pub(crate) const AUDIT_PAGE_SQL: &str = "SELECT id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome, process_epoch, event_seq, time_evidence AS created_evidence FROM audit_events WHERE (? IS NULL OR actor_user_id = ?) AND (? IS NULL OR (target_kind = 'device' AND target_id = ?)) AND (? IS NULL OR process_epoch > ? OR (process_epoch = ? AND (event_seq > ? OR (event_seq = ? AND id > ?)))) ORDER BY process_epoch, event_seq, id LIMIT ?";

pub(crate) const DEVICE_PAGE_KEYS_FIRST_SQL: &str =
    "SELECT public_key FROM devices ORDER BY public_key LIMIT ?";
pub(crate) const DEVICE_PAGE_KEYS_SQL: &str =
    "SELECT public_key FROM devices WHERE public_key > ? ORDER BY public_key LIMIT ?";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAdminView {
    pub public_key: [u8; 32],
    pub display_name: String,
    pub admission_state: AdmissionState,
    pub review_decision: ReviewDecision,
    pub revision: u64,
    pub descriptor: Option<serde_json::Value>,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEventRow {
    pub id: Uuid,
    pub actor_kind: String,
    pub actor_user_id: Option<[u8; 16]>,
    pub event_type: String,
    pub target_kind: String,
    pub target_id: Option<String>,
    pub params_redacted: String,
    pub outcome: String,
    pub process_epoch: Uuid,
    pub event_seq: u64,
    pub created_evidence: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuditFilter {
    pub actor_id: Option<[u8; 16]>,
    pub device_id: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemCounts {
    pub users: u64,
    pub devices: u64,
    pub roles: u64,
    pub grants: u64,
    pub groups: u64,
    pub schema_version: i32,
}

pub trait DeviceRepository: Send + Sync {
    #[allow(clippy::type_complexity)]
    fn device_page<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> RepoFuture<
        'a,
        (
            Vec<(DeviceAdminView, HashSet<Permission>)>,
            Option<[u8; 32]>,
        ),
    >;

    fn find_device<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
    ) -> RepoFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>>;

    fn audit_page<'a>(
        &'a self,
        actor: &'a Session,
        filter: AuditFilter,
        cursor: Option<AuditCursor>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<AuditEventRow>, Option<AuditCursor>)>;

    fn system_counts<'a>(&'a self, actor: &'a Session) -> RepoFuture<'a, SystemCounts>;
}

pub struct SqlxDeviceRepository {
    pub(crate) db: DbPool,
}

impl SqlxDeviceRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

pub fn device_is_visible(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &std::collections::HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
    is_admin: bool,
) -> bool {
    if is_admin {
        return true;
    }
    !crate::authz::policy::evaluate_grants(user_id, device_id, grants, roles, device_groups)
        .is_empty()
}

pub struct VisibleDevicePage {
    limit: usize,
    items: Vec<(DeviceAdminView, HashSet<Permission>)>,
    extra_visible: bool,
}

impl VisibleDevicePage {
    pub fn new(limit: usize) -> Result<Self, ControllerError> {
        if !(1..=200).contains(&limit) {
            return Err(ControllerError::InvalidArgument);
        }
        Ok(Self {
            limit,
            items: Vec::new(),
            extra_visible: false,
        })
    }

    pub fn observe(&mut self, view: DeviceAdminView, permissions: HashSet<Permission>) {
        if view.archived || permissions.is_empty() {
            return;
        }
        if self.items.len() < self.limit {
            self.items.push((view, permissions));
        } else {
            self.extra_visible = true;
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn finish(
        self,
    ) -> (
        Vec<(DeviceAdminView, HashSet<Permission>)>,
        Option<[u8; 32]>,
    ) {
        let next = if self.extra_visible {
            self.items.last().map(|(view, _)| view.public_key)
        } else {
            None
        };
        (self.items, next)
    }
}

pub fn project_device_detail(
    snapshot: &AuthorizationSnapshot,
    id: [u8; 32],
) -> Result<Option<(DeviceAdminView, HashSet<Permission>)>, ControllerError> {
    if !snapshot.actor.active || snapshot.actor.must_change_password {
        return Ok(None);
    }
    let view = match snapshot.devices.get(&id) {
        Some(v) if !v.archived => v,
        _ => return Ok(None),
    };
    let device_groups = snapshot.device_groups.get(&id).cloned().unwrap_or_default();
    let perms = eval_memory_permissions(
        &snapshot.actor,
        id,
        &snapshot.grants,
        &snapshot.roles,
        &device_groups,
    );
    if perms.is_empty() {
        Ok(None)
    } else if !perms.contains(&Permission::DeviceRead) {
        Err(ControllerError::PermissionDenied)
    } else {
        Ok(Some((view.clone(), perms)))
    }
}

impl DeviceRepository for SqlxDeviceRepository {
    fn device_page<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> RepoFuture<
        'a,
        (
            Vec<(DeviceAdminView, HashSet<Permission>)>,
            Option<[u8; 32]>,
        ),
    > {
        Box::pin(async move {
            let mut page = VisibleDevicePage::new(limit)?;
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            let auth = load_actor_authorization_in_tx(&mut tx, actor.user.id).await?;
            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let mut after = cursor;
            loop {
                let raw_keys = match after {
                    Some(key) => {
                        sqlx::query_scalar::<_, Vec<u8>>(DEVICE_PAGE_KEYS_SQL)
                            .bind(key.as_slice())
                            .bind(scan_limit)
                            .fetch_all(&mut *tx)
                            .await?
                    }
                    None => {
                        sqlx::query_scalar::<_, Vec<u8>>(DEVICE_PAGE_KEYS_FIRST_SQL)
                            .bind(scan_limit)
                            .fetch_all(&mut *tx)
                            .await?
                    }
                };
                if raw_keys.is_empty() {
                    break;
                }
                let keys = raw_keys
                    .into_iter()
                    .map(|raw| {
                        <[u8; 32]>::try_from(raw).map_err(|_| {
                            ControllerError::Config("identity admin read snapshot.device_id".into())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let (groups, devices) = load_device_snapshot_in_tx(&mut tx, &keys).await?;
                for key in &keys {
                    let Some(view) = devices.get(key) else {
                        continue;
                    };
                    let permissions = eval_memory_permissions(
                        &auth.actor,
                        *key,
                        &auth.grants,
                        &auth.roles,
                        &groups.get(key).cloned().unwrap_or_default(),
                    );
                    page.observe(view.clone(), permissions);
                    if page.extra_visible {
                        break;
                    }
                }
                if page.extra_visible || keys.len() < limit + 1 {
                    break;
                }
                after = keys.last().copied();
            }
            tx.commit().await?;
            Ok(page.finish())
        })
    }

    fn find_device<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
    ) -> RepoFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            let snapshot = match load_snapshot_in_tx(&mut tx, actor.user.id, &[id]).await {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
            let projected = match project_device_detail(&snapshot, id) {
                Ok(p) => p,
                Err(e) => {
                    let _ = tx.rollback().await;
                    return Err(e);
                }
            };
            tx.commit().await?;
            Ok(projected)
        })
    }

    fn audit_page<'a>(
        &'a self,
        actor: &'a Session,
        filter: AuditFilter,
        cursor: Option<AuditCursor>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<AuditEventRow>, Option<AuditCursor>)> {
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            crate::authz::sqlx_repo::verify_management_read(&mut tx, actor).await?;

            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let actor_filter_raw = filter.actor_id.map(|id| id.to_vec());
            let device_filter_hex = filter.device_id.map(hex::encode);
            let (c_epoch_1, c_epoch_2, c_epoch_3, c_seq_1, c_seq_2, c_id) = match cursor {
                Some(c) => (
                    Some(c.process_epoch.as_bytes().to_vec()),
                    Some(c.process_epoch.as_bytes().to_vec()),
                    Some(c.process_epoch.as_bytes().to_vec()),
                    Some(c.event_seq),
                    Some(c.event_seq),
                    Some(c.id.as_bytes().to_vec()),
                ),
                None => (None, None, None, None, None, None),
            };

            let rows = sqlx::query(AUDIT_PAGE_SQL)
                .bind(actor_filter_raw.as_deref())
                .bind(actor_filter_raw.as_deref())
                .bind(device_filter_hex.as_deref())
                .bind(device_filter_hex.as_deref())
                .bind(c_epoch_1.as_deref())
                .bind(c_epoch_2.as_deref())
                .bind(c_epoch_3.as_deref())
                .bind(c_seq_1)
                .bind(c_seq_2)
                .bind(c_id.as_deref())
                .bind(scan_limit)
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| ControllerError::Config("identity admin read audit.query".into()))?;

            use sqlx::Row;
            let mut items = Vec::new();
            for r in rows {
                let id_raw: Vec<u8> = r.try_get("id").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let id = Uuid::from_slice(&id_raw).map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let actor_kind: String = r.try_get("actor_kind").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let actor_user_id_raw: Option<Vec<u8>> =
                    r.try_get("actor_user_id").map_err(|_| {
                        ControllerError::Config("identity admin read audit.decode".into())
                    })?;
                let actor_user_id = match actor_user_id_raw {
                    Some(bytes) => Some(<[u8; 16]>::try_from(bytes).map_err(|_| {
                        ControllerError::Config("identity admin read audit.decode".into())
                    })?),
                    None => None,
                };
                let event_type: String = r.try_get("event_type").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let target_kind: String = r.try_get("target_kind").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let target_id: Option<String> = r.try_get("target_id").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let params_redacted: String = r.try_get("params_redacted").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let outcome: String = r.try_get("outcome").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let process_epoch_raw: Vec<u8> = r.try_get("process_epoch").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let process_epoch = Uuid::from_slice(&process_epoch_raw).map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                let event_seq: u64 = r.try_get("event_seq").map_err(|_| {
                    ControllerError::Config("identity admin read audit.decode".into())
                })?;
                if event_seq == 0 {
                    return Err(ControllerError::Config(
                        "identity admin read audit.decode".into(),
                    ));
                }
                let time_evidence: Option<String> =
                    r.try_get("created_evidence").map_err(|_| {
                        ControllerError::Config("identity admin read audit.decode".into())
                    })?;
                let created_evidence = match time_evidence {
                    Some(s) => serde_json::from_str(&s).map_err(|_| {
                        ControllerError::Config("identity admin read audit.decode".into())
                    })?,
                    None => {
                        return Err(ControllerError::Config(
                            "identity admin read audit.decode".into(),
                        ));
                    }
                };

                items.push(AuditEventRow {
                    id,
                    actor_kind,
                    actor_user_id,
                    event_type,
                    target_kind,
                    target_id,
                    params_redacted,
                    outcome,
                    process_epoch,
                    event_seq,
                    created_evidence,
                });
            }

            let next_cursor = if items.len() > limit {
                items.truncate(limit);
                let last = items.last().expect("items not empty");
                Some(AuditCursor {
                    process_epoch: last.process_epoch,
                    event_seq: last.event_seq,
                    id: last.id,
                })
            } else {
                None
            };

            tx.commit().await?;
            Ok((items, next_cursor))
        })
    }

    fn system_counts<'a>(&'a self, actor: &'a Session) -> RepoFuture<'a, SystemCounts> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            crate::authz::sqlx_repo::verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| {
                    ControllerError::Config("identity admin read system_counts.users".into())
                })?;
            let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| {
                    ControllerError::Config("identity admin read system_counts.devices".into())
                })?;
            let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| {
                    ControllerError::Config("identity admin read system_counts.roles".into())
                })?;
            let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM grants")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| {
                    ControllerError::Config("identity admin read system_counts.grants".into())
                })?;
            let groups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_groups")
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| {
                    ControllerError::Config("identity admin read system_counts.groups".into())
                })?;
            let meta_row = sqlx::query(
                "SELECT CAST(schema_version AS SIGNED) AS schema_version FROM schema_meta LIMIT 1",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| {
                ControllerError::Config("identity admin read system_counts.meta".into())
            })?;
            let schema_version: i64 = meta_row.try_get("schema_version").map_err(|_| {
                ControllerError::Config("identity admin read system_counts.meta".into())
            })?;

            tx.commit().await?;
            Ok(SystemCounts {
                users: users as u64,
                devices: devices as u64,
                roles: roles as u64,
                grants: grants as u64,
                groups: groups as u64,
                schema_version: schema_version as i32,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn audit_keys_keep_namespaces_distinct_and_sql_has_exact_bindings() {
        let mut keys = vec![
            AuditCursor {
                process_epoch: Uuid::from_u128(0x00000000000040008000000000000003),
                event_seq: 7,
                id: Uuid::new_v4(),
            },
            AuditCursor {
                process_epoch: Uuid::from_u128(0x00000000000040008000000000000001),
                event_seq: 7,
                id: Uuid::new_v4(),
            },
            AuditCursor {
                process_epoch: Uuid::from_u128(0x00000000000040008000000000000002),
                event_seq: 7,
                id: Uuid::new_v4(),
            },
        ];
        keys.sort();
        assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(keys.len(), 3);
        assert_eq!(AUDIT_PAGE_SQL.matches('?').count(), 11);
    }

    #[test]
    fn visible_device_page_cursor_requires_actual_extra_visible_across_batches() {
        let make = |id: u8, archived: bool| DeviceAdminView {
            public_key: [id; 32],
            display_name: "synthetic".into(),
            admission_state: crate::model::AdmissionState::Approved,
            review_decision: crate::model::ReviewDecision::Approved,
            revision: 1,
            descriptor: None,
            archived,
        };
        let cases = [
            (vec![vec![(1, true, false), (2, true, false)]], None), // exact limit无下一项
            (
                vec![vec![(1, true, false), (2, true, false), (3, false, false)]],
                None,
            ),
            (
                vec![vec![(1, true, false), (2, true, false), (3, true, true)]],
                None,
            ),
            (
                vec![
                    vec![(1, true, false), (2, true, false)],
                    vec![(3, false, false)],
                    vec![(4, false, false)],
                ],
                None,
            ),
            (
                vec![
                    vec![(1, true, false), (2, true, false)],
                    vec![(3, false, false)],
                    vec![(4, true, false)],
                ],
                Some([2; 32]),
            ),
        ];
        for (batches, expected) in cases {
            let mut page = VisibleDevicePage::new(2).unwrap();
            for batch in batches {
                for (id, visible, archived) in batch {
                    let permissions = if visible {
                        HashSet::from([Permission::DeviceRead])
                    } else {
                        HashSet::new()
                    };
                    page.observe(make(id, archived), permissions);
                }
            }
            let (items, next) = page.finish();
            assert_eq!(items.len(), 2);
            assert_eq!(next, expected);
        }
    }

    #[test]
    fn device_visibility_requires_a_permission_for_the_specific_device() {
        // 纯判定函数：group 授权只覆盖该组内设备，device 授权只覆盖目标设备。
        let group = uuid::Uuid::from_bytes([3u8; 16]);
        let target = [7u8; 32];
        let other = [8u8; 32];
        let grants = vec![crate::authz::policy::GrantRecord {
            id: uuid::Uuid::nil(),
            user_id: [1u8; 16],
            source: crate::authz::policy::GrantSource::Direct(vec![
                crate::authz::policy::Permission::DeviceRead,
            ]),
            scope: crate::authz::policy::GrantScope::Group(group),
            revision: 1,
        }];
        let roles = std::collections::HashMap::new();
        let mut groups = HashSet::new();
        groups.insert(group);
        assert!(device_is_visible(
            [1u8; 16], target, &grants, &roles, &groups, false
        ));
        let mut empty = HashSet::new();
        empty_insert_nothing(&mut empty);
        assert!(!device_is_visible(
            [1u8; 16], other, &grants, &roles, &empty, false
        ));
        // 管理员永远可见全部设备。
        assert!(device_is_visible(
            [2u8; 16],
            other,
            &[],
            &roles,
            &empty,
            true
        ));
    }

    #[test]
    fn project_device_detail_table_driven_cases() {
        use crate::authz::policy::{GrantScope, GrantSource, Permission, SnapshotActor};
        use crate::authz::sqlx_repo::AuthorizationSnapshot;
        use crate::devices::repository::DeviceAdminView;
        use crate::model::{AdmissionState, ReviewDecision};
        use std::collections::{HashMap, HashSet};

        let target_dev = [0x11u8; 32];
        let missing_dev = [0x99u8; 32];
        let archived_dev = [0x22u8; 32];

        let active_admin = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: true,
            must_change_password: false,
        };
        let active_user = SnapshotActor {
            id: [2u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let inactive_user = SnapshotActor {
            id: [3u8; 16],
            active: false,
            is_admin: false,
            must_change_password: false,
        };
        let must_change_user = SnapshotActor {
            id: [4u8; 16],
            active: true,
            is_admin: false,
            must_change_password: true,
        };

        let make_view = |pk: [u8; 32], archived: bool| DeviceAdminView {
            public_key: pk,
            display_name: "dev-test".into(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 10,
            descriptor: None,
            archived,
        };

        let mut base_devices = HashMap::new();
        base_devices.insert(target_dev, make_view(target_dev, false));
        base_devices.insert(archived_dev, make_view(archived_dev, true));

        enum CaseExpect {
            OkNone,
            Forbidden,
            OkSome(HashSet<Permission>),
        }

        struct TestCase {
            name: &'static str,
            actor: SnapshotActor,
            grants: Vec<crate::authz::policy::GrantRecord>,
            query_id: [u8; 32],
            expect: CaseExpect,
        }

        let cases = vec![
            TestCase {
                name: "1. regular user read success returns Some(view, perms)",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkSome([Permission::DeviceRead].into_iter().collect()),
            },
            TestCase {
                name: "2. regular user reboot-only returns 403 PermissionDenied",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::Forbidden,
            },
            TestCase {
                name: "3. regular user zero permission on target returns Ok(None) 404",
                actor: active_user,
                grants: vec![],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "4. missing target with all grant returns Ok(None) 404 without leaking existence",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![
                        Permission::DeviceRead,
                        Permission::DeviceReboot,
                    ]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: missing_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "5. archived target with all grant returns Ok(None) 404 without leaking existence",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![
                        Permission::DeviceRead,
                        Permission::DeviceReboot,
                    ]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: archived_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "6. current admin missing target returns Ok(None) 404 without 403 or leaking",
                actor: active_admin,
                grants: vec![],
                query_id: missing_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "7. current admin archived target returns Ok(None) 404 without 403 or leaking",
                actor: active_admin,
                grants: vec![],
                query_id: archived_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "8. stale session admin degraded in snapshot to reboot-only returns 403 PermissionDenied",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::Forbidden,
            },
            TestCase {
                name: "9. snapshot inactive actor with grant has zero view returns Ok(None) 404",
                actor: inactive_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: inactive_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "10. snapshot must_change_password actor with grant has zero view returns Ok(None) 404",
                actor: must_change_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: must_change_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "11. current admin active on existing non-archived target returns Some with all permissions",
                actor: active_admin,
                grants: vec![],
                query_id: target_dev,
                expect: CaseExpect::OkSome(
                    crate::authz::policy::ALL_DEVICE_PERMISSIONS
                        .iter()
                        .copied()
                        .collect(),
                ),
            },
        ];

        for tc in cases {
            let snap = AuthorizationSnapshot {
                actor: tc.actor,
                authz_epoch: 1,
                grants: tc.grants,
                roles: HashMap::new(),
                device_groups: HashMap::new(),
                devices: base_devices.clone(),
            };
            let res = project_device_detail(&snap, tc.query_id);
            match tc.expect {
                CaseExpect::OkNone => {
                    assert!(
                        matches!(res, Ok(None)),
                        "case {} expected Ok(None), got {:?}",
                        tc.name,
                        res
                    );
                }
                CaseExpect::Forbidden => {
                    assert!(
                        matches!(res, Err(ControllerError::PermissionDenied)),
                        "case {} expected PermissionDenied, got {:?}",
                        tc.name,
                        res
                    );
                }
                CaseExpect::OkSome(expected_perms) => {
                    let (view, perms) = res.expect("must succeed").expect("must be Some");
                    assert_eq!(
                        view.public_key, tc.query_id,
                        "case {} view public_key mismatch",
                        tc.name
                    );
                    assert_eq!(perms, expected_perms, "case {} perms mismatch", tc.name);
                }
            }
        }
    }

    fn empty_insert_nothing(_set: &mut HashSet<uuid::Uuid>) {}
}
