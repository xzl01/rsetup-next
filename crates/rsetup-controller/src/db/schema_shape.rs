use crate::ControllerError;

pub(super) const STATISTICS_QUERY: &str = "SELECT CAST(index_name AS CHAR) AS index_name, CAST(column_name AS CHAR) AS column_name, CAST(non_unique AS SIGNED) AS non_unique, CAST(seq_in_index AS SIGNED) AS seq_in_index, CAST(sub_part AS SIGNED) AS sub_part FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? ORDER BY index_name, seq_in_index";
pub(super) const FK_CONSTRAINTS_QUERY: &str = "SELECT CAST(table_name AS CHAR) AS table_name, CAST(constraint_name AS CHAR) AS constraint_name FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'FOREIGN KEY'";
pub(super) const FK_REFERENCES_QUERY: &str = "SELECT CAST(COUNT(*) AS SIGNED) AS fk_count FROM information_schema.referential_constraints WHERE constraint_schema = DATABASE()";
pub(super) const FK_COLUMNS_QUERY: &str = "SELECT CAST(COUNT(*) AS SIGNED) AS fk_count FROM information_schema.key_column_usage WHERE table_schema = DATABASE() AND referenced_table_schema IS NOT NULL";

pub(super) const TABLES: &[(&str, &[&str], &[&str])] = &[
    (
        "schema_meta",
        &[
            "singleton",
            "schema_version",
            "instance_id",
            "initialized",
            "authz_epoch",
            "admin_guard_revision",
        ],
        &["PRIMARY"],
    ),
    (
        "users",
        &[
            "id",
            "username",
            "display_name",
            "password_hash",
            "active",
            "is_admin",
            "must_change_password",
            "revision",
            "created_time",
        ],
        &["PRIMARY", "uq_users_username"],
    ),
    (
        "sessions",
        &[
            "token_hash",
            "user_id",
            "process_epoch",
            "created_time",
            "revoked",
        ],
        &["PRIMARY", "ix_sessions_user_id"],
    ),
    (
        "roles",
        &["id", "name", "builtin", "archived", "revision"],
        &["PRIMARY", "uq_roles_name"],
    ),
    ("role_permissions", &["role_id", "permission"], &["PRIMARY"]),
    (
        "device_groups",
        &["id", "name", "archived", "revision"],
        &["PRIMARY", "uq_device_groups_name"],
    ),
    (
        "devices",
        &[
            "public_key",
            "display_name",
            "descriptor_json",
            "admission_state",
            "review_decision",
            "revision",
            "first_seen",
            "last_seen",
            "archived",
        ],
        &["PRIMARY", "ix_devices_admission"],
    ),
    (
        "group_members",
        &["group_id", "device_id"],
        &["PRIMARY", "ix_group_members_device"],
    ),
    (
        "grants",
        &[
            "id",
            "user_id",
            "source_kind",
            "role_id",
            "permissions",
            "scope_kind",
            "scope_group_id",
            "scope_device_id",
            "revision",
        ],
        &["PRIMARY", "ix_grants_user_id"],
    ),
    (
        "admission_decisions",
        &[
            "id",
            "device_id",
            "actor_id",
            "decision",
            "previous_revision",
            "new_revision",
            "reason",
            "time_evidence",
        ],
        &["PRIMARY", "ix_admission_decisions_device_id"],
    ),
    (
        "audit_events",
        &[
            "id",
            "actor_kind",
            "actor_user_id",
            "event_type",
            "target_kind",
            "target_id",
            "params_redacted",
            "outcome",
            "time_evidence",
            "process_epoch",
            "event_seq",
        ],
        &[
            "PRIMARY",
            "uq_audit_epoch_seq",
            "ix_audit_actor",
            "ix_audit_target",
        ],
    ),
];

