use crate::{AdmissionSnapshot, AdmissionState, ControllerConfig, ControllerError, ReviewDecision};
use sqlx::Row;

#[derive(Clone)]
pub struct DbPool(pub sqlx::MySqlPool);
impl DbPool {
    pub async fn connect(config: &ControllerConfig) -> Result<Self, ControllerError> {
        Ok(Self(sqlx::MySqlPool::connect(&config.database_url).await?))
    }
}

const MIGRATION: &str = include_str!("../migrations/0001_identity_devices.sql");
const TABLES: &[(&str, &[&str], &[&str])] = &[
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

#[derive(Clone, Debug)]
struct ColumnMeta {
    name: String,
    data_type: String,
    column_type: String,
    nullable: bool,
}

fn validate_column_shape(
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
    if actual_type != declared_type
        || (length_sensitive && declared_len != actual_len.as_deref())
        || actual.nullable == optional
    {
        return Err(ControllerError::Config(format!(
            "migration incompatible column {table}.{}: expected {expected}, got {} {} nullable={}",
            actual.name, actual.data_type, actual.column_type, actual.nullable
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexMeta {
    name: String,
    columns: Vec<String>,
    unique: bool,
}

fn validate_indexes(
    table: &str,
    expected: &[IndexMeta],
    actual: &[IndexMeta],
) -> Result<(), ControllerError> {
    for index in expected {
        if !actual.iter().any(|found| found == index) {
            return Err(ControllerError::Config(format!(
                "migration incompatible index {table}.{}",
                index.name
            )));
        }
    }
    Ok(())
}

fn ddl_parts(table: &str) -> Vec<String> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let ddl = MIGRATION
        .split(';')
        .find(|statement| statement.trim().starts_with(&prefix))
        .expect("table declared in migration")
        .trim();
    let body = ddl
        .strip_prefix(&prefix)
        .unwrap()
        .rsplit_once(") CHARACTER SET")
        .unwrap()
        .0;
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut start = 0;
    for (index, c) in body.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(body[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].trim().to_owned());
    parts
}

fn normalize_check(value: &str) -> String {
    let mut text = value
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '`')
        .flat_map(char::to_lowercase)
        .collect::<String>();
    while text.starts_with('(') && text.ends_with(')') {
        let mut depth = 0;
        let fully_wrapped = text.char_indices().all(|(i, c)| {
            if c == '(' {
                depth += 1;
            }
            if c == ')' {
                depth -= 1;
            }
            depth > 0 || i == text.len() - 1
        });
        if !fully_wrapped {
            break;
        }
        text = text[1..text.len() - 1].to_owned();
    }
    text
}

fn expected_indexes(parts: &[String], names: &[&str]) -> Result<Vec<IndexMeta>, ControllerError> {
    let mut expected = Vec::new();
    for &name in names {
        let definition = parts
            .iter()
            .find(|part| {
                if name == "PRIMARY" {
                    part.contains("PRIMARY KEY")
                } else {
                    part.contains(&format!(" {name} ("))
                }
            })
            .ok_or_else(|| {
                ControllerError::Config(format!("migration DDL missing index {name}"))
            })?;
        let columns = if name == "PRIMARY" && !definition.starts_with("PRIMARY KEY (") {
            vec![definition.split_whitespace().next().unwrap().to_owned()]
        } else {
            definition
                .split_once('(')
                .unwrap()
                .1
                .split_once(')')
                .unwrap()
                .0
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect()
        };
        expected.push(IndexMeta {
            name: name.to_owned(),
            columns,
            unique: name == "PRIMARY" || definition.contains("UNIQUE KEY"),
        });
    }
    Ok(expected)
}

pub async fn migrate(db: &DbPool) -> Result<(), ControllerError> {
    // Each DDL is independently durable on MySQL and TiDB; an interrupted run resumes safely.
    for statement in MIGRATION
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        sqlx::query(statement).execute(&db.0).await?;
    }
    for &(table, columns, indexes) in TABLES {
        let parts = ddl_parts(table);
        for &column in columns {
            let declaration = parts
                .iter()
                .find_map(|part| part.strip_prefix(&format!("{column} ")))
                .ok_or_else(|| {
                    ControllerError::Config(format!(
                        "migration DDL missing column {table}.{column}"
                    ))
                })?;
            let row = sqlx::query("SELECT data_type, column_type, is_nullable FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
                .bind(table).bind(column).fetch_optional(&db.0).await?
                .ok_or_else(|| ControllerError::Config(format!("migration missing column {table}.{column}")))?;
            validate_column_shape(
                table,
                declaration,
                &ColumnMeta {
                    name: column.to_owned(),
                    data_type: row.try_get("data_type")?,
                    column_type: row.try_get("column_type")?,
                    nullable: row.try_get::<String, _>("is_nullable")? == "YES",
                },
            )?;
        }
        let expected = expected_indexes(&parts, indexes)?;
        let rows = sqlx::query("SELECT index_name, column_name, non_unique, seq_in_index FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? ORDER BY index_name, seq_in_index")
            .bind(table).fetch_all(&db.0).await?;
        let mut actual: Vec<IndexMeta> = Vec::new();
        for row in rows {
            let name: String = row.try_get("index_name")?;
            let column: String = row.try_get("column_name")?;
            let non_unique: i64 = row.try_get("non_unique")?;
            if let Some(index) = actual.iter_mut().find(|entry| entry.name == name) {
                index.columns.push(column);
            } else {
                actual.push(IndexMeta {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                });
            }
        }
        validate_indexes(table, &expected, &actual)?;
        for part in parts.iter().filter(|part| part.starts_with("CONSTRAINT ")) {
            let name = part.split_whitespace().nth(1).unwrap();
            let stored: Option<String> = sqlx::query_scalar("SELECT cc.check_clause FROM information_schema.table_constraints tc JOIN information_schema.check_constraints cc ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name WHERE tc.table_schema = DATABASE() AND tc.table_name = ? AND tc.constraint_name = ? AND tc.constraint_type = 'CHECK'")
                .bind(table).bind(name).fetch_optional(&db.0).await?;
            let declared = part.split_once("CHECK ").unwrap().1;
            if !stored
                .as_deref()
                .is_some_and(|value| normalize_check(value) == normalize_check(declared))
            {
                return Err(ControllerError::Config(format!(
                    "migration incompatible check constraint {table}.{name}"
                )));
            }
        }
    }
    sqlx::query("INSERT IGNORE INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, 1, ?, FALSE, 0, 0)")
        .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).execute(&db.0).await?;
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await?;
    if version != 1 {
        return Err(ControllerError::Config(format!(
            "unsupported schema version {version}"
        )));
    }
    Ok(())
}

pub trait AdmissionStore {
    fn load(
        &self,
        public_key: [u8; 32],
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
    fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: i64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
}

fn decode_snapshot(
    state: &str,
    decision: &str,
    revision: i64,
) -> Result<AdmissionSnapshot, ControllerError> {
    let admission_state = match state {
        "PENDING" => AdmissionState::Pending,
        "APPROVED" => AdmissionState::Approved,
        "REVOKED" => AdmissionState::Revoked,
        _ => {
            return Err(ControllerError::Config(
                "invalid stored admission state".into(),
            ));
        }
    };
    let review_decision = match decision {
        "none" => ReviewDecision::None,
        "approved" => ReviewDecision::Approved,
        "denied" => ReviewDecision::Denied,
        "revoked" => ReviewDecision::Revoked,
        _ => {
            return Err(ControllerError::Config(
                "invalid stored review decision".into(),
            ));
        }
    };
    Ok(AdmissionSnapshot {
        admission_state,
        review_decision,
        revision,
    })
}

fn next_snapshot(
    snapshot: AdmissionSnapshot,
    decision: ReviewDecision,
) -> Result<AdmissionSnapshot, ControllerError> {
    use AdmissionState::{Approved, Pending, Revoked};
    use ReviewDecision::{Approved as Yes, Denied, None, Revoked as No};
    let admission_state = match (snapshot.admission_state, snapshot.review_decision, decision) {
        (Pending, None, Yes) | (Revoked, No, Yes) => Approved,
        (Pending, None, Denied) => Pending,
        (Pending, Denied, None) => Pending,
        (Approved, Yes, No) => Revoked,
        _ => return Err(ControllerError::RevisionConflict),
    };
    Ok(AdmissionSnapshot {
        admission_state,
        review_decision: decision,
        revision: snapshot
            .revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?,
    })
}

fn system_fallback_time_evidence() -> String {
    static CLOCK_EPOCH: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();
    let wall = chrono::Utc::now().to_rfc3339();
    serde_json::json!({
        "system_wall_utc": wall,
        "reference_utc": wall,
        "quality": "system_fallback",
        "clock_epoch": CLOCK_EPOCH.get_or_init(uuid::Uuid::new_v4).to_string(),
        "source": null,
        "sample_age_ms": null,
        "offset_ms": null,
        "uncertainty_ms": null
    })
    .to_string()
}

impl AdmissionStore for DbPool {
    async fn load(&self, public_key: [u8; 32]) -> Result<AdmissionSnapshot, ControllerError> {
        let row = sqlx::query(
            "SELECT admission_state, review_decision, revision FROM devices WHERE public_key = ?",
        )
        .bind(public_key.as_slice())
        .fetch_optional(&self.0)
        .await?
        .ok_or(ControllerError::NotFound)?;
        decode_snapshot(
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get("revision")?,
        )
    }

    async fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: i64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> Result<AdmissionSnapshot, ControllerError> {
        use std::sync::{
            OnceLock,
            atomic::{AtomicI64, Ordering},
        };
        static PROCESS_EPOCH: OnceLock<uuid::Uuid> = OnceLock::new();
        static EVENT_SEQ: AtomicI64 = AtomicI64::new(0);
        let mut tx = self.0.begin().await?;
        let row = sqlx::query("SELECT admission_state, review_decision, revision FROM devices WHERE public_key = ? FOR UPDATE")
            .bind(public_key.as_slice()).fetch_optional(&mut *tx).await?.ok_or(ControllerError::NotFound)?;
        let current = decode_snapshot(
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get("revision")?,
        )?;
        if current.revision != expected_revision || current.admission_state != expected_state {
            return Err(ControllerError::RevisionConflict);
        }
        let next = next_snapshot(current, decision)?;
        let state = match next.admission_state {
            AdmissionState::Pending => "PENDING",
            AdmissionState::Approved => "APPROVED",
            AdmissionState::Revoked => "REVOKED",
        };
        let decision_value = match next.review_decision {
            ReviewDecision::None => "none",
            ReviewDecision::Approved => "approved",
            ReviewDecision::Denied => "denied",
            ReviewDecision::Revoked => "revoked",
        };
        let changed = sqlx::query("UPDATE devices SET admission_state = ?, review_decision = ?, revision = ? WHERE public_key = ? AND revision = ? AND admission_state = ? AND review_decision = ?")
            .bind(state).bind(decision_value).bind(next.revision).bind(public_key.as_slice()).bind(expected_revision)
            .bind(match current.admission_state { AdmissionState::Pending => "PENDING", AdmissionState::Approved => "APPROVED", AdmissionState::Revoked => "REVOKED" })
            .bind(match current.review_decision { ReviewDecision::None => "none", ReviewDecision::Approved => "approved", ReviewDecision::Denied => "denied", ReviewDecision::Revoked => "revoked" })
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        let evidence = system_fallback_time_evidence();
        sqlx::query("INSERT INTO admission_decisions (id, device_id, actor_id, decision, previous_revision, new_revision, reason, time_evidence) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).bind(public_key.as_slice())
            .bind(actor_id.as_ref().map(|id| id.as_slice())).bind(decision_value)
            .bind(current.revision).bind(next.revision).bind(reason).bind(&evidence)
            .execute(&mut *tx).await?;
        let epoch = PROCESS_EPOCH.get_or_init(uuid::Uuid::new_v4);
        let seq = EVENT_SEQ.fetch_add(1, Ordering::Relaxed);
        sqlx::query("INSERT INTO audit_events (id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome, time_evidence, process_epoch, event_seq) VALUES (?, ?, ?, ?, 'device', ?, '{}', 'success', ?, ?, ?)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice())
            .bind(if actor_id.is_some() { "user" } else { "system" })
            .bind(actor_id.as_ref().map(|id| id.as_slice())).bind(audit_code(decision, current.admission_state))
            .bind(hex::encode(public_key)).bind(&evidence).bind(epoch.as_bytes().as_slice()).bind(seq)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(next)
    }
}

fn audit_code(decision: ReviewDecision, previous: AdmissionState) -> &'static str {
    match (decision, previous) {
        (ReviewDecision::Approved, AdmissionState::Revoked) => "admission.reauthorize",
        (ReviewDecision::Approved, _) => "admission.approve",
        (ReviewDecision::Denied, _) => "admission.reject",
        (ReviewDecision::None, _) => "admission.reopen",
        (ReviewDecision::Revoked, _) => "admission.revoke",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_declares_all_identity_and_admission_tables() {
        for &(table, ..) in TABLES {
            assert!(
                MIGRATION.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
                "missing {table}"
            );
        }
    }

    #[test]
    fn fallback_evidence_has_stable_reference_and_unknown_fields() {
        let a: serde_json::Value = serde_json::from_str(&system_fallback_time_evidence()).unwrap();
        let b: serde_json::Value = serde_json::from_str(&system_fallback_time_evidence()).unwrap();
        assert_eq!(a["quality"], "system_fallback");
        let wall = a["system_wall_utc"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(wall).unwrap();
        assert_eq!(a["reference_utc"], wall);
        let epoch = uuid::Uuid::parse_str(a["clock_epoch"].as_str().unwrap()).unwrap();
        assert_eq!(epoch.get_version_num(), 4);
        assert_eq!(a["clock_epoch"], b["clock_epoch"]);
        for key in ["source", "sample_age_ms", "offset_ms", "uncertainty_ms"] {
            assert!(a.get(key).unwrap().is_null(), "{key} must be unknown");
        }
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

    #[test]
    fn incompatible_binary_length_type_and_nullability_are_rejected() {
        let base = ColumnMeta {
            name: "public_key".into(),
            data_type: "binary".into(),
            column_type: "binary(32)".into(),
            nullable: false,
        };
        assert!(validate_column_shape("devices", "BINARY(32) NOT NULL", &base).is_ok());
        for altered in [
            ColumnMeta {
                column_type: "binary(16)".into(),
                ..base.clone()
            },
            ColumnMeta {
                data_type: "varbinary".into(),
                column_type: "varbinary(32)".into(),
                ..base.clone()
            },
            ColumnMeta {
                nullable: true,
                ..base.clone()
            },
        ] {
            assert!(validate_column_shape("devices", "BINARY(32) NOT NULL", &altered).is_err());
        }
    }

    #[test]
    fn wrong_unique_index_or_column_order_is_rejected() {
        let expected = [IndexMeta {
            name: "uq_audit_epoch_seq".into(),
            columns: vec!["process_epoch".into(), "event_seq".into()],
            unique: true,
        }];
        for broken in [
            IndexMeta {
                unique: false,
                ..expected[0].clone()
            },
            IndexMeta {
                columns: vec!["event_seq".into(), "process_epoch".into()],
                ..expected[0].clone()
            },
        ] {
            assert!(validate_indexes("audit_events", &expected, &[broken]).is_err());
        }
    }

    #[test]
    fn ddl_parser_covers_all_declared_columns_and_constraints() {
        for &(table, columns, indexes) in TABLES {
            let parts = ddl_parts(table);
            for column in columns {
                assert!(
                    parts
                        .iter()
                        .any(|part| part.starts_with(&format!("{column} "))),
                    "missing DDL shape {table}.{column}"
                );
            }
            for index in indexes {
                assert!(
                    parts.iter().any(|part| if *index == "PRIMARY" {
                        part.contains("PRIMARY KEY")
                    } else {
                        part.contains(index)
                    }),
                    "missing DDL index {table}.{index}"
                );
            }
            for part in parts.iter().filter(|part| {
                !part.starts_with("PRIMARY KEY (")
                    && !part.starts_with("UNIQUE KEY ")
                    && !part.starts_with("KEY ")
                    && !part.starts_with("CONSTRAINT ")
            }) {
                let name = part.split_whitespace().next().unwrap();
                assert!(
                    columns.contains(&name),
                    "unchecked DDL column {table}.{name}"
                );
            }
            assert_eq!(
                expected_indexes(&parts, indexes).unwrap().len(),
                indexes.len()
            );
        }
        assert!(
            ddl_parts("devices")
                .iter()
                .any(|part| part.contains("chk_devices_state"))
        );
    }

    #[test]
    fn check_clause_normalization_accepts_outer_wrappers_but_not_changed_values() {
        assert_eq!(normalize_check("CHECK"), "check");
        assert_eq!(
            normalize_check("(singleton = 1)"),
            normalize_check("singleton = 1")
        );
        assert_ne!(
            normalize_check("singleton = 2"),
            normalize_check("singleton = 1")
        );
    }

    #[test]
    fn nullable_json_and_boolean_alias_match_migration() {
        let json = ColumnMeta {
            name: "descriptor_json".into(),
            data_type: "json".into(),
            column_type: "json".into(),
            nullable: true,
        };
        let bool_col = ColumnMeta {
            name: "initialized".into(),
            data_type: "tinyint".into(),
            column_type: "tinyint(1)".into(),
            nullable: false,
        };
        assert!(validate_column_shape("devices", "JSON NULL", &json).is_ok());
        assert!(
            validate_column_shape("schema_meta", "BOOLEAN NOT NULL DEFAULT FALSE", &bool_col)
                .is_ok()
        );
    }

    #[test]
    fn inline_primary_key_uses_column_name_not_type_width() {
        let parts = ddl_parts("devices");
        let primary = expected_indexes(&parts, &["PRIMARY"]).unwrap();
        assert_eq!(primary[0].columns, vec!["public_key"]);
        assert!(primary[0].unique);
    }

    #[test]
    fn admission_decision_rejects_illegal_transition() {
        let snapshot = AdmissionSnapshot {
            admission_state: AdmissionState::Revoked,
            review_decision: ReviewDecision::Revoked,
            revision: 2,
        };
        assert!(next_snapshot(snapshot, ReviewDecision::Denied).is_err());
        assert_eq!(
            next_snapshot(snapshot, ReviewDecision::Approved)
                .unwrap()
                .revision,
            3
        );
    }

    #[test]
    fn denied_device_requires_reopen_before_approval() {
        let snapshot = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::Denied,
            revision: 4,
        };
        assert!(matches!(
            next_snapshot(snapshot, ReviewDecision::Approved),
            Err(ControllerError::RevisionConflict)
        ));
    }

    #[test]
    fn admission_audit_codes_are_stable_and_neutral() {
        assert_eq!(
            audit_code(ReviewDecision::Approved, AdmissionState::Pending),
            "admission.approve"
        );
        assert_eq!(
            audit_code(ReviewDecision::Approved, AdmissionState::Revoked),
            "admission.reauthorize"
        );
        assert_eq!(
            audit_code(ReviewDecision::Denied, AdmissionState::Pending),
            "admission.reject"
        );
        assert_eq!(
            audit_code(ReviewDecision::None, AdmissionState::Pending),
            "admission.reopen"
        );
        assert_eq!(
            audit_code(ReviewDecision::Revoked, AdmissionState::Approved),
            "admission.revoke"
        );
    }
}
