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
        &["token_hash", "user_id", "process_epoch", "revoked"],
        &["PRIMARY", "ix_sessions_user_id"],
    ),
    (
        "roles",
        &["id", "name", "builtin", "archived"],
        &["PRIMARY", "uq_roles_name"],
    ),
    ("role_permissions", &["role_id", "permission"], &["PRIMARY"]),
    (
        "device_groups",
        &["id", "name", "archived"],
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
        ],
        &["PRIMARY", "ix_grants_user_id"],
    ),
    (
        "admission_decisions",
        &[
            "id",
            "device_id",
            "decision",
            "previous_revision",
            "new_revision",
        ],
        &["PRIMARY", "ix_admission_decisions_device_id"],
    ),
    (
        "audit_events",
        &[
            "id",
            "actor_kind",
            "actor_user_id",
            "target_kind",
            "target_id",
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
        for &column in columns {
            let present: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
                .bind(table).bind(column).fetch_one(&db.0).await?;
            if present != 1 {
                return Err(ControllerError::Config(format!(
                    "migration missing column {table}.{column}"
                )));
            }
        }
        for &index in indexes {
            let present: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = ? AND index_name = ?")
                .bind(table).bind(index).fetch_one(&db.0).await?;
            if present == 0 {
                return Err(ControllerError::Config(format!(
                    "migration missing index {table}.{index}"
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
        let evidence = serde_json::json!({"wall_clock": chrono::Utc::now().to_rfc3339(), "quality": "system_fallback"}).to_string();
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

fn audit_code(decision: ReviewDecision, previous: AdmissionState) -> &'static str {
    match (decision, previous) {
        (ReviewDecision::Approved, AdmissionState::Revoked) => "admission.reauthorize",
        (ReviewDecision::Approved, _) => "admission.approve",
        (ReviewDecision::Denied, _) => "admission.reject",
        (ReviewDecision::None, _) => "admission.reopen",
        (ReviewDecision::Revoked, _) => "admission.revoke",
    }
}