/// Schema v4 runtime 五表的形状契约（列名与索引名），与 `schema/v4.sql` 一一对应。
/// 它与 [`TABLES`] 的 11 张 identity 表共同构成 v4 的 16 表完整形状校验。
pub(super) const RUNTIME_TABLES: &[(&str, &[&str], &[&str])] = &[
    (
        "task_previews",
        &[
            "token_nonce",
            "actor_user_id",
            "target_hash",
            "targets_json",
            "preview_token_hash",
            "preview_generation",
            "created_at",
            "expires_at",
            "consumed_at",
        ],
        &[
            "PRIMARY",
            "ix_task_previews_actor",
            "uq_task_previews_token_hash",
            "ix_task_previews_expires",
        ],
    ),
    (
        "main_tasks",
        &[
            "id",
            "actor_user_id",
            "idempotency_key",
            "request_hash",
            "target_hash",
            "preview_token_hash",
            "state",
            "outcome",
            "revision",
            "counts_json",
            "created_time_json",
            "created_at",
            "updated_at",
        ],
        &[
            "PRIMARY",
            "uq_main_tasks_actor_idem",
            "ix_main_tasks_actor_created",
            "ix_main_tasks_state",
        ],
    ),
    (
        "sub_tasks",
        &[
            "id",
            "main_task_id",
            "device_id",
            "state",
            "reason_code",
            "revision",
            "lock_generation",
            "pre_boot_id",
            "controller_epoch",
            "session_epoch",
            "stream_epoch",
            "ticket_digest",
            "request_digest",
            "queue_deadline",
            "observe_deadline",
            "evidence_json",
            "created_at",
            "updated_at",
        ],
        &[
            "PRIMARY",
            "uq_sub_tasks_main_device",
            "ix_sub_tasks_main",
            "ix_sub_tasks_device_state",
        ],
    ),
    (
        "device_operation_locks",
        &[
            "device_id",
            "main_task_id",
            "owner_sub_task_id",
            "generation",
            "acquired_at",
            "released_at",
        ],
        &["PRIMARY", "uq_device_locks_owner", "ix_device_locks_main"],
    ),
    (
        "recovery_checks",
        &[
            "id",
            "process_epoch",
            "subtask_id",
            "outcome",
            "detail_redacted",
            "checked_at",
        ],
        &[
            "PRIMARY",
            "ix_recovery_checks_subtask",
            "uq_recovery_checks_epoch_subtask",
        ],
    ),
];

#[derive(Clone, Debug)]
pub(super) struct ColumnMeta {
    pub(super) name: String,
    pub(super) data_type: String,
    pub(super) column_type: String,
    pub(super) nullable: bool,
    pub(super) character_set_name: Option<String>,
    pub(super) collation_name: Option<String>,
    pub(super) default: Option<String>,
}

pub(super) fn validate_column_shape(
    table: &str,
    expected: &str,
    actual: &ColumnMeta,
) -> Result<(), ControllerError> {
    let tokens: Vec<_> = expected.split_whitespace().collect();
    let declared = tokens[0].to_ascii_lowercase();
    let declared_type = match declared.split('(').next().unwrap() {
        "boolean" => "tinyint",
        other => other,
    };
    let actual_type = actual.data_type.to_ascii_lowercase();
    let declared_len = declared
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'));
    let actual_len = actual
        .column_type
        .to_ascii_lowercase()
        .split_once('(')
        .and_then(|(_, rest)| rest.split(')').next())
        .map(str::to_owned);
    let length_sensitive = matches!(
        declared_type,
        "binary" | "varbinary" | "varchar" | "char" | "datetime"
    );
    let optional = tokens.windows(2).any(|part| part == ["NOT", "NULL"]);
    let declared_unsigned = tokens
        .iter()
        .any(|token| token.eq_ignore_ascii_case("UNSIGNED"));
    let actual_unsigned = actual
        .column_type
        .split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("unsigned"));
    let declared_charset = tokens
        .windows(3)
        .find(|part| {
            part[0].eq_ignore_ascii_case("CHARACTER") && part[1].eq_ignore_ascii_case("SET")
        })
        .map(|part| part[2]);
    let declared_collation = tokens
        .windows(2)
        .find(|part| part[0].eq_ignore_ascii_case("COLLATE"))
        .map(|part| part[1]);
    if actual_type != declared_type
        || declared_unsigned != actual_unsigned
        || (length_sensitive && declared_len != actual_len.as_deref())
        || actual.nullable == optional
        || declared_charset.is_some_and(|expected| {
            !actual
                .character_set_name
                .as_deref()
                .is_some_and(|found| found.eq_ignore_ascii_case(expected))
        })
        || declared_collation.is_some_and(|expected| {
            !actual
                .collation_name
                .as_deref()
                .is_some_and(|found| found.eq_ignore_ascii_case(expected))
        })
    {
        return Err(ControllerError::Config(format!(
            "migration incompatible column {table}.{}: expected {expected}, got {} {} nullable={}",
            actual.name, actual.data_type, actual.column_type, actual.nullable
        )));
    }
    validate_column_default(table, &actual.name, &tokens, actual.default.as_deref())?;
    Ok(())
}

