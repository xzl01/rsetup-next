//! Production `LiveAuth` adapter over the object-safe `AuthServiceApi`
//! boundary (Task 3d-a; Task 3f adds `is_live_session`).
//!
//! Compile-only deliverable: nothing wires `LiveAuth` into
//! `build_http_router` or `main`; the four auth handlers and the login
//! concurrency gates are NOT all ready, so `main` stays health-only.
//! `login` / `authenticate` / `change_password` / `logout` are plain boxed
//! forwards to the identical inherent `AuthService` methods — no fabricated
//! success, no process-epoch accessor, no internal state exposed.
//! `is_live_session` is a direct forward to the service clock peek:
//! pure in-memory, no DB, no idle renewal; a hit is only a necessary
//! condition, `authenticate` stays DB-authoritative.
//!
//! `authz_epoch` is the per-request authorization-epoch read: a fresh,
//! fixed, bound-only `SELECT ... LIMIT 2` of `schema_meta` on EVERY call,
//! strictly decoded as `(singleton: i8, authz_epoch: u64)`, then
//! cardinality-guarded to exactly one `singleton == 1` row — never the
//! process `SessionClock.epoch`. Every failure path (missing/extra row,
//! wrong singleton, decode error, query/driver error) is a fixed, redacted
//! `ControllerError::Config`; no raw SQLx error, database URL, username,
//! digest or row content ever crosses this boundary. The existing `me`
//! `backend_error` maps that `Config` to a fixed 503 `NOT_READY`.
//!
//! The SQLx execution itself is compile-only (no socket in the tests);
//! real-database behavior is a separate gated acceptance.

use std::sync::Arc;

use sqlx::Row;

use crate::auth::service::{AuthService, Login, Session, SessionPage};
use crate::auth::sqlx_repo::SqlxIdentityRepository;
use crate::db::DbPool;
use crate::error::ControllerError;
use crate::http_auth::{AuthFuture, AuthServiceApi};

/// Fixed, bound-only, read-only `schema_meta` singleton read.
/// `LIMIT 2` bounds the fetch: a polluted duplicate row is rejected by the
/// cardinality guard, never silently picked. No bind, no write or lock
/// keyword.
const EPOCH_SQL: &str = "SELECT singleton, authz_epoch FROM schema_meta LIMIT 2";

/// Fixed, redacted, non-secret configuration error for the `authz_epoch`
/// boundary. The code is a stable identifier and never carries row content
/// or the underlying driver message.
fn epoch_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("live authz epoch {code}"))
}

/// Pure cardinality + singleton guard for the `LIMIT 2`-bounded
/// `schema_meta` read: exactly one row with `singleton == 1` yields its
/// `authz_epoch` (u64 high half preserved); missing, duplicate or
/// wrong-singleton rows are a fixed redacted `Config`.
fn require_authz_singleton(rows: Vec<(i8, u64)>) -> Result<u64, ControllerError> {
    match rows.as_slice() {
        [(1, epoch)] => Ok(*epoch),
        [] => Err(epoch_error("missing")),
        [(_other, _)] => Err(epoch_error("singleton")),
        _ => Err(epoch_error("duplicate")),
    }
}

/// Production adapter: the shared auth service plus the pool for the
/// per-request `schema_meta` read. Constructed once; every method is
/// object-safe (`Arc<dyn AuthServiceApi>`).
pub struct LiveAuth {
    service: Arc<AuthService<SqlxIdentityRepository>>,
    db: DbPool,
}

impl LiveAuth {
    pub fn new(service: Arc<AuthService<SqlxIdentityRepository>>, db: DbPool) -> Self {
        Self { service, db }
    }
}

