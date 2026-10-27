use crate::auth::service::Session;
use crate::db::DbPool;
use crate::error::ControllerError;

use super::codec::read_error;
use super::model::{AuthorizationSnapshot, RepoFuture};
use super::queries::ADMIN_TARGET_DEVICE_KEYS_SQL;
use super::snapshot_read::{
    acquire_authorization_read_connection, ensure_actor_admin_readonly,
    load_admin_target_snapshot_in_tx, load_snapshot_in_tx,
};
use super::traits::AuthorizationSnapshotRepository;

pub struct SqlxAuthorizationSnapshotRepository {
    pub(crate) db: DbPool,
}

impl SqlxAuthorizationSnapshotRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl AuthorizationSnapshotRepository for SqlxAuthorizationSnapshotRepository {
    fn load_snapshot<'a>(
        &'a self,
        actor_id: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot = load_snapshot_in_tx(&mut tx, actor_id, device_ids).await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }

    fn load_admin_target_snapshot<'a>(
        &'a self,
        caller: &'a crate::auth::service::Session,
        target_user: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot =
                load_admin_target_snapshot_in_tx(&mut tx, caller, target_user, device_ids).await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }

    fn load_admin_target_page<'a>(
        &'a self,
        caller: &'a Session,
        target: [u8; 16],
        device_id: Option<[u8; 32]>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> RepoFuture<'a, serde_json::Value> {
        let db = &self.db;
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            // 同连接同事务：caller 当前 admin（DB raw_bool），行缺失 → PermissionDenied。
            ensure_actor_admin_readonly(&mut tx, caller).await?;
            // target 存在性同 tx 检查：缺失是 protected 目标 404，不误映 PermissionDenied；
            // target inactive / must_change_password 属合法存在，来源/权限可为空。
            let exists = sqlx::query_scalar::<_, Vec<u8>>("SELECT id FROM users WHERE id=?")
                .bind(target.as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("snapshot.actor"))?;
            if exists.is_none() {
                return Err(ControllerError::NotFound);
            }
            let keys: Vec<[u8; 32]> = match device_id {
                Some(id) => vec![id],
                None => {
                    let scan_limit =
                        u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
                    sqlx::query_scalar::<_, Vec<u8>>(ADMIN_TARGET_DEVICE_KEYS_SQL)
                        .bind(cursor.map(|key| key.to_vec()))
                        .bind(cursor.map(|key| key.to_vec()))
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await?
                        .into_iter()
                        .map(|raw| raw.try_into().map_err(|_| read_error("snapshot.device_id")))
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            // 单快照：target 的 grants/roles/device_groups/devices 全部在同一 tx 内加载，
            // schema_meta 单例与版本防线由 load_snapshot_in_tx 承担，不另开第二套。
            let snapshot = load_snapshot_in_tx(&mut tx, target, &keys).await?;
            if let Some(id) = device_id
                && !snapshot.devices.get(&id).is_some_and(|view| !view.archived)
            {
                return Err(ControllerError::NotFound);
            }
            let has_next = device_id.is_none() && keys.len() > limit;
            let mut items = Vec::new();
            let mut last = None;
            for key in keys.iter().take(limit) {
                let permissions =
                    crate::authz::service::snapshot_effective_permissions(&snapshot, *key);
                let mut effective: Vec<_> = permissions
                    .iter()
                    .map(|permission| permission.as_str())
                    .collect();
                effective.sort_unstable();
                let sources = crate::authz::service::snapshot_permission_sources(&snapshot, *key)
                    .into_iter()
                    .map(|entry| {
                        let source = match entry.source {
                            crate::authz::policy::PermissionSourceKind::Admin => {
                                serde_json::json!({"kind":"admin"})
                            }
                            crate::authz::policy::PermissionSourceKind::Role(id) => {
                                serde_json::json!({"kind":"role","role_id":id.to_string()})
                            }
                            crate::authz::policy::PermissionSourceKind::Direct(id) => {
                                serde_json::json!({"kind":"direct","grant_id":id.to_string()})
                            }
                        };
                        serde_json::json!({"permission":entry.permission.as_str(),"source":source})
                    })
                    .collect::<Vec<_>>();
                items.push(serde_json::json!({
                    "device_id": hex::encode(key),
                    "effective_permissions": effective,
                    "sources": sources,
                }));
                last = Some(*key);
            }
            tx.commit().await?;
            // 内部 raw key（末项公钥 hex64）；出口签名归 HTTP 层。
            Ok(serde_json::json!({
                "items": items,
                "next_cursor": if has_next { last.map(hex::encode) } else { None },
            }))
        })
    }
}
