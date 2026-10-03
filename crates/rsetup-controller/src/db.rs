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
const IDENTITY_MIGRATION: &str = include_str!("../migrations/0002_identity_contract.sql");
const V3_MIGRATION: &str = include_str!("../migrations/0003_identity_application_integrity.sql");
const STATISTICS_QUERY: &str = "SELECT CAST(index_name AS CHAR) AS index_name, CAST(column_name AS CHAR) AS column_name, CAST(non_unique AS SIGNED) AS non_unique, CAST(seq_in_index AS SIGNED) AS seq_in_index, CAST(sub_part AS SIGNED) AS sub_part FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? ORDER BY index_name, seq_in_index";
const FK_CONSTRAINTS_QUERY: &str = "SELECT CAST(table_name AS CHAR) AS table_name, CAST(constraint_name AS CHAR) AS constraint_name FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'FOREIGN KEY'";
const FK_REFERENCES_QUERY: &str = "SELECT CAST(COUNT(*) AS SIGNED) AS fk_count FROM information_schema.referential_constraints WHERE constraint_schema = DATABASE()";
const FK_COLUMNS_QUERY: &str = "SELECT CAST(COUNT(*) AS SIGNED) AS fk_count FROM information_schema.key_column_usage WHERE table_schema = DATABASE() AND referenced_table_schema IS NOT NULL";
// Fixed statements and identifiers: never interpolate database-supplied table/column names.
const IDENTITY_COLUMNS: &[(&str, &str, &str, &str)] = &[
    (
        "schema_meta",
        "authz_epoch",
        "SELECT 1 FROM schema_meta WHERE authz_epoch < 0 LIMIT 1",
        "ALTER TABLE schema_meta MODIFY COLUMN authz_epoch BIGINT UNSIGNED NOT NULL DEFAULT 0",
    ),
    (
        "schema_meta",
        "admin_guard_revision",
        "SELECT 1 FROM schema_meta WHERE admin_guard_revision < 0 LIMIT 1",
        "ALTER TABLE schema_meta MODIFY COLUMN admin_guard_revision BIGINT UNSIGNED NOT NULL DEFAULT 0",
    ),
    (
        "users",
        "revision",
        "SELECT 1 FROM users WHERE revision < 0 LIMIT 1",
        "ALTER TABLE users MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "roles",
        "revision",
        "SELECT 1 FROM roles WHERE revision < 0 LIMIT 1",
        "ALTER TABLE roles MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "device_groups",
        "revision",
        "SELECT 1 FROM device_groups WHERE revision < 0 LIMIT 1",
        "ALTER TABLE device_groups MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "devices",
        "revision",
        "SELECT 1 FROM devices WHERE revision < 0 LIMIT 1",
        "ALTER TABLE devices MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "grants",
        "revision",
        "SELECT 1 FROM grants WHERE revision < 0 LIMIT 1",
        "ALTER TABLE grants MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "admission_decisions",
        "previous_revision",
        "SELECT 1 FROM admission_decisions WHERE previous_revision < 0 LIMIT 1",
        "ALTER TABLE admission_decisions MODIFY COLUMN previous_revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "admission_decisions",
        "new_revision",
        "SELECT 1 FROM admission_decisions WHERE new_revision < 0 LIMIT 1",
        "ALTER TABLE admission_decisions MODIFY COLUMN new_revision BIGINT UNSIGNED NOT NULL",
    ),
    (
        "audit_events",
        "event_seq",
        "SELECT 1 FROM audit_events WHERE event_seq < 0 LIMIT 1",
        "ALTER TABLE audit_events MODIFY COLUMN event_seq BIGINT UNSIGNED NOT NULL",
    ),
    (
        "users",
        "username",
        "",
        "ALTER TABLE users MODIFY COLUMN username VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL",
    ),
];
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
    character_set_name: Option<String>,
    collation_name: Option<String>,
    default: Option<String>,
}

// Task 2 consumes this from the migration preflight; Task 1 only tests the pure rule.
#[allow(dead_code)]
pub(crate) fn valid_username(s: &str) -> bool {
    let bytes = s.as_bytes();
    (3..=64).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b'.' | b'_' | b'-')
        })
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
        return validate_false_default_metadata(table, column, default);
    }
    if default.is_some() {
        return Err(ControllerError::Config(format!(
            "migration incompatible default {table}.{column}"
        )));
    }
    Ok(())
}

fn validate_object_names(
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct IndexMeta {
    name: String,
    columns: Vec<String>,
    unique: bool,
    sub_parts: Vec<Option<i64>>,
}

fn validate_indexes(
    table: &str,
    expected: &[IndexMeta],
    actual: &[IndexMeta],
) -> Result<(), ControllerError> {
    if expected.len() != actual.len() {
        return Err(ControllerError::Config(format!(
            "migration incompatible index set in {table}"
        )));
    }
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

fn ddl_parts_from(migration: &str, table: &str) -> Vec<String> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {table} (");
    let ddl = migration
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

fn ddl_parts(table: &str) -> Vec<String> {
    ddl_parts_from(V3_MIGRATION, table)
}

fn normalize_check(value: &str) -> String {
    // Normalize SQL formatting, not the bytes of quoted string literals. MySQL may
    // introduce _utf8mb4 on readback; other introducers have different semantics.
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if bytes.get(i..i + 2) == Some(b"/*") && bytes.get(i + 2) != Some(&b'!') {
            if let Some(end) = bytes[i + 2..].windows(2).position(|pair| pair == b"*/") {
                i += end + 4;
                continue;
            }
        }
        if byte == b'#'
            || (bytes.get(i..i + 2) == Some(b"--")
                && bytes.get(i + 2).is_some_and(u8::is_ascii_whitespace))
        {
            i += if byte == b'#' { 1 } else { 2 };
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if byte == b'_'
            && (i == 0
                || !matches!(bytes[i - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'$'))
            && bytes
                .get(i..i + 8)
                .is_some_and(|word| word.eq_ignore_ascii_case(b"_utf8mb4"))
            && bytes.get(i + 8) == Some(&b'\'')
        {
            i += 8;
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            let delimiter = byte;
            if delimiter != b'`' {
                out.push(byte);
            }
            i += 1;
            while i < bytes.len() {
                let current = bytes[i];
                if current == delimiter {
                    if bytes.get(i + 1) == Some(&delimiter) {
                        if delimiter == b'`' {
                            out.push(current.to_ascii_lowercase());
                        } else {
                            out.extend_from_slice(&bytes[i..i + 2]);
                        }
                        i += 2;
                        continue;
                    }
                    if delimiter != b'`' {
                        out.push(current);
                    }
                    i += 1;
                    break;
                }
                if current == b'\\' && delimiter != b'`' && i + 1 < bytes.len() {
                    out.extend_from_slice(&bytes[i..i + 2]);
                    i += 2;
                    continue;
                }
                out.push(if delimiter == b'`' {
                    current.to_ascii_lowercase()
                } else {
                    current
                });
                i += 1;
            }
            continue;
        }
        if !byte.is_ascii_whitespace() {
            out.push(byte.to_ascii_lowercase());
        }
        i += 1;
    }
    let mut text = String::from_utf8(out).expect("ASCII normalization preserves UTF-8");
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

fn check_clause_matches(table: &str, name: &str, stored: &str, declared: &str) -> bool {
    if normalize_check(stored) == normalize_check(declared) {
        return true;
    }
    // MySQL 8.0.46 with CAST(check_clause AS CHAR) was observed returning
    // literal backslashes before each quote. Accept only these two complete
    // readbacks against their unchanged fixed 0001 declarations.
    match (table, name, stored) {
        (
            "devices",
            "chk_devices_state",
            "(`admission_state` in (_utf8mb4\\'PENDING\\',_utf8mb4\\'APPROVED\\',_utf8mb4\\'REVOKED\\'))",
        ) => declared == "(admission_state IN ('PENDING','APPROVED','REVOKED'))",
        (
            "devices",
            "chk_devices_decision",
            "(`review_decision` in (_utf8mb4\\'none\\',_utf8mb4\\'approved\\',_utf8mb4\\'denied\\',_utf8mb4\\'revoked\\'))",
        ) => declared == "(review_decision IN ('none','approved','denied','revoked'))",
        _ => false,
    }
}

// Treat a stored CHECK as the 0001 expression only when its tokens can be
// compared without changing SQL string-literal bytes or joining identifiers.
// Unknown metadata spellings fail closed; this is not a general SQL parser.
fn legacy_check_tokens(value: &str) -> Option<Vec<String>> {
    let bytes = value.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i] == b'`' {
            i += 1;
            let name_start = i;
            while i < bytes.len() && bytes[i] != b'`' {
                if !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    return None;
                }
                i += 1;
            }
            if i == bytes.len()
                || i == name_start
                || !matches!(
                    &value[name_start..i].to_ascii_lowercase()[..],
                    "singleton"
                        | "admission_state"
                        | "review_decision"
                        | "source_kind"
                        | "role_id"
                        | "permissions"
                        | "scope_kind"
                        | "scope_group_id"
                        | "scope_device_id"
                )
            {
                return None;
            }
            tokens.push(value[name_start..i].to_ascii_lowercase());
            i += 1;
        } else if bytes[i] == b'\''
            || bytes
                .get(i..i + 8)
                .is_some_and(|part| part.eq_ignore_ascii_case(b"_utf8mb4"))
                && bytes.get(i + 8) == Some(&b'\'')
        {
            if bytes[i] != b'\'' {
                i += 8;
            }
            let quote_start = i;
            i += 1;
            loop {
                if i == bytes.len() {
                    return None;
                }
                if bytes[i] == b'\\' {
                    return None;
                }
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            tokens.push(value[quote_start..i].to_owned());
        } else if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(value[start..i].to_ascii_lowercase());
        } else if matches!(bytes[i], b'(' | b')' | b',' | b'=') {
            tokens.push(value[i..i + 1].to_owned());
            i += 1;
        } else {
            return None;
        }
    }
    while tokens.first().is_some_and(|s| s == "(") && tokens.last().is_some_and(|s| s == ")") {
        let mut depth = 0;
        let wrapped = tokens.iter().enumerate().all(|(idx, token)| {
            if token == "(" {
                depth += 1;
            }
            if token == ")" {
                depth -= 1;
            }
            depth > 0 || idx == tokens.len() - 1
        });
        if !wrapped {
            break;
        }
        tokens.remove(0);
        tokens.pop();
    }
    Some(tokens)
}

fn known_legacy_check(table: &str, name: &str) -> Option<(&'static str, &'static str)> {
    let (table, name) = match (table, name) {
        ("schema_meta", "chk_schema_singleton") => ("schema_meta", "chk_schema_singleton"),
        ("devices", "chk_devices_state") => ("devices", "chk_devices_state"),
        ("devices", "chk_devices_decision") => ("devices", "chk_devices_decision"),
        ("grants", "chk_grants_source") => ("grants", "chk_grants_source"),
        ("grants", "chk_grants_scope") => ("grants", "chk_grants_scope"),
        _ => return None,
    };
    Some((table, name))
}

fn validate_legacy_check_metadata(
    rows: &[(Option<String>, Option<String>, Option<String>)],
) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    let mut confirmed = Vec::new();
    for (table, name, clause) in rows {
        let proof = (|| {
            let (table, name) = known_legacy_check(table.as_deref()?, name.as_deref()?)?;
            if confirmed.contains(&(table, name)) { return None; }
            let declared = ddl_parts_from(MIGRATION, table).into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))?;
            let expression = declared.split_once("CHECK ")?.1;
            let stored = clause.as_deref()?;
            let tokens_match = legacy_check_tokens(stored)
                .zip(legacy_check_tokens(expression))
                .is_some_and(|(stored, expected)| stored == expected);
            if !tokens_match && !check_clause_matches(table, name, stored, expression) {
                return None;
            }
            // The old comparator also normalizes arbitrary outside-literal
            // whitespace. Only its two exact, observed escaped forms are safe.
            if !tokens_match && !matches!((table, name, stored),
                ("devices", "chk_devices_state", "(`admission_state` in (_utf8mb4\\'PENDING\\',_utf8mb4\\'APPROVED\\',_utf8mb4\\'REVOKED\\'))") |
                ("devices", "chk_devices_decision", "(`review_decision` in (_utf8mb4\\'none\\',_utf8mb4\\'approved\\',_utf8mb4\\'denied\\',_utf8mb4\\'revoked\\'))")) {
                return None;
            }
            Some((table, name))
        })().ok_or_else(|| ControllerError::Config("legacy CHECK metadata unavailable or incompatible".into()))?;
        confirmed.push(proof);
    }
    Ok(confirmed)
}

fn validate_legacy_column(
    version: i32,
    table: &str,
    column: &str,
    actual: &ColumnMeta,
) -> Result<bool, ControllerError> {
    match version {
        1 => classify_identity_column(table, column, actual),
        2 => {
            let old = classify_identity_column(table, column, actual)?;
            if old {
                return Err(ControllerError::Config(format!(
                    "migration target column not ready {table}.{column}"
                )));
            }
            Ok(false)
        }
        _ => Err(ControllerError::Config(
            "unsupported legacy shape version".into(),
        )),
    }
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
            sub_parts: vec![None; columns.len()],
            columns,
            unique: name == "PRIMARY" || definition.contains("UNIQUE KEY"),
        });
    }
    Ok(expected)
}