impl AuthServiceApi for LiveAuth {
    fn login<'a>(&'a self, username: &'a str, password: &'a str) -> AuthFuture<'a, Login> {
        // Plain boxed forward to the identical inherent method: no
        // parameter rewrite, no retry, no error remap.
        Box::pin(self.service.login(username, password))
    }

    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
        Box::pin(self.service.authenticate(raw))
    }

    fn change_password<'a>(
        &'a self,
        session: &'a Session,
        current: &'a str,
        new_password: &'a str,
    ) -> AuthFuture<'a, ()> {
        Box::pin(self.service.change_password(session, current, new_password))
    }

    fn logout<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, ()> {
        Box::pin(self.service.logout(session))
    }

    fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        // Direct forward to the service clock peek: pure in-memory, no DB,
        // no idle renewal. A hit is only a necessary condition; the
        // DB-authoritative `authenticate` is unchanged.
        self.service.is_live_session(digest)
    }

    fn list_sessions<'a>(
        &'a self,
        session: &'a Session,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> AuthFuture<'a, SessionPage> {
        Box::pin(self.service.list_sessions(session, cursor, limit))
    }

    fn revoke_by_alias<'a>(&'a self, session: &'a Session, id: &'a str) -> AuthFuture<'a, bool> {
        Box::pin(self.service.revoke_by_alias(session, id))
    }

    fn revoke_others<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, u64> {
        Box::pin(self.service.revoke_others(session))
    }

    fn authz_epoch(&self) -> AuthFuture<'_, u64> {
        // A fresh read on EVERY request; never the process
        // `SessionClock.epoch`. Driver/decode failures are the fixed
        // redacted `Config`; never a raw SQLx error.
        Box::pin(async {
            let rows = sqlx::query(EPOCH_SQL)
                .fetch_all(&self.db.0)
                .await
                .map_err(|_| epoch_error("query"))?;
            let decoded = rows
                .into_iter()
                .map(|row| {
                    let singleton = row
                        .try_get::<i8, _>("singleton")
                        .map_err(|_| epoch_error("decode"))?;
                    let authz_epoch = row
                        .try_get::<u64, _>("authz_epoch")
                        .map_err(|_| epoch_error("decode"))?;
                    Ok((singleton, authz_epoch))
                })
                .collect::<Result<Vec<(i8, u64)>, ControllerError>>()?;
            require_authz_singleton(decoded)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed, redacted `Config` shape: the stable identifier prefix and no
    /// row content in the message.
    fn assert_fixed_redacted_config(err: &ControllerError) {
        match err {
            ControllerError::Config(msg) => {
                assert!(
                    msg.starts_with("live authz epoch "),
                    "not the fixed redacted shape: {msg:?}"
                );
            }
            other => panic!("expected the fixed redacted Config, got {other:?}"),
        }
    }

    #[test]
    fn authz_singleton_accepts_valid_single_row() {
        assert_eq!(require_authz_singleton(vec![(1, 7)]).ok(), Some(7));
    }

    #[test]
    fn authz_singleton_preserves_u64_high_half() {
        assert_eq!(
            require_authz_singleton(vec![(1, u64::MAX)]).ok(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn authz_singleton_rejects_missing_rows_fixed_redacted() {
        let err = require_authz_singleton(Vec::new()).expect_err("a missing row must not decode");
        assert_fixed_redacted_config(&err);
    }

    #[test]
    fn authz_singleton_rejects_duplicate_rows_fixed_redacted() {
        let err = require_authz_singleton(vec![(1, 7), (1, 8)])
            .expect_err("duplicate rows must not decode");
        assert_fixed_redacted_config(&err);
    }

    #[test]
    fn authz_singleton_rejects_wrong_singleton_fixed_redacted() {
        for singleton in [0i8, 2, -1, i8::MAX] {
            let err = require_authz_singleton(vec![(singleton, 7)])
                .expect_err("a wrong singleton must not decode");
            assert_fixed_redacted_config(&err);
        }
    }

    #[test]
    fn epoch_sql_is_fixed_boundless_read_only_singleton_select() {
        let normalized = EPOCH_SQL.trim().to_ascii_uppercase();
        assert!(
            normalized.starts_with("SELECT"),
            "EPOCH_SQL must be a single fixed SELECT: {EPOCH_SQL:?}"
        );
        assert!(
            !EPOCH_SQL.contains(';'),
            "no statement separator: {EPOCH_SQL:?}"
        );
        assert_eq!(
            EPOCH_SQL.matches('?').count(),
            0,
            "zero binds: {EPOCH_SQL:?}"
        );
        let compact = normalized.replace(char::is_whitespace, "");
        for needle in ["SINGLETON", "AUTHZ_EPOCH", "LIMIT2"] {
            assert!(
                compact.contains(needle),
                "missing {needle} in {EPOCH_SQL:?}"
            );
        }
        let forbidden = [
            "SET", "USE", "UPDATE", "INSERT", "DELETE", "DROP", "ALTER", "TRUNCATE", "GRANT",
            "BEGIN", "COMMIT", "ROLLBACK", "FOR", "LOCK",
        ];
        for token in EPOCH_SQL.split_whitespace() {
            let bare = token
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_uppercase();
            assert!(
                !forbidden.contains(&bare.as_str()),
                "forbidden keyword token in EPOCH_SQL: {bare:?}"
            );
        }
    }

    #[tokio::test]
    async fn live_auth_is_object_safe_and_constructible_without_a_socket() {
        // Lazy synthetic pool (same shape as the http_auth tests): the
        // construction never connects, and no LiveAuth method is ever
        // awaited, so this test opens no socket and needs no real DB.
        let db = crate::db::DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .unwrap(),
        );
        let repo = Arc::new(SqlxIdentityRepository::new(db.clone()));
        let service = AuthService::new(repo).expect("the fixed dummy Argon2id hash must succeed");
        let live = LiveAuth::new(Arc::new(service), db);
        // Compile-only object-safety proof: the adapter must coerce to the
        // object-safe boundary the HTTP layer consumes.
        let api: Arc<dyn AuthServiceApi> = Arc::new(live);
        // The precheck is a pure peek (no socket): a freshly constructed
        // service has minted no session, so any digest must be NOT live —
        // which also proves the boundary is not an always-true probe.
        assert!(
            !api.is_live_session(&[0x7c; 32]),
            "a fresh process clock has no live entries; the probe must not be always-true"
        );
    }
}
