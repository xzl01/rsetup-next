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

// These two layouts are a closed MySQL 8.0.46 serialization of the fixed
// 0001 predicate trees: each = / IS [NOT] NULL atom is parenthesized, and
// AND/OR retain the original grouping. print_expr introduces _utf8mb4 and
// the I_S view's print escapes each quote once more. Neither observed text
// nor the legacy tokenizer is used to construct the accepted bytes.
fn mysql_8046_grant_clause(table: &str, name: &str, stored: &str, declared: &str) -> bool {
    let (original, serialized) = match (table, name) {
        ("grants", "chk_grants_source") => (
            "((source_kind = 'role' AND role_id IS NOT NULL AND permissions IS NULL) OR (source_kind = 'direct' AND role_id IS NULL AND permissions IS NOT NULL))",
            concat!(
                "(((`source_kind` = _utf8mb4'role') AND (`role_id` IS NOT NULL) AND (`permissions` IS NULL)) OR ",
                "((`source_kind` = _utf8mb4'direct') AND (`role_id` IS NULL) AND (`permissions` IS NOT NULL)))",
            ),
        ),
        ("grants", "chk_grants_scope") => (
            "((scope_kind = 'all' AND scope_group_id IS NULL AND scope_device_id IS NULL) OR (scope_kind = 'group' AND scope_group_id IS NOT NULL AND scope_device_id IS NULL) OR (scope_kind = 'device' AND scope_group_id IS NULL AND scope_device_id IS NOT NULL))",
            concat!(
                "(((`scope_kind` = _utf8mb4'all') AND (`scope_group_id` IS NULL) AND (`scope_device_id` IS NULL)) OR ",
                "((`scope_kind` = _utf8mb4'group') AND (`scope_group_id` IS NOT NULL) AND (`scope_device_id` IS NULL)) OR ",
                "((`scope_kind` = _utf8mb4'device') AND (`scope_group_id` IS NULL) AND (`scope_device_id` IS NOT NULL)))",
            ),
        ),
        _ => return false,
    };
    if declared != original {
        return false;
    }
    let expected = serialized.replace('\'', "\\'");
    let expected = expected.as_bytes();
    let actual = stored.as_bytes();
    if actual.len() != expected.len() {
        return false;
    }
    // Only whole, fixed SQL keyword words may vary in ASCII case. All other
    // bytes, including quoting, spaces, introducers and wrappers, are exact.
    let mut pos = 0;
    while pos < expected.len() {
        if expected[pos].is_ascii_alphabetic() {
            let end = pos
                + expected[pos..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphabetic())
                    .count();
            let word = &expected[pos..end];
            let keyword = [b"AND".as_slice(), b"OR", b"IS", b"NOT", b"NULL"].contains(&word);
            if if keyword {
                !actual[pos..end].eq_ignore_ascii_case(word)
            } else {
                actual[pos..end] != *word
            } {
                return false;
            }
            pos = end;
        } else {
            if actual[pos] != expected[pos] {
                return false;
            }
            pos += 1;
        }
    }
    true
}

fn legacy_check_metadata_error() -> ControllerError {
    ControllerError::Config("legacy CHECK metadata unavailable or incompatible".into())
}

type LegacyCheckVersionRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

// Only the caller which read VERSION(), ENFORCED and the complete clauses on
// one held connection can supply this context. The older pure comparison has
// no path to the new permission.
fn validate_legacy_check_metadata_with_version(
    rows: &[LegacyCheckVersionRow],
    version: Result<Option<&str>, ()>,
) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    let version = version
        .map_err(|_| legacy_check_metadata_error())?
        .ok_or_else(legacy_check_metadata_error)?;
    let mut confirmed = Vec::new();
    for (table, name, clause, enforced) in rows {
        let (table, name) = known_legacy_check(
            table.as_deref().ok_or_else(legacy_check_metadata_error)?,
            name.as_deref().ok_or_else(legacy_check_metadata_error)?,
        )
        .ok_or_else(legacy_check_metadata_error)?;
        if confirmed.contains(&(table, name)) {
            return Err(legacy_check_metadata_error());
        }
        let stored = clause.as_deref().ok_or_else(legacy_check_metadata_error)?;
        let old_row = (
            Some(table.to_owned()),
            Some(name.to_owned()),
            Some(stored.to_owned()),
        );
        if validate_legacy_check_metadata(&[old_row]).is_err() {
            let declared = ddl_parts_from(MIGRATION, table)
                .into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))
                .ok_or_else(legacy_check_metadata_error)?;
            let expression = declared
                .split_once("CHECK ")
                .ok_or_else(legacy_check_metadata_error)?
                .1;
            if version != "8.0.46"
                || enforced.as_deref() != Some("YES")
                || !mysql_8046_grant_clause(table, name, stored, expression)
            {
                return Err(legacy_check_metadata_error());
            }
        }
        confirmed.push((table, name));
    }
    Ok(confirmed)
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

#[cfg(test)]
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
            confirmed_checks.extend(read_legacy_checks_for_table(db, table).await?);
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

// The grants branch acquires once. VERSION(), ENFORCED and the complete
// check_clause rows are read on that very connection; no second sample can
// turn an interrupted or incompatible read into permission to drop a CHECK.
async fn read_legacy_checks_for_table(
    db: &DbPool,
    table: &str,
) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    const BASIC: &str = "SELECT CAST(tc.table_name AS CHAR) AS table_name, CAST(tc.constraint_name AS CHAR) AS constraint_name, CAST(cc.check_clause AS CHAR) AS check_clause FROM information_schema.table_constraints tc LEFT JOIN information_schema.check_constraints cc ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name WHERE tc.table_schema = DATABASE() AND tc.table_name = ? AND tc.constraint_type = 'CHECK'";
    if table == "grants" {
        let mut connection =
            db.0.acquire()
                .await
                .map_err(|_| legacy_check_metadata_error())?;
        let version: Option<String> = sqlx::query_scalar("SELECT CAST(VERSION() AS CHAR)")
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| legacy_check_metadata_error())?;
        let version = version.ok_or_else(legacy_check_metadata_error)?;
        if version == "8.0.46" {
            let rows = sqlx::query("SELECT CAST(tc.table_name AS CHAR) AS table_name, CAST(tc.constraint_name AS CHAR) AS constraint_name, CAST(cc.check_clause AS CHAR) AS check_clause, CAST(tc.enforced AS CHAR) AS enforced FROM information_schema.table_constraints tc LEFT JOIN information_schema.check_constraints cc ON cc.constraint_schema = tc.constraint_schema AND cc.constraint_name = tc.constraint_name WHERE tc.table_schema = DATABASE() AND tc.table_name = ? AND tc.constraint_type = 'CHECK'")
                .bind(table).fetch_all(&mut *connection).await.map_err(|_| legacy_check_metadata_error())?;
            let mut metadata = Vec::with_capacity(rows.len());
            for row in rows {
                metadata.push((
                    row.try_get("table_name")
                        .map_err(|_| legacy_check_metadata_error())?,
                    row.try_get("constraint_name")
                        .map_err(|_| legacy_check_metadata_error())?,
                    row.try_get("check_clause")
                        .map_err(|_| legacy_check_metadata_error())?,
                    row.try_get("enforced")
                        .map_err(|_| legacy_check_metadata_error())?,
                ));
            }
            return validate_legacy_check_metadata_with_version(&metadata, Ok(Some(&version)));
        }
        let rows = sqlx::query(BASIC)
            .bind(table)
            .fetch_all(&mut *connection)
            .await
            .map_err(|_| legacy_check_metadata_error())?;
        let mut metadata = Vec::with_capacity(rows.len());
        for row in rows {
            metadata.push((
                row.try_get("table_name")
                    .map_err(|_| legacy_check_metadata_error())?,
                row.try_get("constraint_name")
                    .map_err(|_| legacy_check_metadata_error())?,
                row.try_get("check_clause")
                    .map_err(|_| legacy_check_metadata_error())?,
            ));
        }
        return validate_legacy_check_metadata(&metadata);
    }
    // Preserve old behavior and TiDB compatibility for all non-grants tables.
    let rows = sqlx::query(BASIC)
        .bind(table)
        .fetch_all(&db.0)
        .await
        .map_err(|_| legacy_check_metadata_error())?;
    let mut metadata = Vec::with_capacity(rows.len());
    for row in rows {
        metadata.push((
            row.try_get("table_name")
                .map_err(|_| legacy_check_metadata_error())?,
            row.try_get("constraint_name")
                .map_err(|_| legacy_check_metadata_error())?,
            row.try_get("check_clause")
                .map_err(|_| legacy_check_metadata_error())?,
        ));
    }
    validate_legacy_check_metadata(&metadata)
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
    FixtureV2,
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

#[allow(dead_code)] // historical reference; active fixture preflight uses full legacy probe
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

// Historical 0001 is immutable; only its eleven known CREATE statements may
// run in a previously confirmed empty schema. Never repair partial creation.
fn v1_create_statements() -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
    let mut statements = Vec::new();
    for statement in MIGRATION
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let table = statement
            .strip_prefix("CREATE TABLE IF NOT EXISTS ")
            .and_then(|rest| rest.split_once(" ("))
            .map(|(name, _)| name)
            .ok_or_else(|| legacy_preflight_error("baseline SQL"))?;
        if !TABLES.iter().any(|(known, _, _)| *known == table)
            || statements.iter().any(|(name, _)| *name == table)
            || statement.to_ascii_uppercase().contains("FOREIGN KEY")
        {
            return Err(legacy_preflight_error("baseline SQL"));
        }
        statements.push((table, statement));
    }
    if statements.len() != TABLES.len() {
        return Err(legacy_preflight_error("baseline SQL"));
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
        TestMigrationMode::FixtureV2 => create_historical_v2_fixture(db).await,
        TestMigrationMode::FixtureV1 | TestMigrationMode::FixturePartialV1 => {
            create_historical_v1_fixture(db, mode).await
        }
    }
}

trait FixtureV2Probe: LegacyUpgradeProbe + FreshIdentityMigrationProbe {
    async fn create_v1_table(&self, statement: &'static str) -> Result<(), ControllerError>;
    async fn validate_created_v1(&self) -> Result<(), ControllerError>;
    async fn insert_v1_meta(&self) -> Result<(), ControllerError>;
    async fn cas_fixture_v2(&self) -> Result<u64, ControllerError>;
}

async fn fixture_v2_with_probe(probe: &impl FixtureV2Probe) -> Result<(), ControllerError> {
    // Non-resumable by design: only None + reliably zero tables may run 0001.
    if IdentitySchemaProbe::read_version(probe).await?.is_some() {
        return Err(legacy_preflight_error("fixture-v2 requires empty schema"));
    }
    let statements = v1_create_statements()?;
    if !probe.schema_tables().await?.is_empty() {
        return Err(legacy_preflight_error("fixture-v2 requires zero tables"));
    }
    let mut expected = Vec::new();
    for (table, statement) in statements {
        probe.create_v1_table(statement).await?;
        expected.push(table);
        let actual = probe.schema_tables().await?;
        if actual.len() != expected.len()
            || actual.iter().any(|name| !expected.contains(&name.as_str()))
        {
            return Err(legacy_preflight_error("fixture-v2 CREATE drift"));
        }
    }
    probe.validate_created_v1().await?;
    probe.validate_empty_data().await?;
    probe.insert_v1_meta().await?;
    if !matches!(IdentitySchemaProbe::read_version(probe).await, Ok(Some(1))) {
        return Err(legacy_preflight_error("fixture-v2 v1 insert uncertain"));
    }
    let mut trusted = probe.preflight(1).await?;
    if trusted.old_columns.iter().any(|old| !*old) {
        return Err(legacy_preflight_error("fixture-v2 v1 columns"));
    }
    for (index, &(_, _, _, statement)) in IDENTITY_COLUMNS.iter().enumerate() {
        let before = probe.preflight(1).await?;
        require_legacy_snapshot(&trusted, &before)?;
        if !before.old_columns[index] {
            return Err(legacy_preflight_error("fixture-v2 ALTER precondition"));
        }
        let ddl = probe.alter(statement).await;
        // A failed nontransactional ALTER might still have applied. Re-read,
        // but never convert an uncertain result into success.
        let after = probe.preflight(1).await;
        ddl?;
        let after = after?;
        let mut expected = before;
        expected.old_columns[index] = false;
        require_legacy_snapshot(&expected, &after)?;
        trusted = after;
    }
    let final_read = probe.preflight(1).await?;
    require_legacy_snapshot(&trusted, &final_read)?;
    if final_read.old_columns.iter().any(|old| *old) {
        return Err(legacy_preflight_error("fixture-v2 target columns"));
    }
    let cas = probe.cas_fixture_v2().await;
    if !matches!(cas, Ok(1)) {
        let _observed = IdentitySchemaProbe::read_version(probe).await;
        return Err(legacy_preflight_error("fixture-v2 CAS uncertain"));
    }
    if !matches!(IdentitySchemaProbe::read_version(probe).await, Ok(Some(2))) {
        return Err(legacy_preflight_error("fixture-v2 CAS uncertain"));
    }
    let ready = probe.preflight(2).await?;
    require_legacy_snapshot(&final_read, &ready)
}

impl FixtureV2Probe for DbPool {
    async fn create_v1_table(&self, statement: &'static str) -> Result<(), ControllerError> {
        if !v1_create_statements()?
            .iter()
            .any(|&(_, sql)| sql == statement)
        {
            return Err(legacy_preflight_error("fixture-v2 CREATE whitelist"));
        }
        sqlx::query(statement)
            .execute(&self.0)
            .await
            .map_err(|_| legacy_preflight_error("fixture-v2 CREATE failed"))?;
        Ok(())
    }
    async fn validate_created_v1(&self) -> Result<(), ControllerError> {
        validate_legacy_shape(self, 1).await?;
        require_no_identity_foreign_keys(self).await?;
        for &(table, column, _, _) in IDENTITY_COLUMNS {
            let actual = read_column(self, table, column).await?;
            if !classify_identity_column(table, column, &actual)? {
                return Err(legacy_preflight_error("fixture-v2 v1 columns"));
            }
        }
        Ok(())
    }
    async fn insert_v1_meta(&self) -> Result<(), ControllerError> {
        let result = sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, 1, ?, FALSE, 0, 0)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).execute(&self.0).await
            .map_err(|_| legacy_preflight_error("fixture-v2 v1 insert uncertain"))?;
        if result.rows_affected() != 1 {
            return Err(legacy_preflight_error("fixture-v2 v1 insert uncertain"));
        }
        Ok(())
    }
    async fn cas_fixture_v2(&self) -> Result<u64, ControllerError> {
        let result = sqlx::query(
            "UPDATE schema_meta SET schema_version = 2 WHERE singleton = 1 AND schema_version = 1",
        )
        .execute(&self.0)
        .await
        .map_err(|_| legacy_preflight_error("fixture-v2 CAS uncertain"))?;
        Ok(result.rows_affected())
    }
}