trait IdentitySchemaProbe {
    fn read_version(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i32>, ControllerError>> + Send;
    fn validate_v2_shape(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    #[cfg(test)]
    fn validate_legacy_v2_shape(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_fk(&self) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

// Retain the historical v2 probe solely as a unit-test reference: Upgrade
// cannot call it until Task3B adds a separately reviewed legacy preflight.
#[cfg(test)]
async fn check_legacy_v2_ready(probe: &impl IdentitySchemaProbe) -> Result<(), ControllerError> {
    if probe.read_version().await? != Some(2) {
        return Err(ControllerError::Config(
            "legacy identity migration requires schema version 2".into(),
        ));
    }
    probe.validate_legacy_v2_shape().await
}
async fn check_identity_schema_with_probe(
    probe: &impl IdentitySchemaProbe,
) -> Result<(), ControllerError> {
    identity_schema_decision(probe.read_version().await?)?;
    probe.validate_v2_shape().await?;
    probe.validate_data().await?;
    probe.validate_fk().await
}

const IDENTITY_SCHEMA_VERSION: i32 = 3;

fn identity_schema_decision(version: Option<i32>) -> Result<(), ControllerError> {
    match version {
        Some(IDENTITY_SCHEMA_VERSION) => Ok(()),
        None | Some(1) | Some(2) => Err(ControllerError::SchemaNotReady {
            found: version,
            required: IDENTITY_SCHEMA_VERSION,
        }),
        other => Err(ControllerError::Config(format!(
            "unsupported identity schema version {other:?}"
        ))),
    }
}

#[derive(Clone)]
pub struct TestMigrationConfig {
    pub test_url: String,
    pub allow_destructive: bool,
    pub expected_database: String,
    pub backup_ref: String,
    pub migration_ack: String,
}
impl std::fmt::Debug for TestMigrationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestMigrationConfig")
            .field("test_url", &"[REDACTED]")
            .field("allow_destructive", &self.allow_destructive)
            .field("expected_database", &"[REDACTED]")
            .field("backup_ref", &"[REDACTED]")
            .field("migration_ack", &"[REDACTED]")
            .finish()
    }
}
impl TestMigrationConfig {
    #[cfg(test)]
    fn fixture(url: &str, name: &str) -> Self {
        Self {
            test_url: url.into(),
            allow_destructive: false,
            expected_database: name.into(),
            backup_ref: String::new(),
            migration_ack: String::new(),
        }
    }
    // Raw URL equality here and in run_identity_test_migration only rejects literal reuse;
    // different URLs do not prove physical isolation. The operator must confirm the target.
    pub fn from_test_env() -> Result<Self, ControllerError> {
        let test_url = std::env::var("CONTROLLER_TEST_DATABASE_URL").unwrap_or_default();
        if std::env::var("CONTROLLER_DATABASE_URL").is_ok_and(|service| service == test_url) {
            return Err(ControllerError::Config(
                "test migration cannot reuse service URL".into(),
            ));
        }
        Ok(Self {
            test_url,
            allow_destructive: std::env::var("CONTROLLER_TEST_ALLOW_DESTRUCTIVE")
                .is_ok_and(|v| v == "1"),
            expected_database: std::env::var("CONTROLLER_TEST_EXPECTED_DATABASE")
                .unwrap_or_default(),
            backup_ref: std::env::var("CONTROLLER_TEST_BACKUP_REF").unwrap_or_default(),
            migration_ack: std::env::var("CONTROLLER_TEST_MIGRATION_ACK").unwrap_or_default(),
        })
    }
    pub fn authorize(&self, actual: &str) -> Result<(), ControllerError> {
        authorize_test_migration(self, actual)
    }
}
fn authorize_test_migration(c: &TestMigrationConfig, actual: &str) -> Result<(), ControllerError> {
    let safe = !c.test_url.trim().is_empty()
        && c.allow_destructive
        && (1..=64).contains(&c.expected_database.len())
        && c.expected_database
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && c.expected_database == actual
        && !c.backup_ref.trim().is_empty()
        && c.migration_ack == "isolated-exclusive-backed-up-disposable";
    if safe {
        Ok(())
    } else {
        Err(ControllerError::Config(
            "test migration authorization required".into(),
        ))
    }
}
fn schema_metadata_privilege_error() -> ControllerError {
    ControllerError::Config("test schema metadata privileges unverified".into())
}

// Only the direct schema-wide ALL form observed for these isolated dev engines
// proves that the account can enumerate schema objects. Never parse account
// names into diagnostics; roles, partial revokes and unknown grant syntax deny.
fn grant_grantee_is_simple_account(grantee: &str) -> bool {
    fn quoted_part(input: &[u8]) -> Option<usize> {
        let quote = *input.first()?;
        if quote != b'`' && quote != b'\'' {
            return None;
        }
        let mut i = 1;
        while i < input.len() {
            match input[i] {
                b'\\' if quote == b'\'' && i + 1 < input.len() => i += 2,
                byte if byte == quote && input.get(i + 1) == Some(&quote) => i += 2,
                byte if byte == quote && i > 1 => return Some(i + 1),
                b'\n' | b'\r' | 0 => return None,
                _ => i += 1,
            }
        }
        None
    }
    let bytes = grantee.as_bytes();
    let Some(user_end) = quoted_part(bytes) else {
        return false;
    };
    if bytes.get(user_end) != Some(&b'@') {
        return false;
    }
    quoted_part(&bytes[user_end + 1..]) == Some(bytes.len() - user_end - 1)
}

fn validate_schema_metadata_grants(
    grants: &[Option<String>],
    expected_database: &str,
) -> Result<(), ControllerError> {
    if !(1..=64).contains(&expected_database.len())
        || !expected_database
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(schema_metadata_privilege_error());
    }
    let direct_prefix = format!("GRANT ALL PRIVILEGES ON `{expected_database}`.* TO ");
    let mut direct = false;
    let mut account: Option<&str> = None;
    for row in grants {
        let text = row.as_deref().ok_or_else(schema_metadata_privilege_error)?;
        let (is_direct, remainder) = if let Some(rest) = text.strip_prefix(&direct_prefix) {
            (true, rest)
        } else if let Some(rest) = text.strip_prefix("GRANT USAGE ON *.* TO ") {
            (false, rest)
        } else {
            return Err(schema_metadata_privilege_error());
        };
        let grantee = remainder
            .strip_suffix(" WITH GRANT OPTION")
            .unwrap_or(remainder);
        if !grant_grantee_is_simple_account(grantee)
            || account.is_some_and(|previous| previous != grantee)
        {
            return Err(schema_metadata_privilege_error());
        }
        account = Some(grantee);
        direct |= is_direct;
    }
    if direct {
        Ok(())
    } else {
        Err(schema_metadata_privilege_error())
    }
}

async fn require_schema_metadata_privilege(
    connection: &mut sqlx::pool::PoolConnection<sqlx::MySql>,
    expected_database: &str,
) -> Result<(), ControllerError> {
    let rows = sqlx::query("SHOW GRANTS")
        .fetch_all(&mut **connection)
        .await
        .map_err(|_| schema_metadata_privilege_error())?;
    let grants = rows
        .iter()
        .map(|row| {
            row.try_get::<Option<String>, _>(0)
                .map_err(|_| schema_metadata_privilege_error())
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_schema_metadata_grants(&grants, expected_database)
}

#[cfg(test)]
fn preflight_usernames<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<(), ControllerError> {
    let mut seen = std::collections::HashSet::new();
    for name in values {
        if !valid_username(name) || !seen.insert(name.as_bytes().to_vec()) {
            return Err(ControllerError::Config(
                "invalid or conflicting stored username".into(),
            ));
        }
    }
    Ok(())
}
fn classify_identity_column(
    table: &str,
    name: &str,
    actual: &ColumnMeta,
) -> Result<bool, ControllerError> {
    let (old, target) = identity_column_declarations(table, name)?;
    if validate_column_shape(table, target, actual).is_ok() {
        return Ok(false);
    }
    validate_column_shape(table, old, actual)?;
    Ok(true)
}
fn identity_column_declarations(
    table: &str,
    name: &str,
) -> Result<(&'static str, &'static str), ControllerError> {
    match (table, name) {
        ("users", "username") => Ok((
            "VARCHAR(128) CHARACTER SET utf8mb4 NOT NULL",
            "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL",
        )),
        ("schema_meta", "authz_epoch" | "admin_guard_revision") => Ok((
            "BIGINT NOT NULL DEFAULT 0",
            "BIGINT UNSIGNED NOT NULL DEFAULT 0",
        )),
        ("users" | "roles" | "device_groups" | "devices" | "grants", "revision")
        | ("admission_decisions", "previous_revision" | "new_revision")
        | ("audit_events", "event_seq") => Ok(("BIGINT NOT NULL", "BIGINT UNSIGNED NOT NULL")),
        _ => Err(ControllerError::Config(
            "unknown identity migration column".into(),
        )),
    }
}
async fn run_authorized_migration_with<D, E, C, CF, G, GF, U, UF>(
    config: &TestMigrationConfig,
    connect_and_name: C,
    verify_grants: G,
    upgrade: U,
) -> Result<(), ControllerError>
where
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = Result<(D, String), ControllerError>>,
    G: FnOnce(D, String) -> GF,
    GF: std::future::Future<Output = Result<E, ControllerError>>,
    U: FnOnce(E) -> UF,
    UF: std::future::Future<Output = Result<(), ControllerError>>,
{
    authorize_test_migration(config, &config.expected_database)?;
    let (db, actual) = connect_and_name().await?;
    authorize_test_migration(config, &actual)?;
    let db = verify_grants(db, actual).await?;
    upgrade(db).await
}

fn validate_v3_table_collation(table: &str, table_collation: &str) -> Result<(), ControllerError> {
    if table_collation.to_ascii_lowercase().starts_with("utf8mb4_") {
        Ok(())
    } else {
        Err(ControllerError::Config(format!(
            "migration incompatible table charset {table}"
        )))
    }
}

fn validate_v3_column_charset(
    table: &str,
    declaration: &str,
    actual: &ColumnMeta,
    table_collation: &str,
) -> Result<(), ControllerError> {
    let declared_type = declaration
        .split_whitespace()
        .next()
        .unwrap()
        .to_ascii_lowercase();
    let character_column = matches!(
        declared_type.split('(').next().unwrap(),
        "varchar" | "char" | "text" | "tinytext" | "mediumtext" | "longtext"
    );
    if !character_column {
        return Ok(());
    }
    // The old username declaration is annotated with its inherited utf8mb4
    // charset for classification, but does not declare its own collation.
    // It must still inherit the table collation from the original 0001 DDL.
    if declaration.contains("CHARACTER SET ") && !declaration.contains("CHARACTER SET utf8mb4") {
        return Ok(());
    }
    if actual.character_set_name.as_deref() != Some("utf8mb4")
        || !actual
            .collation_name
            .as_deref()
            .is_some_and(|found| found.eq_ignore_ascii_case(table_collation))
    {
        return Err(ControllerError::Config(format!(
            "migration incompatible character metadata {table}.{}",
            actual.name
        )));
    }
    Ok(())
}

async fn validate_schema_shape(db: &DbPool, target: Option<bool>) -> Result<(), ControllerError> {
    validate_schema_shape_with_contract(db, target, MIGRATION).await
}

async fn validate_v3_schema_shape(db: &DbPool) -> Result<(), ControllerError> {
    validate_schema_shape_with_contract(db, Some(true), V3_MIGRATION).await
}

// Reusable by Task 3's legacy preflight before its first DDL. Call only after
// confirming full schema visibility/shape with the same DbPool identity; zero
// metadata rows alone cannot establish that the account can see all objects.
#[allow(dead_code)]
pub(crate) async fn require_no_identity_foreign_keys(db: &DbPool) -> Result<(), ControllerError> {
    let constraints = sqlx::query(FK_CONSTRAINTS_QUERY)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| ())
        .and_then(|row| {
            row.map(|row| {
                Ok((
                    row.try_get::<Option<String>, _>("table_name")
                        .map_err(|_| ())?,
                    row.try_get::<Option<String>, _>("constraint_name")
                        .map_err(|_| ())?,
                ))
            })
            .transpose()
        });
    let references: Result<Option<i64>, ()> = sqlx::query_scalar(FK_REFERENCES_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    let columns: Result<Option<i64>, ()> = sqlx::query_scalar(FK_COLUMNS_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    validate_fk_metadata(constraints, references, columns)
}

// SQL/decoding errors and NULLs must never be interpreted as an empty set.
type FkConstraintsRead = Result<Option<(Option<String>, Option<String>)>, ()>;

fn validate_fk_metadata(
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

async fn validate_schema_shape_with_contract(
    db: &DbPool,
    target: Option<bool>,
    migration: &str,
) -> Result<(), ControllerError> {
    validate_schema_shape_with_policy(db, target, migration, None)
        .await
        .map(|_| ())
}

// Private SELECT-only entry point for Task 3B1B; not wired to Upgrade.
#[allow(dead_code)]
async fn validate_legacy_shape(
    db: &DbPool,
    version: i32,
) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    if !matches!(version, 1 | 2) {
        return Err(ControllerError::Config(
            "unsupported legacy shape version".into(),
        ));
    }
    validate_schema_shape_with_policy(db, None, MIGRATION, Some(version)).await
}

async fn validate_schema_shape_with_policy(
    db: &DbPool,
    target: Option<bool>,
    migration: &str,
    legacy_version: Option<i32>,
) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    let mut confirmed_checks = Vec::new();
    let present: Vec<String> = sqlx::query_scalar(
        "SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_all(&db.0)
    .await?;
    if present.len() != TABLES.len()
        || present
            .iter()
            .any(|name| !TABLES.iter().any(|(table, _, _)| name == table))
    {
        return Err(ControllerError::Config(
            "identity schema has missing or unexpected tables; manual inspection required".into(),
        ));
    }
    for &(table, columns, indexes) in TABLES {
        let table_collation = if migration == V3_MIGRATION || legacy_version.is_some() {
            let table_collation: String = sqlx::query_scalar("SELECT CAST(table_collation AS CHAR) AS table_collation FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = ?")
                .bind(table).fetch_one(&db.0).await?;
            validate_v3_table_collation(table, &table_collation)?;
            Some(table_collation)
        } else {
            None
        };
        let parts = if migration == V3_MIGRATION {
            ddl_parts(table)
        } else {
            ddl_parts_from(migration, table)
        };
        let actual_columns: Vec<String> = sqlx::query_scalar("SELECT CAST(column_name AS CHAR) AS column_name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ?")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(
            table,
            "column",
            &columns.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            &actual_columns,
        )?;
        for &column in columns {
            let declaration = parts
                .iter()
                .find_map(|part| part.strip_prefix(&format!("{column} ")))
                .ok_or_else(|| {
                    ControllerError::Config(format!(
                        "migration DDL missing column {table}.{column}"
                    ))
                })?;
            let actual = read_column(db, table, column).await?;
            if migration == V3_MIGRATION {
                validate_column_shape(table, declaration, &actual)?;
            } else if IDENTITY_COLUMNS
                .iter()
                .any(|&(t, c, _, _)| t == table && c == column)
            {
                if let Some(version) = legacy_version {
                    validate_legacy_column(version, table, column, &actual)?;
                } else {
                    match target {
                        Some(true) => {
                            if classify_identity_column(table, column, &actual)? {
                                return Err(ControllerError::Config(format!(
                                    "migration target column not ready {table}.{column}"
                                )));
                            }
                        }
                        Some(false) => validate_column_shape(
                            table,
                            identity_column_declarations(table, column)?.0,
                            &actual,
                        )?,
                        None => {
                            classify_identity_column(table, column, &actual)?;
                        }
                    }
                }
            } else {
                validate_column_shape(table, declaration, &actual)?;
            }
            if migration == V3_MIGRATION || legacy_version.is_some() {
                let charset_declaration = if legacy_version.is_some()
                    && (table, column) == ("users", "username")
                    && !classify_identity_column(table, column, &actual)?
                {
                    identity_column_declarations(table, column)?.1
                } else {
                    declaration
                };
                validate_v3_column_charset(
                    table,
                    charset_declaration,
                    &actual,
                    table_collation.as_deref().unwrap(),
                )?;
            }
            // Every column's default is checked by validate_column_shape,
            // including the legacy old/target classification above.
        }
        let expected = expected_indexes(&parts, indexes)?;
        let rows = sqlx::query(STATISTICS_QUERY)
            .bind(table)
            .fetch_all(&db.0)
            .await?;
        let mut actual: Vec<IndexMeta> = Vec::new();
        for row in rows {
            let name: String = row.try_get("index_name")?;
            let column: String = row.try_get("column_name")?;
            let non_unique: i64 = row.try_get("non_unique")?;
            let sub_part: Option<i64> = row.try_get("sub_part")?;
            let seq: i64 = row.try_get("seq_in_index")?;
            if let Some(index) = actual.iter_mut().find(|entry| entry.name == name) {
                if seq != index.columns.len() as i64 + 1 || index.unique != (non_unique == 0) {
                    return Err(ControllerError::Config(format!(
                        "migration incompatible index order {table}.{name}"
                    )));
                }
                index.columns.push(column);
                index.sub_parts.push(sub_part);
            } else {
                if seq != 1 {
                    return Err(ControllerError::Config(format!(
                        "migration incompatible index order {table}.{name}"
                    )));
                }
                actual.push(IndexMeta {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                    sub_parts: vec![sub_part],
                });
            }
        }
        validate_indexes(table, &expected, &actual)?;
        if legacy_version.is_some() {
            let rows = sqlx::query("SELECT CAST(tc.table_name AS CHAR) AS table_name, CAST(tc.constraint_name AS CHAR) AS constraint_name, CAST(cc.check_clause AS CHAR) AS check_clause FROM information_schema.table_constraints tc LEFT JOIN information_schema.check_constraints cc ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name WHERE tc.table_schema = DATABASE() AND tc.table_name = ? AND tc.constraint_type = 'CHECK'")
                .bind(table).fetch_all(&db.0).await
                .map_err(|_| ControllerError::Config("legacy CHECK metadata unavailable or incompatible".into()))?;
            let mut metadata = Vec::with_capacity(rows.len());
            for row in rows {
                metadata.push((
                    row.try_get("table_name").map_err(|_| {
                        ControllerError::Config(
                            "legacy CHECK metadata unavailable or incompatible".into(),
                        )
                    })?,
                    row.try_get("constraint_name").map_err(|_| {
                        ControllerError::Config(
                            "legacy CHECK metadata unavailable or incompatible".into(),
                        )
                    })?,
                    row.try_get("check_clause").map_err(|_| {
                        ControllerError::Config(
                            "legacy CHECK metadata unavailable or incompatible".into(),
                        )
                    })?,
                ));
            }
            confirmed_checks.extend(validate_legacy_check_metadata(&metadata)?);
            continue;
        }
        let expected_checks: Vec<String> = parts
            .iter()
            .filter(|part| part.starts_with("CONSTRAINT "))
            .map(|part| part.split_whitespace().nth(1).unwrap().to_owned())
            .collect();
        let actual_checks: Vec<String> = sqlx::query_scalar("SELECT CAST(constraint_name AS CHAR) AS constraint_name FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND table_name = ? AND constraint_type = 'CHECK'")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(table, "check", &expected_checks, &actual_checks)?;
        for part in parts.iter().filter(|part| part.starts_with("CONSTRAINT ")) {
            let name = part.split_whitespace().nth(1).unwrap();
            let stored: Option<String> = sqlx::query_scalar("SELECT CAST(cc.check_clause AS CHAR) AS check_clause FROM information_schema.table_constraints tc JOIN information_schema.check_constraints cc ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name WHERE tc.table_schema = DATABASE() AND tc.table_name = ? AND tc.constraint_name = ? AND tc.constraint_type = 'CHECK'")
                .bind(table).bind(name).fetch_optional(&db.0).await?;
            let declared = part.split_once("CHECK ").unwrap().1;
            if !stored
                .as_deref()
                .is_some_and(|value| check_clause_matches(table, name, value, declared))
            {
                return Err(ControllerError::Config(format!(
                    "migration incompatible check constraint {table}.{name}"
                )));
            }
        }
    }
    Ok(confirmed_checks)
}

async fn read_column(
    db: &DbPool,
    table: &str,
    column: &str,
) -> Result<ColumnMeta, ControllerError> {
    let row = sqlx::query("SELECT CAST(data_type AS CHAR) AS data_type, CAST(column_type AS CHAR) AS column_type, CAST(is_nullable AS CHAR) AS is_nullable, CAST(character_set_name AS CHAR) AS character_set_name, CAST(collation_name AS CHAR) AS collation_name, CAST(column_default AS CHAR) AS column_default FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
        .bind(table).bind(column).fetch_optional(&db.0).await?
        .ok_or_else(|| ControllerError::Config(format!("migration missing column {table}.{column}")))?;
    Ok(ColumnMeta {
        name: column.to_owned(),
        data_type: row.try_get("data_type")?,
        column_type: row.try_get("column_type")?,
        nullable: parse_nullable_metadata(
            table,
            column,
            row.try_get::<Option<String>, _>("is_nullable")?.as_deref(),
        )?,
        character_set_name: row.try_get("character_set_name")?,
        collation_name: row.try_get("collation_name")?,
        default: row.try_get("column_default")?,
    })
}

fn parse_nullable_metadata(
    table: &str,
    column: &str,
    value: Option<&str>,
) -> Result<bool, ControllerError> {
    match value {
        Some("YES") => Ok(true),
        Some("NO") => Ok(false),
        _ => Err(ControllerError::Config(format!(
            "migration incompatible nullability metadata {table}.{column}"
        ))),
    }
}

fn validate_false_default_metadata(
    table: &str,
    column: &str,
    default: Option<&str>,
) -> Result<(), ControllerError> {
    // BOOLEAN is a TINYINT alias; only numeric 0 is accepted here. Other
    // engine readback representations require independent verification.
    if default == Some("0") {
        Ok(())
    } else {
        Err(ControllerError::Config(format!(
            "migration incompatible default {table}.{column}"
        )))
    }
}

async fn read_version(db: &DbPool) -> Result<Option<i32>, ControllerError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'schema_meta'")
        .fetch_one(&db.0).await?;
    if exists == 0 {
        return Ok(None);
    }
    if exists != 1 {
        return Err(ControllerError::Config(
            "damaged schema_meta table identity".into(),
        ));
    }
    let rows = sqlx::query("SELECT singleton, schema_version FROM schema_meta LIMIT 2")
        .fetch_all(&db.0)
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] if row.try_get::<i8, _>("singleton")? == 1 => {
            Ok(Some(row.try_get("schema_version")?))
        }
        _ => Err(ControllerError::Config(
            "damaged schema_meta singleton".into(),
        )),
    }
}

impl IdentitySchemaProbe for DbPool {
    async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
        read_version(self).await
    }
    async fn validate_v2_shape(&self) -> Result<(), ControllerError> {
        validate_v3_schema_shape(self).await
    }
    #[cfg(test)]
    async fn validate_legacy_v2_shape(&self) -> Result<(), ControllerError> {
        validate_schema_shape(self, Some(true)).await
    }
    async fn validate_data(&self) -> Result<(), ControllerError> {
        crate::integrity::check_identity_data(self).await
    }
    async fn validate_fk(&self) -> Result<(), ControllerError> {
        require_no_identity_foreign_keys(self).await
    }
}

pub async fn check_identity_schema(db: &DbPool) -> Result<(), ControllerError> {
    check_identity_schema_with_probe(db).await
}

#[derive(Clone, Copy, Debug)]
pub enum TestMigrationMode {
    Upgrade,
    FixtureV1,
    FixturePartialV1,
}

pub async fn run_identity_test_migration(
    config: &TestMigrationConfig,
    mode: TestMigrationMode,
) -> Result<(), ControllerError> {
    if std::env::var("CONTROLLER_DATABASE_URL").is_ok_and(|service| service == config.test_url) {
        return Err(ControllerError::Config(
            "test migration cannot reuse service URL".into(),
        ));
    }
    run_authorized_migration_with(
        config,
        || async {
            let db = DbPool(
                sqlx::MySqlPool::connect(&config.test_url)
                    .await
                    .map_err(|_| {
                        ControllerError::Config("test database connection failed".into())
                    })?,
            );
            let mut connection = db.0.acquire().await.map_err(|_| {
                ControllerError::Config("test database identity query failed".into())
            })?;
            let actual: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
                .fetch_one(&mut *connection)
                .await
                .map_err(|_| {
                    ControllerError::Config("test database identity query failed".into())
                })?;
            let actual = actual.ok_or_else(|| {
                ControllerError::Config("test database identity query failed".into())
            })?;
            Ok(((db, connection), actual))
        },
        |(db, mut connection), actual| async move {
            require_schema_metadata_privilege(&mut connection, &actual).await?;
            Ok(db)
        },
        |db| async move { upgrade_identity_schema(&db, mode).await },
    )
    .await
}

async fn scan_legacy_usernames(db: &DbPool) -> Result<(), ControllerError> {
    // Full UNIQUE(username) was verified in the shape gate; no unbounded name set.
    let mut stream = sqlx::query("SELECT username FROM users").fetch(&db.0);
    while let Some(item) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        let row = item.map_err(|_| legacy_preflight_error("username.read"))?;
        let username: &str = row
            .try_get("username")
            .map_err(|_| legacy_preflight_error("username.decode"))?;
        if !valid_username(username) {
            return Err(legacy_preflight_error("username.invalid"));
        }
    }
    Ok(())
}

