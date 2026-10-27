//! 授权仓储 SQLx 实现薄根（mod.rs）。
//!
//! 承载 `crates/rsetup-controller/src/authz/sqlx_repo` 模块树声明与对内外符号的全部再导出，
//! 无任何实现体。

mod authz_repo;
mod codec;
mod model;
mod queries;
mod snapshot_codec;
mod snapshot_read;
mod snapshot_repo;
mod traits;
mod tx_guard;

/// 测试专用 fixture：生产 SQL 常量的文本形状门禁。只解析字符串，不连引擎、不伪造
/// `MySqlRow`，因此不声称任何真实引擎行为（真库端到端由受控 executor 复跑）。
#[cfg(test)]
pub(crate) mod sql_shape;

// 1. authz_repo 模块再导出
pub use self::authz_repo::SqlxAuthzRepository;

// 2. model 模块再导出 (pub 5项: 7 个公开再导出名中的 RepoFuture, UserRecord + GroupRecord, GrantColumns, AuthorizationSnapshot)
pub use self::model::{AuthorizationSnapshot, GrantColumns, GroupRecord, RepoFuture, UserRecord};

// 3. traits 模块再导出 (pub 2项: AuthzRepository, AuthorizationSnapshotRepository)
pub use self::traits::{AuthorizationSnapshotRepository, AuthzRepository};

// 4. snapshot_repo 模块再导出 (pub 1项: SqlxAuthorizationSnapshotRepository)
pub use self::snapshot_repo::SqlxAuthorizationSnapshotRepository;

// 5. codec 模块再导出
#[allow(unused_imports)]
pub(crate) use self::codec::{
    admin_change_removes_admin_state, ordered_user_ids, read_error, validate_group_name,
};
pub use self::codec::{
    decode_grant, decode_user, grant_columns, page_cursor, validate_display_name,
};

// 6. snapshot_codec 模块再导出
pub use self::snapshot_codec::{
    build_authorization_snapshot, decode_device_admin_view, decode_group_memberships_for_device,
    decode_snapshot_actor, decode_snapshot_group_rows, decode_snapshot_meta, decode_snapshot_roles,
    require_snapshot_isolation, validate_snapshot_grant_references,
};

// 7. queries 模块再导出
#[allow(unused_imports)]
pub(crate) use self::queries::{
    ACTOR_READONLY_SQL, ADMIN_TARGET_DEVICE_KEYS_SQL, DEVICE_GROUPS_FOR_DEVICE_SQL,
    DEVICES_BY_IDS_SQL_TEMPLATE, FIND_GRANT_SQL, FIND_GROUP_SQL, FIND_ROLE_SQL, FIND_USER_SQL,
    GROUPS_FOR_DEVICES_SQL_TEMPLATE, LIST_ACTOR_GRANTS_SQL, LIST_GRANTS_FOR_USER_SQL,
    LIST_GRANTS_SQL, LIST_GROUP_DEVICE_IDS_SQL, LIST_GROUPS_FIRST_SQL, LIST_GROUPS_SQL,
    LIST_ROLES_FIRST_SQL, LIST_ROLES_SQL, LIST_USERS_FIRST_SQL, LIST_USERS_SQL,
    ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE, ROLES_BY_IDS_SQL_TEMPLATE, SNAPSHOT_META_SQL,
};

// 8. snapshot_read 模块再导出
#[allow(unused_imports)]
pub(crate) use self::snapshot_read::{
    ActorAuthorization, acquire_authorization_read_connection, ensure_actor_admin_readonly,
    load_actor_authorization_in_tx, load_admin_target_snapshot_in_tx, load_device_snapshot_in_tx,
    load_snapshot_in_tx, verify_management_read,
};

// 9. tx_guard 模块再导出
pub(crate) use self::tx_guard::bump_authz_epoch;