fn validate_column_default(
    table: &str,
    column: &str,
    declaration: &[&str],
    default: Option<&str>,
) -> Result<(), ControllerError> {
    let declared = declaration
        .windows(2)
        .find(|part| part[0].eq_ignore_ascii_case("DEFAULT"))
        .map(|part| part[1]);
    let expected = match declared {
        Some(value) if value.eq_ignore_ascii_case("FALSE") || value == "0" => Some("0"),
        None if !declaration
            .iter()
            .any(|token| token.eq_ignore_ascii_case("DEFAULT")) =>
        {
            None
        }
        _ => {
            return Err(ControllerError::Config(format!(
                "migration unsupported default declaration {table}.{column}"
            )));
        }
    };
    if expected == Some("0") {
        return super::probe::validate_false_default_metadata(table, column, default);
    }
    if default.is_some() {
        return Err(ControllerError::Config(format!(
            "migration incompatible default {table}.{column}"
        )));
    }
    Ok(())
}

pub(super) fn validate_object_names(
    table: &str,
    kind: &str,
    expected: &[String],
    actual: &[String],
) -> Result<(), ControllerError> {
    if expected.len() != actual.len() || expected.iter().any(|name| !actual.contains(name)) {
        return Err(ControllerError::Config(format!(
            "migration incompatible {kind} names in {table}"
        )));
    }
    Ok(())
}

/// 表集合判定纯函数：v4 的 11 张 identity 表 + 5 张 runtime 表必须**恰好**齐备，
/// 不得重复、不得缺表，未知表一律拒绝（fail-closed，绝不"顺手"忽略多余表）。
pub(super) fn validate_table_set(present: &[String]) -> Result<(), ControllerError> {
    let table_set_error = || {
        ControllerError::Config(
            "v4 schema has missing or unexpected tables; manual inspection required".into(),
        )
    };
    let mut seen = std::collections::HashSet::new();
    for name in present {
        let known = TABLES.iter().any(|(table, _, _)| name == table)
            || RUNTIME_TABLES.iter().any(|(table, _, _)| name == table);
        if !known || !seen.insert(name.as_str()) {
            return Err(table_set_error());
        }
    }
    if TABLES
        .iter()
        .chain(RUNTIME_TABLES)
        .any(|(table, _, _)| !seen.contains(table))
    {
        return Err(table_set_error());
    }
    Ok(())
}

// SQL/decoding errors and NULLs must never be interpreted as an empty set.
pub(super) type FkConstraintsRead = Result<Option<(Option<String>, Option<String>)>, ()>;

pub(super) fn validate_fk_metadata(
    constraints: FkConstraintsRead,
    references: Result<Option<i64>, ()>,
    columns: Result<Option<i64>, ()>,
) -> Result<(), ControllerError> {
    if !matches!(constraints, Ok(None))
        || !matches!(references, Ok(Some(0)))
        || !matches!(columns, Ok(Some(0)))
    {
        return Err(ControllerError::Config(
            "identity foreign key metadata unavailable or nonempty".into(),
        ));
    }
    Ok(())
}

