//! 事务内执行者（Actor）重验与权限状态解析。
//!
//! 包含 `ActorRow` 结构体、`active_admin` / `writable` 判定方法、
//! 严格布尔解码 `strict_bool` 以及事务内 actor 行查询锁定 `read_actor_in_tx`。

use sqlx::Transaction;
use sqlx::mysql::MySql;
use uuid::Uuid;

use crate::error::ControllerError;
use crate::tasks::repository::{column, task_row_decode_error, tx_row};

/// 事务内重验后的 actor 行（`users` 环节的唯一取锁点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ActorRow {
    active: bool,
    pub(super) is_admin: bool,
    must_change_password: bool,
}

impl ActorRow {
    /// `integrity::require_active_admin` 同语义：`active && is_admin &&
    /// !must_change_password` 三条件缺一即拒绝管理写操作（并发降权/停用/未改
    /// 初始口令都必须在事务内失败关闭，绝不信 HTTP 层快照的 `is_admin` 布尔）。
    pub(super) fn active_admin(&self) -> bool {
        self.active && self.is_admin && !self.must_change_password
    }

    /// 提交/取消等业务写操作的最低要求：账号有效且未待改初始口令（不要求 admin）。
    pub(super) fn writable(&self) -> bool {
        self.active && !self.must_change_password
    }
}

/// 严格 `0`/`1` 解码 `CAST(... AS SIGNED)` 布尔列；污染值即 DB 损坏（503）。
fn strict_bool(raw: i64) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(task_row_decode_error()),
    }
}

/// actor 行事务内重验：`CAST(... AS SIGNED)` 严格解码，污染值即 DB 损坏（503）。
/// 三列一次读全（`active`/`is_admin`/`must_change_password`），写操作路径不得只读其中
/// 一部分而漏掉「未改初始口令」这一 fail-closed 条件。
pub(super) async fn read_actor_in_tx(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
) -> Result<ActorRow, ControllerError> {
    let row = tx_row(
        tx,
        "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, \
         CAST(must_change_password AS SIGNED) AS must_change_password \
         FROM users WHERE id = ? FOR UPDATE",
        params![user_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(ControllerError::PermissionDenied)?;
    Ok(ActorRow {
        active: strict_bool(column::<i64>(&row, "active")?)?,
        is_admin: strict_bool(column::<i64>(&row, "is_admin")?)?,
        must_change_password: strict_bool(column::<i64>(&row, "must_change_password")?)?,
    })
}
