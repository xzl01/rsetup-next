use crate::db::DbPool;
use crate::error::ControllerError;

use super::codec::{decode_user, ordered_user_ids};
use super::model::UserRecord;

/// 目标为 users 行时的行锁快照（`FOR UPDATE`），解码为安全 `UserRecord`。
type UserRow = UserRecord;

async fn select_user_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    id: [u8; 16],
) -> Result<Option<UserRow>, ControllerError> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id,username,display_name,CAST(active AS SIGNED) AS active,CAST(is_admin AS SIGNED) AS is_admin,CAST(must_change_password AS SIGNED) AS must_change_password,revision FROM users WHERE id=? FOR UPDATE",
    )
    .bind(id.as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        decode_user(
            row.try_get("id")?,
            row.try_get("username")?,
            row.try_get("display_name")?,
            row.try_get("active")?,
            row.try_get("is_admin")?,
            row.try_get("must_change_password")?,
            row.try_get("revision")?,
        )
    })
    .transpose()
}

/// 管理写事务统一入口：begin 后立即取 schema guard 行锁（期望版本固定为
/// 编译期 `SUPPORTED_SCHEMA_VERSION`），串行化全部管理写。
pub(super) async fn begin_guard(
    db: &DbPool,
) -> Result<sqlx::Transaction<'_, sqlx::MySql>, ControllerError> {
    let mut tx = db.0.begin().await?;
    crate::integrity::lock_integrity_guard(&mut tx).await?;
    Ok(tx)
}

/// 目标为 users 行时：guard → 按 UUID 字节序锁 actor/target → 用**锁定行**
/// 严格重验 actor 仍为有效管理员 → 返回目标行（缺失即 NotFound）。
pub(super) async fn lock_user_pair_and_verify_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: [u8; 16],
    target: [u8; 16],
) -> Result<Option<UserRow>, ControllerError> {
    let (first, second) = ordered_user_ids(actor, target);
    let first_row = select_user_for_update(tx, first).await?;
    let second_row = match second {
        Some(id) => select_user_for_update(tx, id).await?,
        None => None,
    };
    let actor_row = if first == actor {
        first_row.as_ref()
    } else {
        second_row.as_ref()
    };
    let actor_row = actor_row.ok_or(ControllerError::PermissionDenied)?;
    if !(actor_row.active && actor_row.is_admin && !actor_row.must_change_password) {
        return Err(ControllerError::PermissionDenied);
    }
    Ok(if first == target {
        first_row
    } else {
        second_row
    })
}

/// `authz_epoch` checked CAS 自增；singleton 行已被 guard 锁定，CAS 仍防未走 guard 的写者。
pub(crate) async fn bump_authz_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
) -> Result<u64, ControllerError> {
    let current: u64 =
        sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
            .fetch_one(&mut **tx)
            .await?;
    let next = current
        .checked_add(1)
        .ok_or(ControllerError::RevisionConflict)?;
    let changed = sqlx::query(
        "UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?",
    )
    .bind(next)
    .bind(current)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(next)
}

pub(super) fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}