/// v4 表级 collation 门禁：字符表必须继承 utf8mb4 系列 collation。
pub(super) fn validate_table_collation(
    table: &str,
    table_collation: &str,
) -> Result<(), ControllerError> {
    if table_collation.to_ascii_lowercase().starts_with("utf8mb4_") {
        Ok(())
    } else {
        Err(ControllerError::Config(format!(
            "v4 schema incompatible table charset {table}"
        )))
    }
}

/// v4 字符列门禁：列若自身声明了字符集（如 `users.username` 的
/// `CHARACTER SET ascii COLLATE ascii_bin`），其精确字符集/collation 已由
/// [`validate_column_shape`] 比对；其余字符列必须继承表的 utf8mb4 collation。
pub(super) fn validate_column_charset(
    table: &str,
    declaration: &str,
    actual: &ColumnMeta,
    table_collation: &str,
) -> Result<(), ControllerError> {
    let declared_type = declaration
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let character_column = matches!(
        declared_type.split('(').next().unwrap_or_default(),
        "varchar" | "char" | "text" | "tinytext" | "mediumtext" | "longtext"
    ) || declared_type.starts_with("enum")
        || declared_type.starts_with("set");
    if !character_column || declaration.contains("CHARACTER SET ") {
        return Ok(());
    }
    if actual.character_set_name.as_deref() != Some("utf8mb4")
        || !actual
            .collation_name
            .as_deref()
            .is_some_and(|found| found.eq_ignore_ascii_case(table_collation))
    {
        return Err(ControllerError::Config(format!(
            "v4 schema incompatible character metadata {table}.{}",
            actual.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::schema_parse::{IndexMeta, ddl_parts, expected_indexes, validate_indexes};
    use super::*;

    #[test]
    fn table_set_requires_exactly_the_sixteen_v4_tables() {
        let sixteen: Vec<String> = TABLES
            .iter()
            .chain(RUNTIME_TABLES)
            .map(|(table, ..)| (*table).to_owned())
            .collect();
        assert!(validate_table_set(&sixteen).is_ok());
        // 旧 identity-only 形状（11 表）不再可接受。
        let identity_only: Vec<String> = sixteen[..TABLES.len()].to_vec();
        assert!(
            validate_table_set(&identity_only).is_err(),
            "缺 runtime 五表的旧形状必须拒绝"
        );
        let mut missing_runtime = sixteen.clone();
        missing_runtime.retain(|name| name != "recovery_checks");
        assert!(
            validate_table_set(&missing_runtime).is_err(),
            "缺任意一张 runtime 表必须拒绝"
        );
        let mut unknown = sixteen.clone();
        unknown.push("legacy_users".into());
        assert!(validate_table_set(&unknown).is_err(), "未知表必须拒绝");
        let mut duplicated = sixteen.clone();
        duplicated.push("users".into());
        assert!(validate_table_set(&duplicated).is_err(), "重复表必须拒绝");
    }

    #[test]
    fn fk_metadata_requires_three_independent_trustworthy_empty_views() {
        // Breaking the final metadata decision to accept any nonzero count or
        // unknown result must turn one of these cases red.
        let empty = Ok(None);
        let zero = Ok(Some(0));
        assert!(validate_fk_metadata(empty.clone(), zero, zero).is_ok());
        for first in [
            Ok(Some((Some("users".into()), Some("fk_named".into())))),
            Ok(Some((
                Some("other_table".into()),
                Some("other_table_ibfk_1".into()),
            ))),
            Ok(Some((None, Some("fk_unknown".into())))),
            Ok(Some((Some("users".into()), None))),
            Ok(Some((Some(String::new()), Some("fk_invalid".into())))),
            Err(()),
        ] {
            assert!(validate_fk_metadata(first, zero, zero).is_err());
        }
        for invalid in [Ok(Some(1)), Ok(Some(2)), Ok(Some(-1)), Ok(None), Err(())] {
            assert!(validate_fk_metadata(empty.clone(), invalid, zero).is_err());
            assert!(validate_fk_metadata(empty.clone(), zero, invalid).is_err());
        }
        // Referential constraints count FKs, key column usage counts FK columns;
        // equality of positive counts is neither required nor sufficient.
        assert!(validate_fk_metadata(empty, Ok(Some(1)), Ok(Some(2))).is_err());
    }

    #[test]
    fn fk_metadata_errors_never_echo_names_or_query_details() {
        let cases = [
            validate_fk_metadata(
                Ok(Some((
                    Some("user_supplied_secret".into()),
                    Some("fk_secret".into()),
                ))),
                Ok(Some(0)),
                Ok(Some(0)),
            ),
            validate_fk_metadata(Err(()), Ok(Some(0)), Ok(Some(0))),
            validate_fk_metadata(Ok(None), Err(()), Ok(Some(0))),
            validate_fk_metadata(Ok(None), Ok(Some(0)), Err(())),
        ];
        for case in cases {
            assert!(
                matches!(case, Err(ControllerError::Config(ref reason)) if reason == "identity foreign key metadata unavailable or nonempty")
            );
        }
    }

    #[test]
    fn closed_identity_metadata_rejects_extra_or_missing_objects() {
        for kind in ["column", "check"] {
            let expected = vec!["known".to_owned()];
            assert!(validate_object_names("users", kind, &expected, &expected).is_ok());
            assert!(
                validate_object_names(
                    "users",
                    kind,
                    &expected,
                    &["known".into(), "unknown".into()]
                )
                .is_err(),
                "{kind}"
            );
            assert!(
                validate_object_names("users", kind, &expected, &[]).is_err(),
                "{kind}"
            );
        }
        let expected =
            expected_indexes(&ddl_parts("users"), &["PRIMARY", "uq_users_username"]).unwrap();
        let mut extra = expected.clone();
        extra.push(IndexMeta {
            name: "ix_extra".into(),
            columns: vec!["username".into()],
            unique: false,
            sub_parts: vec![None],
        });
        assert!(validate_indexes("users", &expected, &extra).is_err());
        assert!(validate_indexes("users", &expected, &expected[..1]).is_err());
        let mut prefix = expected.clone();
        prefix[1].sub_parts = vec![Some(3)];
        assert!(validate_indexes("users", &expected, &prefix).is_err());
    }

    #[test]
    fn inline_primary_key_uses_column_name_not_type_width() {
        let parts = ddl_parts("devices");
        let primary = expected_indexes(&parts, &["PRIMARY"]).unwrap();
        assert_eq!(primary[0].columns, vec!["public_key"]);
        assert!(primary[0].unique);
    }

    #[test]
    fn same_width_signed_counter_is_incompatible() {
        let signed = ColumnMeta {
            name: "event_seq".into(),
            data_type: "bigint".into(),
            column_type: "bigint".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        let unsigned = ColumnMeta {
            column_type: "bigint unsigned".into(),
            ..signed.clone()
        };
        assert!(
            validate_column_shape("audit_events", "BIGINT UNSIGNED NOT NULL", &unsigned).is_ok()
        );
        assert!(
            validate_column_shape("audit_events", "BIGINT UNSIGNED NOT NULL", &signed).is_err()
        );
    }

    #[test]
    fn migration_checks_all_columns_including_audit_evidence() {
        for (table, required) in [
            ("sessions", "created_time"),
            ("roles", "revision"),
            ("devices", "first_seen"),
            ("grants", "revision"),
            ("admission_decisions", "time_evidence"),
            ("audit_events", "event_type"),
            ("audit_events", "time_evidence"),
        ] {
            assert!(
                TABLES
                    .iter()
                    .find(|entry| entry.0 == table)
                    .unwrap()
                    .1
                    .contains(&required),
                "unchecked {table}.{required}"
            );
        }
    }
}
