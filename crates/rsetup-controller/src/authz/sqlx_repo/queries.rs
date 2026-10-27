pub(crate) const ACTOR_READONLY_SQL: &str = "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ?";

pub(crate) const SNAPSHOT_META_SQL: &str = "SELECT CAST(singleton AS SIGNED) AS singleton, CAST(schema_version AS SIGNED) AS schema_version, authz_epoch FROM schema_meta LIMIT 2";

pub(crate) const LIST_ACTOR_GRANTS_SQL: &str = "SELECT g.id, g.user_id, g.source_kind, g.role_id, CAST(g.permissions AS CHAR) AS permissions, g.scope_kind, \
     g.scope_group_id, g.scope_device_id, g.revision, \
     CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, \
     CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, \
     CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists \
     FROM grants g LEFT JOIN users u ON u.id = g.user_id \
     LEFT JOIN roles r ON r.id = g.role_id \
     LEFT JOIN device_groups dg ON dg.id = g.scope_group_id \
     LEFT JOIN devices d ON d.public_key = g.scope_device_id \
     WHERE g.user_id = ? ORDER BY g.id";

pub(crate) const ROLES_BY_IDS_SQL_TEMPLATE: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, \
     CAST(archived AS SIGNED) AS archived, revision \
     FROM roles WHERE id IN ({placeholders}) ORDER BY id";

pub(crate) const ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE: &str = "SELECT role_id, permission FROM role_permissions \
     WHERE role_id IN ({placeholders}) ORDER BY role_id, permission";

pub(crate) const GROUPS_FOR_DEVICES_SQL_TEMPLATE: &str = "SELECT gm.device_id, gm.group_id, CAST(dg.archived AS SIGNED) AS group_archived, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS member_device_exists \
     FROM group_members gm LEFT JOIN device_groups dg ON dg.id = gm.group_id \
     LEFT JOIN devices d ON d.public_key = gm.device_id \
     WHERE gm.device_id IN ({placeholders}) ORDER BY gm.device_id, gm.group_id";

/// 授权快照的设备批量读（设备详情、审批与改名的授权前置都经 `load_device_snapshot_in_tx`
/// 走它）：`descriptor_json` 必须 `CAST(... AS CHAR)` 文本投影，与 `devices::repository`
/// 的两条设备读同口径——v4 `JSON` 列（wire 245）在未启用 sqlx `json` feature 的解码器上
/// 只能按文本取列，裸列名会失败关闭为去敏 503。
pub(crate) const DEVICES_BY_IDS_SQL_TEMPLATE: &str = "SELECT public_key, display_name, CAST(descriptor_json AS CHAR) AS descriptor_json, admission_state, \
     review_decision, revision, CAST(archived AS SIGNED) AS archived \
     FROM devices WHERE public_key IN ({placeholders}) ORDER BY public_key";

pub(crate) const LIST_USERS_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users WHERE id > ? ORDER BY id LIMIT ?";
pub(crate) const LIST_USERS_FIRST_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users ORDER BY id LIMIT ?";

pub(crate) const LIST_ROLES_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id > ? ORDER BY id LIMIT ?";
pub(crate) const LIST_ROLES_FIRST_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles ORDER BY id LIMIT ?";

/// 管理读 grant 查询：列 + 持久引用存在性四列（owner/role/group/device）。全部 LEFT JOIN
/// 走主键，1:1 不放大行数；引用缺失由 `validate_snapshot_grant_references` 判为固定去敏 `Config`。
pub(crate) const LIST_GRANTS_SQL: &str = "SELECT g.id, g.user_id, g.source_kind, g.role_id, CAST(g.permissions AS CHAR) AS permissions, g.scope_kind, g.scope_group_id, g.scope_device_id, g.revision, CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists FROM grants g LEFT JOIN users u ON u.id = g.user_id LEFT JOIN roles r ON r.id = g.role_id LEFT JOIN device_groups dg ON dg.id = g.scope_group_id LEFT JOIN devices d ON d.public_key = g.scope_device_id WHERE (? IS NULL OR g.user_id = ?) AND (? IS NULL OR g.id > ?) ORDER BY g.id LIMIT ?";
pub(crate) const LIST_GRANTS_FOR_USER_SQL: &str = "SELECT g.id, g.user_id, g.source_kind, g.role_id, CAST(g.permissions AS CHAR) AS permissions, g.scope_kind, g.scope_group_id, g.scope_device_id, g.revision, CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists FROM grants g LEFT JOIN users u ON u.id = g.user_id LEFT JOIN roles r ON r.id = g.role_id LEFT JOIN device_groups dg ON dg.id = g.scope_group_id LEFT JOIN devices d ON d.public_key = g.scope_device_id WHERE g.user_id = ? ORDER BY g.id";