async fn username_encoding_is_lossless(db: &DbPool) -> Result<(), ControllerError> {
    // The shared bounded scanner already validates each username as lowercase ASCII.
    // Retain the independent server-side target conversion guard without fetching names.
    let bad = sqlx::query(
        "SELECT 1 FROM users WHERE BINARY username <> BINARY CONVERT(username USING ascii) LIMIT 1",
    )
    .fetch_optional(&db.0)
    .await
    .map_err(|_| legacy_preflight_error("username.encoding_read"))?;
    if bad.is_some() {
        return Err(legacy_preflight_error("username.encoding"));
    }
    Ok(())
}

async fn username_target_has_no_collision(db: &DbPool) -> Result<(), ControllerError> {
    let collision = sqlx::query("SELECT 1 FROM users GROUP BY CONVERT(username USING ascii) COLLATE ascii_bin HAVING COUNT(*) > 1 LIMIT 1")
        .fetch_optional(&db.0).await
        .map_err(|_| legacy_preflight_error("username.collision_read"))?;
    if collision.is_some() {
        return Err(legacy_preflight_error("username.collision"));
    }
    Ok(())
}

// The status is only an observation of this read-only pass, never authority to
// ALTER or DROP: Task 3B2 must re-read each column/CHECK immediately before DDL
// and use identifiers and statements from the fixed source whitelist only.
#[allow(dead_code)] // consumed by the subsequent legacy write phase
struct LegacyPreflight {
    old_columns: [bool; 11], // positional keys are IDENTITY_COLUMNS, never database names
    observed_checks: Vec<(&'static str, &'static str)>,
}

fn legacy_preflight_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("legacy preflight {code}"))
}

fn validate_identity_status(version: i32, status: &[bool]) -> Result<(), ControllerError> {
    if status.len() != 11 || IDENTITY_COLUMNS.len() != 11 {
        return Err(legacy_preflight_error("column.count"));
    }
    if version == 2 && status.iter().any(|old| *old) {
        return Err(legacy_preflight_error("column.v2_old"));
    }
    if !matches!(version, 1 | 2) {
        return Err(legacy_preflight_error("version"));
    }
    Ok(())
}

trait LegacyPreflightProbe {
    fn read_legacy_meta(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<(i8, i32)>, ControllerError>> + Send;
    fn legacy_shape(
        &self,
        version: i32,
    ) -> impl std::future::Future<
        Output = Result<Vec<(&'static str, &'static str)>, ControllerError>,
    > + Send;
    fn no_foreign_keys(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn identity_rows(
        &self,
        version: i32,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn column_status(
        &self,
        version: i32,
    ) -> impl std::future::Future<Output = Result<Vec<bool>, ControllerError>> + Send;
    fn negative(
        &self,
        query: &'static str,
    ) -> impl std::future::Future<Output = Result<bool, ControllerError>> + Send;
    fn username_encoding(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn username_collision(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

// Reused by the strict pre-CAS v3 gate after the legacy steps have finished.
async fn preflight_legacy_for_v3(
    db: &DbPool,
    version: i32,
) -> Result<LegacyPreflight, ControllerError> {
    preflight_legacy_with_probe(db, version).await
}

async fn preflight_legacy_with_probe(
    probe: &impl LegacyPreflightProbe,
    version: i32,
) -> Result<LegacyPreflight, ControllerError> {
    if !matches!(version, 1 | 2) {
        return Err(legacy_preflight_error("version"));
    }
    let meta = probe
        .read_legacy_meta()
        .await
        .map_err(|_| legacy_preflight_error("meta.read"))?;
    if meta.as_slice() != [(1, version)] {
        return Err(legacy_preflight_error("meta.singleton"));
    }
    let observed_checks = probe
        .legacy_shape(version)
        .await
        .map_err(|_| legacy_preflight_error("shape"))?;
    let mut unique_checks = Vec::new();
    for &(table, name) in &observed_checks {
        if known_legacy_check(table, name) != Some((table, name))
            || unique_checks.contains(&(table, name))
        {
            return Err(legacy_preflight_error("check.whitelist"));
        }
        unique_checks.push((table, name));
    }
    probe
        .no_foreign_keys()
        .await
        .map_err(|_| legacy_preflight_error("foreign_keys"))?;
    probe
        .identity_rows(version)
        .await
        .map_err(|_| legacy_preflight_error("data"))?;
    let status = probe
        .column_status(version)
        .await
        .map_err(|_| legacy_preflight_error("column.read"))?;
    validate_identity_status(version, &status)?;
    for ((table, column, negative_query, _), old) in IDENTITY_COLUMNS.iter().zip(&status) {
        if *old
            && !negative_query.is_empty()
            && probe
                .negative(negative_query)
                .await
                .map_err(|_| legacy_preflight_error("counter.read"))?
        {
            return Err(ControllerError::Config(format!(
                "negative identity column {table}.{column}"
            )));
        }
    }
    probe
        .username_encoding()
        .await
        .map_err(|_| legacy_preflight_error("username.encoding"))?;
    probe
        .username_collision()
        .await
        .map_err(|_| legacy_preflight_error("username.collision"))?;
    Ok(LegacyPreflight {
        old_columns: status
            .try_into()
            .map_err(|_| legacy_preflight_error("column.count"))?,
        observed_checks,
    })
}

impl LegacyPreflightProbe for DbPool {
    async fn read_legacy_meta(&self) -> Result<Vec<(i8, i32)>, ControllerError> {
        let rows = sqlx::query("SELECT singleton,schema_version FROM schema_meta LIMIT 2")
            .fetch_all(&self.0)
            .await
            .map_err(|_| legacy_preflight_error("meta.read"))?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("singleton")
                        .map_err(|_| legacy_preflight_error("meta.decode"))?,
                    row.try_get("schema_version")
                        .map_err(|_| legacy_preflight_error("meta.decode"))?,
                ))
            })
            .collect()
    }
    async fn legacy_shape(
        &self,
        version: i32,
    ) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
        validate_legacy_shape(self, version).await
    }
    async fn no_foreign_keys(&self) -> Result<(), ControllerError> {
        require_no_identity_foreign_keys(self).await
    }
    async fn identity_rows(&self, version: i32) -> Result<(), ControllerError> {
        crate::integrity::validate_identity_rows_for_version(self, version).await
    }
    async fn column_status(&self, version: i32) -> Result<Vec<bool>, ControllerError> {
        let mut status = Vec::with_capacity(IDENTITY_COLUMNS.len());
        for &(table, column, _, _) in IDENTITY_COLUMNS {
            let actual = read_column(self, table, column)
                .await
                .map_err(|_| legacy_preflight_error("column.read"))?;
            status.push(validate_legacy_column(version, table, column, &actual)?);
        }
        Ok(status)
    }
    async fn negative(&self, query: &'static str) -> Result<bool, ControllerError> {
        if !IDENTITY_COLUMNS
            .iter()
            .any(|entry| entry.2 == query && !query.is_empty())
        {
            return Err(legacy_preflight_error("counter.query"));
        }
        Ok(sqlx::query(query)
            .fetch_optional(&self.0)
            .await
            .map_err(|_| legacy_preflight_error("counter.read"))?
            .is_some())
    }
    async fn username_encoding(&self) -> Result<(), ControllerError> {
        username_encoding_is_lossless(self).await
    }
    async fn username_collision(&self) -> Result<(), ControllerError> {
        username_target_has_no_collision(self).await
    }
}

// Only these five 0001 CHECKs may be removed. Each SQL spelling is fixed here,
// not formatted from metadata returned by the database.
const LEGACY_CHECK_DROPS: &[(&str, &str, &str)] = &[
    (
        "schema_meta",
        "chk_schema_singleton",
        "ALTER TABLE schema_meta DROP CHECK chk_schema_singleton",
    ),
    (
        "devices",
        "chk_devices_state",
        "ALTER TABLE devices DROP CHECK chk_devices_state",
    ),
    (
        "devices",
        "chk_devices_decision",
        "ALTER TABLE devices DROP CHECK chk_devices_decision",
    ),
    (
        "grants",
        "chk_grants_source",
        "ALTER TABLE grants DROP CHECK chk_grants_source",
    ),
    (
        "grants",
        "chk_grants_scope",
        "ALTER TABLE grants DROP CHECK chk_grants_scope",
    ),
];

fn legacy_check_drop_sql(table: &str, name: &str) -> Result<&'static str, ControllerError> {
    LEGACY_CHECK_DROPS
        .iter()
        .find(|&&(fixed_table, fixed_name, _)| (table, name) == (fixed_table, fixed_name))
        .map(|&(_, _, statement)| statement)
        .ok_or_else(|| legacy_preflight_error("drop whitelist"))
}

fn same_legacy_snapshot(expected: &LegacyPreflight, actual: &LegacyPreflight) -> bool {
    expected.old_columns == actual.old_columns
        && expected.observed_checks.len() == actual.observed_checks.len()
        && expected
            .observed_checks
            .iter()
            .all(|check| actual.observed_checks.contains(check))
}

fn require_legacy_snapshot(
    expected: &LegacyPreflight,
    actual: &LegacyPreflight,
) -> Result<(), ControllerError> {
    if same_legacy_snapshot(expected, actual) {
        Ok(())
    } else {
        Err(legacy_preflight_error("unexpected state drift"))
    }
}

impl LegacyUpgradeProbe for DbPool {
    async fn alter(&self, statement: &'static str) -> Result<(), ControllerError> {
        if IDENTITY_MIGRATION
            .split(';')
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .ne(IDENTITY_COLUMNS.iter().map(|entry| entry.3))
            || !IDENTITY_COLUMNS.iter().any(|entry| entry.3 == statement)
        {
            return Err(legacy_preflight_error("alter whitelist"));
        }
        sqlx::query(statement)
            .execute(&self.0)
            .await
            .map_err(|_| legacy_preflight_error("alter failed"))?;
        Ok(())
    }

    async fn drop_known_check(
        &self,
        table: &'static str,
        name: &'static str,
    ) -> Result<(), ControllerError> {
        let statement = legacy_check_drop_sql(table, name)?;
        sqlx::query(statement)
            .execute(&self.0)
            .await
            .map_err(|_| legacy_preflight_error("drop failed"))?;
        Ok(())
    }

    async fn strict_v3_shape_without_meta_update(&self) -> Result<(), ControllerError> {
        let version = read_version(self)
            .await?
            .filter(|version| matches!(version, 1 | 2))
            .ok_or_else(|| legacy_preflight_error("version"))?;
        validate_v3_schema_shape(self).await?;
        require_no_identity_foreign_keys(self).await?;
        let snapshot = preflight_legacy_for_v3(self, version).await?;
        if snapshot.old_columns.iter().any(|old| *old) || !snapshot.observed_checks.is_empty() {
            return Err(legacy_preflight_error("target not ready"));
        }
        Ok(())
    }

    async fn cas_version(&self, from: i32, to: i32) -> Result<u64, ControllerError> {
        if !matches!(from, 1 | 2) || to != 3 {
            return Err(legacy_preflight_error("version"));
        }
        let result = sqlx::query(
            "UPDATE schema_meta SET schema_version = 3 WHERE singleton = 1 AND schema_version = ?",
        )
        .bind(from)
        .execute(&self.0)
        .await
        .map_err(|_| legacy_preflight_error("version CAS failed"))?;
        Ok(result.rows_affected())
    }

    async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
        read_version(self).await
    }

    async fn ready_v3(&self) -> Result<(), ControllerError> {
        check_identity_schema(self).await
    }
}

trait LegacyUpgradeProbe: LegacyPreflightProbe {
    async fn preflight(&self, version: i32) -> Result<LegacyPreflight, ControllerError>
    where
        Self: Sized,
    {
        preflight_legacy_with_probe(self, version).await
    }
    async fn alter(&self, statement: &'static str) -> Result<(), ControllerError>;
    async fn drop_known_check(
        &self,
        table: &'static str,
        name: &'static str,
    ) -> Result<(), ControllerError>;
    async fn strict_v3_shape_without_meta_update(&self) -> Result<(), ControllerError>;
    async fn cas_version(&self, from: i32, to: i32) -> Result<u64, ControllerError>;
    async fn read_version(&self) -> Result<Option<i32>, ControllerError>;
    async fn ready_v3(&self) -> Result<(), ControllerError>;
}

async fn upgrade_legacy_with_probe(
    probe: &impl LegacyUpgradeProbe,
    version: i32,
) -> Result<(), ControllerError> {
    if !matches!(version, 1 | 2) {
        return Err(legacy_preflight_error("version"));
    }
    // Full version/shape/CHECK/FK/data/counter/username preflight precedes
    // *every* DDL, not just the first; independently recheck its outcome.
    let mut trusted = probe.preflight(version).await?;
    for (index, &(_, _, _, statement)) in IDENTITY_COLUMNS.iter().enumerate() {
        if !trusted.old_columns[index] {
            continue; // Nontransactional retry: this exact ALTER already landed.
        }
        let before = probe.preflight(version).await?;
        require_legacy_snapshot(&trusted, &before)?;
        if !before.old_columns[index] || version != 1 {
            return Err(legacy_preflight_error("alter precondition"));
        }
        let ddl = probe.alter(statement).await;
        // Even an Err can have applied a nontransactional ALTER. Re-read for
        // diagnosis but never convert an uncertain result into success.
        let after = probe.preflight(version).await;
        ddl?;
        let after = after?;
        let mut expected = before;
        expected.old_columns[index] = false;
        require_legacy_snapshot(&expected, &after)?;
        trusted = after;
    }
    for &(table, name, _) in LEGACY_CHECK_DROPS {
        if !trusted.observed_checks.contains(&(table, name)) {
            continue;
        }
        let before = probe.preflight(version).await?;
        require_legacy_snapshot(&trusted, &before)?;
        if !before.observed_checks.contains(&(table, name)) {
            return Err(legacy_preflight_error("drop precondition"));
        }
        let ddl = probe.drop_known_check(table, name).await;
        let after = probe.preflight(version).await;
        ddl?;
        let after = after?;
        let mut expected = before;
        expected
            .observed_checks
            .retain(|check| *check != (table, name));
        require_legacy_snapshot(&expected, &after)?;
        trusted = after;
    }
    let final_read = probe.preflight(version).await?;
    require_legacy_snapshot(&trusted, &final_read)?;
    if final_read.old_columns.iter().any(|old| *old) || !final_read.observed_checks.is_empty() {
        return Err(legacy_preflight_error("target not ready"));
    }
    probe.strict_v3_shape_without_meta_update().await?;
    let affected = probe.cas_version(version, 3).await;
    if !matches!(affected, Ok(1)) {
        // Read only for operator diagnosis. A successful read of 3 after a
        // failed CAS does NOT prove this caller can claim a completed upgrade.
        let _observed = probe.read_version().await;
        return Err(ControllerError::Config(
            "legacy version CAS outcome uncertain".into(),
        ));
    }
    if !matches!(probe.read_version().await, Ok(Some(3))) {
        // A reported single CAS row is insufficient evidence that the
        // persisted version is 3. Unknown/old/read failure stays uncertain.
        return Err(ControllerError::Config(
            "legacy version CAS outcome uncertain".into(),
        ));
    }
    probe.ready_v3().await
}

async fn preflight_values(db: &DbPool, status: &[bool]) -> Result<(), ControllerError> {
    validate_identity_status(1, status)?;
    scan_legacy_usernames(db).await?;
    for ((table, column, negative_query, _), old) in IDENTITY_COLUMNS.iter().zip(status) {
        if *old && !negative_query.is_empty() {
            let found = sqlx::query(negative_query)
                .fetch_optional(&db.0)
                .await
                .map_err(|_| legacy_preflight_error("counter.read"))?;
            if found.is_some() {
                return Err(ControllerError::Config(format!(
                    "negative identity column {table}.{column}"
                )));
            }
        }
    }
    username_encoding_is_lossless(db).await?;
    username_target_has_no_collision(db).await
}

// The complete v3 baseline is only for an authorized, genuinely empty schema.
// Every identifier comes from the checked-in migration and the fixed TABLES set.
fn v3_create_statements() -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    let mut statements = Vec::new();
    for statement in V3_MIGRATION
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let table = statement
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .and_then(|rest| rest.split_once(" ("))
            .map(|(name, _)| name)
            .ok_or_else(|| ControllerError::Config("invalid v3 baseline SQL".into()))?;
        if !TABLES.iter().any(|(known, _, _)| *known == table)
            || statements.iter().any(|(name, _)| *name == table)
            || statement.to_ascii_uppercase().contains("FOREIGN KEY")
            || statement.to_ascii_uppercase().contains(" CHECK ")
        {
            return Err(ControllerError::Config("invalid v3 baseline SQL".into()));
        }
        statements.push((table, statement));
    }
    if statements.len() != TABLES.len() {
        return Err(ControllerError::Config("incomplete v3 baseline SQL".into()));
    }
    Ok(statements)
}