async fn create_historical_v2_fixture(db: &DbPool) -> Result<(), ControllerError> {
    fixture_v2_with_probe(db).await
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
        for (_, statement) in v1_create_statements()? {
            sqlx::query(statement).execute(&db.0).await?;
        }
        validate_legacy_shape(db, 1).await?;
        require_no_identity_foreign_keys(db).await?;
        for &(table, column, _, _) in IDENTITY_COLUMNS {
            if !classify_identity_column(table, column, &read_column(db, table, column).await?)? {
                return Err(legacy_preflight_error("fixture-v1 old column"));
            }
        }
        sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, 1, ?, FALSE, 0, 0)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).execute(&db.0).await?;
        preflight_legacy_for_v3(db, 1).await?;
    } else if version != Some(1) {
        return Err(ControllerError::Config(
            "unsupported test identity schema version".into(),
        ));
    }
    if matches!(mode, TestMigrationMode::FixtureV1) {
        return Ok(());
    }
    let trusted = preflight_legacy_for_v3(db, 1).await?;
    if matches!(mode, TestMigrationMode::FixturePartialV1) {
        if !trusted.old_columns[0] {
            return Err(ControllerError::Config(
                "fixture partial column already altered".into(),
            ));
        }
        let ddl = sqlx::query(IDENTITY_COLUMNS[0].3).execute(&db.0).await;
        let after = preflight_legacy_for_v3(db, 1).await;
        ddl.map_err(|_| legacy_preflight_error("fixture partial ALTER failed"))?;
        let mut expected = trusted;
        expected.old_columns[0] = false;
        require_legacy_snapshot(&expected, &after?)?;
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

pub(crate) fn system_fallback_time_evidence() -> String {
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

pub(crate) fn next_event_seq(
    counter: &std::sync::atomic::AtomicU64,
) -> Result<u64, ControllerError> {
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
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        if let Some(actor) = actor_id {
            crate::integrity::require_active_admin(&mut tx, actor).await?;
        }
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

    mod readonly_preflight_diagnostic {
        use super::*;
        use std::sync::Mutex;

        // Offline tests below exercise summary mapping, not fabricated driver rows.
        struct UsernameSample<'a> {
            // Raw access succeeded: value non-NULL, type non-NULL, str/bytes compatibility.
            metadata: Option<[bool; 4]>,
            original_str: Result<&'a str, sqlx::Error>,
            original_bytes: Result<&'a [u8], sqlx::Error>,
            cast_bytes: Result<&'a [u8], sqlx::Error>,
        }

        fn username_sample_summary(sample: Option<UsernameSample<'_>>) -> String {
            let code = |value: Option<bool>| match value {
                Some(true) => "true",
                Some(false) => "false",
                None => "not_evaluated",
            };
            let mut fields = [None; 9];
            let mut class = "not_evaluated";
            if let Some(sample) = &sample {
                if let Some(metadata) = sample.metadata {
                    for (field, value) in fields.iter_mut().zip(metadata) {
                        *field = Some(value);
                    }
                }
                fields[4] = Some(sample.original_bytes.is_ok());
                fields[5] = Some(sample.cast_bytes.is_ok());
                if let (Ok(original), Ok(cast)) = (&sample.original_bytes, &sample.cast_bytes) {
                    fields[6] = Some(original == cast);
                }
                if let Ok(raw) = sample.cast_bytes {
                    let utf8 = std::str::from_utf8(raw);
                    fields[7] = Some(utf8.is_ok());
                    fields[8] = utf8.ok().map(valid_username);
                }
                class = if sample.metadata.is_some_and(|metadata| !metadata[0]) {
                    "null"
                } else {
                    match &sample.original_str {
                        Ok(_) => "none",
                        Err(sqlx::Error::ColumnDecode { source, .. })
                            if source.is::<sqlx::error::UnexpectedNullError>() =>
                        {
                            "null"
                        }
                        Err(sqlx::Error::ColumnDecode { .. })
                            if matches!(sample.metadata, Some([true, true, false, _])) =>
                        {
                            "type_mismatch"
                        }
                        Err(
                            sqlx::Error::ColumnDecode { source, .. } | sqlx::Error::Decode(source),
                        ) if source.is::<std::str::Utf8Error>() => "utf8",
                        Err(sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_)) => {
                            "other_decode"
                        }
                        Err(
                            sqlx::Error::ColumnNotFound(_)
                            | sqlx::Error::ColumnIndexOutOfBounds { .. },
                        ) => "column_access",
                        Err(_) => "other",
                    }
                };
            }
            let [
                original_non_null,
                type_non_null,
                str_compatible,
                bytes_compatible,
                original_bytes_ok,
                cast_bytes_ok,
                bytes_equal,
                utf8_valid,
                username_valid,
            ] = fields.map(code);
            format!(
                "sample_present={} original_non_null={original_non_null} type_non_null={type_non_null} str_compatible={str_compatible} bytes_compatible={bytes_compatible} original_bytes_ok={original_bytes_ok} cast_bytes_ok={cast_bytes_ok} bytes_equal={bytes_equal} utf8_valid={utf8_valid} username_valid={username_valid} str_error_class={class} diagnostic_complete=true",
                sample.is_some()
            )
        }

        fn sample_evidence(bytes: &[u8]) -> UsernameSample<'_> {
            UsernameSample {
                metadata: Some([true, true, false, true]),
                original_str: Err(sqlx::Error::ColumnDecode {
                    index: "private-column-marker".into(),
                    source: "private-error-marker".into(),
                }),
                original_bytes: Ok(bytes),
                cast_bytes: Ok(bytes),
            }
        }

        fn assert_sample_fields(summary: &str, expected: &[(&str, &str)]) {
            let fields: Vec<_> = summary.split_whitespace().collect();
            assert_eq!(fields.len(), 12);
            assert!(fields.contains(&"diagnostic_complete=true"));
            for (key, value) in expected {
                assert!(
                    fields.contains(&format!("{key}={value}").as_str()),
                    "missing {key}={value}"
                );
            }
            for field in fields {
                let (_, value) = field.split_once('=').unwrap();
                assert!(
                    [
                        "true",
                        "false",
                        "not_evaluated",
                        "none",
                        "type_mismatch",
                        "null",
                        "utf8",
                        "other_decode",
                        "column_access",
                        "other"
                    ]
                    .contains(&value)
                );
            }
            assert!(!summary.contains("private-"));
        }

        #[test]
        fn username_sample_no_row_is_not_evaluated() {
            assert_eq!(
                username_sample_summary(None),
                "sample_present=false original_non_null=not_evaluated type_non_null=not_evaluated str_compatible=not_evaluated bytes_compatible=not_evaluated original_bytes_ok=not_evaluated cast_bytes_ok=not_evaluated bytes_equal=not_evaluated utf8_valid=not_evaluated username_valid=not_evaluated str_error_class=not_evaluated diagnostic_complete=true"
            );
        }

        #[test]
        fn username_sample_proves_mismatch_without_error_text() {
            let summary = username_sample_summary(Some(sample_evidence(b"private-user-marker")));
            assert_sample_fields(
                &summary,
                &[
                    ("sample_present", "true"),
                    ("original_non_null", "true"),
                    ("type_non_null", "true"),
                    ("str_compatible", "false"),
                    ("bytes_compatible", "true"),
                    ("original_bytes_ok", "true"),
                    ("cast_bytes_ok", "true"),
                    ("bytes_equal", "true"),
                    ("utf8_valid", "true"),
                    ("username_valid", "true"),
                    ("str_error_class", "type_mismatch"),
                ],
            );
        }

        #[test]
        fn username_sample_null_and_missing_metadata_do_not_prove_mismatch() {
            for metadata in [
                Some([false, true, false, true]),
                Some([true, false, false, true]),
                None,
            ] {
                let mut sample = sample_evidence(b"private-user-marker");
                sample.metadata = metadata;
                sample.original_bytes = Err(sqlx::Error::Decode("private-error-marker".into()));
                sample.cast_bytes = Err(sqlx::Error::Decode("private-error-marker".into()));
                let expected_class = if metadata.is_some_and(|m| !m[0]) {
                    "null"
                } else {
                    "other_decode"
                };
                let compatibility = if metadata.is_some() {
                    ("false", "true")
                } else {
                    ("not_evaluated", "not_evaluated")
                };
                assert_sample_fields(
                    &username_sample_summary(Some(sample)),
                    &[
                        ("str_compatible", compatibility.0),
                        ("bytes_compatible", compatibility.1),
                        ("bytes_equal", "not_evaluated"),
                        ("utf8_valid", "not_evaluated"),
                        ("username_valid", "not_evaluated"),
                        ("str_error_class", expected_class),
                    ],
                );
            }
        }

        #[test]
        fn username_sample_strict_utf8_and_username_dependencies() {
            let invalid = [0xff];
            for (bytes, utf8, valid) in [
                (invalid.as_slice(), "false", "not_evaluated"),
                (b"bad name".as_slice(), "true", "false"),
            ] {
                assert_sample_fields(
                    &username_sample_summary(Some(sample_evidence(bytes))),
                    &[("utf8_valid", utf8), ("username_valid", valid)],
                );
            }
            let mut sample = sample_evidence(b"private-user-marker");
            sample.original_bytes = Ok(b"different-private-marker");
            assert_sample_fields(
                &username_sample_summary(Some(sample)),
                &[("bytes_equal", "false")],
            );
        }

        #[test]
        fn username_sample_unknown_errors_are_closed_and_success_is_none() {
            for (result, class) in [
                (
                    Err(sqlx::Error::ColumnDecode {
                        index: "private-column-marker".into(),
                        source: "private-error-marker".into(),
                    }),
                    "other_decode",
                ),
                (
                    Err(sqlx::Error::ColumnNotFound("private-column-marker".into())),
                    "column_access",
                ),
                (
                    Err(sqlx::Error::Protocol("private-error-marker".into())),
                    "other",
                ),
                (Ok("private-user-marker"), "none"),
            ] {
                let mut sample = sample_evidence(b"private-user-marker");
                sample.metadata = Some([true, true, true, true]);
                sample.original_str = result;
                assert_sample_fields(
                    &username_sample_summary(Some(sample)),
                    &[("str_error_class", class)],
                );
            }
        }

        fn version_class(version: &Result<Option<i32>, ControllerError>) -> &'static str {
            match version {
                Ok(None) => "none",
                Ok(Some(1)) => "v1",
                Ok(Some(2)) => "v2",
                Ok(Some(3)) => "v3",
                Ok(Some(_)) => "unsupported",
                Err(_) => "unreadable",
            }
        }

        fn error_class(error: &ControllerError) -> &'static str {
            // Exact matching only; never return a slice of the input error.
            const DATA_CODES: &[&str] = &[
                "transaction.begin",
                "transaction.end",
                "meta.read",
                "meta.decode",
                "meta.singleton",
                "users.read",
                "users.decode",
                "users.username",
                "boolean.read",
                "boolean.decode",
                "schema_meta.initialized",
                "users.active",
                "users.is_admin",
                "users.must_change_password",
                "sessions.revoked",
                "roles.builtin",
                "roles.archived",
                "device_groups.archived",
                "devices.archived",
                "devices.read",
                "devices.decode",
                "devices.state",
                "role_permissions.read",
                "role_permissions.decode",
                "role_permissions.permission",
                "grants.length_read",
                "grants.json_length",
                "grants.read",
                "grants.decode",
                "grants.role_presence",
                "grants.group_presence",
                "grants.device_presence",
                "grants.json",
                "grants.fields",
                "reference.read",
                "sessions",
                "role_permissions",
                "group_members",
                "grants",
                "admission_decisions",
                "audit_events",
            ];
            const PREFLIGHT_CODES: &[&str] = &[
                "version",
                "meta.read",
                "meta.decode",
                "meta.singleton",
                "shape",
                "check.whitelist",
                "foreign_keys",
                "data",
                "column.read",
                "column.count",
                "column.v2_old",
                "counter.query",
                "counter.read",
                "username.encoding_read",
                "username.encoding",
                "username.collision_read",
                "username.collision",
            ];
            if let ControllerError::Config(message) = error {
                for (prefix, codes) in [
                    ("identity data ", DATA_CODES),
                    ("legacy preflight ", PREFLIGHT_CODES),
                ] {
                    if let Some(suffix) = message.strip_prefix(prefix) {
                        return codes
                            .iter()
                            .copied()
                            .find(|code| *code == suffix)
                            .unwrap_or("unclassified");
                    }
                }
                if message.starts_with("negative identity column ")
                    && IDENTITY_COLUMNS.iter().any(|(table, column, _, _)| {
                        message == &format!("negative identity column {table}.{column}")
                    })
                {
                    return "negative_value";
                }
            }
            let shape = legacy_diagnostic_shape_code(error).0;
            if shape == "shape_read_error" {
                "unclassified"
            } else {
                shape
            }
        }

        fn column_bitmap(columns: &[Result<bool, ControllerError>; 11]) -> String {
            columns
                .iter()
                .map(|column| match column {
                    Ok(true) => 'O',
                    Ok(false) => 'T',
                    Err(_) => 'U',
                })
                .collect()
        }

        // Input must be a complete, validated production shape result, never a
        // best-effort name enumeration. No trusted result means all unknown.
        fn check_bitmap(checks: Result<&[(&str, &str)], &ControllerError>) -> String {
            let Ok(checks) = checks else {
                return "UUUUU".into();
            };
            let mut bitmap = ['A'; 5];
            for &(table, name) in checks {
                let Some(index) = LEGACY_CHECK_DROPS
                    .iter()
                    .position(|&(t, n, _)| (t, n) == (table, name))
                else {
                    return "UUUUU".into();
                };
                if bitmap[index] == 'P' {
                    return "UUUUU".into();
                }
                bitmap[index] = 'P';
            }
            bitmap.iter().collect()
        }

        #[derive(Default)]
        struct Observation {
            stages: Vec<&'static str>,
            error: Option<&'static str>,
            checks: Option<Vec<(&'static str, &'static str)>>,
        }

        struct DiagnosticProbe<P> {
            inner: P,
            observation: Mutex<Observation>,
        }

        impl<P> DiagnosticProbe<P> {
            fn new(inner: P) -> Self {
                Self {
                    inner,
                    observation: Mutex::new(Observation::default()),
                }
            }

            async fn record<T>(
                &self,
                stage: &'static str,
                future: impl std::future::Future<Output = Result<T, ControllerError>>,
            ) -> Result<T, ControllerError> {
                // Record before polling the delegated read. Preserve its error
                // before production preflight replaces it with a broader code.
                self.observation.lock().unwrap().stages.push(stage);
                let result = future.await;
                if let Err(error) = &result {
                    self.observation.lock().unwrap().error = Some(error_class(error));
                }
                result
            }
        }

        impl<P: LegacyPreflightProbe + Sync> LegacyPreflightProbe for DiagnosticProbe<P> {
            async fn read_legacy_meta(&self) -> Result<Vec<(i8, i32)>, ControllerError> {
                self.record("meta", self.inner.read_legacy_meta()).await
            }
            async fn legacy_shape(
                &self,
                version: i32,
            ) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
                let result = self.record("shape", self.inner.legacy_shape(version)).await;
                if let Ok(checks) = &result {
                    self.observation.lock().unwrap().checks = Some(checks.clone());
                }
                result
            }
            async fn no_foreign_keys(&self) -> Result<(), ControllerError> {
                self.record("fk", self.inner.no_foreign_keys()).await
            }
            async fn identity_rows(&self, version: i32) -> Result<(), ControllerError> {
                self.record("data", self.inner.identity_rows(version)).await
            }
            async fn column_status(&self, version: i32) -> Result<Vec<bool>, ControllerError> {
                self.record("columns", self.inner.column_status(version))
                    .await
            }
            async fn negative(&self, query: &'static str) -> Result<bool, ControllerError> {
                self.record("negative", self.inner.negative(query)).await
            }
            async fn username_encoding(&self) -> Result<(), ControllerError> {
                self.record("encoding", self.inner.username_encoding())
                    .await
            }
            async fn username_collision(&self) -> Result<(), ControllerError> {
                self.record("collision", self.inner.username_collision())
                    .await
            }
        }

        #[test]
        fn readonly_diagnostic_version_and_errors_are_closed_classes() {
            for (input, expected) in [
                (Ok(None), "none"),
                (Ok(Some(1)), "v1"),
                (Ok(Some(2)), "v2"),
                (Ok(Some(3)), "v3"),
                (Ok(Some(99)), "unsupported"),
                (
                    Err(ControllerError::Config("private-marker".into())),
                    "unreadable",
                ),
            ] {
                assert_eq!(version_class(&input), expected);
            }
            for code in [
                "users.username",
                "transaction.begin",
                "transaction.end",
                "grants.json",
                "meta.singleton",
                "users.active",
                "audit_events",
            ] {
                assert_eq!(
                    error_class(&ControllerError::Config(format!("identity data {code}"))),
                    code
                );
                assert_eq!(
                    error_class(&ControllerError::Config(format!(
                        "identity data {code} private-marker"
                    ))),
                    "unclassified"
                );
            }
            assert_eq!(
                error_class(&ControllerError::Config("private-marker".into())),
                "unclassified"
            );
            assert_eq!(
                error_class(&ControllerError::InvalidArgument),
                "unclassified"
            );
        }

        #[test]
        fn readonly_diagnostic_bitmaps_preserve_order_and_fail_closed() {
            let columns = std::array::from_fn(|index| match index % 3 {
                0 => Ok(true),
                1 => Ok(false),
                _ => Err(ControllerError::Config("private-marker".into())),
            });
            assert_eq!(column_bitmap(&columns), "OTUOTUOTUOT");
            assert_eq!(IDENTITY_COLUMNS.len(), 11);
            assert_eq!(LEGACY_CHECK_DROPS.len(), 5);
            for (index, &(table, name, _)) in LEGACY_CHECK_DROPS.iter().enumerate() {
                let mut expected = ['A'; 5];
                expected[index] = 'P';
                assert_eq!(
                    check_bitmap(Ok(&[(table, name)])),
                    expected.iter().collect::<String>()
                );
                assert_eq!(check_bitmap(Ok(&[(table, name), (table, name)])), "UUUUU");
            }
            assert_eq!(check_bitmap(Ok(&[])), "AAAAA");
            assert_eq!(check_bitmap(Ok(&[("private-marker", "unknown")])), "UUUUU");
            assert_eq!(
                check_bitmap(Err(&ControllerError::InvalidArgument)),
                "UUUUU"
            );
            // The production reader rejects NULL, unknown, duplicate and invalid clauses.
            for rows in [
                vec![(None, None, None)],
                vec![(
                    Some("users".into()),
                    Some("unknown".into()),
                    Some("1".into()),
                )],
                vec![(
                    Some("schema_meta".into()),
                    Some("chk_schema_singleton".into()),
                    None,
                )],
            ] {
                let result = validate_legacy_check_metadata(&rows);
                assert_eq!(check_bitmap(result.as_deref()), "UUUUU");
            }
        }

        #[test]
        fn readonly_diagnostic_static_entrypoint_has_only_read_paths() {
            // Static source evidence only: no physical connection or DB behavior proof.
            let module = include_str!("db.rs")
                .split_once("    mod readonly_preflight_diagnostic {").unwrap().1
                .split_once("    #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n    enum DiagnosticTokenClass").unwrap().0;
            let entry = module.split_once(
                "\n        async fn mysql_failed_upgrade_readonly_preflight_diagnostic()",
            );
            assert!(entry.is_some(), "missing new ignored diagnostic entrypoint");
            let entry = entry.unwrap().1;
            for required in [
                "read_version(&db)",
                "read_column(&db, table, column)",
                "classify_identity_column(table, column, &actual)",
                "preflight_legacy_with_probe(&probe, version)",
                "check_identity_schema(&db)",
                "diagnostic_complete=true",
            ] {
                assert!(entry.contains(required), "missing read path: {required}");
            }
            assert!(entry.contains("let db = DbPool(readonly_diagnostic_pool().await)"));
            let helper = module
                .split_once("\n        async fn readonly_diagnostic_pool() -> sqlx::MySqlPool {")
                .expect("missing shared guarded pool helper")
                .1
                .split_once("\n        #[tokio::test]")
                .unwrap()
                .0;
            let sample = module
                .split_once("\n        async fn mysql_username_decode_readonly_sample() {")
                .expect("missing independent ignored username sample")
                .1
                .split_once("\n        // Read-only diagnostic entrypoint;")
                .unwrap()
                .0;
            for required in [
                "readonly_diagnostic_pool().await",
                "SELECT username AS original, CAST(username AS BINARY) AS raw_bytes FROM users ORDER BY id LIMIT 1",
                "row.try_get_raw(\"original\")",
                "!raw.is_null()",
                "!info.is_null()",
                "<str as sqlx::Type<sqlx::MySql>>::compatible(&info)",
                "<[u8] as sqlx::Type<sqlx::MySql>>::compatible(&info)",
                "row.try_get::<&str, _>(\"original\")",
                "row.try_get::<&[u8], _>(\"original\")",
                "row.try_get::<&[u8], _>(\"raw_bytes\")",
                "username_sample_summary(sample)",
                "pool.close().await",
            ] {
                assert!(
                    sample.contains(required),
                    "missing bounded sample path: {required}"
                );
            }
            assert_eq!(sample.matches("sqlx::query(").count(), 1);
            let hook = helper
                .split_once(".after_connect(")
                .unwrap()
                .1
                .split_once(".connect(&db_url)")
                .unwrap()
                .0;
            for required in [
                "SELECT DATABASE()",
                "SELECT VERSION()",
                "SHOW GRANTS",
                "validate_schema_metadata_grants(&grants, &expected)",
                "8.0.46",
            ] {
                assert!(
                    hook.contains(required),
                    "missing physical connection guard: {required}"
                );
            }
            assert!(helper.contains(".max_connections(1)"));
            for required in [
                "actual.as_deref() != Some(expected.as_str())",
                "version.as_deref() != Some(\"8.0.46\")",
                "diagnostic_connection_rejected",
                "diagnostic_connect_failed",
            ] {
                assert!(
                    helper.contains(required),
                    "missing fail-closed guard: {required}"
                );
            }
            for forbidden in [
                "run_identity_test_migration",
                "upgrade_",
                "fixture_",
                ".execute(",
                ".alter(",
                ".drop_known_check(",
                ".cas_version(",
                "LegacyUpgradeProbe",
                "println!(\"{error}",
                "println!(\"{error:?}",
                "try_get_unchecked",
                "from_utf8_lossy",
                "unsafe",
            ] {
                for source in [entry, helper, sample] {
                    assert!(!source.contains(forbidden), "unexpected path: {forbidden}");
                }
            }
        }

        struct ReadonlyFake {
            version: i32,
            fault: Option<&'static str>,
        }
        impl ReadonlyFake {
            fn gate(&self, stage: &str) -> Result<(), ControllerError> {
                if self.fault == Some(stage) {
                    Err(ControllerError::Config(
                        "identity data users.username".into(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
        impl LegacyPreflightProbe for ReadonlyFake {
            async fn read_legacy_meta(&self) -> Result<Vec<(i8, i32)>, ControllerError> {
                self.gate("meta")?;
                Ok(vec![(1, self.version)])
            }
            async fn legacy_shape(
                &self,
                _: i32,
            ) -> Result<Vec<(&'static str, &'static str)>, ControllerError> {
                self.gate("shape")?;
                Ok(LEGACY_CHECK_ORDER.to_vec())
            }
            async fn no_foreign_keys(&self) -> Result<(), ControllerError> {
                self.gate("fk")
            }
            async fn identity_rows(&self, _: i32) -> Result<(), ControllerError> {
                self.gate("data")
            }
            async fn column_status(&self, _: i32) -> Result<Vec<bool>, ControllerError> {
                self.gate("columns")?;
                Ok(vec![self.version == 1; 11])
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

        #[tokio::test]
        async fn readonly_diagnostic_decorator_tracks_real_preflight_order_and_stops() {
            for version in [1, 2] {
                let mut stages = vec!["meta", "shape", "fk", "data", "columns"];
                if version == 1 {
                    stages.extend(["negative"; 10]);
                }
                stages.extend(["encoding", "collision"]);
                let good = DiagnosticProbe::new(ReadonlyFake {
                    version,
                    fault: None,
                });
                let result = preflight_legacy_with_probe(&good, version).await.unwrap();
                assert_eq!(result.old_columns, [version == 1; 11]);
                assert_eq!(good.observation.lock().unwrap().stages, stages);
                for (index, &stage) in stages.iter().enumerate() {
                    if index > 0 && stages[index - 1] == stage {
                        continue;
                    }
                    let bad = DiagnosticProbe::new(ReadonlyFake {
                        version,
                        fault: Some(stage),
                    });
                    let error = preflight_legacy_with_probe(&bad, version)
                        .await
                        .err()
                        .unwrap();
                    let observation = bad.observation.lock().unwrap();
                    assert_eq!(observation.stages, stages[..=index]);
                    assert_eq!(observation.error, Some("users.username"));
                    if stage == "data" {
                        assert_eq!(
                            error.to_string(),
                            legacy_preflight_error("data").to_string()
                        );
                    }
                }
            }
        }
        async fn readonly_diagnostic_pool() -> sqlx::MySqlPool {
            let db_url = std::env::var("RSETUP_TEST_DATABASE_URL")
                .unwrap_or_else(|_| panic!("diagnostic_env_url_missing"));
            let expected = std::env::var("RSETUP_EXPECTED_DATABASE")
                .unwrap_or_else(|_| panic!("diagnostic_env_database_missing"));
            assert!(!db_url.is_empty(), "diagnostic_env_url_empty");
            assert!(!expected.is_empty(), "diagnostic_env_database_empty");
            sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(1)
                .after_connect(move |connection, _meta| {
                    let expected = expected.clone();
                    Box::pin(async move {
                        let rejected =
                            || sqlx::Error::Protocol("diagnostic_connection_rejected".into());
                        let actual: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|_| rejected())?;
                        if actual.as_deref() != Some(expected.as_str()) {
                            return Err(rejected());
                        }
                        let version: Option<String> = sqlx::query_scalar("SELECT VERSION()")
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|_| rejected())?;
                        if version.as_deref() != Some("8.0.46") {
                            return Err(sqlx::Error::Protocol("diagnostic_wrong_engine".into()));
                        }
                        let rows = sqlx::query("SHOW GRANTS")
                            .fetch_all(&mut *connection)
                            .await
                            .map_err(|_| rejected())?;
                        let grants = rows
                            .iter()
                            .map(|row| row.try_get::<Option<String>, _>(0).map_err(|_| rejected()))
                            .collect::<Result<Vec<_>, _>>()?;
                        validate_schema_metadata_grants(&grants, &expected)
                            .map_err(|_| rejected())?;
                        Ok(())
                    })
                })
                .connect(&db_url)
                .await
                .unwrap_or_else(|_| panic!("diagnostic_connect_failed"))
        }

        #[tokio::test]
        #[ignore = "manual one-row SELECT-only MySQL 8.0.46 sample; parent approval and review required"]
        async fn mysql_username_decode_readonly_sample() {
            use sqlx::{TypeInfo, ValueRef};

            let pool = readonly_diagnostic_pool().await;
            let row = sqlx::query("SELECT username AS original, CAST(username AS BINARY) AS raw_bytes FROM users ORDER BY id LIMIT 1")
                .fetch_optional(&pool)
                .await
                .unwrap_or_else(|_| panic!("diagnostic_sample_query_failed"));
            let sample = row.as_ref().map(|row| UsernameSample {
                metadata: row.try_get_raw("original").ok().map(|raw| {
                    let info = raw.type_info();
                    [
                        !raw.is_null(),
                        !info.is_null(),
                        <str as sqlx::Type<sqlx::MySql>>::compatible(&info),
                        <[u8] as sqlx::Type<sqlx::MySql>>::compatible(&info),
                    ]
                }),
                original_str: row.try_get::<&str, _>("original"),
                original_bytes: row.try_get::<&[u8], _>("original"),
                cast_bytes: row.try_get::<&[u8], _>("raw_bytes"),
            });
            let summary = username_sample_summary(sample);
            pool.close().await;
            println!("{summary}");
        }

        // Read-only diagnostic entrypoint; separate from the old None/meta0 probe.
        #[tokio::test]
        #[ignore = "manual SELECT-only MySQL 8.0.46 preflight; parent approval and review required"]
        async fn mysql_failed_upgrade_readonly_preflight_diagnostic() {
            let db = DbPool(readonly_diagnostic_pool().await);
            let version = read_version(&db).await;
            println!("version_class={}", version_class(&version));
            // Independent observation, not an atomic snapshot with preflight.
            let mut columns = std::array::from_fn(|_| Err(ControllerError::InvalidArgument));
            for (index, &(table, column, _, _)) in IDENTITY_COLUMNS.iter().enumerate() {
                columns[index] = read_column(&db, table, column)
                    .await
                    .and_then(|actual| classify_identity_column(table, column, &actual));
            }
            println!("old_columns={}", column_bitmap(&columns));
            let (stage, status, class, checks) = match version {
                Ok(Some(version @ (1 | 2))) => {
                    let probe = DiagnosticProbe::new(db.clone());
                    let result = preflight_legacy_with_probe(&probe, version).await;
                    let observation = probe.observation.lock().unwrap();
                    let checks = check_bitmap(
                        observation
                            .checks
                            .as_deref()
                            .ok_or(&ControllerError::InvalidArgument),
                    );
                    match result {
                        Ok(_) => ("complete", "passed", "none", checks),
                        Err(error) => (
                            observation.stages.last().copied().unwrap_or("meta"),
                            "failed",
                            observation.error.unwrap_or_else(|| error_class(&error)),
                            checks,
                        ),
                    }
                }
                Ok(Some(3)) => match check_identity_schema(&db).await {
                    // The production v3 ready gate rejects every CHECK.
                    Ok(()) => ("complete", "passed", "none", "AAAAA".into()),
                    Err(error) => ("ready", "failed", error_class(&error), "UUUUU".into()),
                },
                Ok(None | Some(_)) => ("meta", "not_applicable", "none", "UUUUU".into()),
                Err(_) => ("meta", "failed", "version_unreadable", "UUUUU".into()),
            };
            println!("known_checks={checks}");
            println!("preflight_stage={stage} preflight_status={status} error_class={class}");
            db.0.close().await;
            println!("diagnostic_complete=true");
            assert!(status != "failed", "diagnostic_preflight_failed");
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum DiagnosticTokenClass {
        ParenOpen,
        ParenClose,
        Operator,
        KnownIdentifier,
        KnownStringLiteral,
        Keyword,
        Other,
        End,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct DiagnosticTokenDiff {
        observed_tokens_parseable: bool,
        expected_tokens_parseable: bool,
        observed_token_count: Option<u8>,
        expected_token_count: Option<u8>,
        first_mismatch_token_index: Option<u16>,
        first_mismatch_token_class: Option<DiagnosticTokenClass>,
        expected_class: Option<DiagnosticTokenClass>,
    }

    impl DiagnosticTokenClass {
        fn code(&self) -> &'static str {
            match self {
                Self::ParenOpen => "paren/open",
                Self::ParenClose => "paren/close",
                Self::Operator => "operator",
                Self::KnownIdentifier => "known_identifier",
                Self::KnownStringLiteral => "known_string_literal",
                Self::Keyword => "keyword",
                Self::Other => "other",
                Self::End => "end",
            }
        }
    }

    fn diagnostic_token_class(token: Option<&str>) -> DiagnosticTokenClass {
        match token {
            None => DiagnosticTokenClass::End,
            Some("(") => DiagnosticTokenClass::ParenOpen,
            Some(")") => DiagnosticTokenClass::ParenClose,
            Some("=" | ",") => DiagnosticTokenClass::Operator,
            Some(
                "singleton" | "admission_state" | "review_decision" | "source_kind" | "role_id"
                | "permissions" | "scope_kind" | "scope_group_id" | "scope_device_id",
            ) => DiagnosticTokenClass::KnownIdentifier,
            Some("'role'" | "'direct'" | "'all'" | "'group'" | "'device'") => {
                DiagnosticTokenClass::KnownStringLiteral
            }
            Some("and" | "or" | "is" | "not" | "null" | "in") => DiagnosticTokenClass::Keyword,
            Some(_) => DiagnosticTokenClass::Other,
        }
    }

    fn diagnostic_token_diff(observed: &str, expected: &str) -> DiagnosticTokenDiff {
        let observed_tokens = legacy_check_tokens(observed);
        let expected_tokens = legacy_check_tokens(expected);
        let observed_token_count = observed_tokens
            .as_ref()
            .map(|tokens| tokens.len().min(255) as u8);
        let expected_token_count = expected_tokens
            .as_ref()
            .map(|tokens| tokens.len().min(255) as u8);
        let mismatch = observed_tokens
            .as_ref()
            .zip(expected_tokens.as_ref())
            .and_then(|(observed, expected)| {
                (0..observed.len().max(expected.len()))
                    .find(|&index| observed.get(index) != expected.get(index))
                    .and_then(|index| {
                        let mismatch_index = u16::try_from(index).ok()?;
                        Some((
                            mismatch_index,
                            diagnostic_token_class(observed.get(index).map(String::as_str)),
                            diagnostic_token_class(expected.get(index).map(String::as_str)),
                        ))
                    })
            });
        DiagnosticTokenDiff {
            observed_tokens_parseable: observed_tokens.is_some(),
            expected_tokens_parseable: expected_tokens.is_some(),
            observed_token_count,
            expected_token_count,
            first_mismatch_token_index: mismatch.as_ref().map(|(index, _, _)| *index),
            first_mismatch_token_class: mismatch.as_ref().map(|(_, class, _)| *class),
            expected_class: mismatch.map(|(_, _, class)| class),
        }
    }

    #[test]
    fn grant_token_diff_ignores_outer_parentheses_but_detects_reordered_tokens() {
        let expected = "(source_kind = 'role')";
        assert_eq!(
            diagnostic_token_diff("(((source_kind = 'role')))", expected),
            DiagnosticTokenDiff {
                observed_tokens_parseable: true,
                expected_tokens_parseable: true,
                observed_token_count: Some(3),
                expected_token_count: Some(3),
                first_mismatch_token_index: None,
                first_mismatch_token_class: None,
                expected_class: None,
            }
        );
        assert_eq!(
            diagnostic_token_diff("(role_id = source_kind)", "(source_kind = role_id)"),
            DiagnosticTokenDiff {
                observed_tokens_parseable: true,
                expected_tokens_parseable: true,
                observed_token_count: Some(3),
                expected_token_count: Some(3),
                first_mismatch_token_index: Some(0),
                first_mismatch_token_class: Some(DiagnosticTokenClass::KnownIdentifier),
                expected_class: Some(DiagnosticTokenClass::KnownIdentifier),
            }
        );
    }

    #[test]
    fn grant_token_diff_reports_unparseable_without_guessing_mismatch() {
        assert_eq!(
            diagnostic_token_diff("(source_kind = 'ro\\'le')", "(source_kind = 'role')"),
            DiagnosticTokenDiff {
                observed_tokens_parseable: false,
                expected_tokens_parseable: true,
                observed_token_count: None,
                expected_token_count: Some(3),
                first_mismatch_token_index: None,
                first_mismatch_token_class: None,
                expected_class: None,
            }
        );
    }

    #[test]
    fn grant_token_diff_classifies_only_fixed_categories_and_end() {
        assert_eq!(
            diagnostic_token_diff("('rogue')", "('role')"),
            DiagnosticTokenDiff {
                observed_tokens_parseable: true,
                expected_tokens_parseable: true,
                observed_token_count: Some(1),
                expected_token_count: Some(1),
                first_mismatch_token_index: Some(0),
                first_mismatch_token_class: Some(DiagnosticTokenClass::Other),
                expected_class: Some(DiagnosticTokenClass::KnownStringLiteral),
            }
        );
        assert_eq!(
            diagnostic_token_diff("(source_kind)", "(source_kind = 'role')"),
            DiagnosticTokenDiff {
                observed_tokens_parseable: true,
                expected_tokens_parseable: true,
                observed_token_count: Some(1),
                expected_token_count: Some(3),
                first_mismatch_token_index: Some(1),
                first_mismatch_token_class: Some(DiagnosticTokenClass::End),
                expected_class: Some(DiagnosticTokenClass::Operator),
            }
        );
    }

    #[test]
    fn grant_token_diff_bounds_counts_and_classifies_parentheses_keywords() {
        let many = "source_kind ".repeat(300);
        let capped = diagnostic_token_diff(&many, &many);
        assert_eq!(capped.observed_token_count, Some(255));
        assert_eq!(capped.expected_token_count, Some(255));
        assert_eq!(capped.first_mismatch_token_index, None);

        let open = diagnostic_token_diff("source_kind = (role_id)", "source_kind = role_id");
        assert_eq!(open.first_mismatch_token_index, Some(2));
        assert_eq!(
            open.first_mismatch_token_class,
            Some(DiagnosticTokenClass::ParenOpen)
        );
        assert_eq!(
            open.expected_class,
            Some(DiagnosticTokenClass::KnownIdentifier)
        );

        let close = diagnostic_token_diff(
            "source_kind = (role_id) OR role_id",
            "source_kind = (role_id OR role_id)",
        );
        assert_eq!(close.first_mismatch_token_index, Some(4));
        assert_eq!(
            close.first_mismatch_token_class,
            Some(DiagnosticTokenClass::ParenClose)
        );
        assert_eq!(close.expected_class, Some(DiagnosticTokenClass::Keyword));
    }

    // Test-only: compare restored metadata to the original fixed 0001 declaration.
    // The returned summary never contains tokens or CHECK text.
    fn fixed_grant_check_token_diff(index: usize, stored: &str) -> Option<DiagnosticTokenDiff> {
        if !matches!(index, 3 | 4) {
            return None;
        }
        let (table, name, _) = LEGACY_CHECK_DROPS[index];
        let declared = ddl_parts_from(MIGRATION, table)
            .into_iter()
            .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))?;
        let expression = declared.split_once("CHECK ")?.1;
        let restored = stored.replace("\\'", "'");
        Some(diagnostic_token_diff(&restored, expression))
    }

    #[test]
    fn grant_token_diff_only_uses_two_fixed_declared_checks() {
        let stored = "((`source_kind` = _utf8mb4\\'role\\' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4\\'direct\\' AND `role_id` IS NULL AND `permissions` IS NOT NULL))";
        let diff = fixed_grant_check_token_diff(3, stored).expect("fixed source CHECK");
        assert!(diff.observed_tokens_parseable);
        assert!(diff.expected_tokens_parseable);
        assert_eq!(diff.first_mismatch_token_index, None);
        assert_eq!(fixed_grant_check_token_diff(0, stored), None);
        assert_eq!(fixed_grant_check_token_diff(1, stored), None);
        assert_eq!(fixed_grant_check_token_diff(2, stored), None);
        assert_eq!(fixed_grant_check_token_diff(5, stored), None);

        let scope = "((`scope_kind` = _utf8mb4\\'all\\' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4\\'group\\' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4\\'device\\' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))";
        let scope_diff = fixed_grant_check_token_diff(4, scope).expect("fixed scope CHECK");
        assert!(scope_diff.observed_tokens_parseable);
        assert!(scope_diff.expected_tokens_parseable);
        assert_eq!(scope_diff.first_mismatch_token_index, None);
    }

    #[test]
    fn grant_token_diff_output_stays_inside_check_mismatch_gate() {
        let diagnostic = include_str!("db.rs")
            .split_once("\n    async fn mysql_preserved_v1_legacy_select_gates_diagnostic()")
            .unwrap()
            .1
            .split_once("\n    #[test]\n    fn decode_snapshot_rejects_invalid_pairs")
            .unwrap()
            .0;
        let check_guard = diagnostic
            .find("if code == \"check_unavailable_or_incompatible\"")
            .unwrap();
        let mismatch_guard = diagnostic.find("Some(index @ (3 | 4))").unwrap();
        let probe = diagnostic
            .find("fixed_grant_check_escape_probe(index, stored)")
            .unwrap();
        let token_diff = diagnostic
            .find("fixed_grant_check_token_diff(index, stored)")
            .expect("missing token diff");
        let output = diagnostic
            .find("observed_tokens_parseable={}")
            .expect("missing fixed output");
        assert!(
            check_guard < mismatch_guard
                && mismatch_guard < probe
                && probe < token_diff
                && token_diff < output
        );
        assert!(diagnostic.contains("first_mismatch_token_class={:?}"));
        assert!(diagnostic.contains("expected_class={:?}"));
        let wrapped_probe = diagnostic
            .find("fixed_wrapped_grant_probe(index, stored)")
            .expect("missing fixed wrapped-candidate probe");
        let wrapped_output = diagnostic
            .find("wrapped_candidate_exact_bytes={} wrapped_candidate_tokens_equal={}")
            .expect("missing wrapped-candidate booleans");
        assert!(mismatch_guard < wrapped_probe && wrapped_probe < wrapped_output);
    }

    // Diagnostic only: full fixed candidates for the atom-wrapping hypothesis.
    // They are never accepted by the production CHECK validator.
    fn fixed_wrapped_grant_candidate(index: usize) -> Option<&'static str> {
        match index {
            3 => Some(concat!(
                "(((`source_kind` = _utf8mb4'role') AND (`role_id` IS NOT NULL) AND (`permissions` IS NULL)) OR ",
                "((`source_kind` = _utf8mb4'direct') AND (`role_id` IS NULL) AND (`permissions` IS NOT NULL)))",
            )),
            4 => Some(concat!(
                "(((`scope_kind` = _utf8mb4'all') AND (`scope_group_id` IS NULL) AND (`scope_device_id` IS NULL)) OR ",
                "((`scope_kind` = _utf8mb4'group') AND (`scope_group_id` IS NOT NULL) AND (`scope_device_id` IS NULL)) OR ",
                "((`scope_kind` = _utf8mb4'device') AND (`scope_group_id` IS NULL) AND (`scope_device_id` IS NOT NULL)))",
            )),
            _ => None,
        }
    }

    fn fixed_wrapped_grant_probe(index: usize, stored: &str) -> Option<(bool, bool)> {
        let candidate = fixed_wrapped_grant_candidate(index)?;
        let restored = stored.replace("\\'", "'");
        Some((
            stored == candidate.replace('\'', "\\'"),
            legacy_check_tokens(&restored)
                .zip(legacy_check_tokens(candidate))
                .is_some_and(|(observed, expected)| observed == expected),
        ))
    }

    #[test]
    fn fixed_wrapped_grant_candidates_match_only_declared_atom_layouts() {
        // Compare against the original 0001 tokens using fixed atom widths and
        // explicit AND/OR positions, never arbitrary parenthesis removal.
        for (index, groups, count) in [
            (3, &[&[3, 4, 3][..], &[3, 3, 4][..]][..], 41),
            (4, &[&[3, 3, 3][..], &[3, 4, 3][..], &[3, 3, 4][..]][..], 61),
        ] {
            let (table, name, _) = LEGACY_CHECK_DROPS[index];
            let declared = ddl_parts_from(MIGRATION, table)
                .into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))
                .expect("fixed 0001 CHECK");
            let original =
                legacy_check_tokens(declared.split_once("CHECK ").unwrap().1).expect("0001 tokens");
            assert_eq!(original.len(), if index == 3 { 29 } else { 43 });
            let candidate = fixed_wrapped_grant_candidate(index).expect("fixed candidate");
            let observed = legacy_check_tokens(candidate).expect("fixed candidate tokens");
            assert_eq!(observed.len(), count);
            let mut expected_wrapped = Vec::new();
            let mut cursor = 0;
            for (group_index, atoms) in groups.iter().enumerate() {
                assert_eq!(original[cursor], "(");
                expected_wrapped.push(original[cursor].clone());
                cursor += 1;
                for (atom_index, &width) in atoms.iter().enumerate() {
                    expected_wrapped.push("(".to_owned());
                    expected_wrapped.extend(original[cursor..cursor + width].iter().cloned());
                    cursor += width;
                    expected_wrapped.push(")".to_owned());
                    if atom_index + 1 < atoms.len() {
                        assert_eq!(original[cursor], "and");
                        expected_wrapped.push(original[cursor].clone());
                        cursor += 1;
                    }
                }
                assert_eq!(original[cursor], ")");
                expected_wrapped.push(original[cursor].clone());
                cursor += 1;
                if group_index + 1 < groups.len() {
                    assert_eq!(original[cursor], "or");
                    expected_wrapped.push(original[cursor].clone());
                    cursor += 1;
                }
            }
            assert_eq!(cursor, original.len());
            assert_eq!(observed, expected_wrapped);
            assert_eq!(
                fixed_wrapped_grant_probe(index, &candidate.replace('\'', "\\'")),
                Some((true, true))
            );
            assert_eq!(fixed_wrapped_grant_probe(0, candidate), None);
            assert_eq!(fixed_wrapped_grant_probe(1, candidate), None);
            assert_eq!(fixed_wrapped_grant_probe(2, candidate), None);
            assert_eq!(fixed_wrapped_grant_probe(5, candidate), None);
        }
    }

    #[test]
    fn fixed_wrapped_grant_probe_rejects_literal_column_connective_and_paren_mutations() {
        for index in [3, 4] {
            let candidate = fixed_wrapped_grant_candidate(index).expect("fixed candidate");
            let escaped = candidate.replace('\'', "\\'");
            let (literals, columns): (&[&str], &[&str]) = if index == 3 {
                (
                    &["role", "direct"],
                    &["source_kind", "role_id", "permissions"],
                )
            } else {
                (
                    &["all", "group", "device"],
                    &["scope_kind", "scope_group_id", "scope_device_id"],
                )
            };
            for literal in literals {
                let needle = format!("\\'{literal}\\'");
                for (offset, _) in escaped.match_indices(&needle) {
                    let mut changed = escaped.clone();
                    changed.replace_range(offset..offset + needle.len(), "\\'rogue\\'");
                    assert_eq!(
                        fixed_wrapped_grant_probe(index, &changed),
                        Some((false, false))
                    );
                }
            }
            for column in columns {
                let needle = format!("`{column}`");
                for (offset, _) in escaped.match_indices(&needle) {
                    let mut changed = escaped.clone();
                    changed.replace_range(offset..offset + needle.len(), "`rogue`");
                    assert_eq!(
                        fixed_wrapped_grant_probe(index, &changed),
                        Some((false, false))
                    );
                }
            }
            for (needle, replacement) in [(" AND ", " OR "), (" OR ", " AND ")] {
                for (offset, _) in escaped.match_indices(needle) {
                    let mut changed = escaped.clone();
                    changed.replace_range(offset..offset + needle.len(), replacement);
                    assert_eq!(
                        fixed_wrapped_grant_probe(index, &changed),
                        Some((false, false))
                    );
                }
            }
            let extra_atom_parens = escaped
                .replacen("(((", "((((", 1)
                .replacen(") AND ", ")) AND ", 1);
            assert_eq!(
                fixed_wrapped_grant_probe(index, &extra_atom_parens),
                Some((false, false))
            );
            assert_eq!(
                fixed_wrapped_grant_probe(index, &format!("({escaped})")),
                Some((false, true))
            );
            assert_eq!(
                fixed_wrapped_grant_probe(index, candidate),
                Some((false, true))
            );
        }
    }

    // Test-only observation shape; no metadata text is retained in printable fields.
    #[derive(Debug)]
    struct GrantFormatObservation {
        stored_len: usize,
        candidate_len: usize,
        leading_parens: usize,
        trailing_parens: usize,
        ascii_whitespace: usize,
        backticks: usize,
        wrapped_matches: [bool; 5],
        enforced: &'static str,
    }

    // Compare the complete synthetic spelling, permitting case changes only in
    // fixed SQL keywords. All literal/identifier bytes and inner spaces remain exact.
    fn fixed_keyword_case_match(observed: &str, expected: &str) -> bool {
        let observed = observed.as_bytes();
        let expected = expected.as_bytes();
        if observed.len() != expected.len() {
            return false;
        }
        let mut offset = 0;
        while offset < expected.len() {
            if expected[offset].is_ascii_alphabetic() {
                let end = offset
                    + expected[offset..]
                        .iter()
                        .take_while(|byte| byte.is_ascii_alphabetic())
                        .count();
                let word = &expected[offset..end];
                let keyword = [b"AND".as_slice(), b"OR", b"IS", b"NOT", b"NULL"].contains(&word);
                if (keyword && !observed[offset..end].eq_ignore_ascii_case(word))
                    || (!keyword && observed[offset..end] != *word)
                {
                    return false;
                }
                offset = end;
            } else {
                if observed[offset] != expected[offset] {
                    return false;
                }
                offset += 1;
            }
        }
        true
    }

    fn diagnostic_enforced_code(enforced: Option<&str>) -> &'static str {
        match enforced {
            Some("YES") => "yes",
            Some("NO") => "no",
            _ => "unknown",
        }
    }

    fn grant_format_observation(
        index: usize,
        stored: &str,
        enforced: Option<&str>,
    ) -> Option<GrantFormatObservation> {
        let literals: &[&str] = match index {
            3 => &["role", "direct"],
            4 => &["all", "group", "device"],
            _ => return None,
        };
        let candidate = fixed_wrapped_grant_candidate(index)?.replace('\'', "\\'");
        let bytes = stored.as_bytes();
        let leading_parens = stored
            .trim_ascii_start()
            .bytes()
            .take_while(|&b| b == b'(')
            .count();
        let trailing_parens = stored
            .trim_ascii_end()
            .bytes()
            .rev()
            .take_while(|&b| b == b')')
            .count();
        let ascii_whitespace = bytes
            .iter()
            .filter(|byte| byte.is_ascii_whitespace())
            .count();
        let backticks = bytes.iter().filter(|&&byte| byte == b'`').count();
        let pair_count = bytes.windows(2).filter(|pair| *pair == b"\\'").count();
        let slash_count = bytes.iter().filter(|&&byte| byte == b'\\').count();
        if bytes.len() >= 4096
            || candidate.len() >= 4096
            || leading_parens > 8
            || trailing_parens > 8
            || ascii_whitespace > 255
            || pair_count != 2 * literals.len()
            || slash_count != pair_count
            || stored.matches("_utf8mb4").count() != literals.len()
            || literals.iter().any(|literal| {
                let anchor = format!("_utf8mb4\\'{literal}\\'");
                stored.matches(&anchor).count() != 1
            })
        {
            return None;
        }
        let stored_trimmed = stored.trim_ascii();
        let wrapped_matches = std::array::from_fn(|layers| {
            let wrapped = format!("{}{}{}", "(".repeat(layers), candidate, ")".repeat(layers));
            fixed_keyword_case_match(stored_trimmed, &wrapped)
        });
        Some(GrantFormatObservation {
            stored_len: bytes.len(),
            candidate_len: candidate.len(),
            leading_parens,
            trailing_parens,
            ascii_whitespace,
            backticks,
            wrapped_matches,
            enforced: diagnostic_enforced_code(enforced),
        })
    }

    #[test]
    fn grant_format_observation_does_not_emit_check_fingerprint() {
        let source = include_str!("db.rs");
        let probe = source
            .split_once("struct GrantFormatObservation {")
            .unwrap()
            .1
            .split_once("    // Compare the complete synthetic spelling")
            .unwrap()
            .0;
        let helper = source
            .split_once("fn grant_format_observation(")
            .unwrap()
            .1
            .split_once(
                "    #[test]\n    fn grant_format_observation_does_not_emit_check_fingerprint",
            )
            .unwrap()
            .0;
        let diagnostic = source
            .split_once("\n    async fn mysql_preserved_v1_legacy_select_gates_diagnostic()")
            .unwrap()
            .1
            .split_once("    #[test]\n    fn decode_snapshot_rejects_invalid_pairs")
            .unwrap()
            .0;
        assert!(!probe.contains("sha256"));
        assert!(!helper.contains("Sha256"));
        assert!(!diagnostic.contains("stored_sha256"));
        assert!(diagnostic.contains("row.try_get::<Option<String>, _>(\"enforced\")"));
        assert!(diagnostic.contains(".unwrap_or(None)"));
    }

    #[test]
    fn grant_format_observation_accepts_only_fixed_synthetic_anchors() {
        for (index, literal_count, slash_count) in [(3, 2, 4), (4, 3, 6)] {
            let candidate = fixed_wrapped_grant_candidate(index).unwrap();
            let stored = candidate.replace('\'', "\\'");
            let observed = grant_format_observation(index, &stored, Some("YES"))
                .expect("synthetic readback must be observable");
            assert_eq!(observed.stored_len, stored.len());
            assert_eq!(observed.candidate_len, stored.len());
            assert_eq!(observed.leading_parens, 3);
            assert_eq!(observed.trailing_parens, 3);
            assert_eq!(
                observed.ascii_whitespace,
                stored.bytes().filter(u8::is_ascii_whitespace).count()
            );
            assert_eq!(
                observed.backticks,
                stored.bytes().filter(|&byte| byte == b'`').count()
            );
            assert_eq!(observed.wrapped_matches, [true, false, false, false, false]);
            assert_eq!(observed.enforced, "yes");
            assert_eq!(stored.matches("_utf8mb4").count(), literal_count);
            assert_eq!(
                stored.bytes().filter(|&byte| byte == b'\\').count(),
                slash_count
            );
            assert_eq!(
                grant_format_observation(index, &stored, Some("NO"))
                    .unwrap()
                    .enforced,
                "no"
            );
            assert_eq!(
                grant_format_observation(index, &stored, None)
                    .unwrap()
                    .enforced,
                "unknown"
            );
            assert_eq!(
                grant_format_observation(index, &stored, Some("YES"))
                    .unwrap()
                    .enforced
                    .to_string(),
                "yes"
            );
            assert_eq!(
                grant_format_observation(index, &stored, Some("NO"))
                    .unwrap()
                    .enforced
                    .to_string(),
                "no"
            );
            for value in [None, Some("MAYBE"), Some("yes"), Some("")] {
                assert_eq!(
                    grant_format_observation(index, &stored, value)
                        .unwrap()
                        .enforced,
                    "unknown"
                );
            }
        }
    }

    #[test]
    fn grant_format_observation_distinguishes_wrapping_and_keyword_case() {
        let stored = fixed_wrapped_grant_candidate(3)
            .unwrap()
            .replace('\'', "\\'");
        for layers in 1..=4 {
            let wrapped = format!(
                " \t{}{}{}\n",
                "(".repeat(layers),
                stored,
                ")".repeat(layers)
            );
            let observed = grant_format_observation(3, &wrapped, Some("YES")).unwrap();
            assert!(observed.wrapped_matches[layers]);
            assert_eq!(
                observed.wrapped_matches.iter().filter(|&&hit| hit).count(),
                1
            );
        }
        let case_only = stored.replace(" AND ", " and ");
        assert!(
            grant_format_observation(3, &case_only, Some("YES"))
                .unwrap()
                .wrapped_matches[0]
        );
        let literal_changed = stored.replace("\\'role\\'", "\\'ROLE\\'");
        assert!(grant_format_observation(3, &literal_changed, Some("YES")).is_none());
        let inner_space = stored.replacen(" AND ", "  AND ", 1);
        assert_eq!(
            grant_format_observation(3, &inner_space, Some("YES"))
                .unwrap()
                .wrapped_matches,
            [false; 5]
        );
    }

    #[test]
    fn grant_format_observation_rejects_anchor_and_bound_mutations() {
        for index in [3, 4] {
            let stored = fixed_wrapped_grant_candidate(index)
                .unwrap()
                .replace('\'', "\\'");
            for changed in [
                stored.replacen("_utf8mb4", "_latin1", 1),
                stored.replacen("_utf8mb4", "", 1),
                stored.replacen("\\'", "'", 1),
                stored.replacen("\\'", "\\\\'", 1),
                format!("{stored}\\x"),
                format!("{}{}", " ".repeat(256), stored),
                format!("{}{}{}", "(".repeat(9), stored, ")".repeat(9)),
                "X".repeat(4096),
            ] {
                assert!(grant_format_observation(index, &changed, Some("YES")).is_none());
            }
        }
    }

    #[test]
    fn diagnostic_session_mode_flags_are_only_fixed_booleans() {
        assert_eq!(
            diagnostic_session_mode_flags(Some("STRICT_TRANS_TABLES"), Some("utf8mb4")),
            (Some(true), Some(true))
        );
        assert_eq!(
            diagnostic_session_mode_flags(
                Some("NO_BACKSLASH_ESCAPES,STRICT_TRANS_TABLES"),
                Some("latin1")
            ),
            (Some(false), Some(false))
        );
        assert_eq!(diagnostic_session_mode_flags(None, None), (None, None));
    }

    fn diagnostic_session_mode_flags(
        mode: Option<&str>,
        charset: Option<&str>,
    ) -> (Option<bool>, Option<bool>) {
        (
            mode.map(|mode| {
                !mode
                    .split(',')
                    .any(|flag| flag.trim_ascii() == "NO_BACKSLASH_ESCAPES")
            }),
            charset.map(|charset| charset == "utf8mb4"),
        )
    }

    #[test]
    fn grant_format_observation_read_path_stays_inside_existing_ignored_shape_failure() {
        let source = include_str!("db.rs");
        let diagnostic = source
            .split_once("\n    async fn mysql_preserved_v1_legacy_select_gates_diagnostic()")
            .unwrap()
            .1
            .split_once("\n    #[test]\n    fn decode_snapshot_rejects_invalid_pairs")
            .unwrap()
            .0;
        let gate = diagnostic
            .find("if code == \"check_unavailable_or_incompatible\"")
            .unwrap();
        let clause = diagnostic
            .find("CAST(cc.check_clause AS CHAR) AS check_clause")
            .unwrap();
        let enforced = diagnostic
            .find("CAST(tc.enforced AS CHAR) AS enforced")
            .unwrap();
        let flag_mode = diagnostic.find("SELECT @@SESSION.sql_mode").unwrap();
        let flag_charset = diagnostic
            .find("SELECT @@SESSION.character_set_connection")
            .unwrap();
        let mismatch = diagnostic.find("Some(index @ (3 | 4))").unwrap();
        let observation = diagnostic
            .find("grant_format_observation(index, stored")
            .unwrap();
        let panic = diagnostic
            .find("panic!(\"diagnostic_shape_{code} table_index={table_index:?}\")")
            .unwrap();
        let acquire = diagnostic[gate..]
            .find("db.0.acquire()")
            .expect("CHECK failure branch must acquire one connection")
            + gate;
        let check_read = diagnostic
            .find("CAST(cc.check_clause AS CHAR) AS check_clause")
            .unwrap();
        assert!(gate < acquire && acquire < flag_mode && flag_mode < flag_charset);
        assert!(flag_charset < check_read && check_read == clause && clause < enforced);
        let sample =
            &diagnostic[acquire..check_read + diagnostic[check_read..].find(".await;").unwrap()];
        assert_eq!(sample.matches("db.0.acquire()").count(), 1);
        assert_eq!(sample.matches("&mut *connection").count(), 3);
        assert!(!sample.contains("&db.0"));
        assert!(diagnostic[check_read..].contains("drop(connection);"));
        assert!(enforced < mismatch && mismatch < observation && observation < panic);
        assert!(!diagnostic.contains("println!(\"{stored}"));
    }

    type DiagnosticCheckRow = Result<(Option<String>, Option<String>, Option<String>), ()>;
    type DiagnosticEnforcedCheckRow = Result<
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
        (),
    >;

    #[derive(Debug, PartialEq, Eq)]
    struct GrantCheckEscapeProbe {
        candidate_equal: bool,
        backslash_quote_pair_count: u8,
        total_backslash_count: u8,
        restored_tokens_equal: bool,
    }

    // Diagnostic only: these are the two full, previously known unescaped
    // MySQL readbacks, not a new rule for accepting legacy CHECK metadata.
    fn fixed_grant_check_escape_probe(index: usize, stored: &str) -> Option<GrantCheckEscapeProbe> {
        let unescaped = match index {
            3 => {
                "((`source_kind` = _utf8mb4'role' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4'direct' AND `role_id` IS NULL AND `permissions` IS NOT NULL))"
            }
            4 => {
                "((`scope_kind` = _utf8mb4'all' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'group' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'device' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))"
            }
            _ => return None,
        };
        let (table, name, _) = LEGACY_CHECK_DROPS[index];
        let declared = ddl_parts_from(MIGRATION, table)
            .into_iter()
            .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))
            .and_then(|part| {
                part.split_once("CHECK ")
                    .map(|(_, expression)| expression.to_owned())
            });
        let restored = stored.replace("\\'", "'");
        let restored_tokens_equal = declared
            .as_deref()
            .and_then(|expression| {
                legacy_check_tokens(&restored).zip(legacy_check_tokens(expression))
            })
            .is_some_and(|(actual, expected)| actual == expected);
        let bytes = stored.as_bytes();
        Some(GrantCheckEscapeProbe {
            candidate_equal: stored == unescaped.replace('\'', "\\'"),
            backslash_quote_pair_count: bytes
                .windows(2)
                .filter(|pair| *pair == b"\\'")
                .count()
                .min(255) as u8,
            total_backslash_count: bytes.iter().filter(|&&byte| byte == b'\\').count().min(255)
                as u8,
            restored_tokens_equal,
        })
    }

    #[test]
    fn grant_check_escape_probe_matches_only_fully_escaped_fixed_readbacks() {
        for (index, unescaped, quote_pairs) in [
            (
                3,
                "((`source_kind` = _utf8mb4'role' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4'direct' AND `role_id` IS NULL AND `permissions` IS NOT NULL))",
                4,
            ),
            (
                4,
                "((`scope_kind` = _utf8mb4'all' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'group' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'device' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))",
                6,
            ),
        ] {
            let escaped = unescaped.replace('\'', "\\'");
            assert_eq!(
                fixed_grant_check_escape_probe(index, &escaped),
                Some(GrantCheckEscapeProbe {
                    candidate_equal: true,
                    backslash_quote_pair_count: quote_pairs,
                    total_backslash_count: quote_pairs,
                    restored_tokens_equal: true,
                })
            );
            assert_eq!(fixed_grant_check_escape_probe(1, &escaped), None);
            assert_eq!(fixed_grant_check_escape_probe(2, &escaped), None);
        }
    }

    #[test]
    fn grant_check_escape_probe_distinguishes_literal_and_escape_mutations() {
        let unescaped = "((`source_kind` = _utf8mb4'role' AND `role_id` IS NOT NULL AND `permissions` IS NULL) OR (`source_kind` = _utf8mb4'direct' AND `role_id` IS NULL AND `permissions` IS NOT NULL))";
        assert_eq!(
            fixed_grant_check_escape_probe(3, unescaped),
            Some(GrantCheckEscapeProbe {
                candidate_equal: false,
                backslash_quote_pair_count: 0,
                total_backslash_count: 0,
                restored_tokens_equal: true,
            })
        );
        let escaped = unescaped.replace('\'', "\\'");
        assert_eq!(
            fixed_grant_check_escape_probe(3, &escaped.replace("role", "rogue")),
            Some(GrantCheckEscapeProbe {
                candidate_equal: false,
                backslash_quote_pair_count: 4,
                total_backslash_count: 4,
                restored_tokens_equal: false,
            })
        );
        assert_eq!(
            fixed_grant_check_escape_probe(3, &escaped.replacen("\\'role", "'role", 1)),
            Some(GrantCheckEscapeProbe {
                candidate_equal: false,
                backslash_quote_pair_count: 3,
                total_backslash_count: 3,
                restored_tokens_equal: true,
            })
        );
        assert_eq!(
            fixed_grant_check_escape_probe(3, &escaped.replacen("\\'role", "\\\\'role", 1)),
            Some(GrantCheckEscapeProbe {
                candidate_equal: false,
                backslash_quote_pair_count: 4,
                total_backslash_count: 5,
                restored_tokens_equal: false,
            })
        );
        let scope = "((`scope_kind` = _utf8mb4'all' AND `scope_group_id` IS NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'group' AND `scope_group_id` IS NOT NULL AND `scope_device_id` IS NULL) OR (`scope_kind` = _utf8mb4'device' AND `scope_group_id` IS NULL AND `scope_device_id` IS NOT NULL))";
        assert_eq!(
            fixed_grant_check_escape_probe(
                4,
                &scope.replace("'group'", "'groups'").replace('\'', "\\'")
            ),
            Some(GrantCheckEscapeProbe {
                candidate_equal: false,
                backslash_quote_pair_count: 6,
                total_backslash_count: 6,
                restored_tokens_equal: false,
            })
        );
    }

    // Only emit compiled-in CHECK indices and fixed classifications, never metadata bytes.
    fn legacy_diagnostic_check_classifications(
        rows: &[DiagnosticCheckRow],
    ) -> Vec<(Option<usize>, &'static str)> {
        let mut seen = [false; 5];
        rows.iter()
            .map(|row| {
                let Ok((table, name, clause)) = row else {
                    return (None, "decode_failed");
                };
                let Some(index) =
                    LEGACY_CHECK_DROPS
                        .iter()
                        .position(|&(known_table, known_name, _)| {
                            table.as_deref() == Some(known_table)
                                && name.as_deref() == Some(known_name)
                        })
                else {
                    return (None, "unknown_or_duplicate");
                };
                if seen[index] {
                    return (Some(index), "unknown_or_duplicate");
                }
                seen[index] = true;
                let (table, name, _) = LEGACY_CHECK_DROPS[index];
                let Some(stored) = clause.as_deref() else {
                    return (Some(index), "missing_clause");
                };
                let declared = ddl_parts_from(MIGRATION, table)
                    .into_iter()
                    .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))
                    .and_then(|part| {
                        part.split_once("CHECK ")
                            .map(|(_, expression)| expression.to_owned())
                    });
                let Some(declared) = declared else {
                    return (Some(index), "literal_escape_or_other_mismatch");
                };
                let valid = validate_legacy_check_metadata(&[(
                    Some(table.to_owned()),
                    Some(name.to_owned()),
                    Some(stored.to_owned()),
                )])
                .is_ok();
                let token_equal = legacy_check_tokens(stored)
                    .zip(legacy_check_tokens(&declared))
                    .is_some_and(|(stored, expected)| stored == expected);
                let classification = if valid && token_equal {
                    "token_equal"
                } else if valid && check_clause_matches(table, name, stored, &declared) {
                    "known_mysql_spelling"
                } else {
                    "literal_escape_or_other_mismatch"
                };
                (Some(index), classification)
            })
            .collect()
    }

    #[test]
    fn legacy_diagnostic_check_mapper_rejects_unknown_and_duplicate() {
        let known = (
            Some("schema_meta".to_owned()),
            Some("chk_schema_singleton".to_owned()),
            Some("(singleton = 1)".to_owned()),
        );
        assert_eq!(
            legacy_diagnostic_check_classifications(&[
                Ok((
                    Some("unknown".into()),
                    Some("unknown".into()),
                    known.2.clone()
                )),
                Ok(known.clone()),
                Ok(known),
            ]),
            vec![
                (None, "unknown_or_duplicate"),
                (Some(0), "token_equal"),
                (Some(0), "unknown_or_duplicate"),
            ]
        );
    }

    #[test]
    fn legacy_diagnostic_check_mapper_distinguishes_mismatch_from_token_equality() {
        assert_eq!(
            legacy_diagnostic_check_classifications(&[
                Ok((
                    Some("schema_meta".into()),
                    Some("chk_schema_singleton".into()),
                    Some("(singleton = 2)".into()),
                )),
                Ok((
                    Some("grants".into()),
                    Some("chk_grants_source".into()),
                    None
                )),
                Err(()),
            ]),
            vec![
                (Some(0), "literal_escape_or_other_mismatch"),
                (Some(3), "missing_clause"),
                (None, "decode_failed"),
            ]
        );
    }

    #[test]
    fn legacy_diagnostic_check_mapper_preserves_exact_known_mysql_spelling_boundary() {
        let observed = "(`admission_state` in (_utf8mb4\\'PENDING\\',_utf8mb4\\'APPROVED\\',_utf8mb4\\'REVOKED\\'))";
        let altered = observed.replace("PENDING", "PEND ING");
        let rows = [
            Ok((
                Some("devices".into()),
                Some("chk_devices_state".into()),
                Some(observed.into()),
            )),
            Ok((
                Some("grants".into()),
                Some("chk_grants_source".into()),
                Some(altered),
            )),
        ];
        assert_eq!(
            legacy_diagnostic_check_classifications(&rows),
            vec![
                (Some(1), "known_mysql_spelling"),
                (Some(3), "literal_escape_or_other_mismatch"),
            ]
        );
    }

    // Diagnostic output may only contain codes and indices from compiled-in lists.
    fn legacy_diagnostic_shape_code(error: &ControllerError) -> (&'static str, Option<usize>) {
        let ControllerError::Config(message) = error else {
            return ("shape_read_error", None);
        };
        if message == "identity schema has missing or unexpected tables; manual inspection required"
        {
            return ("table_set", None);
        }
        if message == "legacy CHECK metadata unavailable or incompatible" {
            return ("check_unavailable_or_incompatible", None);
        }
        for (prefix, code) in [
            ("migration incompatible table charset ", "table_charset"),
            ("migration incompatible column names in ", "column_set"),
            ("migration missing column ", "column_set"),
            ("migration incompatible column ", "column_shape"),
            ("migration incompatible default ", "column_default"),
            (
                "migration incompatible character metadata ",
                "column_charset",
            ),
            ("migration incompatible index order ", "index_order"),
            ("migration incompatible index set in ", "index_set"),
            ("migration incompatible index ", "index_set"),
        ] {
            if let Some(suffix) = message.strip_prefix(prefix) {
                let index = TABLES.iter().position(|(table, _, _)| {
                    suffix == *table
                        || suffix
                            .strip_prefix(table)
                            .is_some_and(|rest| rest.starts_with('.'))
                });
                return (code, index);
            }
        }
        ("shape_read_error", None)
    }

    #[test]
    fn legacy_diagnostic_mapper_redacts_errors_and_uses_static_table_indices() {
        assert_eq!(
            legacy_diagnostic_shape_code(&ControllerError::Config(
                "migration incompatible default grants.revision secret-marker".into()
            )),
            ("column_default", Some(8))
        );
        assert_eq!(
            legacy_diagnostic_shape_code(&ControllerError::Config(
                "legacy CHECK metadata unavailable or incompatible".into()
            )),
            ("check_unavailable_or_incompatible", None)
        );
        assert_eq!(
            legacy_diagnostic_shape_code(&ControllerError::Config(
                "migration incompatible column users.username: got sensitive-marker".into()
            )),
            ("column_shape", Some(1))
        );
        assert_eq!(
            legacy_diagnostic_shape_code(&ControllerError::Config(
                "unknown sensitive-marker".into()
            )),
            ("shape_read_error", None)
        );
    }

    #[test]
    fn legacy_diagnostic_rechecks_identity_engine_and_grants_on_every_physical_connection() {
        // No live DB here: this guards the hook's placement and read-only queries,
        // not a runtime reconnect or a physical server identity proof.
        let diagnostic = include_str!("db.rs")
            .split_once("\n    async fn mysql_preserved_v1_legacy_select_gates_diagnostic()")
            .unwrap()
            .1
            .split_once("\n    #[test]\n    fn decode_snapshot_rejects_invalid_pairs")
            .unwrap()
            .0;
        let hook_start = diagnostic.find(".after_connect(").expect("missing hook");
        let connect = diagnostic.find(".connect(&db_url)").unwrap();
        assert!(hook_start < connect);
        let hook = &diagnostic[hook_start..connect];
        let database = hook.find("SELECT DATABASE()").unwrap();
        let version = hook.find("SELECT VERSION()").unwrap();
        let grants = hook.find("SHOW GRANTS").unwrap();
        let validate = hook.find("validate_schema_metadata_grants(").unwrap();
        assert!(database < version && version < grants && grants < validate);
        assert!(hook.contains("&mut *connection"));
        assert!(hook.contains("diagnostic_wrong_engine"));
        assert!(hook.contains("diagnostic_connection_rejected"));
        assert!(hook.contains("8.0.46"));
    }

    #[test]
    fn legacy_diagnostic_counts_five_checks_before_validating_shape() {
        let diagnostic = include_str!("db.rs")
            .split_once("\n    async fn mysql_preserved_v1_legacy_select_gates_diagnostic()")
            .unwrap()
            .1
            .split_once("\n    #[test]\n    fn decode_snapshot_rejects_invalid_pairs")
            .unwrap()
            .0;
        let count = diagnostic
            .find("information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'CHECK'")
            .expect("missing read-only CHECK count");
        let five = diagnostic.find("check_count == 5").unwrap();
        let shape = diagnostic.find("validate_legacy_shape(&db, 1)").unwrap();
        assert!(count < five && five < shape);
        assert!(diagnostic.contains("diagnostic_check_count_not_five"));
        assert!(diagnostic.contains("diagnostic_check_count_read_failed"));
    }

    #[tokio::test]
    #[ignore = "manual SELECT-only diagnostic; parent must verify isolated preserved MySQL target"]
    async fn mysql_preserved_v1_legacy_select_gates_diagnostic() {
        let db_url = std::env::var("RSETUP_TEST_DATABASE_URL")
            .unwrap_or_else(|_| panic!("diagnostic_env_url_missing"));
        let expected = std::env::var("RSETUP_EXPECTED_DATABASE")
            .unwrap_or_else(|_| panic!("diagnostic_env_database_missing"));
        assert!(!db_url.is_empty(), "diagnostic_env_url_empty");
        assert!(!expected.is_empty(), "diagnostic_env_database_empty");
        let hook_expected = expected.clone();
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .after_connect(move |connection, _meta| {
                let expected = hook_expected.clone();
                Box::pin(async move {
                    let diagnostic_connection_rejected =
                        || sqlx::Error::Protocol("diagnostic_connection_rejected".into());
                    let actual: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|_| diagnostic_connection_rejected())?;
                    if actual.as_deref() != Some(expected.as_str()) {
                        return Err(diagnostic_connection_rejected());
                    }
                    let version: Option<String> = sqlx::query_scalar("SELECT VERSION()")
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(|_| diagnostic_connection_rejected())?;
                    if version.as_deref() != Some("8.0.46") {
                        return Err(sqlx::Error::Protocol("diagnostic_wrong_engine".into()));
                    }
                    let rows = sqlx::query("SHOW GRANTS")
                        .fetch_all(&mut *connection)
                        .await
                        .map_err(|_| diagnostic_connection_rejected())?;
                    let grants = rows
                        .iter()
                        .map(|row| {
                            row.try_get::<Option<String>, _>(0)
                                .map_err(|_| diagnostic_connection_rejected())
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    validate_schema_metadata_grants(&grants, &expected)
                        .map_err(|_| diagnostic_connection_rejected())?;
                    Ok(())
                })
            })
            .connect(&db_url)
            .await
            .unwrap_or_else(|_| panic!("diagnostic_connect_failed"));
        let mut connection = pool
            .acquire()
            .await
            .unwrap_or_else(|_| panic!("diagnostic_identity_connection_failed"));
        let actual: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
            .fetch_one(&mut *connection)
            .await
            .unwrap_or_else(|_| panic!("diagnostic_identity_read_failed"));
        assert!(
            actual.as_deref() == Some(expected.as_str()),
            "diagnostic_identity_mismatch"
        );
        assert!(
            require_schema_metadata_privilege(&mut connection, &expected)
                .await
                .is_ok(),
            "diagnostic_grants_failed"
        );
        drop(connection);

        let db = DbPool(pool);
        let version = read_version(&db)
            .await
            .unwrap_or_else(|_| panic!("diagnostic_version_read_failed"));
        assert!(version.is_none(), "diagnostic_version_not_none");
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
        )
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("diagnostic_table_count_read_failed"));
        assert!(
            table_count == 11 && TABLES.len() == 11,
            "diagnostic_table_count_not_eleven"
        );
        let schema_meta_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_meta")
            .fetch_one(&db.0)
            .await
            .unwrap_or_else(|_| panic!("diagnostic_schema_meta_read_failed"));
        assert!(schema_meta_rows == 0, "diagnostic_schema_meta_not_empty");
        let check_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'CHECK'",
        )
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("diagnostic_check_count_read_failed"));
        assert!(check_count == 5, "diagnostic_check_count_not_five");

        if let Err(error) = validate_legacy_shape(&db, 1).await {
            let (code, table_index) = legacy_diagnostic_shape_code(&error);
            if code == "check_unavailable_or_incompatible" {
                // One checked physical session for both variables and CHECK metadata.
                // A failed read aborts this sample; never reborrow a replacement session.
                let mut connection =
                    db.0.acquire()
                        .await
                        .unwrap_or_else(|_| panic!("diagnostic_check_connection_failed"));
                let mode: Option<String> = sqlx::query_scalar("SELECT @@SESSION.sql_mode")
                    .fetch_one(&mut *connection)
                    .await
                    .unwrap_or_else(|_| panic!("diagnostic_session_mode_read_failed"));
                let charset: Option<String> =
                    sqlx::query_scalar("SELECT @@SESSION.character_set_connection")
                        .fetch_one(&mut *connection)
                        .await
                        .unwrap_or_else(|_| panic!("diagnostic_session_charset_read_failed"));
                let rows = sqlx::query("SELECT CAST(tc.table_name AS CHAR) AS table_name, CAST(tc.constraint_name AS CHAR) AS constraint_name, CAST(cc.check_clause AS CHAR) AS check_clause, CAST(tc.enforced AS CHAR) AS enforced FROM information_schema.table_constraints tc LEFT JOIN information_schema.check_constraints cc ON cc.constraint_schema=tc.constraint_schema AND cc.constraint_name=tc.constraint_name WHERE tc.table_schema=DATABASE() AND tc.constraint_type='CHECK'")
                    .fetch_all(&mut *connection)
                    .await;
                drop(connection);
                match rows {
                    Ok(rows) => {
                        let (backslash_mode_safe, charset_utf8mb4) =
                            diagnostic_session_mode_flags(mode.as_deref(), charset.as_deref());
                        let fixed_flag = |flag| match flag {
                            Some(true) => "true",
                            Some(false) => "false",
                            None => "unknown",
                        };
                        println!(
                            "diagnostic_not_no_backslash_escapes={} diagnostic_charset_utf8mb4={}",
                            fixed_flag(backslash_mode_safe),
                            fixed_flag(charset_utf8mb4)
                        );
                        let decoded: Vec<DiagnosticEnforcedCheckRow> = rows
                            .iter()
                            .map(|row| {
                                Ok((
                                    row.try_get::<Option<String>, _>("table_name")
                                        .map_err(|_| ())?,
                                    row.try_get::<Option<String>, _>("constraint_name")
                                        .map_err(|_| ())?,
                                    row.try_get::<Option<String>, _>("check_clause")
                                        .map_err(|_| ())?,
                                    row.try_get::<Option<String>, _>("enforced").unwrap_or(None),
                                ))
                            })
                            .collect();
                        println!("diagnostic_check_rows={}", decoded.len());
                        let classifications: Vec<DiagnosticCheckRow> = decoded
                            .iter()
                            .map(|row| match row {
                                Ok((table, name, clause, _)) => {
                                    Ok((table.clone(), name.clone(), clause.clone()))
                                }
                                Err(_) => Err(()),
                            })
                            .collect();
                        for ((index, classification), row) in
                            legacy_diagnostic_check_classifications(&classifications)
                                .into_iter()
                                .zip(decoded.iter())
                        {
                            println!("check_index={index:?} classification={classification}");
                            if let (
                                Some(index @ (3 | 4)),
                                "literal_escape_or_other_mismatch",
                                Ok((_, _, Some(stored), enforced)),
                            ) = (index, classification, row)
                            {
                                if let Some(probe) = fixed_grant_check_escape_probe(index, stored) {
                                    println!(
                                        "check_index={index} candidate_equal={} backslash_quote_pair_count={} total_backslash_count={} restored_tokens_equal={}",
                                        probe.candidate_equal,
                                        probe.backslash_quote_pair_count,
                                        probe.total_backslash_count,
                                        probe.restored_tokens_equal,
                                    );
                                    if let Some(diff) = fixed_grant_check_token_diff(index, stored)
                                    {
                                        println!(
                                            "check_index={index} observed_tokens_parseable={} expected_tokens_parseable={} observed_token_count={:?} expected_token_count={:?} first_mismatch_token_index={:?} first_mismatch_token_class={:?} expected_class={:?}",
                                            diff.observed_tokens_parseable,
                                            diff.expected_tokens_parseable,
                                            diff.observed_token_count,
                                            diff.expected_token_count,
                                            diff.first_mismatch_token_index,
                                            diff.first_mismatch_token_class
                                                .map(|class| class.code()),
                                            diff.expected_class.map(|class| class.code()),
                                        );
                                    }
                                    if let Some((exact_bytes, tokens_equal)) =
                                        fixed_wrapped_grant_probe(index, stored)
                                    {
                                        println!(
                                            "check_index={index} wrapped_candidate_exact_bytes={} wrapped_candidate_tokens_equal={}",
                                            exact_bytes, tokens_equal,
                                        );
                                    }
                                    if let Some(observation) =
                                        grant_format_observation(index, stored, enforced.as_deref())
                                    {
                                        println!(
                                            "check_index={index} enforced={} stored_len={} candidate_len={} leading_parens={} trailing_parens={} ascii_whitespace={} backticks={} wrapped_layers_0_to_4={:?}",
                                            observation.enforced,
                                            observation.stored_len,
                                            observation.candidate_len,
                                            observation.leading_parens,
                                            observation.trailing_parens,
                                            observation.ascii_whitespace,
                                            observation.backticks,
                                            observation.wrapped_matches,
                                        );
                                    } else {
                                        println!(
                                            "check_index={index} enforced={} grant_format_observation=unknown",
                                            diagnostic_enforced_code(enforced.as_deref())
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => println!("diagnostic_check_metadata_read_failed"),
                }
            }
            panic!("diagnostic_shape_{code} table_index={table_index:?}");
        }
        assert!(
            require_no_identity_foreign_keys(&db).await.is_ok(),
            "diagnostic_fk_unavailable_or_nonempty"
        );
        for (index, &(table, column, _, _)) in IDENTITY_COLUMNS.iter().enumerate() {
            let actual = read_column(&db, table, column).await.unwrap_or_else(|_| {
                panic!("diagnostic_old_column_read_failed identity_index={index}")
            });
            let old = classify_identity_column(table, column, &actual).unwrap_or_else(|_| {
                panic!("diagnostic_old_column_shape_failed identity_index={index}")
            });
            assert!(old, "diagnostic_old_column_target identity_index={index}");
        }
        println!("legacy_select_gates_ok");
    }

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
        check_sample: Option<(String, Option<String>, String)>,
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
        fixture_tables: Option<Vec<String>>,
    }
    impl LegacyUpgradeFake {
        fn new(version: i32) -> Self {
            Self(std::sync::Mutex::new(LegacyUpgradeFakeState {
                version,
                columns: [version == 1; 11],
                checks: LEGACY_CHECK_ORDER.to_vec(),
                check_sample: None,
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
                fixture_tables: None,
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
            let state = self.0.lock().unwrap();
            if let Some((version, enforced, clause)) = &state.check_sample {
                validate_legacy_check_metadata_with_version(
                    &[(
                        Some("grants".into()),
                        Some("chk_grants_source".into()),
                        Some(clause.clone()),
                        enforced.clone(),
                    )],
                    Ok(Some(version)),
                )?;
            }
            Ok(state.checks.clone())
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
            let state = self.0.lock().unwrap();
            Ok(state
                .fixture_tables
                .clone()
                .unwrap_or_else(|| vec!["unversioned".to_owned()]))
        }
        async fn create_table(&self, _: &str) -> Result<(), ControllerError> {
            panic!("legacy route must not create fresh tables")
        }
        async fn validate_empty_data(&self) -> Result<(), ControllerError> {
            if self.0.lock().unwrap().fixture_tables.is_some() {
                self.gate("fixture_empty_data")
            } else {
                panic!("legacy route must not use fresh row scan")
            }
        }
        async fn insert_v3_meta(&self) -> Result<(), ControllerError> {
            panic!("legacy route must not insert fresh metadata")
        }
    }

    impl FixtureV2Probe for LegacyUpgradeFake {
        async fn create_v1_table(&self, statement: &'static str) -> Result<(), ControllerError> {
            let (table, _) = v1_create_statements()?
                .into_iter()
                .find(|&(_, sql)| sql == statement)
                .unwrap();
            let mut state = self.0.lock().unwrap();
            state.events.push(format!("CREATE {table}"));
            state.fixture_tables.as_mut().unwrap().push(table.into());
            Ok(())
        }
        async fn validate_created_v1(&self) -> Result<(), ControllerError> {
            self.gate("fixture_v1_shape")?;
            self.gate("fixture_v1_fk")?;
            self.gate("fixture_v1_columns")
        }
        async fn insert_v1_meta(&self) -> Result<(), ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push("fixture_insert".into());
            state.version = 1;
            state.read_version_reply = None;
            Ok(())
        }
        async fn cas_fixture_v2(&self) -> Result<u64, ControllerError> {
            let mut state = self.0.lock().unwrap();
            state.events.push("fixture_cas".into());
            assert_eq!(state.version, 1);
            if state.cas_error {
                state.version = 2;
                return Err(legacy_preflight_error("fixture CAS uncertain"));
            }
            if state.cas_rows == 1 && state.cas_updates_version {
                state.version = 2;
            }
            Ok(state.cas_rows)
        }
    }

    fn empty_v2_fixture_fake() -> LegacyUpgradeFake {
        let fake = LegacyUpgradeFake::new(1);
        let mut state = fake.0.lock().unwrap();
        state.read_version_reply = Some(None);
        state.fixture_tables = Some(Vec::new());
        drop(state);
        fake
    }

    #[tokio::test]
    async fn fixture_v2_fake_builds_old_schema_then_eleven_alters_and_cas_to_two() {
        for count in [0, 5] {
            let fake = empty_v2_fixture_fake();
            if count == 0 {
                fake.0.lock().unwrap().checks.clear();
            }
            fixture_v2_with_probe(&fake).await.unwrap();
            let events = fake.events();
            assert_eq!(
                events.iter().filter(|e| e.starts_with("CREATE ")).count(),
                11
            );
            assert_eq!(
                events.iter().filter(|e| e.starts_with("ALTER ")).count(),
                11
            );
            assert!(
                !events
                    .iter()
                    .any(|e| e.starts_with("DROP ") || e == "ready")
            );
            assert!(
                events.iter().position(|e| e == "fixture_v1_fk").unwrap()
                    < events.iter().position(|e| e == "fixture_insert").unwrap()
            );
            assert!(
                events.iter().position(|e| e == "data").unwrap()
                    < events.iter().position(|e| e.starts_with("ALTER ")).unwrap()
            );
            assert_eq!(events.last().unwrap(), "collision");
            let state = fake.0.lock().unwrap();
            assert_eq!(state.version, 2);
            assert_eq!(state.checks.len(), count);
            assert_eq!(state.columns, [false; 11]);
        }
    }

    #[tokio::test]
    async fn fixture_v2_fake_refuses_nonempty_or_versioned_schema_without_ddl() {
        for version in [Some(1), Some(2), Some(3), None] {
            let fake = empty_v2_fixture_fake();
            {
                let mut state = fake.0.lock().unwrap();
                state.read_version_reply = Some(version);
                if version.is_none() {
                    state.fixture_tables = Some(vec!["users".into()]);
                }
            }
            assert!(fixture_v2_with_probe(&fake).await.is_err());
            assert!(!fake.events().iter().any(|e| e.starts_with("CREATE ")
                || e.starts_with("ALTER ")
                || e == "fixture_cas"));
        }
    }

    #[tokio::test]
    async fn fixture_v2_fake_refuses_bad_preflight_before_first_alter_and_uncertain_cas() {
        let fake = empty_v2_fixture_fake();
        fake.0.lock().unwrap().fault_gate = Some("data");
        assert!(fixture_v2_with_probe(&fake).await.is_err());
        assert!(
            !fake
                .events()
                .iter()
                .any(|e| e.starts_with("ALTER ") || e == "fixture_cas")
        );
        for (rows, advances) in [(0, true), (1, false)] {
            let fake = empty_v2_fixture_fake();
            {
                let mut state = fake.0.lock().unwrap();
                state.cas_rows = rows;
                state.cas_updates_version = advances;
            }
            assert!(fixture_v2_with_probe(&fake).await.is_err());
            assert!(fake.events().contains(&"fixture_cas".into()));
        }
    }

    #[test]
    fn fixture_v2_dispatch_requires_separate_historical_route() {
        let source = include_str!("db.rs");
        let dispatch = source
            .split_once("async fn upgrade_identity_schema(")
            .unwrap()
            .1
            .split_once("async fn create_historical_v1_fixture(")
            .unwrap()
            .0;
        assert!(dispatch.contains("TestMigrationMode::FixtureV2"));
        assert!(dispatch.contains("create_historical_v2_fixture(db).await"));
        assert!(!dispatch.contains("check_identity_schema(db).await"));
    }

    #[test]
    fn historical_fixtures_require_proven_check_subset_not_exact_five() {
        let source = include_str!("db.rs");
        let fixture = source
            .split_once("async fn create_historical_v1_fixture(")
            .unwrap()
            .1
            .split_once("\npub trait AdmissionStore")
            .unwrap()
            .0;
        assert!(fixture.contains("validate_legacy_shape(db, 1).await?"));
        assert!(!fixture.contains("validate_schema_shape(db, Some(false)).await?"));
        assert!(!fixture.contains("validate_schema_shape(db, None).await?"));
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
                        | "enforced"
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
        assert_eq!(queries, 15);
        assert_eq!(string_columns, 23);
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
    fn mysql_8046_grant_readbacks_require_fixed_original_and_enforcement() {
        for (index, name) in [(3, "chk_grants_source"), (4, "chk_grants_scope")] {
            let expected = ddl_parts_from(MIGRATION, "grants")
                .into_iter()
                .find(|part| part.starts_with(&format!("CONSTRAINT {name} CHECK ")))
                .unwrap();
            assert!(expected.contains("CHECK (("));
            let stored = fixed_wrapped_grant_candidate(index)
                .unwrap()
                .replace('\'', "\\'");
            assert!(mysql_8046_grant_clause(
                "grants",
                name,
                &stored,
                expected.split_once("CHECK ").unwrap().1
            ));
            assert!(!mysql_8046_grant_clause(
                "grants",
                name,
                &stored,
                &expected
                    .split_once("CHECK ")
                    .unwrap()
                    .1
                    .replacen("AND", "OR", 1)
            ));
            assert!(
                validate_legacy_check_metadata(&[(
                    Some("grants".into()),
                    Some(name.into()),
                    Some(stored.clone())
                )])
                .is_err(),
                "old permission must stay closed for {name}"
            );
            assert_eq!(
                validate_legacy_check_metadata_with_version(
                    &[(
                        Some("grants".into()),
                        Some(name.into()),
                        Some(stored),
                        Some("YES".into())
                    )],
                    Ok(Some("8.0.46"))
                )
                .unwrap(),
                vec![("grants", name)]
            );
        }
    }

    #[test]
    fn mysql_8046_grants_compatibility_has_no_token_whitespace_or_metadata_fallback() {
        let accepts = |clause: String,
                       version: Result<Option<&str>, ()>,
                       enforced: Option<&str>,
                       table: &str,
                       name: &str| {
            validate_legacy_check_metadata_with_version(
                &[(
                    Some(table.into()),
                    Some(name.into()),
                    Some(clause),
                    enforced.map(str::to_owned),
                )],
                version,
            )
            .is_ok()
        };
        for (index, name) in [(3, "chk_grants_source"), (4, "chk_grants_scope")] {
            let stored = fixed_wrapped_grant_candidate(index)
                .unwrap()
                .replace('\'', "\\'");
            assert!(accepts(
                stored
                    .replace(" AND ", " aNd ")
                    .replace(" OR ", " Or ")
                    .replace(" IS ", " is ")
                    .replace(" NOT ", " nOt ")
                    .replace(" NULL", " nUlL"),
                Ok(Some("8.0.46")),
                Some("YES"),
                "grants",
                name
            ));
            for version in [
                Ok(None),
                Ok(Some("8.0.45")),
                Ok(Some("8.0.47")),
                Ok(Some("8.0.11-TiDB-v8.0.46")),
                Err(()),
            ] {
                assert!(!accepts(
                    stored.clone(),
                    version,
                    Some("YES"),
                    "grants",
                    name
                ));
            }
            for enforced in [None, Some("NO"), Some("yes"), Some("UNKNOWN"), Some("")] {
                assert!(!accepts(
                    stored.clone(),
                    Ok(Some("8.0.46")),
                    enforced,
                    "grants",
                    name
                ));
            }
            assert!(!accepts(
                stored.clone(),
                Ok(Some("8.0.46")),
                Some("YES"),
                "devices",
                name
            ));
            assert!(!accepts(
                stored.clone(),
                Ok(Some("8.0.46")),
                Some("YES"),
                "grants",
                "chk_devices_state"
            ));
            let literals = if index == 3 {
                &["role", "direct"][..]
            } else {
                &["all", "group", "device"][..]
            };
            for literal in literals {
                let needle = format!("\\'{literal}\\'");
                for (offset, _) in stored.match_indices(&needle) {
                    let mut changed = stored.clone();
                    changed.replace_range(
                        offset + 2..offset + 2 + literal.len(),
                        &literal.to_ascii_uppercase(),
                    );
                    assert!(!accepts(
                        changed,
                        Ok(Some("8.0.46")),
                        Some("YES"),
                        "grants",
                        name
                    ));
                }
            }
            let columns = if index == 3 {
                &["source_kind", "role_id", "permissions"][..]
            } else {
                &["scope_kind", "scope_group_id", "scope_device_id"][..]
            };
            for column in columns {
                let needle = format!("`{column}`");
                for (offset, _) in stored.match_indices(&needle) {
                    let mut changed = stored.clone();
                    changed.replace_range(
                        offset + 1..offset + 1 + column.len(),
                        &column.to_ascii_uppercase(),
                    );
                    assert!(!accepts(
                        changed,
                        Ok(Some("8.0.46")),
                        Some("YES"),
                        "grants",
                        name
                    ));
                }
            }
            for (needle, replacement) in [
                (" AND ", " OR "),
                (" OR ", " AND "),
                (" IS NOT NULL", " IS NULL"),
                (" IS NULL", " IS NOT NULL"),
                ("_utf8mb4", "_latin1"),
                ("_utf8mb4", ""),
                ("\\'", "'"),
                ("\\'", "\\\\'"),
                ("`", ""),
                (" = ", " != "),
            ] {
                if stored.contains(needle) {
                    assert!(
                        !accepts(
                            stored.replacen(needle, replacement, 1),
                            Ok(Some("8.0.46")),
                            Some("YES"),
                            "grants",
                            name
                        ),
                        "{name}: {needle}"
                    );
                }
            }
            for changed in [
                format!(" {stored}"),
                format!("{stored} "),
                stored.replacen(" AND ", "  AND ", 1),
                stored.replacen(" AND ", " AND  ", 1),
                format!("({stored})"),
                stored.replacen("(((", "((((", 1),
                format!("{stored} OR 1=1"),
                stored.replacen("`", "`X", 1),
                stored.replacen("\\'", "\\'x", 1),
            ] {
                assert!(!accepts(
                    changed,
                    Ok(Some("8.0.46")),
                    Some("YES"),
                    "grants",
                    name
                ));
            }
            let row = (
                Some("grants".into()),
                Some(name.into()),
                Some(stored),
                Some("YES".into()),
            );
            for invalid in [
                (None, row.1.clone(), row.2.clone(), row.3.clone()),
                (row.0.clone(), None, row.2.clone(), row.3.clone()),
                (row.0.clone(), row.1.clone(), None, row.3.clone()),
                (row.0.clone(), row.1.clone(), row.2.clone(), None),
            ] {
                assert!(
                    validate_legacy_check_metadata_with_version(&[invalid], Ok(Some("8.0.46")))
                        .is_err()
                );
            }
            assert!(
                validate_legacy_check_metadata_with_version(
                    &[row.clone(), row],
                    Ok(Some("8.0.46"))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn grant_metadata_reader_keeps_version_enforcement_and_clauses_on_one_connection() {
        let source = include_str!("db.rs")
            .split_once("\nasync fn read_column(")
            .unwrap()
            .0;
        assert!(
            source.contains("async fn read_legacy_checks_for_table("),
            "expected a held-connection legacy CHECK reader"
        );
        let reader = source
            .split_once("async fn read_legacy_checks_for_table(")
            .unwrap()
            .1;
        assert_eq!(reader.matches("db.0.acquire()").count(), 1);
        assert!(reader.contains("CAST(VERSION() AS CHAR)"));
        assert!(reader.contains("CAST(tc.enforced AS CHAR) AS enforced"));
        assert!(reader.contains(".bind(table).fetch_all(&mut *connection)"));
        assert!(reader.contains(".fetch_one(&mut *connection)"));
        assert!(reader.contains("validate_legacy_check_metadata_with_version"));
        assert!(reader.contains("if version == \"8.0.46\""));
    }

    #[tokio::test]
    async fn injected_grant_metadata_proof_blocks_upgrade_before_first_ddl() {
        for (version, enforced, tamper) in [
            ("8.0.46", Some("YES"), true),
            ("8.0.45", Some("YES"), false),
            ("8.0.46", Some("NO"), false),
        ] {
            let fake = LegacyUpgradeFake::new(1);
            let stored = fixed_wrapped_grant_candidate(3)
                .unwrap()
                .replace('\'', "\\'");
            fake.0.lock().unwrap().check_sample = Some((
                version.into(),
                enforced.map(str::to_owned),
                if tamper {
                    stored.replacen("`role_id`", "`rogue_id`", 1)
                } else {
                    stored
                },
            ));
            assert!(upgrade_legacy_with_probe(&fake, 1).await.is_err());
            assert!(
                fake.writes().is_empty(),
                "invalid CHECK must block before DDL"
            );
        }
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