pub(crate) const LIST_GROUPS_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id WHERE g.id > ? GROUP BY g.id ORDER BY g.id LIMIT ?";
pub(crate) const LIST_GROUPS_FIRST_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id GROUP BY g.id ORDER BY g.id LIMIT ?";

pub(crate) const FIND_USER_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users WHERE id = ?";
pub(crate) const FIND_ROLE_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id = ?";
pub(crate) const FIND_GRANT_SQL: &str = "SELECT g.id, g.user_id, g.source_kind, g.role_id, CAST(g.permissions AS CHAR) AS permissions, g.scope_kind, g.scope_group_id, g.scope_device_id, g.revision, CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists FROM grants g LEFT JOIN users u ON u.id = g.user_id LEFT JOIN roles r ON r.id = g.role_id LEFT JOIN device_groups dg ON dg.id = g.scope_group_id LEFT JOIN devices d ON d.public_key = g.scope_device_id WHERE g.id = ?";
pub(crate) const FIND_GROUP_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id WHERE g.id = ? GROUP BY g.id";

pub(crate) const LIST_GROUP_DEVICE_IDS_SQL: &str = "SELECT m.device_id FROM group_members m JOIN devices d ON d.public_key = m.device_id JOIN device_groups g ON g.id = m.group_id WHERE m.group_id = ? AND d.archived = 0 AND g.archived = 0 ORDER BY m.device_id";

pub(crate) const DEVICE_GROUPS_FOR_DEVICE_SQL: &str = "SELECT gm.group_id, CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, CAST(dg.archived AS SIGNED) AS group_archived, CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists, CAST(d.archived AS SIGNED) AS device_archived FROM group_members gm LEFT JOIN device_groups dg ON dg.id = gm.group_id LEFT JOIN devices d ON d.public_key = gm.device_id WHERE gm.device_id = ? ORDER BY gm.group_id";