trait FreshIdentityMigrationProbe: IdentitySchemaProbe {
    fn schema_tables(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>, ControllerError>> + Send;
    fn create_table(
        &self,
        statement: &str,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_empty_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn insert_v3_meta(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

async fn upgrade_fresh_v3(probe: &impl FreshIdentityMigrationProbe) -> Result<(), ControllerError> {
    match probe.read_version().await? {
        Some(3) => return check_identity_schema_with_probe(probe).await,
        Some(1 | 2) => {
            return Err(ControllerError::Config(
                "legacy v3 upgrade not ready".into(),
            ));
        }
        Some(_) => {
            return Err(ControllerError::Config(
                "unsupported test identity schema version".into(),
            ));
        }
        None => {}
    }
    let statements = v3_create_statements()?;
    if !probe.schema_tables().await?.is_empty() {
        return Err(ControllerError::Config(
            "nonempty unversioned test schema requires manual handling".into(),
        ));
    }
    let mut expected = Vec::new();
    for (table, statement) in statements {
        // CREATE is non-transactional. A failed or unverifiable step leaves the
        // partial unversioned schema for an operator, never an automatic retry.
        probe.create_table(statement).await?;
        expected.push(table);
        let actual = probe.schema_tables().await?;
        if actual.len() != expected.len()
            || actual.iter().any(|name| !expected.contains(&name.as_str()))
        {
            return Err(ControllerError::Config(
                "v3 CREATE result not reflected in schema metadata".into(),
            ));
        }
    }
    probe.validate_v2_shape().await?;
    probe.validate_fk().await?;
    probe.validate_empty_data().await?;
    if probe.insert_v3_meta().await.is_err() {
        // Even when a subsequent read sees version 3, the outcome of this INSERT
        // is uncertain; never claim success or reset an operator's partial schema.
        let _observed = probe.read_version().await;
        let _shape = probe.validate_v2_shape().await;
        let _fk = probe.validate_fk().await;
        return Err(ControllerError::Config(
            "v3 metadata insert uncertain; inspect schema manually".into(),
        ));
    }
    check_identity_schema_with_probe(probe).await
}

impl FreshIdentityMigrationProbe for DbPool {
    async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
        Ok(sqlx::query_scalar("SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
            .fetch_all(&self.0).await?)
    }
    async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
        sqlx::query(statement).execute(&self.0).await.map_err(|_| {
            ControllerError::Config(
                "v3 CREATE failed; partial schema needs manual inspection".into(),
            )
        })?;
        Ok(())
    }
    async fn validate_empty_data(&self) -> Result<(), ControllerError> {
        crate::integrity::validate_identity_rows(self, false).await?;
        // The bounded scanner checks values; freshness additionally requires *no*
        // business rows, including otherwise valid rows inserted by another writer.
        for &(table, _, _) in TABLES {
            let statement = format!("SELECT 1 FROM {table} LIMIT 1");
            if sqlx::query(&statement)
                .fetch_optional(&self.0)
                .await?
                .is_some()
            {
                return Err(ControllerError::Config(
                    "v3 fresh schema contains rows before version insert".into(),
                ));
            }
        }
        Ok(())
    }
    async fn insert_v3_meta(&self) -> Result<(), ControllerError> {
        let result = sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, 3, ?, FALSE, 0, 0)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).execute(&self.0).await?;
        if result.rows_affected() != 1 {
            return Err(ControllerError::Config(
                "v3 metadata insert affected unexpected rows".into(),
            ));
        }
        Ok(())
    }
}

async fn upgrade_authorized_with_probe<P>(probe: &P) -> Result<(), ControllerError>
where
    P: FreshIdentityMigrationProbe + LegacyUpgradeProbe,
{
    match IdentitySchemaProbe::read_version(probe).await? {
        Some(version @ (1 | 2)) => upgrade_legacy_with_probe(probe, version).await,
        _ => upgrade_fresh_v3(probe).await,
    }
}

async fn upgrade_identity_schema(
    db: &DbPool,
    mode: TestMigrationMode,
) -> Result<(), ControllerError> {
    match mode {
        TestMigrationMode::Upgrade => upgrade_authorized_with_probe(db).await,
        // Historical fixtures remain explicitly selectable development tools;
        // neither path is reachable from the production startup or Upgrade.
        TestMigrationMode::FixtureV1 | TestMigrationMode::FixturePartialV1 => {
            create_historical_v1_fixture(db, mode).await
        }
    }
}

async fn create_historical_v1_fixture(
    db: &DbPool,
    mode: TestMigrationMode,
) -> Result<(), ControllerError> {
    if IDENTITY_MIGRATION
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ne(IDENTITY_COLUMNS.iter().map(|(_, _, _, alter)| *alter))
    {
        return Err(ControllerError::Config(
            "identity migration SQL disagrees with fixed whitelist".into(),
        ));
    }
    // All access reaches this private writer only through the authorized test command.
    let version = read_version(db).await?;
    if matches!(mode, TestMigrationMode::FixturePartialV1) && version != Some(1) {
        return Err(ControllerError::Config(
            "fixture-partial-v1 requires existing v1 test schema".into(),
        ));
    }
    if matches!(mode, TestMigrationMode::FixtureV1) && version.is_some() {
        return Err(ControllerError::Config(
            "fixture-v1 requires empty test database".into(),
        ));
    }
    if version == Some(2) {
        return Err(ControllerError::Config(
            "historical fixture requires v1".into(),
        ));
    }
    if version.is_none() {
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
        )
        .fetch_one(&db.0)
        .await?;
        if tables != 0 {
            return Err(ControllerError::Config(
                "nonempty unversioned test schema requires manual handling".into(),
            ));
        }
        // 0001 remains byte-identical. A failed CREATE leaves an unversioned partial schema;
        // never repair it automatically: the operator must inspect the stopped test fixture.
        for statement in MIGRATION
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sqlx::query(statement).execute(&db.0).await?;
        }
        validate_schema_shape(db, Some(false)).await?;
        sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, 1, ?, FALSE, 0, 0)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).execute(&db.0).await?;
    } else if version != Some(1) {
        return Err(ControllerError::Config(
            "unsupported test identity schema version".into(),
        ));
    }
    if matches!(mode, TestMigrationMode::FixtureV1) {
        return Ok(());
    }
    validate_schema_shape(db, None).await?;
    let mut status = Vec::new();
    for &(table, column, _, _) in IDENTITY_COLUMNS {
        status.push(classify_identity_column(
            table,
            column,
            &read_column(db, table, column).await?,
        )?);
    }
    preflight_values(db, &status).await?;
    if matches!(mode, TestMigrationMode::FixturePartialV1) {
        if !status[0] {
            return Err(ControllerError::Config(
                "fixture partial column already altered".into(),
            ));
        }
        sqlx::query(IDENTITY_COLUMNS[0].3).execute(&db.0).await?;
        if classify_identity_column(
            IDENTITY_COLUMNS[0].0,
            IDENTITY_COLUMNS[0].1,
            &read_column(db, IDENTITY_COLUMNS[0].0, IDENTITY_COLUMNS[0].1).await?,
        )? {
            return Err(ControllerError::Config(
                "partial fixture ALTER not reflected in metadata".into(),
            ));
        }
        return Ok(());
    }
    Err(ControllerError::Config(
        "historical fixture mode not supported for this state".into(),
    ))
}