/// 管理来源页的候选设备 keyset 扫描（Task 13a）：只收未归档设备，3 个绑定
/// （`cursor` 两次 + `limit + 1`）。`device_id` 筛选在调用侧短路，不进本 SQL。
pub(crate) const ADMIN_TARGET_DEVICE_KEYS_SQL: &str = "SELECT public_key FROM devices WHERE archived=FALSE AND (? IS NULL OR public_key > ?) ORDER BY public_key LIMIT ?";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::sqlx_repo::sql_shape;
    use crate::devices::repository::{APPROVAL_PAGE_SQL, AUDIT_PAGE_SQL, DEVICE_FILTER_KEYS_SQL};

    #[test]
    fn list_sql_binds_are_pinned_to_cursor_and_limit() {
        // 每页查询恰好两个绑定：(cursor, limit)。绑定数与列顺序漂移必须在离线就暴露。
        assert_eq!(LIST_USERS_SQL.matches('?').count(), 2);
        assert_eq!(LIST_ROLES_SQL.matches('?').count(), 2);
        assert_eq!(LIST_GRANTS_SQL.matches('?').count(), 5); // (? IS NULL OR g.user_id = ?) AND (? IS NULL OR g.id > ?) LIMIT ?
        assert_eq!(LIST_GRANTS_FOR_USER_SQL.matches('?').count(), 1);
        assert_eq!(FIND_GRANT_SQL.matches('?').count(), 1);
        assert_eq!(DEVICE_GROUPS_FOR_DEVICE_SQL.matches('?').count(), 1);
        // 管理读 grant 三查询必须自带 owner/role/group/device 引用存在性四列：
        // 孤儿 grant 只能在解码处失败关闭为固定去敏 Config，不得原样返回。
        for sql in [FIND_GRANT_SQL, LIST_GRANTS_SQL, LIST_GRANTS_FOR_USER_SQL] {
            for column in [
                "owner_exists",
                "role_exists",
                "group_exists",
                "device_exists",
            ] {
                assert!(
                    sql.contains(column),
                    "admin grant read SQL must pin reference column {column}: {sql}"
                );
            }
        }
        // 管理侧 member→device 查询必须同时带 group 与 device 的存在/归档四列。
        for column in [
            "group_exists",
            "group_archived",
            "device_exists",
            "device_archived",
        ] {
            assert!(
                DEVICE_GROUPS_FOR_DEVICE_SQL.contains(column),
                "device_groups_for_device SQL must pin {column}: {DEVICE_GROUPS_FOR_DEVICE_SQL}"
            );
        }

        assert_eq!(LIST_GROUPS_SQL.matches('?').count(), 2);
        assert_eq!(AUDIT_PAGE_SQL.matches('?').count(), 11); // filter 4 + cursor 6 + limit 1
        // 内部快照层：固定 SQL 绑定数与模板占位符标记漂移必须离线暴露（占位符由同事务层按去重 id 数 1:1 展开，不拼进模板字面量）。
        assert_eq!(SNAPSHOT_META_SQL.matches('?').count(), 0);
        assert_eq!(LIST_ACTOR_GRANTS_SQL.matches('?').count(), 1); // (user_id)
        assert!(ROLES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(GROUPS_FOR_DEVICES_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(DEVICES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
    }

    #[test]
    fn management_page_filter_sql_binds_are_pinned_to_seven_five_three() {
        // Task 13a 三条新 SQL 的绑定数与关键形状必须离线钉死；这只是 SQL 文本形状检查，
        // 不声称真实引擎过滤/隔离行为（真实过滤归 Task 14 受控真库用例）。
        // 设备统一扫描：cursor 2 + search 2 + group 2 + limit 1 = 7。
        assert_eq!(DEVICE_FILTER_KEYS_SQL.matches('?').count(), 7);
        for token in [
            "LOCATE",
            "group_members",
            "ORDER BY d.public_key",
            "dg.archived=FALSE",
        ] {
            assert!(
                DEVICE_FILTER_KEYS_SQL.contains(token),
                "device filter SQL must pin {token}: {DEVICE_FILTER_KEYS_SQL}"
            );
        }
        // 审批页：cursor 2 + decision 2 + limit 1 = 5。
        assert_eq!(APPROVAL_PAGE_SQL.matches('?').count(), 5);
        assert!(APPROVAL_PAGE_SQL.contains("review_decision"));
        assert!(APPROVAL_PAGE_SQL.contains("CAST(archived AS SIGNED)"));
        // 来源页 keyset：cursor 2 + limit 1 = 3。
        assert_eq!(ADMIN_TARGET_DEVICE_KEYS_SQL.matches('?').count(), 3);
        assert!(ADMIN_TARGET_DEVICE_KEYS_SQL.contains("archived=FALSE"));
    }

    /// RC1 真库根因回归 3/3：授权快照的设备批量读（`load_device_snapshot_in_tx`，
    /// 设备详情 + approve/rename 的授权前置都走它）必须把 `descriptor_json` 以 CHAR
    /// 文本投影。这是 GET /devices/{id}、审批与改名的后续入口，首 run 未触达但同根因。
    #[test]
    fn devices_by_ids_sql_template_projects_descriptor_json_as_char_text() {
        sql_shape::assert_device_descriptor_text_projection(
            "DEVICES_BY_IDS_SQL_TEMPLATE",
            DEVICES_BY_IDS_SQL_TEMPLATE,
            &[
                "public_key",
                "display_name",
                "CAST(descriptor_json AS CHAR) AS descriptor_json",
                "admission_state",
                "review_decision",
                "revision",
                "CAST(archived AS SIGNED) AS archived",
            ],
            "FROM devices WHERE public_key IN ({placeholders}) ORDER BY public_key",
            0,
        );
        // 模板占位符由调用侧按去重 id 数 1:1 展开，绑定顺序与数量随 `{placeholders}` 变化，
        // 但模板自身不得出现 `?`；展开后每个 id 恰好一个绑定。
        assert!(DEVICES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        let expanded = DEVICES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", "?, ?, ?");
        assert_eq!(expanded.matches('?').count(), 3);
    }
}