pub trait AdmissionStore {
    fn load(
        &self,
        public_key: [u8; 32],
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
    fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: u64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
}

fn decode_snapshot(
    state: &str,
    decision: &str,
    revision: u64,
) -> Result<AdmissionSnapshot, ControllerError> {
    crate::validate_device_fields(state, decision).map_err(|_| {
        ControllerError::Config("invalid stored admission state/decision combination".into())
    })?;
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

fn next_event_seq(counter: &std::sync::atomic::AtomicU64) -> Result<u64, ControllerError> {
    counter
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |n| n.checked_add(1),
        )
        .map_err(|_| ControllerError::RevisionConflict)
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
            row.try_get::<u64, _>("revision")?,
        )
    }

    async fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: u64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> Result<AdmissionSnapshot, ControllerError> {
        use std::sync::{OnceLock, atomic::AtomicU64};
        static PROCESS_EPOCH: OnceLock<uuid::Uuid> = OnceLock::new();
        static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);
        let mut tx = self.0.begin().await?;
        let row = sqlx::query("SELECT admission_state, review_decision, revision FROM devices WHERE public_key = ? FOR UPDATE")
            .bind(public_key.as_slice()).fetch_optional(&mut *tx).await?.ok_or(ControllerError::NotFound)?;
        let current = decode_snapshot(
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get::<u64, _>("revision")?,
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
        let seq = next_event_seq(&EVENT_SEQ)?;
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
    fn decode_snapshot_rejects_invalid_pairs_even_when_values_are_known() {
        for (state, decision) in [
            ("PENDING", "none"),
            ("PENDING", "denied"),
            ("APPROVED", "approved"),
            ("REVOKED", "revoked"),
        ] {
            assert_eq!(decode_snapshot(state, decision, 42).unwrap().revision, 42);
        }
        for (state, decision) in [
            ("PENDING", "approved"),
            ("APPROVED", "none"),
            ("REVOKED", "denied"),
            ("pending", "none"),
            ("PENDING", "NONE"),
        ] {
            assert!(
                decode_snapshot(state, decision, 42).is_err(),
                "{state}/{decision}"
            );
        }
    }

    #[test]
    fn schema_metadata_grants_require_exact_direct_schema_all() {
        let target = "dev_identity";
        let direct = "GRANT ALL PRIVILEGES ON `dev_identity`.* TO `tester`@`localhost`";
        let usage = "GRANT USAGE ON *.* TO `tester`@`localhost`";
        assert!(validate_schema_metadata_grants(&[Some(direct.into())], target).is_ok());
        assert!(
            validate_schema_metadata_grants(
                &[
                    Some(usage.into()),
                    Some(format!("{direct} WITH GRANT OPTION"))
                ],
                target,
            )
            .is_ok()
        );
        for invalid in [
            "GRANT ALL PRIVILEGES ON `dev_identity_backup`.* TO `tester`@`localhost`",
            "GRANT ALL PRIVILEGES ON `other_dev_identity`.* TO `tester`@`localhost`",
            "GRANT ALL PRIVILEGES ON *.* TO `tester`@`localhost`",
            "GRANT USAGE ON *.* TO `tester`@`localhost`",
            "GRANT `metadata_role`@`localhost` TO `tester`@`localhost`",
            "not a recognized SHOW GRANTS row",
            "GRANT ALL PRIVILEGES ON `dev_identity`.* TO `tester`@`localhost` arbitrary",
        ] {
            assert!(validate_schema_metadata_grants(&[Some(invalid.into())], target).is_err());
        }
        for grants in [
            vec![],
            vec![None],
            vec![Some(direct.into()), None],
            vec![
                Some(direct.into()),
                Some("REVOKE SELECT ON `dev_identity`.`hidden` FROM `tester`@`localhost`".into()),
            ],
            vec![
                Some(direct.into()),
                Some("GRANT USAGE ON *.* TO `different`@`localhost`".into()),
            ],
            vec![
                Some(direct.into()),
                Some("GRANT `metadata_role`@`localhost` TO `tester`@`localhost`".into()),
            ],
        ] {
            assert!(validate_schema_metadata_grants(&grants, target).is_err());
        }
        assert!(
            validate_schema_metadata_grants(&[Some(direct.into())], "dev_identity_bad").is_err()
        );
        assert!(validate_schema_metadata_grants(&[Some(direct.into())], "dev_identity`").is_err());
        assert!(
            !format!(
                "{}",
                validate_schema_metadata_grants(&[None], target).unwrap_err()
            )
            .contains(target)
        );
    }

    #[test]
    fn explicit_migration_checks_session_grants_before_any_upgrade_branch() {
        let source = include_str!("db.rs");
        let entry = source
            .split_once("pub async fn run_identity_test_migration(")
            .unwrap()
            .1
            .split_once("\nasync fn preflight_values(")
            .unwrap()
            .0;
        let name = entry.find("SELECT DATABASE()").unwrap();
        let grants = entry
            .find("require_schema_metadata_privilege(&mut connection, &actual).await?")
            .unwrap();
        let upgrade = entry
            .find("upgrade_identity_schema(&db, mode).await")
            .unwrap();
        let gate = source
            .split_once("async fn run_authorized_migration_with<")
            .unwrap()
            .1
            .split_once("\nfn validate_v3_table_collation(")
            .unwrap()
            .0;
        let authorized = gate
            .find("authorize_test_migration(config, &actual)?")
            .unwrap();
        let verify = gate.find("verify_grants(db, actual).await?").unwrap();
        let dispatch = gate.find("upgrade(db).await").unwrap();
        assert!(name < grants && grants < upgrade);
        assert!(authorized < verify && verify < dispatch);
    }

    #[test]
    fn fresh_upgrades_to_check_free_v3_not_historical_v2() {
        // This deliberately guards the executable dispatch, not just the already
        // present v3 SQL file: a v3 file alone did not make Upgrade use it.
        let source = include_str!("db.rs");
        let dispatch = source
            .split_once("async fn upgrade_identity_schema(")
            .unwrap()
            .1
            .split_once("\npub trait AdmissionStore")
            .unwrap()
            .0;
        assert!(dispatch.contains("TestMigrationMode::Upgrade => upgrade_authorized_with_probe"));
        assert!(source.contains("_ => upgrade_fresh_v3(probe).await"));
        assert!(
            dispatch.contains("TestMigrationMode::FixtureV1 | TestMigrationMode::FixturePartialV1")
        );
    }

    #[test]
    fn fresh_v3_fixed_baseline_has_exactly_eleven_create_only_statements() {
        let statements = v3_create_statements().unwrap();
        assert_eq!(statements.len(), 11);
        assert_eq!(statements[0].0, "schema_meta");
        assert_eq!(
            statements
                .iter()
                .filter(|(name, _)| *name == "users")
                .count(),
            1
        );
        assert!(statements.iter().all(|(name, sql)| {
            TABLES.iter().any(|(known, _, _)| known == name)
                && sql.starts_with(&format!("CREATE TABLE IF NOT EXISTS {name} ("))
                && !sql.contains("CHECK (")
                && !sql.contains("FOREIGN KEY")
        }));
    }

    #[tokio::test]
    async fn upgrade_fresh_v3_fake_records_closed_checks_and_no_legacy_writes() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct State {
            version: Option<i32>,
            tables: Vec<String>,
            calls: Vec<&'static str>,
            creates: usize,
            inserts: usize,
            fail_version: bool,
            fail_tables: bool,
            fail_at_create: Option<usize>,
            stale_metadata: bool,
            fail_empty: bool,
            fail_shape: bool,
            fail_fk: bool,
            fail_insert: bool,
        }
        #[derive(Default)]
        struct Fake(Mutex<State>);
        impl IdentitySchemaProbe for Fake {
            async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("version");
                if state.fail_version {
                    return Err(ControllerError::Config(
                        "version metadata unavailable".into(),
                    ));
                }
                Ok(state.version)
            }
            async fn validate_v2_shape(&self) -> Result<(), ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("shape");
                if state.fail_shape {
                    Err(ControllerError::Config("shape".into()))
                } else {
                    Ok(())
                }
            }
            #[cfg(test)]
            async fn validate_legacy_v2_shape(&self) -> Result<(), ControllerError> {
                panic!("historical shape is not part of Upgrade")
            }
            async fn validate_data(&self) -> Result<(), ControllerError> {
                self.0.lock().unwrap().calls.push("data");
                Ok(())
            }
            async fn validate_fk(&self) -> Result<(), ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("fk");
                if state.fail_fk {
                    Err(ControllerError::Config("fk".into()))
                } else {
                    Ok(())
                }
            }
        }
        impl FreshIdentityMigrationProbe for Fake {
            async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("tables");
                if state.fail_tables {
                    return Err(ControllerError::Config("table metadata unavailable".into()));
                }
                let mut actual = state.tables.clone();
                if state.stale_metadata && state.creates != 0 {
                    actual.pop();
                }
                Ok(actual)
            }
            async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
                let (name, _) = v3_create_statements()?
                    .into_iter()
                    .find(|(_, sql)| *sql == statement)
                    .ok_or_else(|| ControllerError::Config("unwhitelisted CREATE".into()))?;
                let mut state = self.0.lock().unwrap();
                state.calls.push("create");
                state.creates += 1;
                if state.fail_at_create == Some(state.creates) {
                    return Err(ControllerError::Config("CREATE failed".into()));
                }
                state.tables.push(name.to_owned());
                Ok(())
            }
            async fn validate_empty_data(&self) -> Result<(), ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("empty");
                if state.fail_empty {
                    Err(ControllerError::Config("nonempty".into()))
                } else {
                    Ok(())
                }
            }
            async fn insert_v3_meta(&self) -> Result<(), ControllerError> {
                let mut state = self.0.lock().unwrap();
                state.calls.push("insert");
                state.inserts += 1;
                if state.fail_insert {
                    Err(ControllerError::Config("insert uncertain".into()))
                } else {
                    state.version = Some(3);
                    Ok(())
                }
            }
        }
        let fresh = Fake::default();
        upgrade_fresh_v3(&fresh).await.unwrap();
        {
            let state = fresh.0.lock().unwrap();
            assert_eq!(
                (state.version, state.creates, state.inserts),
                (Some(3), 11, 1)
            );
            assert_eq!(&state.calls[..2], &["version", "tables"]);
            assert_eq!(
                &state.calls[24..],
                &[
                    "shape", "fk", "empty", "insert", "version", "shape", "data", "fk"
                ]
            );
        }
        let before = fresh.0.lock().unwrap().calls.len();
        upgrade_fresh_v3(&fresh).await.unwrap();
        {
            let state = fresh.0.lock().unwrap();
            assert_eq!((state.creates, state.inserts), (11, 1));
            assert_eq!(
                &state.calls[before..],
                &["version", "version", "shape", "data", "fk"]
            );
        }

        for version in [Some(1), Some(2), Some(4)] {
            let fake = Fake(Mutex::new(State {
                version,
                ..State::default()
            }));
            let error = upgrade_fresh_v3(&fake).await.unwrap_err();
            assert!(matches!(error, ControllerError::Config(_)));
            let state = fake.0.lock().unwrap();
            assert_eq!((state.creates, state.inserts), (0, 0));
            assert_eq!(state.calls, ["version"]);
        }
        for state in [
            State {
                fail_version: true,
                ..State::default()
            },
            State {
                fail_tables: true,
                ..State::default()
            },
            State {
                tables: vec!["schema_meta".into()],
                ..State::default()
            },
            State {
                fail_at_create: Some(3),
                ..State::default()
            },
            State {
                stale_metadata: true,
                ..State::default()
            },
            State {
                fail_shape: true,
                ..State::default()
            },
            State {
                fail_fk: true,
                ..State::default()
            },
            State {
                fail_empty: true,
                ..State::default()
            },
            State {
                fail_insert: true,
                ..State::default()
            },
        ] {
            let fake = Fake(Mutex::new(state));
            let error = upgrade_fresh_v3(&fake).await.unwrap_err();
            let state = fake.0.lock().unwrap();
            assert_eq!(state.inserts, if state.fail_insert { 1 } else { 0 });
            if state.fail_at_create.is_some() {
                assert_eq!(
                    state.tables.len(),
                    2,
                    "failed nontransactional CREATE is not rolled back"
                );
            }
            if state.fail_insert {
                assert!(format!("{error}").contains("uncertain"));
                assert_ne!(
                    state.calls.last(),
                    Some(&"insert"),
                    "uncertain insert must re-read"
                );
            }
        }
    }

    #[test]
    fn historical_fixtures_never_write_v2_version() {
        let body = include_str!("db.rs")
            .split_once("async fn create_historical_v1_fixture(")
            .unwrap()
            .1
            .split_once("\npub trait AdmissionStore")
            .unwrap()
            .0;
        assert!(!body.contains("SET schema_version = 2"));
        assert!(!body.contains("check_legacy_v2_ready(db).await"));
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

    #[tokio::test]
    async fn readonly_probe_requires_version_shape_data_and_trusted_empty_fk_metadata() {
        use std::sync::Mutex;
        struct Fake {
            version: Option<i32>,
            broken_shape: bool,
            bad_data: bool,
            fk: &'static str,
            calls: Mutex<Vec<&'static str>>,
        }
        impl IdentitySchemaProbe for Fake {
            async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
                self.calls.lock().unwrap().push("version");
                Ok(self.version)
            }
            async fn validate_v2_shape(&self) -> Result<(), ControllerError> {
                self.calls.lock().unwrap().push("shape");
                if self.broken_shape {
                    Err(ControllerError::Config("broken shape".into()))
                } else {
                    Ok(())
                }
            }
            #[cfg(test)]
            async fn validate_legacy_v2_shape(&self) -> Result<(), ControllerError> {
                panic!("startup must never use historical v2 shape")
            }
            async fn validate_data(&self) -> Result<(), ControllerError> {
                self.calls.lock().unwrap().push("data");
                if self.bad_data {
                    Err(ControllerError::Config("polluted identity data".into()))
                } else {
                    Ok(())
                }
            }
            async fn validate_fk(&self) -> Result<(), ControllerError> {
                self.calls.lock().unwrap().push("fk");
                match self.fk {
                    "empty" => Ok(()),
                    "present" => Err(ControllerError::Config(
                        "identity foreign keys present".into(),
                    )),
                    "unknown" => Err(ControllerError::Config(
                        "identity FK metadata unavailable".into(),
                    )),
                    _ => unreachable!(),
                }
            }
        }
        for (version, broken_shape, bad_data, fk, expected_calls, expected_error) in [
            (None, false, false, "empty", &[][..], "not-ready"),
            (Some(1), false, false, "empty", &[][..], "not-ready"),
            (Some(2), false, false, "empty", &[][..], "not-ready"),
            (Some(4), false, false, "empty", &[][..], "unsupported"),
            (
                Some(3),
                true,
                false,
                "empty",
                &["shape"][..],
                "broken shape",
            ),
            (
                Some(3),
                false,
                true,
                "empty",
                &["shape", "data"][..],
                "polluted identity data",
            ),
            (
                Some(3),
                false,
                false,
                "unknown",
                &["shape", "data", "fk"][..],
                "identity FK metadata unavailable",
            ),
            (
                Some(3),
                false,
                false,
                "present",
                &["shape", "data", "fk"][..],
                "identity foreign keys present",
            ),
            (
                Some(3),
                false,
                false,
                "empty",
                &["shape", "data", "fk"][..],
                "ready",
            ),
        ] {
            let fake = Fake {
                version,
                broken_shape,
                bad_data,
                fk,
                calls: Mutex::new(Vec::new()),
            };
            let result = check_identity_schema_with_probe(&fake).await;
            match expected_error {
                "not-ready" => assert!(
                    matches!(result, Err(ControllerError::SchemaNotReady { found, required: 3 }) if found == version)
                ),
                "ready" => assert!(result.is_ok(), "four valid gates must be ready: {result:?}"),
                "unsupported" => assert!(result.is_err()),
                message => assert!(
                    matches!(result, Err(ControllerError::Config(ref found)) if found == message),
                    "{result:?}"
                ),
            }
            let calls = fake.calls.lock().unwrap();
            assert_eq!(calls.first(), Some(&"version"));
            assert_eq!(&calls[1..], expected_calls, "version={version:?} fk={fk}");
        }
    }

    #[tokio::test]
    async fn legacy_v2_probe_accepts_only_version_two_with_complete_historical_shape() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Fake {
            version: Option<i32>,
            shape_valid: bool,
            shape_calls: AtomicUsize,
        }
        impl IdentitySchemaProbe for Fake {
            async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
                Ok(self.version)
            }
            async fn validate_v2_shape(&self) -> Result<(), ControllerError> {
                panic!("startup v3 shape must not be used by legacy runner")
            }
            #[cfg(test)]
            async fn validate_legacy_v2_shape(&self) -> Result<(), ControllerError> {
                self.shape_calls.fetch_add(1, Ordering::SeqCst);
                if self.shape_valid {
                    Ok(())
                } else {
                    Err(ControllerError::Config(
                        "incomplete historical v2 shape".into(),
                    ))
                }
            }
            async fn validate_data(&self) -> Result<(), ControllerError> {
                panic!("legacy runner must not scan v3 data")
            }
            async fn validate_fk(&self) -> Result<(), ControllerError> {
                panic!("legacy v2 terminal check must not use the startup FK gate")
            }
        }
        for (version, shape_valid, expected_calls, accepted) in [
            (None, true, 0, false),
            (Some(1), true, 0, false),
            (Some(3), true, 0, false),
            (Some(2), false, 1, false),
            (Some(2), true, 1, true),
        ] {
            let fake = Fake {
                version,
                shape_valid,
                shape_calls: AtomicUsize::new(0),
            };
            assert_eq!(check_legacy_v2_ready(&fake).await.is_ok(), accepted);
            assert_eq!(fake.shape_calls.load(Ordering::SeqCst), expected_calls);
        }
    }

    #[test]
    fn empty_and_legacy_versions_are_not_ready_without_ddl() {
        for version in [None, Some(1), Some(2)] {
            assert!(
                matches!(identity_schema_decision(version), Err(ControllerError::SchemaNotReady { found, required: 3 }) if found == version)
            );
        }
        assert!(identity_schema_decision(Some(3)).is_ok());
        for version in [Some(0), Some(4), Some(-1)] {
            assert!(identity_schema_decision(version).is_err());
        }
    }

    #[test]
    fn test_migration_needs_all_independent_confirmations() {
        let mut config =
            TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
        assert!(authorize_test_migration(&config, "test_identity").is_err());
        config.allow_destructive = true;
        assert!(authorize_test_migration(&config, "test_identity").is_err());
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(authorize_test_migration(&config, "production").is_err());
        assert!(authorize_test_migration(&config, "test_identity").is_ok());
    }

    #[test]
    fn explicitly_authorized_unprefixed_dev_database_is_allowed() {
        let mut config = TestMigrationConfig::fixture(
            "mysql://fixture/development_identity",
            "development_identity",
        );
        config.allow_destructive = true;
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(config.authorize("development_identity").is_ok());
        assert!(config.authorize("development_other").is_err());
        config.expected_database = "development-identity".into();
        assert!(config.authorize("development-identity").is_ok());
        assert!(config.authorize("development_identity").is_err());
        config.expected_database = "development_identity".into();
        for invalid in [
            TestMigrationConfig {
                allow_destructive: false,
                ..config.clone()
            },
            TestMigrationConfig {
                backup_ref: String::new(),
                ..config.clone()
            },
            TestMigrationConfig {
                migration_ack: String::new(),
                ..config.clone()
            },
            TestMigrationConfig {
                test_url: String::new(),
                ..config.clone()
            },
        ] {
            assert!(invalid.authorize("development_identity").is_err());
        }
        for invalid_name in ["", " development_identity", "développement"] {
            let invalid = TestMigrationConfig {
                expected_database: invalid_name.into(),
                ..config.clone()
            };
            assert!(invalid.authorize(invalid_name).is_err());
        }
    }

    #[test]
    fn test_migration_database_name_accepts_64_ascii_chars_but_rejects_65() {
        let name_64 = "a".repeat(64);
        let name_65 = "a".repeat(65);
        let mut config = TestMigrationConfig::fixture("mysql://fixture/dev", &name_64);
        config.allow_destructive = true;
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(config.authorize(&name_64).is_ok());
        config.expected_database = name_65.clone();
        assert!(config.authorize(&name_65).is_err());
    }

    #[test]
    fn test_migration_config_debug_hides_fake_password() {
        let mut config = TestMigrationConfig::fixture(
            "mysql://fixture:fake-password-for-debug@localhost/test_identity",
            "test_identity",
        );
        config.backup_ref = "fake-backup-reference".into();
        let output = format!("{config:?}");
        for sensitive in [
            "fake-password-for-debug",
            "test_identity",
            "fake-backup-reference",
        ] {
            assert!(!output.contains(sensitive));
        }
    }

    const LEGACY_CHECK_ORDER: [(&str, &str); 5] = [
        ("schema_meta", "chk_schema_singleton"),
        ("devices", "chk_devices_state"),
        ("devices", "chk_devices_decision"),
        ("grants", "chk_grants_source"),
        ("grants", "chk_grants_scope"),
    ];

    struct LegacyUpgradeFake(std::sync::Mutex<LegacyUpgradeFakeState>);
    struct LegacyUpgradeFakeState {
        version: i32,
        columns: [bool; 11],
        checks: Vec<(&'static str, &'static str)>,
        events: Vec<String>,
        fault_gate: Option<&'static str>,
        fail_ddl_at: Option<(usize, bool)>,
        noop_drop: bool,
        drift_on_read: Option<usize>,
        cas_rows: u64,
        cas_updates_version: bool,
        cas_corrupt_shape: bool,
        cas_error: bool,
        read_version_reply: Option<Option<i32>>,
    }
    impl LegacyUpgradeFake {
        fn new(version: i32) -> Self {
            Self(std::sync::Mutex::new(LegacyUpgradeFakeState {
                version,
                columns: [version == 1; 11],
                checks: LEGACY_CHECK_ORDER.to_vec(),
                events: Vec::new(),
                fault_gate: None,
                fail_ddl_at: None,
                noop_drop: false,
                drift_on_read: None,
                cas_rows: 1,
                cas_updates_version: true,
                cas_corrupt_shape: false,
                cas_error: false,
                read_version_reply: None,
            }))
        }
        fn gate(&self, name: &'static str) -> Result<(), ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push(name.into());
            if state.fault_gate == Some(name) {
                return Err(ControllerError::Config("simulated gate failure".into()));
            }
            Ok(())
        }
        fn events(&self) -> Vec<String> {
            self.0.lock().unwrap().events.clone()
        }
        fn writes(&self) -> Vec<String> {
            self.events()
                .into_iter()
                .filter(|event| {
                    event.starts_with("ALTER ") || event.starts_with("DROP ") || event == "cas"
                })
                .collect()
        }
    }
    impl LegacyPreflightProbe for LegacyUpgradeFake {
        async fn read_legacy_meta(&self) -> Result<Vec<(i8, i32)>, ControllerError> {
            self.gate("meta")?;
            let mut state = self.0.lock().unwrap();
            let count = state.events.iter().filter(|event| *event == "meta").count();
            if state.drift_on_read == Some(count) {
                state.checks.clear();
            }
            Ok(vec![(1, state.version)])
        }
        async fn legacy_shape(
            &self,
            _: i32,
        ) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
            self.gate("shape")?;
            Ok(self.0.lock().unwrap().checks.clone())
        }
        async fn no_foreign_keys(&self) -> Result<(), ControllerError> {
            self.gate("fk")
        }
        async fn identity_rows(&self, _: i32) -> Result<(), ControllerError> {
            self.gate("data")
        }
        async fn column_status(&self, _: i32) -> Result<Vec<bool>, ControllerError> {
            self.gate("status")?;
            Ok(self.0.lock().unwrap().columns.to_vec())
        }
        async fn negative(&self, _: &'static str) -> Result<bool, ControllerError> {
            self.gate("negative")?;
            Ok(false)
        }
        async fn username_encoding(&self) -> Result<(), ControllerError> {
            self.gate("encoding")
        }
        async fn username_collision(&self) -> Result<(), ControllerError> {
            self.gate("collision")
        }
    }
    impl LegacyUpgradeProbe for LegacyUpgradeFake {
        async fn alter(&self, statement: &'static str) -> Result<(), ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push(format!("ALTER {statement}"));
            let ordinal = state
                .events
                .iter()
                .filter(|s| s.starts_with("ALTER ") || s.starts_with("DROP "))
                .count();
            if state.fail_ddl_at == Some((ordinal, false)) {
                return Err(ControllerError::Config("simulated before ALTER".into()));
            }
            let index = IDENTITY_COLUMNS
                .iter()
                .position(|entry| entry.3 == statement)
                .unwrap();
            state.columns[index] = false;
            if state.fail_ddl_at == Some((ordinal, true)) {
                return Err(ControllerError::Config("simulated after ALTER".into()));
            }
            Ok(())
        }
        async fn drop_known_check(
            &self,
            table: &'static str,
            name: &'static str,
        ) -> Result<(), ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push(format!("DROP {table}.{name}"));
            let ordinal = state
                .events
                .iter()
                .filter(|s| s.starts_with("ALTER ") || s.starts_with("DROP "))
                .count();
            if state.fail_ddl_at == Some((ordinal, false)) {
                return Err(ControllerError::Config("simulated before DROP".into()));
            }
            if !state.noop_drop {
                state.checks.retain(|entry| *entry != (table, name));
            }
            if state.fail_ddl_at == Some((ordinal, true)) {
                return Err(ControllerError::Config("simulated after DROP".into()));
            }
            Ok(())
        }
        async fn strict_v3_shape_without_meta_update(&self) -> Result<(), ControllerError> {
            self.gate("strict")
        }
        async fn cas_version(&self, from: i32, to: i32) -> Result<u64, ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push("cas".into());
            assert_eq!((from, to), (state.version, 3));
            if state.cas_error {
                state.version = 3; // Unknown result: even an applied CAS is not success.
                return Err(ControllerError::Config("simulated uncertain CAS".into()));
            }
            if state.cas_rows == 1 && state.cas_updates_version {
                state.version = to;
            }
            if state.cas_rows == 1 && state.cas_corrupt_shape {
                state.columns[0] = true;
            }
            Ok(state.cas_rows)
        }
        async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
            self.gate("read_version")?;
            let state = self.0.lock().unwrap();
            Ok(state.read_version_reply.unwrap_or(Some(state.version)))
        }
        async fn ready_v3(&self) -> Result<(), ControllerError> {
            self.gate("ready")?;
            self.gate("ready_version")?;
            if self.0.lock().unwrap().version != 3 {
                return Err(legacy_preflight_error("ready.version"));
            }
            self.gate("ready_shape")?;
            {
                let state = self.0.lock().unwrap();
                if state.columns.iter().any(|old| *old) || !state.checks.is_empty() {
                    return Err(legacy_preflight_error("ready.shape"));
                }
            }
            self.gate("ready_data")?;
            self.gate("ready_fk")
        }
    }

    impl IdentitySchemaProbe for LegacyUpgradeFake {
        async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
            LegacyUpgradeProbe::read_version(self).await
        }
        async fn validate_v2_shape(&self) -> Result<(), ControllerError> {
            assert_eq!(self.0.lock().unwrap().version, 3);
            self.gate("fresh_shape")
        }
        async fn validate_legacy_v2_shape(&self) -> Result<(), ControllerError> {
            panic!("legacy route must not use historical version-2 startup")
        }
        async fn validate_data(&self) -> Result<(), ControllerError> {
            assert_eq!(self.0.lock().unwrap().version, 3);
            self.gate("fresh_data")
        }
        async fn validate_fk(&self) -> Result<(), ControllerError> {
            assert_eq!(self.0.lock().unwrap().version, 3);
            self.gate("fresh_fk")
        }
    }
    impl FreshIdentityMigrationProbe for LegacyUpgradeFake {
        async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
            self.gate("fresh_tables")?;
            Ok(vec!["unversioned".to_owned()])
        }
        async fn create_table(&self, _: &str) -> Result<(), ControllerError> {
            panic!("legacy route must not create fresh tables")
        }
        async fn validate_empty_data(&self) -> Result<(), ControllerError> {
            panic!("legacy route must not use fresh row scan")
        }
        async fn insert_v3_meta(&self) -> Result<(), ControllerError> {
            panic!("legacy route must not insert fresh metadata")
        }
    }

    #[tokio::test]
    async fn authorized_upgrade_routes_legacy_versions_to_resumable_probe() {
        for version in [1, 2] {
            let fake = LegacyUpgradeFake::new(version);
            {
                let mut state = fake.0.lock().unwrap();
                state.columns = [false; 11];
                state.checks.clear();
            }
            upgrade_authorized_with_probe(&fake).await.unwrap();
            assert_eq!(fake.writes(), vec!["cas"]);
            assert_eq!(fake.0.lock().unwrap().version, 3);
            assert_eq!(fake.events()[0], "read_version");
        }
    }

    #[tokio::test]
    async fn authorized_upgrade_v3_is_readonly_and_unknown_version_is_rejected() {
        let ready = LegacyUpgradeFake::new(3);
        {
            let mut state = ready.0.lock().unwrap();
            state.columns = [false; 11];
            state.checks.clear();
        }
        upgrade_authorized_with_probe(&ready).await.unwrap();
        assert!(ready.writes().is_empty());
        assert_eq!(
            ready.events(),
            vec![
                "read_version",
                "read_version",
                "read_version",
                "fresh_shape",
                "fresh_data",
                "fresh_fk"
            ]
        );
        let unknown = LegacyUpgradeFake::new(4);
        assert!(upgrade_authorized_with_probe(&unknown).await.is_err());
        assert_eq!(unknown.events(), vec!["read_version", "read_version"]);
        assert!(unknown.writes().is_empty());
        let unversioned = LegacyUpgradeFake::new(1);
        unversioned.0.lock().unwrap().read_version_reply = Some(None);
        assert!(upgrade_authorized_with_probe(&unversioned).await.is_err());
        assert_eq!(
            unversioned.events(),
            vec!["read_version", "read_version", "fresh_tables"]
        );
        assert!(unversioned.writes().is_empty());
    }

    #[test]
    fn sqlx_legacy_adapter_has_fixed_ddl_and_narrow_version_cas() {
        let source = include_str!("db.rs");
        let adapter = source
            .split_once("impl LegacyUpgradeProbe for DbPool {")
            .map_or("", |(_, rest)| {
                rest.split_once("\ntrait LegacyUpgradeProbe").unwrap().0
            });
        assert!(adapter.contains("IDENTITY_COLUMNS.iter().any(|entry| entry.3 == statement)"));
        assert!(adapter.contains("legacy_check_drop_sql(table, name)?"));
        assert!(adapter.contains(
            "UPDATE schema_meta SET schema_version = 3 WHERE singleton = 1 AND schema_version = ?"
        ));
        assert!(adapter.contains(".bind(from)"));
        assert!(adapter.contains("result.rows_affected()"));
        assert!(adapter.contains("validate_v3_schema_shape(self).await?"));
        assert!(adapter.contains("require_no_identity_foreign_keys(self).await?"));
        assert!(adapter.contains("preflight_legacy_for_v3(self, version).await?"));
        assert!(adapter.contains("check_identity_schema(self).await"));
        assert!(!adapter.contains("format!(\"ALTER"));
        assert!(!adapter.contains("DROP DATABASE"));
        assert!(!adapter.contains("tidb_enable_check_constraint"));
    }

    #[test]
    fn legacy_drop_templates_are_fixed_whitelisted_sql() {
        assert_eq!(LEGACY_CHECK_DROPS.len(), LEGACY_CHECK_ORDER.len());
        for (index, &(table, name, statement)) in LEGACY_CHECK_DROPS.iter().enumerate() {
            assert_eq!((table, name), LEGACY_CHECK_ORDER[index]);
            assert_eq!(statement, format!("ALTER TABLE {table} DROP CHECK {name}"));
            assert_eq!(legacy_check_drop_sql(table, name).unwrap(), statement);
            assert!(!statement.contains("DROP DATABASE"));
            assert!(!statement.contains("GLOBAL"));
        }
        assert!(legacy_check_drop_sql("devices", "unknown").is_err());
        assert!(legacy_check_drop_sql("other", "chk_devices_state").is_err());
    }

    #[tokio::test]
    async fn legacy_upgrade_only_writes_fixed_remaining_steps_and_reads_every_boundary() {
        for version in [1, 2] {
            let fake = LegacyUpgradeFake::new(version);
            {
                let mut state = fake.0.lock().unwrap();
                state.columns = [false; 11];
                if version == 1 {
                    state.columns[1] = true;
                    state.columns[10] = true;
                }
                state.checks = vec![
                    LEGACY_CHECK_ORDER[4],
                    LEGACY_CHECK_ORDER[1],
                    LEGACY_CHECK_ORDER[0],
                ];
            }
            upgrade_legacy_with_probe(&fake, version).await.unwrap();
            let mut expected = Vec::new();
            if version == 1 {
                expected.push(format!("ALTER {}", IDENTITY_COLUMNS[1].3));
                expected.push(format!("ALTER {}", IDENTITY_COLUMNS[10].3));
            }
            for (table, name) in [
                LEGACY_CHECK_ORDER[0],
                LEGACY_CHECK_ORDER[1],
                LEGACY_CHECK_ORDER[4],
            ] {
                expected.push(format!("DROP {table}.{name}"));
            }
            expected.push("cas".into());
            assert_eq!(fake.writes(), expected);
            let events = fake.events();
            let preflight_reads = events.iter().filter(|event| *event == "meta").count();
            assert_eq!(preflight_reads, 2 + 2 * (expected.len() - 1));
            for index in 0..events.len() {
                if events[index].starts_with("ALTER ") || events[index].starts_with("DROP ") {
                    assert_eq!(events[index - 1], "collision");
                    assert_eq!(events[index + 1], "meta");
                }
            }
            assert!(events.ends_with(&[
                "strict".into(),
                "cas".into(),
                "read_version".into(),
                "ready".into(),
                "ready_version".into(),
                "ready_shape".into(),
                "ready_data".into(),
                "ready_fk".into(),
            ]));
            assert_eq!(fake.0.lock().unwrap().version, 3);
        }
        let full = LegacyUpgradeFake::new(1);
        full.0.lock().unwrap().checks.clear();
        upgrade_legacy_with_probe(&full, 1).await.unwrap();
        assert_eq!(full.writes().len(), IDENTITY_COLUMNS.len() + 1);
        assert_eq!(
            full.events()
                .iter()
                .filter(|event| *event == "meta")
                .count(),
            2 + 2 * IDENTITY_COLUMNS.len()
        );
        for (index, &(_, _, _, statement)) in IDENTITY_COLUMNS.iter().enumerate() {
            assert_eq!(full.writes()[index], format!("ALTER {statement}"));
        }
        let empty = LegacyUpgradeFake::new(2);
        empty.0.lock().unwrap().checks.clear();
        upgrade_legacy_with_probe(&empty, 2).await.unwrap();
        assert_eq!(empty.writes(), vec!["cas"]);
    }

    #[tokio::test]
    async fn legacy_upgrade_fails_closed_before_writes_on_bad_gates_and_drift() {
        for gate in [
            "meta",
            "shape",
            "fk",
            "data",
            "status",
            "negative",
            "encoding",
            "collision",
        ] {
            let fake = LegacyUpgradeFake::new(1);
            fake.0.lock().unwrap().fault_gate = Some(gate);
            assert!(upgrade_legacy_with_probe(&fake, 1).await.is_err(), "{gate}");
            assert!(fake.writes().is_empty(), "{gate}");
        }
        for version in [0, 3] {
            let fake = LegacyUpgradeFake::new(version);
            assert!(upgrade_legacy_with_probe(&fake, version).await.is_err());
            assert!(fake.events().is_empty());
        }
        let v2_old = LegacyUpgradeFake::new(2);
        v2_old.0.lock().unwrap().columns[0] = true;
        assert!(upgrade_legacy_with_probe(&v2_old, 2).await.is_err());
        assert!(v2_old.writes().is_empty());
        for checks in [
            vec![("outsider", "unknown")],
            vec![LEGACY_CHECK_ORDER[0]; 2],
        ] {
            let fake = LegacyUpgradeFake::new(2);
            fake.0.lock().unwrap().checks = checks;
            assert!(upgrade_legacy_with_probe(&fake, 2).await.is_err());
            assert!(fake.writes().is_empty());
        }
        let drift = LegacyUpgradeFake::new(1);
        drift.0.lock().unwrap().drift_on_read = Some(2);
        assert!(upgrade_legacy_with_probe(&drift, 1).await.is_err());
        assert!(drift.writes().is_empty());
        for (version, read_at, expected_writes) in [(1, 3, 1), (2, 2, 0), (2, 3, 1)] {
            let drift = LegacyUpgradeFake::new(version);
            drift.0.lock().unwrap().drift_on_read = Some(read_at);
            assert!(upgrade_legacy_with_probe(&drift, version).await.is_err());
            assert_eq!(drift.writes().len(), expected_writes);
            assert_eq!(drift.0.lock().unwrap().version, version);
        }
    }

    #[tokio::test]
    async fn legacy_upgrade_nontransactional_errors_are_retryable_but_not_success() {
        for (version, first_drop) in [(1, false), (2, true)] {
            for ordinal in [1, 2] {
                for applied in [false, true] {
                    let fake = LegacyUpgradeFake::new(version);
                    if !first_drop {
                        let mut state = fake.0.lock().unwrap();
                        state.columns = [false; 11];
                        state.columns[0] = true;
                        state.columns[1] = true;
                    }
                    fake.0.lock().unwrap().fail_ddl_at = Some((ordinal, applied));
                    assert!(upgrade_legacy_with_probe(&fake, version).await.is_err());
                    assert_eq!(fake.0.lock().unwrap().version, version);
                    assert_eq!(fake.writes().len(), ordinal);
                    assert_eq!(
                        fake.events()
                            .iter()
                            .filter(|event| *event == "meta")
                            .count(),
                        1 + 2 * ordinal
                    );
                    fake.0.lock().unwrap().fail_ddl_at = None;
                    upgrade_legacy_with_probe(&fake, version).await.unwrap();
                    let total_ddl = if first_drop { 5 } else { 7 };
                    let already_applied = ordinal - 1 + usize::from(applied);
                    assert_eq!(
                        fake.writes().len() - ordinal,
                        total_ddl - already_applied + 1
                    );
                    assert_eq!(fake.0.lock().unwrap().version, 3);
                }
            }
        }
        let noop = LegacyUpgradeFake::new(2);
        noop.0.lock().unwrap().noop_drop = true;
        assert!(upgrade_legacy_with_probe(&noop, 2).await.is_err());
        assert_eq!(noop.writes(), vec!["DROP schema_meta.chk_schema_singleton"]);
        assert_eq!(
            noop.events()
                .iter()
                .filter(|event| *event == "meta")
                .count(),
            3
        );
        assert_eq!(noop.0.lock().unwrap().version, 2);
    }

    #[tokio::test]
    async fn legacy_upgrade_cas_and_post_cas_failures_never_claim_success() {
        for rows in [0, 2] {
            let fake = LegacyUpgradeFake::new(2);
            {
                let mut state = fake.0.lock().unwrap();
                state.checks.clear();
                state.cas_rows = rows;
            }
            assert!(upgrade_legacy_with_probe(&fake, 2).await.is_err());
            assert_eq!(fake.0.lock().unwrap().version, 2);
            assert!(fake.events().contains(&"read_version".into()));
        }
        let uncertain = LegacyUpgradeFake::new(2);
        uncertain.0.lock().unwrap().checks.clear();
        uncertain.0.lock().unwrap().cas_error = true;
        assert!(upgrade_legacy_with_probe(&uncertain, 2).await.is_err());
        assert_eq!(uncertain.0.lock().unwrap().version, 3);
        assert!(uncertain.events().contains(&"read_version".into()));
        let ready = LegacyUpgradeFake::new(2);
        ready.0.lock().unwrap().checks.clear();
        ready.0.lock().unwrap().fault_gate = Some("ready");
        assert!(upgrade_legacy_with_probe(&ready, 2).await.is_err());
        assert_eq!(ready.0.lock().unwrap().version, 3);
        let strict = LegacyUpgradeFake::new(2);
        strict.0.lock().unwrap().checks.clear();
        strict.0.lock().unwrap().fault_gate = Some("strict");
        assert!(upgrade_legacy_with_probe(&strict, 2).await.is_err());
        assert!(strict.writes().is_empty());
    }

    #[tokio::test]
    async fn legacy_upgrade_rejects_cas_ok_one_when_version_remains_legacy() {
        for version in [1, 2] {
            let fake = LegacyUpgradeFake::new(version);
            {
                let mut state = fake.0.lock().unwrap();
                state.columns = [false; 11];
                state.checks.clear();
                state.cas_rows = 1;
                state.cas_updates_version = false;
            }
            // The original permissive ready_v3 let this pass during RED;
            // the state machine must not infer version 3 from rowcount alone.
            let result = upgrade_legacy_with_probe(&fake, version).await;
            assert!(
                matches!(result, Err(ControllerError::Config(ref message)) if message == "legacy version CAS outcome uncertain"),
                "version {version}: {result:?}"
            );
            assert_eq!(fake.0.lock().unwrap().version, version);
            assert!(fake.events().contains(&"read_version".into()));
            assert!(!fake.events().contains(&"ready".into()));
        }
    }

    #[tokio::test]
    async fn legacy_upgrade_rejects_unknown_post_cas_version_without_ready() {
        for reply in [Some(1), Some(2), None] {
            let fake = LegacyUpgradeFake::new(2);
            {
                let mut state = fake.0.lock().unwrap();
                state.checks.clear();
                state.read_version_reply = Some(reply);
            }
            let result = upgrade_legacy_with_probe(&fake, 2).await;
            assert!(
                matches!(result, Err(ControllerError::Config(ref message)) if message == "legacy version CAS outcome uncertain"),
                "reply {reply:?}: {result:?}"
            );
            assert_eq!(fake.0.lock().unwrap().version, 3);
            assert!(!fake.events().contains(&"ready".into()));
        }
        let read_failure = LegacyUpgradeFake::new(2);
        {
            let mut state = read_failure.0.lock().unwrap();
            state.checks.clear();
            state.fault_gate = Some("read_version");
        }
        let result = upgrade_legacy_with_probe(&read_failure, 2).await;
        assert!(
            matches!(result, Err(ControllerError::Config(ref message)) if message == "legacy version CAS outcome uncertain")
        );
        assert!(!read_failure.events().contains(&"ready".into()));
    }

    #[tokio::test]
    async fn legacy_upgrade_rejects_post_cas_shape_data_and_fk_failures() {
        for gate in ["ready_shape", "ready_data", "ready_fk"] {
            let fake = LegacyUpgradeFake::new(2);
            {
                let mut state = fake.0.lock().unwrap();
                state.checks.clear();
                state.fault_gate = Some(gate);
            }
            assert!(upgrade_legacy_with_probe(&fake, 2).await.is_err(), "{gate}");
            assert_eq!(fake.0.lock().unwrap().version, 3);
            assert!(fake.events().contains(&gate.into()));
        }
        let shape_drift = LegacyUpgradeFake::new(2);
        {
            let mut state = shape_drift.0.lock().unwrap();
            state.checks.clear();
            state.cas_corrupt_shape = true;
        }
        assert!(upgrade_legacy_with_probe(&shape_drift, 2).await.is_err());
        assert_eq!(shape_drift.0.lock().unwrap().version, 3);
        assert!(shape_drift.events().contains(&"ready_shape".into()));
    }

    #[tokio::test]
    async fn legacy_preflight_orders_all_select_gates_and_never_calls_a_writer() {
        use std::sync::Mutex;
        struct Fake {
            meta: Vec<(i8, i32)>,
            status: Vec<bool>,
            fault: Option<&'static str>,
            negative_at: Option<usize>,
            checks: Vec<(&'static str, &'static str)>,
            calls: Mutex<Vec<&'static str>>,
            writes: Mutex<usize>,
        }
        impl Fake {
            fn clean(version: i32) -> Self {
                Self {
                    meta: vec![(1, version)],
                    status: vec![version == 1; IDENTITY_COLUMNS.len()],
                    fault: None,
                    negative_at: None,
                    checks: vec![("devices", "chk_devices_state")],
                    calls: Mutex::new(Vec::new()),
                    writes: Mutex::new(0),
                }
            }
            fn step(&self, name: &'static str) -> Result<(), ControllerError> {
                self.calls.lock().unwrap().push(name);
                if self.fault == Some(name) {
                    Err(ControllerError::Config("simulated metadata failure".into()))
                } else {
                    Ok(())
                }
            }
            fn write(&self) {
                *self.writes.lock().unwrap() += 1;
            }
        }
        impl LegacyPreflightProbe for Fake {
            async fn read_legacy_meta(&self) -> Result<Vec<(i8, i32)>, ControllerError> {
                self.step("meta")?;
                Ok(self.meta.clone())
            }
            async fn legacy_shape(
                &self,
                _: i32,
            ) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
                self.step("shape")?;
                Ok(self.checks.clone())
            }
            async fn no_foreign_keys(&self) -> Result<(), ControllerError> {
                self.step("fk")
            }
            async fn identity_rows(&self, _: i32) -> Result<(), ControllerError> {
                self.step("data")
            }
            async fn column_status(&self, _: i32) -> Result<Vec<bool>, ControllerError> {
                self.step("status")?;
                Ok(self.status.clone())
            }
            async fn negative(&self, query: &'static str) -> Result<bool, ControllerError> {
                self.step("negative")?;
                Ok(self
                    .negative_at
                    .is_some_and(|i| IDENTITY_COLUMNS[i].2 == query))
            }
            async fn username_encoding(&self) -> Result<(), ControllerError> {
                self.step("encoding")
            }
            async fn username_collision(&self) -> Result<(), ControllerError> {
                self.step("collision")
            }
        }
        for version in [1, 2] {
            let good = Fake::clean(version);
            let status = preflight_legacy_with_probe(&good, version).await.unwrap();
            assert_eq!(status.old_columns.len(), 11);
            assert_eq!(status.old_columns, [version == 1; 11]);
            assert_eq!(
                status.observed_checks,
                vec![("devices", "chk_devices_state")]
            );
            let calls = good.calls.lock().unwrap().clone();
            assert_eq!(&calls[..5], &["meta", "shape", "fk", "data", "status"]);
            assert_eq!(&calls[calls.len() - 2..], &["encoding", "collision"]);
            assert_eq!(
                calls.iter().filter(|&&name| name == "negative").count(),
                if version == 1 { 10 } else { 0 }
            );
            assert_eq!(*good.writes.lock().unwrap(), 0);

            let empty_checks = Fake {
                checks: vec![],
                ..Fake::clean(version)
            };
            assert!(
                preflight_legacy_with_probe(&empty_checks, version)
                    .await
                    .unwrap()
                    .observed_checks
                    .is_empty()
            );
            for checks in [
                vec![("unknown_table", "chk_looks_known")],
                vec![("devices", "chk_devices_state"); 2],
            ] {
                let untrusted = Fake {
                    checks,
                    ..Fake::clean(version)
                };
                assert!(
                    preflight_legacy_with_probe(&untrusted, version)
                        .await
                        .is_err()
                );
                assert_eq!(&*untrusted.calls.lock().unwrap(), &["meta", "shape"]);
                assert_eq!(*untrusted.writes.lock().unwrap(), 0);
            }
            for meta in [
                vec![],
                vec![(0, version)],
                vec![(1, version + 1)],
                vec![(1, version), (1, version)],
            ] {
                let bad = Fake {
                    meta,
                    ..Fake::clean(version)
                };
                assert!(preflight_legacy_with_probe(&bad, version).await.is_err());
                assert_eq!(&*bad.calls.lock().unwrap(), &["meta"]);
                assert_eq!(*bad.writes.lock().unwrap(), 0);
            }
            for fault in [
                "meta",
                "shape",
                "fk",
                "data",
                "status",
                "encoding",
                "collision",
            ] {
                let bad = Fake {
                    fault: Some(fault),
                    ..Fake::clean(version)
                };
                assert!(
                    preflight_legacy_with_probe(&bad, version).await.is_err(),
                    "{fault}"
                );
                assert_eq!(bad.calls.lock().unwrap().last(), Some(&fault));
                assert_eq!(*bad.writes.lock().unwrap(), 0);
            }
            for length in [0, 10, 12] {
                let bad = Fake {
                    status: vec![false; length],
                    ..Fake::clean(version)
                };
                assert!(preflight_legacy_with_probe(&bad, version).await.is_err());
                assert_eq!(
                    &*bad.calls.lock().unwrap(),
                    &["meta", "shape", "fk", "data", "status"]
                );
            }
            if version == 2 {
                let bad = Fake {
                    status: vec![true; 11],
                    ..Fake::clean(version)
                };
                assert!(preflight_legacy_with_probe(&bad, version).await.is_err());
                assert_eq!(
                    &*bad.calls.lock().unwrap(),
                    &["meta", "shape", "fk", "data", "status"]
                );
            } else {
                for i in 0..10 {
                    let bad = Fake {
                        negative_at: Some(i),
                        ..Fake::clean(version)
                    };
                    assert!(
                        preflight_legacy_with_probe(&bad, version).await.is_err(),
                        "negative {i}"
                    );
                    assert_eq!(bad.calls.lock().unwrap().last(), Some(&"negative"));
                    assert_eq!(*bad.writes.lock().unwrap(), 0);
                }
                let bad = Fake {
                    fault: Some("negative"),
                    ..Fake::clean(version)
                };
                assert!(preflight_legacy_with_probe(&bad, version).await.is_err());
                let mut mixed = Fake::clean(version);
                mixed.status = vec![false; 11];
                mixed.status[0] = true;
                assert!(preflight_legacy_with_probe(&mixed, version).await.is_ok());
                assert_eq!(
                    mixed
                        .calls
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|&&name| name == "negative")
                        .count(),
                    1
                );
            }
        }
        for version in [0, 3, -1] {
            let bad = Fake::clean(version);
            assert!(preflight_legacy_with_probe(&bad, version).await.is_err());
            assert!(bad.calls.lock().unwrap().is_empty());
        }
        // A separate writer witness is deliberately never invoked by this read-only entry.
        let fake = Fake::clean(1);
        let _writer: fn(&Fake) = Fake::write;
        assert_eq!(*fake.writes.lock().unwrap(), 0);
    }

    #[test]
    fn preflight_refuses_invalid_and_target_collision() {
        let long = "a".repeat(65);
        for names in [
            &["alice", "Éric"][..],
            &["alice", long.as_str()],
            &["alice", "Alice"],
            &["alice", "alice"],
        ] {
            assert!(
                preflight_usernames(names.iter().copied()).is_err(),
                "{names:?}"
            );
        }
        assert!(preflight_usernames(["alice", "bob_1"]).is_ok());
    }

    #[test]
    fn mixed_identity_columns_are_resumable() {
        let old = ColumnMeta {
            name: "revision".into(),
            data_type: "bigint".into(),
            column_type: "bigint".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        assert!(classify_identity_column("devices", "revision", &old).unwrap());
        let new = ColumnMeta {
            column_type: "bigint unsigned".into(),
            ..old.clone()
        };
        assert!(!classify_identity_column("devices", "revision", &new).unwrap());
        let drift = ColumnMeta {
            data_type: "varchar".into(),
            column_type: "varchar(20)".into(),
            ..old
        };
        assert!(classify_identity_column("devices", "revision", &drift).is_err());
    }

    #[tokio::test]
    async fn unauthorized_command_never_invokes_upgrade() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let config = TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
        let result = run_authorized_migration_with(
            &config,
            || async { Ok(((), "test_identity".into())) },
            |db, _| async move { Ok(db) },
            |_| async {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
    }

    #[tokio::test]
    async fn every_failed_authorization_blocks_connection_or_upgrade() {
        use std::cell::Cell;
        let base = TestMigrationConfig {
            test_url: "mysql://fixture/test_identity".into(),
            allow_destructive: true,
            expected_database: "test_identity".into(),
            backup_ref: "snapshot-42".into(),
            migration_ack: "isolated-exclusive-backed-up-disposable".into(),
        };
        for config in [
            TestMigrationConfig {
                test_url: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                allow_destructive: false,
                ..base.clone()
            },
            TestMigrationConfig {
                backup_ref: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                migration_ack: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                expected_database: String::new(),
                ..base.clone()
            },
        ] {
            let connected = Cell::new(0);
            let upgraded = Cell::new(0);
            assert!(
                run_authorized_migration_with(
                    &config,
                    || async {
                        connected.set(connected.get() + 1);
                        Ok(((), "test_identity".into()))
                    },
                    |db, _| async move { Ok(db) },
                    |_| async {
                        upgraded.set(upgraded.get() + 1);
                        Ok(())
                    }
                )
                .await
                .is_err()
            );
            assert_eq!((connected.get(), upgraded.get()), (0, 0));
        }
        let upgraded = Cell::new(0);
        assert!(
            run_authorized_migration_with(
                &base,
                || async { Ok(((), "test_other".into())) },
                |db, _| async move { Ok(db) },
                |_| async {
                    upgraded.set(upgraded.get() + 1);
                    Ok(())
                }
            )
            .await
            .is_err()
        );
        assert_eq!(upgraded.get(), 0);
        run_authorized_migration_with(
            &base,
            || async { Ok(((), "test_identity".into())) },
            |db, _| async move { Ok(db) },
            |_| async {
                upgraded.set(upgraded.get() + 1);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(upgraded.get(), 1);
    }

    #[tokio::test]
    async fn failed_grants_read_blocks_upgrade_after_actual_database_authorization() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        let config = TestMigrationConfig {
            test_url: "mysql://fixture/test_identity".into(),
            allow_destructive: true,
            expected_database: "test_identity".into(),
            backup_ref: "snapshot-placeholder".into(),
            migration_ack: "isolated-exclusive-backed-up-disposable".into(),
        };
        let result = run_authorized_migration_with(
            &config,
            || async {
                calls.borrow_mut().push("connect_and_database");
                Ok(((), "test_identity".into()))
            },
            |_, actual| {
                calls.borrow_mut().push("read_grants");
                assert_eq!(actual, "test_identity");
                async { Err::<(), _>(schema_metadata_privilege_error()) }
            },
            |_| async {
                calls.borrow_mut().push("upgrade_or_fixture_ddl");
                Ok(())
            },
        )
        .await;
        assert!(matches!(result, Err(ControllerError::Config(_))));
        assert_eq!(*calls.borrow(), ["connect_and_database", "read_grants"]);
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
    fn information_schema_string_projections_are_cast_for_sqlx() {
        // These SQL literals are kept on single source lines; catch new uncast
        // information_schema string projections without connecting to a database.
        let source = include_str!("db.rs");
        let production = source.split_once("\n#[cfg(test)]\nmod tests {").unwrap().0;
        let mut queries = 0;
        let mut string_columns = 0;
        for line in production
            .lines()
            .filter(|line| line.contains("SELECT ") && line.contains("information_schema."))
        {
            queries += 1;
            let projections = line
                .split_once("SELECT ")
                .unwrap()
                .1
                .split_once(" FROM ")
                .unwrap()
                .0;
            for projection in projections.split(", ") {
                if matches!(
                    projection,
                    "COUNT(*)"
                        | "CAST(COUNT(*) AS SIGNED) AS fk_count"
                        | "CAST(non_unique AS SIGNED) AS non_unique"
                        | "CAST(seq_in_index AS SIGNED) AS seq_in_index"
                        | "CAST(sub_part AS SIGNED) AS sub_part"
                ) {
                    continue;
                }
                let column = projection
                    .strip_prefix("CAST(")
                    .and_then(|value| value.split_once(" AS CHAR) AS "))
                    .unwrap_or_else(|| {
                        panic!("uncast information_schema projection: {projection}")
                    });
                assert_eq!(column.0.rsplit('.').next().unwrap(), column.1);
                assert!(matches!(
                    column.1,
                    "table_name"
                        | "table_collation"
                        | "column_name"
                        | "index_name"
                        | "constraint_name"
                        | "check_clause"
                        | "data_type"
                        | "column_type"
                        | "is_nullable"
                        | "character_set_name"
                        | "collation_name"
                        | "column_default"
                ));
                string_columns += 1;
            }
        }
        assert_eq!(queries, 14);
        assert_eq!(string_columns, 19);
    }

    #[test]
    fn statistics_numeric_projections_keep_lowercase_result_names() {
        let projections = STATISTICS_QUERY
            .split_once("SELECT ")
            .unwrap()
            .1
            .split_once(" FROM information_schema.statistics ")
            .unwrap()
            .0;
        assert_eq!(
            projections.split(", ").collect::<Vec<_>>(),
            [
                "CAST(index_name AS CHAR) AS index_name",
                "CAST(column_name AS CHAR) AS column_name",
                "CAST(non_unique AS SIGNED) AS non_unique",
                "CAST(seq_in_index AS SIGNED) AS seq_in_index",
                "CAST(sub_part AS SIGNED) AS sub_part",
            ]
        );
    }

    #[test]
    fn fixed_identity_ddl_exactly_matches_0002() {
        let declared: Vec<_> = IDENTITY_MIGRATION
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let allowed: Vec<_> = IDENTITY_COLUMNS.iter().map(|entry| entry.3).collect();
        assert_eq!(declared, allowed);
    }

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
    fn v3_baseline_matches_v2_columns_and_indexes_without_checks_or_foreign_keys() {
        let baseline = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0003_identity_application_integrity.sql"
        ))
        // A missing file must still fail on the actual v1-vs-v3 contract, not a compile error.
        .unwrap_or_else(|_| MIGRATION.to_owned());
        let mut expected = MIGRATION.to_owned();
        for &(table, column, _, alter) in IDENTITY_COLUMNS {
            let old = if (table, column) == ("users", "username") {
                "VARCHAR(128) NOT NULL"
            } else {
                identity_column_declarations(table, column).unwrap().0
            };
            let new = alter.split_once(" MODIFY COLUMN ").unwrap().1;
            let old_definition = format!("{column} {old}");
            assert!(
                expected.contains(&old_definition),
                "v1 missing {table}.{column}"
            );
            expected = expected.replacen(&old_definition, new, 1);
        }
        expected = expected.replace(
            ", CONSTRAINT chk_schema_singleton CHECK (singleton = 1)",
            "",
        );
        expected = expected
            .lines()
            .filter(|line| !line.trim_start().starts_with("CONSTRAINT "))
            .collect::<Vec<_>>()
            .join("\n");
        expected = expected.replace(
            "(admission_state, review_decision),\n",
            "(admission_state, review_decision)\n",
        );
        expected = expected.replace("(user_id),\n) CHARACTER SET", "(user_id)\n) CHARACTER SET");
        assert!(!baseline.to_ascii_uppercase().contains("FOREIGN KEY"));
        assert!(!baseline.to_ascii_uppercase().contains("CHECK "));
        let statements = |sql: &str| {
            sql.split(';')
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        };
        assert_eq!(statements(&baseline), statements(&expected));
        assert_eq!(statements(&baseline).len(), TABLES.len());
    }

    #[test]
    fn v3_shape_contract_uses_check_free_unsigned_declarations() {
        let parts = ddl_parts("schema_meta");
        assert!(parts.contains(&"authz_epoch BIGINT UNSIGNED NOT NULL DEFAULT 0".to_owned()));
        assert!(!parts.iter().any(|part| part.starts_with("CONSTRAINT ")));
        let users = ddl_parts("users");
        assert!(users.contains(
            &"username VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL".to_owned()
        ));
        assert_eq!(
            expected_indexes(&users, &["PRIMARY", "uq_users_username"])
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn v3_shape_policy_is_check_free_and_keeps_legacy_checks() {
        assert_eq!(
            ddl_parts_from(V3_MIGRATION, "devices"),
            ddl_parts("devices")
        );
        assert!(
            ddl_parts_from(MIGRATION, "devices")
                .iter()
                .any(|part| part.starts_with("CONSTRAINT "))
        );
        for &(table, _, _) in TABLES {
            let parts = ddl_parts(table);
            assert!(!parts.iter().any(|p| p.starts_with("CONSTRAINT ")));
        }
        assert!(
            validate_object_names("devices", "check", &[], &["chk_devices_state".into()]).is_err()
        );
    }

    #[test]
    fn v3_shape_policy_routes_to_v3_baseline_without_legacy_checks() {
        let source = include_str!("db.rs");
        let body = source
            .split_once("async fn validate_schema_shape_with_contract(")
            .unwrap()
            .1
            .split_once("\nasync fn read_column(")
            .unwrap()
            .0;
        assert!(body.contains("ddl_parts(table)"));
        assert!(body.contains("validate_column_shape(table, declaration, &actual)"));
        assert!(body.contains("validate_indexes(table, &expected, &actual)"));
        assert!(
            body.contains(
                "validate_object_names(table, \"check\", &expected_checks, &actual_checks)"
            )
        );
        assert!(
            source.contains(
                "validate_schema_shape_with_contract(db, Some(true), V3_MIGRATION).await"
            )
        );
    }

    #[test]
    fn v3_table_charset_rejects_non_utf8mb4_collation() {
        assert!(validate_v3_table_collation("users", "utf8mb4_0900_ai_ci").is_ok());
        assert!(validate_v3_table_collation("users", "utf8mb4_bin").is_ok());
        assert!(validate_v3_table_collation("users", "latin1_swedish_ci").is_err());
        assert!(validate_v3_table_collation("users", "utf8mb4fake_ci").is_err());
    }

    #[test]
    fn v3_checks_implicit_character_columns_against_table_default() {
        let source = include_str!("db.rs");
        let body = source
            .split_once("async fn validate_schema_shape_with_contract(")
            .unwrap()
            .1
            .split_once("\nasync fn read_column(")
            .unwrap()
            .0;
        assert!(body.contains("validate_v3_column_charset("));
    }

    #[test]
    fn v3_implicit_character_metadata_rejects_column_overrides() {
        let good = ColumnMeta {
            name: "display_name".into(),
            data_type: "varchar".into(),
            column_type: "varchar(128)".into(),
            nullable: false,
            character_set_name: Some("utf8mb4".into()),
            collation_name: Some("utf8mb4_bin".into()),
            default: None,
        };
        for (table, declaration) in [
            ("users", "VARCHAR(128) NOT NULL"),
            ("roles", "VARCHAR(128) NOT NULL"),
            ("devices", "VARCHAR(128) NOT NULL"),
        ] {
            assert!(validate_v3_column_charset(table, declaration, &good, "utf8mb4_bin").is_ok());
            for bad in [
                ColumnMeta {
                    character_set_name: Some("latin1".into()),
                    ..good.clone()
                },
                ColumnMeta {
                    character_set_name: Some("ascii".into()),
                    ..good.clone()
                },
                ColumnMeta {
                    collation_name: Some("utf8mb4_general_ci".into()),
                    ..good.clone()
                },
                ColumnMeta {
                    collation_name: None,
                    ..good.clone()
                },
            ] {
                assert!(
                    validate_v3_column_charset(table, declaration, &bad, "utf8mb4_bin").is_err()
                );
            }
        }
        assert!(
            validate_v3_column_charset(
                "users",
                "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL",
                &ColumnMeta {
                    character_set_name: Some("ascii".into()),
                    collation_name: Some("ascii_bin".into()),
                    ..good
                },
                "utf8mb4_bin"
            )
            .is_ok()
        );
    }

    #[test]
    fn legacy_username_utf8mb4_collation_matches_table_default() {
        let old = ColumnMeta {
            name: "username".into(),
            data_type: "varchar".into(),
            column_type: "varchar(128)".into(),
            nullable: false,
            character_set_name: Some("utf8mb4".into()),
            collation_name: Some("utf8mb4_bin".into()),
            default: None,
        };
        let old_declaration = identity_column_declarations("users", "username").unwrap().0;
        assert!(validate_v3_table_collation("users", "utf8mb4_bin").is_ok());
        assert!(validate_column_shape("users", old_declaration, &old).is_ok());
        assert!(validate_v3_column_charset("users", old_declaration, &old, "utf8mb4_bin").is_ok());
        let changed = ColumnMeta {
            collation_name: Some("utf8mb4_general_ci".into()),
            ..old.clone()
        };
        assert!(
            validate_v3_column_charset("users", old_declaration, &changed, "utf8mb4_bin").is_err()
        );
        // Original 0001 has no explicit charset and already rejects this drift.
        assert!(
            validate_v3_column_charset("users", "VARCHAR(128) NOT NULL", &changed, "utf8mb4_bin")
                .is_err()
        );
        let target = ColumnMeta {
            column_type: "varchar(64)".into(),
            character_set_name: Some("ascii".into()),
            collation_name: Some("ascii_bin".into()),
            ..old
        };
        assert!(
            validate_v3_column_charset(
                "users",
                identity_column_declarations("users", "username").unwrap().1,
                &target,
                "utf8mb4_bin"
            )
            .is_ok()
        );
    }

    #[test]
    fn v3_false_defaults_are_part_of_the_readonly_shape_gate() {
        let source = include_str!("db.rs");
        let body = source
            .split_once("async fn validate_schema_shape_with_contract(")
            .unwrap()
            .1
            .split_once("\nasync fn read_column(")
            .unwrap()
            .0;
        assert!(body.contains("validate_column_shape(table, declaration, &actual)"));
        assert!(source.contains("CAST(column_default AS CHAR) AS column_default"));
    }

    #[test]
    fn every_legacy_and_v3_column_rejects_unexpected_defaults() {
        let validate_with_default =
            |table: &str, declaration: &str, actual: &ColumnMeta, default: Option<&str>| {
                validate_column_shape(
                    table,
                    declaration,
                    &ColumnMeta {
                        default: default.map(str::to_owned),
                        ..actual.clone()
                    },
                )
            };
        let active = ColumnMeta {
            name: "active".into(),
            data_type: "tinyint".into(),
            column_type: "tinyint(1)".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        for migration in [MIGRATION, V3_MIGRATION] {
            let declaration = ddl_parts_from(migration, "users")
                .into_iter()
                .find_map(|part| part.strip_prefix("active ").map(str::to_owned))
                .unwrap();
            assert!(validate_with_default("users", &declaration, &active, None).is_ok());
            assert!(validate_with_default("users", &declaration, &active, Some("1")).is_err());
        }
    }

    #[test]
    fn every_ddl_column_default_is_closed_in_legacy_and_v3() {
        for migration in [MIGRATION, V3_MIGRATION] {
            let mut columns = 0;
            let mut explicit_zero = 0;
            for &(table, names, _) in TABLES {
                let parts = ddl_parts_from(migration, table);
                for &name in names {
                    let declaration = parts
                        .iter()
                        .find_map(|part| part.strip_prefix(&format!("{name} ")))
                        .unwrap();
                    let token = declaration
                        .split_whitespace()
                        .next()
                        .unwrap()
                        .to_ascii_lowercase();
                    let data_type = token.split('(').next().unwrap();
                    let actual_type = if data_type == "boolean" {
                        "tinyint(1)".to_owned()
                    } else if declaration.contains("UNSIGNED") {
                        format!("{token} unsigned")
                    } else {
                        token.clone()
                    };
                    let default = declaration.contains("DEFAULT ").then(|| "0".to_owned());
                    let actual = ColumnMeta {
                        name: name.into(),
                        data_type: if data_type == "boolean" {
                            "tinyint"
                        } else {
                            data_type
                        }
                        .into(),
                        column_type: actual_type,
                        nullable: !declaration.contains("NOT NULL"),
                        character_set_name: declaration
                            .contains("CHARACTER SET ascii")
                            .then(|| "ascii".into()),
                        collation_name: declaration
                            .contains("COLLATE ascii_bin")
                            .then(|| "ascii_bin".into()),
                        default: default.clone(),
                    };
                    assert!(
                        validate_column_shape(table, declaration, &actual).is_ok(),
                        "{table}.{name}"
                    );
                    let drift = ColumnMeta {
                        default: Some("1".into()),
                        ..actual.clone()
                    };
                    let error = validate_column_shape(table, declaration, &drift).unwrap_err();
                    assert!(!format!("{error}").contains("1"));
                    if default.is_some() {
                        explicit_zero += 1;
                        assert!(
                            validate_column_shape(
                                table,
                                declaration,
                                &ColumnMeta {
                                    default: None,
                                    ..actual.clone()
                                }
                            )
                            .is_err()
                        );
                    } else {
                        assert!(
                            validate_column_shape(
                                table,
                                declaration,
                                &ColumnMeta {
                                    default: Some("CURRENT_TIMESTAMP".into()),
                                    ..actual.clone()
                                }
                            )
                            .is_err()
                        );
                    }
                    columns += 1;
                }
            }
            assert_eq!(columns, 70);
            assert_eq!(explicit_zero, 5);
        }
    }

    #[test]
    fn v3_false_defaults_reject_true_null_and_unverified_readbacks_without_value_leaks() {
        for (table, column) in [
            ("schema_meta", "initialized"),
            ("sessions", "revoked"),
            ("devices", "archived"),
        ] {
            assert!(validate_false_default_metadata(table, column, Some("0")).is_ok());
            for bad in [
                None,
                Some("1"),
                Some("TRUE"),
                Some("FALSE"),
                Some("unexpected sensitive value"),
            ] {
                let error = validate_false_default_metadata(table, column, bad).unwrap_err();
                assert!(!format!("{error}").contains("unexpected sensitive value"));
            }
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
    fn username_requires_ascii_bin_not_matching_width_only() {
        let good = ColumnMeta {
            name: "username".into(),
            data_type: "varchar".into(),
            column_type: "varchar(64)".into(),
            nullable: false,
            character_set_name: Some("ascii".into()),
            collation_name: Some("ascii_bin".into()),
            default: None,
        };
        assert!(
            validate_column_shape(
                "users",
                "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL",
                &good
            )
            .is_ok()
        );
        for bad in [
            ColumnMeta {
                character_set_name: Some("utf8mb4".into()),
                ..good.clone()
            },
            ColumnMeta {
                collation_name: Some("ascii_general_ci".into()),
                ..good.clone()
            },
        ] {
            assert!(
                validate_column_shape(
                    "users",
                    "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL",
                    &bad
                )
                .is_err()
            );
        }
    }

    #[test]
    fn username_rules_reject_noncanonical_input() {
        let long_ok = "a".repeat(64);
        let long_bad = "a".repeat(65);
        for good in ["abc", "a.b_c-1", long_ok.as_str()] {
            assert!(valid_username(good));
        }
        for bad in [
            "ab",
            long_bad.as_str(),
            "Éric",
            "Alice",
            "alice!",
            "-alice",
            "a b",
        ] {
            assert!(!valid_username(bad), "{bad:?}");
        }
    }

    #[test]
    fn all_identity_counters_require_unsigned_but_schema_version_stays_int() {
        for (table, column) in [
            ("schema_meta", "authz_epoch"),
            ("schema_meta", "admin_guard_revision"),
            ("users", "revision"),
            ("roles", "revision"),
            ("device_groups", "revision"),
            ("devices", "revision"),
            ("grants", "revision"),
            ("admission_decisions", "previous_revision"),
            ("admission_decisions", "new_revision"),
            ("audit_events", "event_seq"),
        ] {
            let signed = ColumnMeta {
                name: column.into(),
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
                validate_column_shape(table, "BIGINT UNSIGNED NOT NULL", &unsigned).is_ok(),
                "{table}.{column}"
            );
            assert!(
                validate_column_shape(table, "BIGINT UNSIGNED NOT NULL", &signed).is_err(),
                "{table}.{column}"
            );
        }
        let version = ColumnMeta {
            name: "schema_version".into(),
            data_type: "int".into(),
            column_type: "int".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        assert!(validate_column_shape("schema_meta", "INT NOT NULL", &version).is_ok());
    }

    #[test]
    fn incompatible_binary_length_type_and_nullability_are_rejected() {
        let base = ColumnMeta {
            name: "public_key".into(),
            data_type: "binary".into(),
            column_type: "binary(32)".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
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
            sub_parts: vec![None, None],
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
            ddl_parts_from(MIGRATION, "devices")
                .iter()
                .any(|part| part.contains("chk_devices_state"))
        );
    }

    #[test]
    fn legacy_check_subsets_require_semantic_proof_even_when_empty() {
        let mut originals = Vec::new();
        for (table, name) in [
            ("schema_meta", "chk_schema_singleton"),
            ("devices", "chk_devices_state"),
            ("devices", "chk_devices_decision"),
            ("grants", "chk_grants_source"),
            ("grants", "chk_grants_scope"),
        ] {
            let part = ddl_parts_from(MIGRATION, table)
                .into_iter()
                .find(|p| p.starts_with(&format!("CONSTRAINT {name} ")))
                .unwrap();
            originals.push((
                Some(table.to_owned()),
                Some(name.to_owned()),
                Some(part.split_once("CHECK ").unwrap().1.to_owned()),
            ));
        }
        for mask in 0u32..32 {
            let rows: Vec<_> = originals
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, row)| row.clone())
                .collect();
            let confirmed = validate_legacy_check_metadata(&rows).unwrap();
            assert_eq!(confirmed.len(), mask.count_ones() as usize, "subset {mask}");
        }
        let mut bad = originals[1].clone();
        for changed in [
            "(admission_state IN ('PEND ING','APPROVED','REVOKED'))",
            "(admission_state IN ('pending','APPROVED','REVOKED'))",
            "(admission_state IN (_latin1'PENDING','APPROVED','REVOKED'))",
            "(admission_state IN ('PENDING','APPROVED','REVOKED') OR 1=1)",
            "(admission _state IN ('PENDING','APPROVED','REVOKED'))",
        ] {
            bad.2 = Some(changed.to_owned());
            assert!(validate_legacy_check_metadata(&[bad.clone()]).is_err());
        }
        let observed = "(`admission_state` in (_utf8mb4\\'PENDING\\',_utf8mb4\\'APPROVED\\',_utf8mb4\\'REVOKED\\'))";
        bad.2 = Some(observed.to_owned());
        assert!(validate_legacy_check_metadata(&[bad.clone()]).is_ok());
        bad.2 = Some(observed.replace("PENDING", "PEND ING"));
        assert!(validate_legacy_check_metadata(&[bad]).is_err());
        for row in [
            (
                Some("grants".into()),
                originals[1].1.clone(),
                originals[1].2.clone(),
            ),
            (
                Some("devices".into()),
                Some("unknown".into()),
                originals[1].2.clone(),
            ),
            (None, originals[1].1.clone(), originals[1].2.clone()),
            (originals[1].0.clone(), None, originals[1].2.clone()),
            (originals[1].0.clone(), originals[1].1.clone(), None),
        ] {
            assert!(validate_legacy_check_metadata(&[row]).is_err());
        }
        assert!(
            validate_legacy_check_metadata(&[originals[1].clone(), originals[1].clone()]).is_err()
        );
        assert!(legacy_check_tokens("('PENDING' /* metadata */)").is_none());
        let readbacks = [
            ("schema_meta", "chk_schema_singleton", "(`singleton` = 1)"),
            (
                "devices",
                "chk_devices_state",
                "(`admission_state` in (_utf8mb4'PENDING',_utf8mb4'APPROVED',_utf8mb4'REVOKED'))",
            ),
            (
                "devices",
                "chk_devices_decision",
                "(`review_decision` in (_utf8mb4'none',_utf8mb4'approved',_utf8mb4'denied',_utf8mb4'revoked'))",
            ),
            (
                "grants",
                "chk_grants_source",
                "((`source_kind` = _utf8mb4'role' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4'direct' AND `role_id` IS NULL AND `permissions` IS NOT NULL))",
            ),
            (
                "grants",
                "chk_grants_scope",
                "((`scope_kind` = _utf8mb4'all' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'group' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'device' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))",
            ),
        ];
        for (table, name, clause) in readbacks {
            assert!(
                validate_legacy_check_metadata(&[(
                    Some(table.into()),
                    Some(name.into()),
                    Some(clause.into())
                )])
                .is_ok(),
                "{table}.{name}"
            );
        }
        let grants_unobserved_backslashes = readbacks[3].2.replace("'role'", "\\'role\\'");
        assert!(
            validate_legacy_check_metadata(&[(
                Some("grants".into()),
                Some("chk_grants_source".into()),
                Some(grants_unobserved_backslashes)
            )])
            .is_err()
        );
    }

    #[test]
    fn v1_mixed_and_v2_target_columns_are_version_specific() {
        for (table, column, _, _) in IDENTITY_COLUMNS {
            let (old, target) = identity_column_declarations(table, column).unwrap();
            let meta = |decl: &str| {
                let type_token = decl.split_whitespace().next().unwrap().to_ascii_lowercase();
                let data_type = type_token.split('(').next().unwrap().to_owned();
                ColumnMeta {
                    name: (*column).into(),
                    data_type,
                    column_type: if decl.contains("UNSIGNED") {
                        format!("{type_token} unsigned")
                    } else {
                        type_token
                    },
                    nullable: false,
                    character_set_name: if *column == "username" {
                        Some(
                            if decl.contains("ascii") {
                                "ascii"
                            } else {
                                "utf8mb4"
                            }
                            .into(),
                        )
                    } else {
                        None
                    },
                    collation_name: if decl.contains("ascii_bin") {
                        Some("ascii_bin".into())
                    } else {
                        None
                    },
                    default: decl.contains("DEFAULT 0").then(|| "0".into()),
                }
            };
            let old = meta(old);
            let target = meta(target);
            assert!(validate_legacy_column(1, table, column, &old).unwrap());
            assert!(!validate_legacy_column(1, table, column, &target).unwrap());
            assert!(validate_legacy_column(2, table, column, &old).is_err());
            assert!(!validate_legacy_column(2, table, column, &target).unwrap());
        }
        let old = ColumnMeta {
            name: "revision".into(),
            data_type: "bigint".into(),
            column_type: "bigint".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        assert!(validate_legacy_column(3, "users", "revision", &old).is_err());
    }

    #[test]
    fn check_literals_are_byte_sensitive_while_external_format_is_not() {
        let declared = "admission_state IN ('PENDING','it''s')";
        for changed in [
            "admission_state IN ('PEND ING','it''s')",
            "admission_state IN ('PEND`ING','it''s')",
            "admission_state IN ('pending','it''s')",
            "admission_state IN ('PENDING','its')",
            "admission_state IN ('PENDING','it''''s')",
            "admission_state IN (_latin1'PENDING','it''s')",
        ] {
            assert_ne!(
                normalize_check(declared),
                normalize_check(changed),
                "{changed}"
            );
        }
        assert_eq!(
            normalize_check(declared),
            normalize_check("((`admission_state` in (_utf8mb4'PENDING', _utf8mb4'it''s')))"),
        );
        assert_eq!(
            normalize_check("source_kind = 'it''_utf8mb4role'"),
            normalize_check("(`source_kind` = _utf8mb4'it''_utf8mb4role')"),
        );
        assert_eq!(
            normalize_check("source_kind = \"Ro\"\"le\""),
            normalize_check("(`source_kind` = \"Ro\"\"le\")"),
        );
        assert_ne!(
            normalize_check("source_kind = \"Ro\"\"le\""),
            normalize_check("source_kind = \"Ro\"\" le\""),
        );
    }

    #[test]
    fn check_normalization_ignores_sql_comments_outside_literals_only() {
        let declared = "source_kind = 'role'";
        assert_eq!(
            normalize_check(declared),
            normalize_check("(`source_kind` /* metadata */ = _utf8mb4'role')")
        );
        assert_eq!(
            normalize_check(declared),
            normalize_check("source_kind -- metadata\n = _utf8mb4'role'")
        );
        assert_ne!(
            normalize_check(declared),
            normalize_check("source_kind = 'ro/* metadata */le'")
        );
    }

    #[test]
    fn mysql_8046_escaped_check_readback_accepts_only_observed_forms() {
        for (name, observed) in [
            (
                "chk_devices_state",
                "(`admission_state` in (_utf8mb4\\'PENDING\\',_utf8mb4\\'APPROVED\\',_utf8mb4\\'REVOKED\\'))",
            ),
            (
                "chk_devices_decision",
                "(`review_decision` in (_utf8mb4\\'none\\',_utf8mb4\\'approved\\',_utf8mb4\\'denied\\',_utf8mb4\\'revoked\\'))",
            ),
        ] {
            let part = ddl_parts_from(MIGRATION, "devices")
                .into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} ")))
                .unwrap();
            let declared = part.split_once("CHECK ").unwrap().1;
            assert!(
                check_clause_matches("devices", name, observed, declared),
                "{name}"
            );
            for changed in [
                observed.replace("PENDING", "PEND ING"),
                observed.replace("PENDING", "pending"),
                observed.replace("none", "None"),
                observed.replace("_utf8mb4", "_latin1"),
                observed.replace("\\'approved", "\\'app''roved"),
                observed.replace("\\'PENDING", "'PENDING"),
                observed.replace("\\'", "\\\\'"),
                format!("{observed} OR 1=1"),
            ] {
                if changed != observed {
                    assert!(
                        !check_clause_matches("devices", name, &changed, declared),
                        "{changed}"
                    );
                }
            }
            assert!(!check_clause_matches("grants", name, observed, declared));
            assert!(!check_clause_matches(
                "devices",
                "chk_grants_source",
                observed,
                declared
            ));
            assert!(!check_clause_matches("devices", name, observed, "(1 = 1)"));
        }
        assert!(check_clause_matches(
            "schema_meta",
            "chk_schema_singleton",
            "(`singleton` = 1)",
            "(singleton = 1)"
        ));
    }

    #[test]
    fn check_clause_normalization_accepts_utf8mb4_rewritten_ddl_literals() {
        for (table, name, stored) in [
            (
                "devices",
                "chk_devices_state",
                "(`admission_state` in (_utf8mb4'PENDING',_utf8mb4'APPROVED',_utf8mb4'REVOKED'))",
            ),
            (
                "devices",
                "chk_devices_decision",
                "(`review_decision` in (_utf8mb4'none',_utf8mb4'approved',_utf8mb4'denied',_utf8mb4'revoked'))",
            ),
            (
                "grants",
                "chk_grants_source",
                "((`source_kind` = _utf8mb4'role' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4'direct' AND `role_id` IS NULL AND `permissions` IS NOT NULL))",
            ),
            (
                "grants",
                "chk_grants_scope",
                "((`scope_kind` = _utf8mb4'all' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'group' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'device' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))",
            ),
        ] {
            let part = ddl_parts_from(MIGRATION, table)
                .into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} ")))
                .unwrap();
            let declared = part.split_once("CHECK ").unwrap().1;
            assert_eq!(
                normalize_check(stored),
                normalize_check(declared),
                "{table}.{name}"
            );
        }
    }

    #[test]
    fn check_clause_normalization_preserves_other_literals_and_expressions() {
        let declared = "source_kind = 'role'";
        for changed in [
            "source_kind = _latin1'role'",
            "source_kind = '_utf8mb4role'",
            "source_kind = 'direct'",
            "source_kind = _utf8mb4'role' AND role_id IS NULL",
            "source_kind = x_utf8mb4'role'",
            "source_kind = `_utf8mb4`'role'",
            "source_kind = 'it\\'_utf8mb4role'",
        ] {
            assert_ne!(
                normalize_check(changed),
                normalize_check(declared),
                "{changed}"
            );
        }
        assert_ne!(
            normalize_check("source_kind = 'it''_utf8mb4role'"),
            normalize_check("source_kind = 'it''role'")
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
    fn nullable_readback_accepts_only_yes_or_no() {
        let parse = |value: Option<&str>| parse_nullable_metadata("users", "active", value);
        assert!(parse(Some("YES")).unwrap());
        assert!(!parse(Some("NO")).unwrap());
        for unknown in [None, Some("UNKNOWN"), Some("FALSE"), Some("no"), Some("")] {
            let error = parse(unknown).unwrap_err();
            assert!(!format!("{error}").contains("UNKNOWN"));
        }
    }

    #[test]
    fn nullable_json_and_boolean_alias_match_migration() {
        let json = ColumnMeta {
            name: "descriptor_json".into(),
            data_type: "json".into(),
            column_type: "json".into(),
            nullable: true,
            character_set_name: None,
            collation_name: None,
            default: None,
        };
        let bool_col = ColumnMeta {
            name: "initialized".into(),
            data_type: "tinyint".into(),
            column_type: "tinyint(1)".into(),
            nullable: false,
            character_set_name: None,
            collation_name: None,
            default: Some("0".into()),
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
    fn audit_sequence_never_wraps() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let seq = AtomicU64::new(u64::MAX);
        assert!(next_event_seq(&seq).is_err());
        assert_eq!(seq.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn max_admission_revision_rejects_transition_without_wrap() {
        let current = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: u64::MAX,
        };
        assert!(matches!(
            next_snapshot(current, ReviewDecision::Approved),
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(current.revision, u64::MAX);
    }

    #[test]
    fn admission_revision_crosses_signed_boundary() {
        let current = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: i64::MAX as _,
        };
        let next = next_snapshot(current, ReviewDecision::Approved).unwrap();
        assert_eq!(next.revision, i64::MAX as u64 + 1);
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
