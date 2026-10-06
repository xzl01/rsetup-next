//! Production SQLx read boundary for the identity repository (Task 2a).
//!
//! `find_user` / `find_session` execute fixed, bound-only `SELECT` statements
//! (no string-built SQL) and decode rows strictly: every boolean column is
//! projected as `CAST(... AS SIGNED)` and only `0`/`1` is accepted; a
//! polluted value fails closed with a fixed, non-secret
//! `ControllerError::Config` instead of being filtered out or silently
//! dropped.
//!
//! One accepted, documented exception: a session row whose `user_id`
//! dangles (no matching `users` row) inner-joins to nothing and reads as
//! `Ok(None)` — the session is denied, so authorization stays fail closed.
//! Surfacing that corruption is deferred to the next startup identity
//! integrity check rather than reported here as `Config`.
//!
//! Statement logging is off by construction: this build of sqlx is compiled
//! without its `log`/`tracing` features, and every driver error is replaced
//! by a fixed redacted code, so no username, hash, digest or database URL
//! can reach a log or error surface.
//!
//! The two login writes (`insert_session`, `record_login_failure`) are real
//! transactions: `integrity::lock_integrity_guard` first, then — for
//! `insert_session` — a `FOR UPDATE` re-read of the `users` row strictly
//! revalidated against the verified snapshot, a session insert whose
//! `created_time` comes from the server clock (`UTC_TIMESTAMP(6)`), and a
//! fixed redacted audit row, committed together; every driver error maps to
//! a fixed redacted `Config` code and is never swallowed.
//! `change_password` follows the same transactional boundary for the
//! account write: under the guard it reads `schema_meta.authz_epoch`
//! (strict `u64`, checked increment), re-locks the `users` row and strictly
//! revalidates it against the verified snapshot, point-locks the current
//! session row by `token_hash` (strict `user_id`/`process_epoch`/
//! `revoked = 0`), updates the user row (hash + checked revision +
//! `must_change_password = FALSE`, `rows_affected == 1`), strictly scans
//! then revokes all of the user's sessions, writes the fixed redacted
//! `auth.password.changed` audit plus `auth.session.revoked` only when at
//! least one session row is actually revoked, and bumps
//! `schema_meta.authz_epoch` by exactly one via compare-and-swap in the
//! same transaction before commit; any failure — including an audit write
//! failure — drops the transaction without commit (rollback).
//! `revoke_session` is the third real transaction, for the single session
//! write: under the guard it point-locks the `users` row by the verified
//! session's user id (strict decode; the stored hash/revision are
//! deliberately NOT rechecked, so a concurrent password change cannot
//! prevent cleanup), then point-locks the session row by `token_hash`
//! (strict `user_id`/`process_epoch`/`revoked = 0|1`); a missing `users`
//! row or a wrong owner/epoch fails closed, a missing or already revoked
//! matching session is idempotent `Ok(())` with no audit (also for a
//! now-inactive user), a live session under an inactive user is an
//! integrity violation and fails closed with a fixed redacted `Config`
//! (it cannot be attributed to a valid user actor), and only a live
//! matching session for an active user is revoked via the targeted
//! `UPDATE` whose `rows_affected == 1` is required before the fixed
//! redacted `auth.logout` audit and commit — no
//! `schema_meta.authz_epoch` bump, because a session-only revocation
//! changes no account or grant state; any failure, including an audit
//! write failure, drops the transaction without commit (rollback).
//! The transactions are verified to the driver layer only (compile +
//! offline tests): no real-database atomicity, lock ordering or rollback
//! behavior is claimed here.
//!
//! `list_user_sessions` is the read-only list boundary: one fixed
//! bound-only `SELECT` (no transaction, no lock, no write) of the
//! authenticated actor's own session rows for the caller's process epoch
//! in `token_hash` byte order. `LIMIT 8193` fetches one row beyond the
//! 8192 hard cap, so 8193 fetched rows are a fixed `ResourceExhausted`
//! before any decode or projection — never a silently truncated page.
//! `revoked` carries no SQL predicate: only `0`/`1` decodes and pollution
//! is a fixed redacted `Config`, as are wrong `BINARY` widths
//! (`token_hash` 32 / `user_id` 16 / `process_epoch` 16) and any driver or
//! column error; the `DATETIME(6)` `created_time` is interpreted as UTC
//! with microsecond precision. Nothing here logs or returns a raw row,
//! token, digest or SQL error, and the method is verified to the driver
//! layer only (compile + offline tests): no real-database decode or
//! capacity behavior is claimed.
//!
//! The two session revocation write methods (`revoke_selected_session`,
//! `revoke_other_sessions`) are real transactions following the strict
//! lock order: `tx.begin` -> `integrity::lock_integrity_guard` ->
//! `users` row `FOR UPDATE` by `actor.user.id` via `LOCK_USER_SQL`
//! (strict decode; missing row -> fixed `Config users.missing`; `!active`
//! -> fixed `Config users.active`) -> `sessions`: selected locks both
//! digests in ascending `token_hash` byte order via `LOCK_SESSION_SQL`,
//! while bulk scan-locks in `token_hash` byte order via
//! `LOCK_USER_SESSIONS_SQL` (enforcing `enforce_list_capacity` before
//! decode). Driver errors map to fixed redacted codes; any failure rolls
//! back the transaction; commit-unknown is a fixed failure, never `Ok`.
//! The transactions are verified to the driver layer only (compile +
//! offline tests): real MySQL/TiDB physical lock order, atomicity,
//! rollback and physical decode stay open until an ignored test on a
//! confirmed isolated target with genuine backup approval.
//!
//! The production repository — the original six methods plus the
//! `list_user_sessions` read and `revoke_selected_session` /
//! `revoke_other_sessions` write overrides — can be wired only after
//! independent code review; nothing wires it into `main` or the HTTP
//! surface yet, and `main` remains health-only.

use crate::ControllerError;
use crate::auth::service::{IdentityRepository, IdentityUser, Session, StoredSession};
use crate::db::DbPool;
use sqlx::{Row, mysql::MySqlRow};
use std::sync::{OnceLock, atomic::AtomicU64};

/// Auth-only audit identity, fully independent from the `db.rs` CAS path's
/// function-local `PROCESS_EPOCH`/`EVENT_SEQ`: under
/// `uq_audit_epoch_seq (process_epoch, event_seq)` the two writers can never
/// collide because they always use different epochs.
static AUTH_PROCESS_EPOCH: OnceLock<uuid::Uuid> = OnceLock::new();
static AUTH_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Fixed, bound-only read of at most two users by exact username.
///
/// `LIMIT 2` bounds the fetch: a polluted duplicate username returns two
/// rows and the caller's cardinality guard rejects them with the fixed
/// redacted `users.duplicate` error — never a silent first-row pick, and
/// the server never streams more than two rows.
const FIND_USER_SQL: &str = "
    SELECT
        u.id,
        u.username,
        u.password_hash,
        CAST(u.active AS SIGNED) AS active,
        CAST(u.is_admin AS SIGNED) AS is_admin,
        CAST(u.must_change_password AS SIGNED) AS must_change_password,
        u.revision
    FROM users AS u
    WHERE u.username = ?
    LIMIT 2
";

/// Fixed, bound-only read of at most two session rows joined to their user
/// by exact token hash + process epoch.
///
/// `LIMIT 2` bounds the fetch: a polluted duplicate session returns two
/// rows and the caller's cardinality guard rejects them with the fixed
/// redacted `sessions.duplicate` error — never a silent first-row pick, and
/// the server never streams more than two rows.
///
/// No `revoked` predicate anywhere: pollution (e.g. `revoked = 2`) must be
/// decoded and rejected as corruption, not hidden from existence.
const FIND_SESSION_SQL: &str = "
    SELECT
        u.id,
        u.username,
        u.password_hash,
        CAST(u.active AS SIGNED) AS active,
        CAST(u.is_admin AS SIGNED) AS is_admin,
        CAST(u.must_change_password AS SIGNED) AS must_change_password,
        u.revision,
        CAST(s.revoked AS SIGNED) AS revoked
    FROM sessions AS s
    JOIN users AS u ON u.id = s.user_id
    WHERE s.token_hash = ?
        AND s.process_epoch = ?
    LIMIT 2
";

/// Fixed, bound-only read-only list of the actor's own session rows for
/// the caller's process epoch, in `token_hash` byte order.
///
/// `LIMIT 8193` is the hard cap (8192) plus one: fetching 8193 rows means
/// the user holds at least 8193 candidate rows under this epoch, so the
/// caller rejects the whole page with the fixed `ResourceExhausted` before
/// any decode or public projection — never a silently truncated list.
///
/// No `revoked` predicate anywhere: pollution (e.g. `revoked = 2`) must be
/// decoded strictly and rejected as corruption, not hidden by
/// `WHERE revoked = FALSE`. Two binds only (the verified actor's user id,
/// then the process epoch), no `;`, read-only (no `FOR UPDATE`, no
/// transaction, no write).
const LIST_USER_SESSIONS_SQL: &str = "
    SELECT
        token_hash,
        user_id,
        process_epoch,
        created_time,
        CAST(revoked AS SIGNED) AS revoked
    FROM sessions
    WHERE user_id = ?
        AND process_epoch = ?
    ORDER BY token_hash
    LIMIT 8193
";

/// Fixed in-transaction `users` row lock by exact id: the login write
/// re-reads the current row under `FOR UPDATE` and strictly revalidates it
/// against the verified snapshot before any session row is inserted.
/// No username binding, no `;`, a single bound id.
const LOCK_USER_SQL: &str = "SELECT id, username, password_hash,
    CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin,
    CAST(must_change_password AS SIGNED) AS must_change_password, revision
    FROM users WHERE id = ? FOR UPDATE";

/// Fixed session insert: `created_time` comes from the server clock
/// (`UTC_TIMESTAMP(6)`) and `revoked` is the literal `FALSE` — three binds
/// only (digest, user id, process epoch), no client-supplied timestamps.
const INSERT_SESSION_SQL: &str = "INSERT INTO sessions
    (token_hash, user_id, process_epoch, created_time, revoked)
    VALUES (?, ?, ?, UTC_TIMESTAMP(6), FALSE)";

/// Fixed audit insert; column order matches the CAS audit path in `db.rs`
/// so both writers share the same `audit_events` shape. Eleven binds: the
/// event `id` and `time_evidence` are generated per insert, never bound
/// from request data.
const AUDIT_INSERT_SQL: &str = "INSERT INTO audit_events
    (id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted,
     outcome, time_evidence, process_epoch, event_seq) VALUES (?,?,?,?,?,?,?,?,?,?,?)";

/// Fixed in-transaction point lock of the current session row by exact
/// token hash: `revoked` is projected `CAST(... AS SIGNED)` so a polluted
/// value is decoded and rejected as corruption, never filtered out. One
/// bound digest, no `;`.
const LOCK_SESSION_SQL: &str = "SELECT user_id, CAST(revoked AS SIGNED) AS revoked,
    process_epoch FROM sessions WHERE token_hash = ? FOR UPDATE";

/// Fixed `users` update for the account write: new hash + checked
/// increment of the revision + `must_change_password = FALSE`, guarded by
/// the snapshot's `(id, revision)` pair so any drift affects zero rows.
/// Four binds, no username binding, no `;`.
const UPDATE_USERS_SQL: &str = "UPDATE users SET password_hash = ?, revision = ?,
    must_change_password = FALSE WHERE id = ? AND revision = ?";

/// Fixed in-transaction scan of all of a user's sessions in `token_hash`
/// byte order (same-class lock ordering): `revoked` is decoded strictly
/// before any revoke, so pollution is detected before mutation. One bound
/// id, no `;`.
const SELECT_USER_SESSIONS_SQL: &str = "SELECT token_hash, CAST(revoked AS SIGNED) AS revoked
    FROM sessions WHERE user_id = ? ORDER BY token_hash FOR UPDATE";

/// Fixed in-transaction revoke of all of a user's sessions. Runs only
/// after the strict scan above, so a polluted row can never be silently
/// set `TRUE`. One bound id, no `;`.
const REVOKE_USER_SESSIONS_SQL: &str = "UPDATE sessions SET revoked = TRUE WHERE user_id = ?";

/// Fixed in-transaction targeted revoke of exactly one session row, run
/// only after the strict in-lock decision proved this row is live and
/// matches the authenticated owner and process epoch. The trailing
/// `revoked = FALSE` clause is redundant defence after that strict decode
/// (the caller holds the point lock, so the row cannot drift in between)
/// and the caller's `rows_affected == 1` check still pins the invariant.
/// Three binds, no `;` (`revoked = FALSE` is a literal, not a bind).
const REVOKE_SESSION_SQL: &str = "UPDATE sessions SET revoked = TRUE
    WHERE token_hash = ? AND user_id = ? AND process_epoch = ? AND revoked = FALSE";

/// Fixed read of `schema_meta.authz_epoch` under the already-held
/// integrity guard lock (no new lock is taken).
const READ_AUTHZ_EPOCH_SQL: &str = "SELECT authz_epoch FROM schema_meta WHERE singleton = 1";

/// Fixed compare-and-swap bump of `schema_meta.authz_epoch`: the second
/// bind is the value read under the guard, so the update affects exactly
/// one row or the whole transaction rolls back. No silent skip.
const BUMP_AUTHZ_EPOCH_SQL: &str =
    "UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?";

const LOCK_USER_SESSIONS_SQL: &str = "SELECT token_hash, process_epoch,
    CAST(revoked AS SIGNED) AS revoked
    FROM sessions WHERE user_id = ? AND process_epoch = ?
    ORDER BY token_hash LIMIT 8193 FOR UPDATE";
const REVOKE_OTHER_SESSIONS_SQL: &str = "UPDATE sessions SET revoked = TRUE
    WHERE user_id = ? AND process_epoch = ? AND token_hash <> ? AND revoked = FALSE";

/// Fixed, non-secret, redacted configuration error; the code is a stable
/// identifier and never carries row content.
fn read_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("identity auth read {code}"))
}

/// Fixed audit projection for the two auth login events. The value set is
/// pinned by the repository plan's event table; no username, password,
/// token or digest ever enters the projection, and `params_redacted` is
/// always the empty JSON object.
#[derive(Debug, PartialEq, Eq)]
struct AuditInsert {
    actor_kind: &'static str,
    actor_user_id: Option<[u8; 16]>,
    event_type: &'static str,
    target_kind: Option<&'static str>,
    target_id: Option<String>,
    params_redacted: &'static str,
    outcome: &'static str,
}

/// Fixed, non-secret, redacted configuration error for the write boundary;
/// the code is a stable identifier and never carries row content or the
/// underlying driver error.
fn write_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("identity auth write {code}"))
}

/// Pinned `auth.login.succeeded` projection: the verified user is the actor
/// and the target, identified only by id (hex-encoded for `target_id`).
fn login_succeeded_audit(user_id: [u8; 16]) -> AuditInsert {
    AuditInsert {
        actor_kind: "user",
        actor_user_id: Some(user_id),
        event_type: "auth.login.succeeded",
        target_kind: Some("user"),
        target_id: Some(hex::encode(user_id)),
        params_redacted: "{}",
        outcome: "success",
    }
}

/// Pinned `auth.login.failed` projection: isomorphic for both argument
/// shapes (`None` unknown username / `Some(id)` rejected known account) —
/// always an anonymous system actor, always a null target, no input field
/// or error text ever enters the projection.
fn login_failure_audit(_user_id: Option<[u8; 16]>) -> AuditInsert {
    AuditInsert {
        actor_kind: "system",
        actor_user_id: None,
        event_type: "auth.login.failed",
        target_kind: Some("user"),
        target_id: None,
        params_redacted: "{}",
        outcome: "failure",
    }
}

/// Pinned `auth.password.changed` projection: the verified user is the
/// actor and the target, identified only by id (hex-encoded for
/// `target_id`); no password or token ever enters the projection.
fn password_changed_audit(user_id: [u8; 16]) -> AuditInsert {
    AuditInsert {
        actor_kind: "user",
        actor_user_id: Some(user_id),
        event_type: "auth.password.changed",
        target_kind: Some("user"),
        target_id: Some(hex::encode(user_id)),
        params_redacted: "{}",
        outcome: "success",
    }
}

/// Pinned `auth.session.revoked` projection: isomorphic to
/// `auth.password.changed` for the account write (same actor and target,
/// written only when at least one session row is actually revoked).
fn session_revoked_audit(user_id: [u8; 16]) -> AuditInsert {
    AuditInsert {
        actor_kind: "user",
        actor_user_id: Some(user_id),
        event_type: "auth.session.revoked",
        target_kind: Some("user"),
        target_id: Some(hex::encode(user_id)),
        params_redacted: "{}",
        outcome: "success",
    }
}

/// Pinned `auth.logout` projection: the verified user is the actor and the
/// target, identified only by id (hex-encoded for `target_id`); written
/// only after the live matching session row is actually revoked
/// (`rows_affected == 1`).
fn logout_audit(user_id: [u8; 16]) -> AuditInsert {
    AuditInsert {
        actor_kind: "user",
        actor_user_id: Some(user_id),
        event_type: "auth.logout",
        target_kind: Some("user"),
        target_id: Some(hex::encode(user_id)),
        params_redacted: "{}",
        outcome: "success",
    }
}

/// Strict in-lock revalidation of the current `users` row against the
/// verified login snapshot:
/// - no current row, id mismatch, or an inactive current row is a
///   fixed `InvalidArgument` (active rejection precedes hash/revision);
/// - a `password_hash` or `revision` drift is a fixed
///   `RevisionConflict`;
/// - only an exactly matching current row passes.
fn validate_current_user(
    current: Option<&IdentityUser>,
    snapshot: &IdentityUser,
) -> Result<(), ControllerError> {
    let Some(current) = current else {
        return Err(ControllerError::InvalidArgument);
    };
    if current.id != snapshot.id || !current.active {
        return Err(ControllerError::InvalidArgument);
    }
    if current.password_hash != snapshot.password_hash || current.revision != snapshot.revision {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(())
}

/// Strict in-lock validation of the current session row for the account
/// write: a polluted `revoked` (any raw value outside 0/1) is the fixed
/// redacted `sessions.revoked` `Config`; a revoked row, a `user_id`
/// mismatch against the verified snapshot, or a `process_epoch` mismatch
/// against this process's epoch is a fixed `InvalidArgument`.
fn validate_change_password_session(
    owner: [u8; 16],
    expected_owner: [u8; 16],
    stored_epoch: [u8; 16],
    expected_epoch: [u8; 16],
    revoked: i64,
) -> Result<(), ControllerError> {
    strict_bool(revoked, "sessions.revoked")?;
    if revoked != 0 {
        return Err(ControllerError::InvalidArgument);
    }
    if owner != expected_owner {
        return Err(ControllerError::InvalidArgument);
    }
    if stored_epoch != expected_epoch {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

/// Strict in-lock logout decision for the point-locked session row.
///
/// Check order is pinned: `strict_bool` first (a polluted `revoked` is
/// corruption, never a user-input problem), then stored owner, then
/// stored process epoch, then the revoked state — so a wrong owner fails
/// closed even when the stored row is already revoked (owner check
/// precedes idempotence). `Ok(true)` means the live matching session must
/// be revoked and audited; `Ok(false)` means the missing- or
/// already-revoked idempotent success with no audit.
fn decide_logout(
    owner: [u8; 16],
    expected_owner: [u8; 16],
    stored_epoch: [u8; 16],
    expected_epoch: [u8; 16],
    raw_revoked: i64,
) -> Result<bool, ControllerError> {
    strict_bool(raw_revoked, "sessions.revoked")?;
    if owner != expected_owner {
        return Err(ControllerError::InvalidArgument);
    }
    if stored_epoch != expected_epoch {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(raw_revoked == 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkRowAction {
    ActorLive,
    AlreadyRevoked,
    Revoke,
}

/// strict_bool first; owner/epoch mismatch = definitive known state
/// (Ok(false) -> service NotFound, no cross-user leak); Ok(true) = live row to revoke.
fn decide_selected_target(
    owner: [u8; 16],
    expected_owner: [u8; 16],
    stored_epoch: [u8; 16],
    expected_epoch: [u8; 16],
    raw_revoked: i64,
) -> Result<bool, ControllerError> {
    strict_bool(raw_revoked, "sessions.revoked")?;
    if owner != expected_owner || stored_epoch != expected_epoch {
        return Ok(false);
    }
    Ok(raw_revoked == 0)
}

/// strict_bool first; any drift (owner/epoch/already-revoked) is fixed redacted
/// Config: the actor's own row must be this process's live row.
fn verify_actor_session_row(
    owner: [u8; 16],
    expected_owner: [u8; 16],
    stored_epoch: [u8; 16],
    expected_epoch: [u8; 16],
    raw_revoked: i64,
) -> Result<(), ControllerError> {
    strict_bool(raw_revoked, "sessions.revoked")?;
    if owner != expected_owner || stored_epoch != expected_epoch || raw_revoked != 0 {
        return Err(write_error("sessions.actor"));
    }
    Ok(())
}

/// strict_bool first, then strict epoch compare (the WHERE already pins it;
/// a mismatch is corruption), then actor identity, then revoked state.
fn decide_bulk_row(
    digest: [u8; 32],
    actor_digest: [u8; 32],
    stored_epoch: [u8; 16],
    expected_epoch: [u8; 16],
    raw_revoked: i64,
) -> Result<BulkRowAction, ControllerError> {
    strict_bool(raw_revoked, "sessions.revoked")?;
    if stored_epoch != expected_epoch {
        return Err(write_error("sessions.epoch"));
    }
    if digest == actor_digest {
        if raw_revoked != 0 {
            return Err(write_error("sessions.actor"));
        }
        return Ok(BulkRowAction::ActorLive);
    }
    Ok(if raw_revoked == 0 {
        BulkRowAction::Revoke
    } else {
        BulkRowAction::AlreadyRevoked
    })
}

/// Checked increment of the account password revision; overflow is a
/// fixed `RevisionConflict` and never wraps.
fn next_password_revision(revision: u64) -> Result<u64, ControllerError> {
    revision
        .checked_add(1)
        .ok_or(ControllerError::RevisionConflict)
}

/// Checked increment of `schema_meta.authz_epoch`; overflow is a fixed
/// `RevisionConflict` and never wraps.
fn next_authz_epoch(current: u64) -> Result<u64, ControllerError> {
    current
        .checked_add(1)
        .ok_or(ControllerError::RevisionConflict)
}

/// Shared audit insert inside a login write transaction: per-event id and
/// `time_evidence` (system fallback), auth-only epoch, and the next auth
/// event sequence (overflow propagates as `RevisionConflict`). The column
/// order matches the CAS audit path in `db.rs`.
async fn insert_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    audit: &AuditInsert,
) -> Result<(), ControllerError> {
    let evidence = crate::db::system_fallback_time_evidence();
    let seq = crate::db::next_event_seq(&AUTH_EVENT_SEQ)?;
    let epoch = AUTH_PROCESS_EPOCH.get_or_init(uuid::Uuid::new_v4);
    sqlx::query(AUDIT_INSERT_SQL)
        .bind(uuid::Uuid::new_v4().as_bytes().as_slice())
        .bind(audit.actor_kind)
        .bind(audit.actor_user_id.as_ref().map(|id| id.as_slice()))
        .bind(audit.event_type)
        .bind(audit.target_kind)
        .bind(audit.target_id.as_deref())
        .bind(audit.params_redacted)
        .bind(audit.outcome)
        .bind(&evidence)
        .bind(epoch.as_bytes().as_slice())
        .bind(seq)
        .execute(&mut **tx)
        .await
        .map_err(|_| write_error("audit.insert"))?;
    Ok(())
}

/// Fail-closed cardinality guard for `LIMIT 2`-bounded reads: exactly 0 or
/// 1 row is accepted; 2 rows (a polluted duplicate) is the fixed redacted
/// duplicate error named by the caller's stable `code`
/// (`users.duplicate` / `sessions.duplicate`) — never a first-row pick, and
/// the `code` never carries row content.
fn only_one<T>(rows: Vec<T>, code: &'static str) -> Result<Option<T>, ControllerError> {
    match rows.len() {
        0 => Ok(None),
        1 => Ok(Some(
            rows.into_iter()
                .next()
                .expect("a len-1 vector yields one element"),
        )),
        _ => Err(read_error(code)),
    }
}

/// Strict `0`/`1` decode of a `CAST(... AS SIGNED)` boolean column.
fn strict_bool(raw: i64, code: &'static str) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(read_error(code)),
    }
}

/// Strict decode of one `users` row into an `IdentityUser`.
///
/// The returned struct carries the actual stored hash and revision so the
/// later locked snapshot revalidation compares against real values.
fn decode_user(
    id: Vec<u8>,
    username: String,
    password_hash: String,
    active: i64,
    is_admin: i64,
    must_change_password: i64,
    revision: u64,
) -> Result<IdentityUser, ControllerError> {
    let active = strict_bool(active, "users.active")?;
    let is_admin = strict_bool(is_admin, "users.is_admin")?;
    let must_change_password = strict_bool(must_change_password, "users.must_change_password")?;
    let id = id.try_into().map_err(|_| read_error("users.id"))?;
    if !crate::db::valid_username(&username) {
        return Err(read_error("users.username"));
    }
    Ok(IdentityUser {
        id,
        username,
        password_hash,
        active,
        is_admin,
        must_change_password,
        revision,
    })
}

/// Strict decode of a session row joined to its user.
///
/// Known-invalid states (`revoked = 1`, `active = 0`) map to `Ok(None)`;
/// corruption (any boolean outside 0/1, bad id length, non-canonical
/// username) fails closed with a fixed redacted `Config`.
#[allow(clippy::too_many_arguments)]
fn decode_session(
    id: Vec<u8>,
    username: String,
    password_hash: String,
    active: i64,
    is_admin: i64,
    must_change_password: i64,
    revision: u64,
    revoked: i64,
) -> Result<Option<IdentityUser>, ControllerError> {
    let user = decode_user(
        id,
        username,
        password_hash,
        active,
        is_admin,
        must_change_password,
        revision,
    )?;
    let revoked = strict_bool(revoked, "sessions.revoked")?;
    if revoked || !user.active {
        Ok(None)
    } else {
        Ok(Some(user))
    }
}

/// Fixed hard capacity of the read-only session list: `LIST_USER_SESSIONS_SQL`
/// fetches `LIMIT 8193`, one row beyond this cap, so a full page can be
/// told apart from a truncated one and rejected as `ResourceExhausted`.
const MAX_LISTED_SESSIONS: usize = 8192;

/// Strict decode of one `sessions` row from the read-only list into a
/// `StoredSession`.
///
/// Check order is pinned: `strict_bool` on `revoked` first (a polluted value
/// is corruption, never a user-input problem), then the fixed-width binary
/// decode of `token_hash` (32), `user_id` (16) and `process_epoch` (16),
/// then the UTC interpretation of the `DATETIME(6)` `created_time` with
/// microsecond precision. Every failure is a fixed redacted `Config` — no
/// row content, token, digest or driver error ever enters the message.
fn decode_stored_session(
    digest: Vec<u8>,
    owner_id: Vec<u8>,
    process_epoch: Vec<u8>,
    created_time: chrono::NaiveDateTime,
    revoked: i64,
) -> Result<StoredSession, ControllerError> {
    let revoked = strict_bool(revoked, "sessions.revoked")?;
    let digest: [u8; 32] = digest
        .try_into()
        .map_err(|_| read_error("sessions.token_hash"))?;
    let owner_id: [u8; 16] = owner_id
        .try_into()
        .map_err(|_| read_error("sessions.user_id"))?;
    let process_epoch: [u8; 16] = process_epoch
        .try_into()
        .map_err(|_| read_error("sessions.process_epoch"))?;
    Ok(StoredSession {
        digest,
        owner_id,
        process_epoch,
        created_time: chrono::DateTime::from_naive_utc_and_offset(created_time, chrono::Utc),
        revoked,
    })
}

/// Fixed capacity guard on the raw fetched row count, applied BEFORE any
/// decode or public projection: more than `MAX_LISTED_SESSIONS` rows is
/// always the fixed `ResourceExhausted` — never a silently truncated list.
fn enforce_list_capacity<T>(rows: &[T]) -> Result<(), ControllerError> {
    if rows.len() > MAX_LISTED_SESSIONS {
        return Err(ControllerError::ResourceExhausted);
    }
    Ok(())
}

/// The seven `users` fields as scanned from a row, before strict decode.
struct UserFields {
    id: Vec<u8>,
    username: String,
    password_hash: String,
    active: i64,
    is_admin: i64,
    must_change_password: i64,
    revision: u64,
}

/// Strict column scan: any missing column, NULL, or type/overflow
/// mismatch maps to a fixed redacted code instead of a driver error.
fn scan_user_fields(row: &MySqlRow) -> Result<UserFields, ControllerError> {
    Ok(UserFields {
        id: row.try_get("id").map_err(|_| read_error("users.id"))?,
        username: row
            .try_get("username")
            .map_err(|_| read_error("users.username"))?,
        password_hash: row
            .try_get("password_hash")
            .map_err(|_| read_error("users.password_hash"))?,
        active: row
            .try_get("active")
            .map_err(|_| read_error("users.active"))?,
        is_admin: row
            .try_get("is_admin")
            .map_err(|_| read_error("users.is_admin"))?,
        must_change_password: row
            .try_get("must_change_password")
            .map_err(|_| read_error("users.must_change_password"))?,
        revision: row
            .try_get("revision")
            .map_err(|_| read_error("users.revision"))?,
    })
}

pub struct SqlxIdentityRepository {
    db: DbPool,
}

impl SqlxIdentityRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl IdentityRepository for SqlxIdentityRepository {
    async fn find_user(&self, username: &str) -> Result<Option<IdentityUser>, ControllerError> {
        // `LIMIT 2` + cardinality guard: a polluted duplicate username is the
        // fixed `users.duplicate` error, never a silent first-row pick; driver
        // errors map to a fixed redacted code.
        let rows = sqlx::query(FIND_USER_SQL)
            .bind(username)
            .fetch_all(&self.db.0)
            .await
            .map_err(|_| read_error("users.query"))?;
        let Some(row) = only_one(rows, "users.duplicate")? else {
            return Ok(None);
        };
        let fields = scan_user_fields(&row)?;
        decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )
        .map(Some)
    }

    async fn find_session(
        &self,
        digest: [u8; 32],
        epoch: [u8; 16],
    ) -> Result<Option<IdentityUser>, ControllerError> {
        // `LIMIT 2` + cardinality guard: a polluted duplicate session is
        // the fixed `sessions.duplicate` error, never a silent first-row
        // pick; driver errors map to a fixed redacted code. The strict
        // `revoked` decode below stays after this row check. A dangling
        // `user_id` inner-joins to nothing and reads as `Ok(None)`
        // (session denied) — accepted policy, see module docs.
        let rows = sqlx::query(FIND_SESSION_SQL)
            .bind(digest.as_slice())
            .bind(epoch.as_slice())
            .fetch_all(&self.db.0)
            .await
            .map_err(|_| read_error("sessions.query"))?;
        let Some(row) = only_one(rows, "sessions.duplicate")? else {
            return Ok(None);
        };
        let fields = scan_user_fields(&row)?;
        let revoked: i64 = row
            .try_get("revoked")
            .map_err(|_| read_error("sessions.revoked"))?;
        decode_session(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
            revoked,
        )
    }

    async fn insert_session(
        &self,
        user: &IdentityUser,
        digest: [u8; 32],
        epoch: [u8; 16],
    ) -> Result<(), ControllerError> {
        // Lock order: integrity guard -> the single `users` row (FOR UPDATE)
        // -> no further locks. Every step maps driver errors to a fixed
        // redacted code; a failure drops `tx` without commit (rollback).
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        let row = sqlx::query(LOCK_USER_SQL)
            .bind(user.id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("users.lock"))?;
        let Some(row) = row else {
            return Err(ControllerError::InvalidArgument);
        };
        let fields = scan_user_fields(&row)?;
        let current = decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )?;
        validate_current_user(Some(&current), user)?;
        sqlx::query(INSERT_SESSION_SQL)
            .bind(digest.as_slice())
            .bind(user.id.as_slice())
            .bind(epoch.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.insert"))?;
        insert_audit(&mut tx, &login_succeeded_audit(user.id)).await?;
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok(())
    }

    async fn change_password(
        &self,
        session: &Session,
        epoch: [u8; 16],
        next_hash: &str,
    ) -> Result<(), ControllerError> {
        // Lock order: integrity guard (`schema_meta`) -> the single `users`
        // row (`FOR UPDATE`) -> the current session row (`token_hash` point
        // lock) -> the user's remaining sessions (`token_hash` byte order).
        // `authz_epoch` is read under the guard and bumped by exactly one
        // via compare-and-swap in the same transaction. Every driver error
        // maps to a fixed redacted code; a failure drops `tx` without
        // commit (rollback) and is never swallowed, audit errors included.
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        let epoch_row = sqlx::query(READ_AUTHZ_EPOCH_SQL)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| write_error("epoch.read"))?;
        let old_epoch: u64 = epoch_row
            .try_get("authz_epoch")
            .map_err(|_| write_error("epoch.decode"))?;
        let next_epoch = next_authz_epoch(old_epoch)?;
        let row = sqlx::query(LOCK_USER_SQL)
            .bind(session.user.id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("users.lock"))?;
        let Some(row) = row else {
            return Err(ControllerError::InvalidArgument);
        };
        let fields = scan_user_fields(&row)?;
        let current = decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )?;
        validate_current_user(Some(&current), &session.user)?;
        let session_row = sqlx::query(LOCK_SESSION_SQL)
            .bind(session.digest.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("session.lock"))?;
        let Some(session_row) = session_row else {
            return Err(ControllerError::InvalidArgument);
        };
        let owner: Vec<u8> = session_row
            .try_get("user_id")
            .map_err(|_| write_error("session.lock"))?;
        let owner: [u8; 16] = owner.try_into().map_err(|_| write_error("session.lock"))?;
        let stored_epoch: Vec<u8> = session_row
            .try_get("process_epoch")
            .map_err(|_| write_error("session.lock"))?;
        let stored_epoch: [u8; 16] = stored_epoch
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let revoked: i64 = session_row
            .try_get("revoked")
            .map_err(|_| write_error("session.lock"))?;
        validate_change_password_session(owner, session.user.id, stored_epoch, epoch, revoked)?;
        let next_revision = next_password_revision(session.user.revision)?;
        let updated = sqlx::query(UPDATE_USERS_SQL)
            .bind(next_hash)
            .bind(next_revision)
            .bind(session.user.id.as_slice())
            .bind(session.user.revision)
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("users.update"))?;
        if updated.rows_affected() != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        let rows = sqlx::query(SELECT_USER_SESSIONS_SQL)
            .bind(session.user.id.as_slice())
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.read"))?;
        let mut to_revoke = 0u64;
        for row in &rows {
            let revoked: i64 = row
                .try_get("revoked")
                .map_err(|_| write_error("sessions.read"))?;
            strict_bool(revoked, "sessions.revoked")?;
            if revoked == 0 {
                to_revoke += 1;
            }
        }
        sqlx::query(REVOKE_USER_SESSIONS_SQL)
            .bind(session.user.id.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.revoke"))?;
        insert_audit(&mut tx, &password_changed_audit(session.user.id)).await?;
        if to_revoke > 0 {
            insert_audit(&mut tx, &session_revoked_audit(session.user.id)).await?;
        }
        let bumped = sqlx::query(BUMP_AUTHZ_EPOCH_SQL)
            .bind(next_epoch)
            .bind(old_epoch)
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("epoch.bump"))?;
        if bumped.rows_affected() != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok(())
    }

    async fn revoke_session(
        &self,
        session: &Session,
        epoch: [u8; 16],
    ) -> Result<(), ControllerError> {
        // Lock order: integrity guard (`schema_meta`) -> the single `users`
        // row (`FOR UPDATE`) -> the current session row (`token_hash` point
        // lock). No `schema_meta.authz_epoch` read or bump: a logout
        // revokes one session row and changes no account or grant state,
        // so no authorization cache needs invalidation. The current user
        // row is read for its strict decode and `active` flag only — the
        // stored hash/revision are deliberately NOT rechecked, so a
        // concurrent password change cannot prevent cleanup. Every driver
        // error maps to a fixed redacted code; a failure drops `tx` without
        // commit (rollback) and is never swallowed, audit errors included.
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        let row = sqlx::query(LOCK_USER_SQL)
            .bind(session.user.id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("users.lock"))?;
        let Some(row) = row else {
            // The spec preserves user rows; a missing row under the guard
            // is corruption — fail closed and never report logout complete.
            return Err(ControllerError::InvalidArgument);
        };
        let fields = scan_user_fields(&row)?;
        let current = decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )?;
        let session_row = sqlx::query(LOCK_SESSION_SQL)
            .bind(session.digest.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("session.lock"))?;
        let Some(session_row) = session_row else {
            // Missing session row: already gone — idempotent success with
            // no audit (also for a now-inactive user). Nothing was written,
            // so the transaction is dropped (rollback) and the locks
            // released.
            return Ok(());
        };
        let owner: Vec<u8> = session_row
            .try_get("user_id")
            .map_err(|_| write_error("session.lock"))?;
        let owner: [u8; 16] = owner.try_into().map_err(|_| write_error("session.lock"))?;
        let stored_epoch: Vec<u8> = session_row
            .try_get("process_epoch")
            .map_err(|_| write_error("session.lock"))?;
        let stored_epoch: [u8; 16] = stored_epoch
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let revoked: i64 = session_row
            .try_get("revoked")
            .map_err(|_| write_error("session.lock"))?;
        let should_revoke = decide_logout(owner, session.user.id, stored_epoch, epoch, revoked)?;
        if !should_revoke {
            // Already revoked with matching owner/epoch: idempotent success
            // with no audit, even when the user is now inactive. Nothing
            // was written, so the transaction is dropped (rollback).
            return Ok(());
        }
        if !current.active {
            // A live matching session under an inactive user can only
            // exist by corruption (deactivation revokes sessions in the
            // same transaction); fail closed — it cannot be attributed to
            // a valid user actor, so a fixed redacted Config, not
            // InvalidArgument.
            return Err(write_error("users.active"));
        }
        let updated = sqlx::query(REVOKE_SESSION_SQL)
            .bind(session.digest.as_slice())
            .bind(session.user.id.as_slice())
            .bind(epoch.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.revoke"))?;
        if updated.rows_affected() != 1 {
            // Under the held point lock this is an invariant violation:
            // fixed redacted Config and rollback — never Ok, never audit.
            return Err(write_error("sessions.revoke"));
        }
        insert_audit(&mut tx, &logout_audit(session.user.id)).await?;
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok(())
    }

    async fn record_login_failure(&self, user_id: Option<[u8; 16]>) -> Result<(), ControllerError> {
        // Guard then the fixed anonymous audit row, same transaction; a
        // failure drops `tx` without commit (rollback) and returns the
        // fixed error — the audit write is never swallowed.
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        insert_audit(&mut tx, &login_failure_audit(user_id)).await?;
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok(())
    }

    async fn list_user_sessions(
        &self,
        actor: &Session,
        epoch: [u8; 16],
    ) -> Result<Vec<StoredSession>, ControllerError> {
        // Read-only fixed bound-only SELECT: no transaction, no lock, no
        // write. Bind order is the verified actor's user id, then the
        // caller's process epoch — the actor is the authenticated service
        // session, never a client-supplied id. The capacity guard runs on
        // the raw row count BEFORE any decode or public projection:
        // `LIMIT 8193` admits one row beyond the 8192 hard cap, so 8193
        // fetched rows are the fixed `ResourceExhausted`, never a
        // truncated list. No `revoked` predicate: a polluted value is
        // decoded strictly and rejected. Every driver or decode failure
        // maps to a fixed redacted code — no raw row, token, digest or
        // driver error is ever logged or returned.
        let rows = sqlx::query(LIST_USER_SESSIONS_SQL)
            .bind(actor.user.id.as_slice())
            .bind(epoch.as_slice())
            .fetch_all(&self.db.0)
            .await
            .map_err(|_| read_error("sessions.list"))?;
        enforce_list_capacity(&rows)?;
        let mut sessions = Vec::with_capacity(rows.len());
        for row in &rows {
            let digest: Vec<u8> = row
                .try_get("token_hash")
                .map_err(|_| read_error("sessions.token_hash"))?;
            let owner_id: Vec<u8> = row
                .try_get("user_id")
                .map_err(|_| read_error("sessions.user_id"))?;
            let process_epoch: Vec<u8> = row
                .try_get("process_epoch")
                .map_err(|_| read_error("sessions.process_epoch"))?;
            let created_time: chrono::NaiveDateTime = row
                .try_get("created_time")
                .map_err(|_| read_error("sessions.created_time"))?;
            let revoked: i64 = row
                .try_get("revoked")
                .map_err(|_| read_error("sessions.revoked"))?;
            sessions.push(decode_stored_session(
                digest,
                owner_id,
                process_epoch,
                created_time,
                revoked,
            )?);
        }
        Ok(sessions)
    }

    async fn revoke_selected_session(
        &self,
        actor: &Session,
        epoch: [u8; 16],
        target_digest: [u8; 32],
    ) -> Result<bool, ControllerError> {
        // Lock order:
        // 1. tx.begin
        // 2. lock_integrity_guard
        // 3. users row FOR UPDATE by actor.user.id via LOCK_USER_SQL
        // 4. sessions: point-lock both digests in ascending byte order
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;

        // Lock users row
        let user_row = sqlx::query(LOCK_USER_SQL)
            .bind(actor.user.id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("users.lock"))?;
        let Some(user_row) = user_row else {
            return Err(write_error("users.missing"));
        };
        let fields = scan_user_fields(&user_row)?;
        let current_user = decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )?;
        if !current_user.active {
            return Err(write_error("users.active"));
        }

        // Point-lock both digests in ascending token_hash byte order.
        // Precondition guarantees actor.digest != target_digest.
        let (first_digest, second_digest) = if actor.digest < target_digest {
            (actor.digest, target_digest)
        } else {
            (target_digest, actor.digest)
        };

        let row_1 = sqlx::query(LOCK_SESSION_SQL)
            .bind(first_digest.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("session.lock"))?;
        let row_2 = sqlx::query(LOCK_SESSION_SQL)
            .bind(second_digest.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("session.lock"))?;

        let (actor_row, target_row) = if actor.digest < target_digest {
            (row_1, row_2)
        } else {
            (row_2, row_1)
        };

        // Missing actor row -> sessions.missing
        let Some(actor_row) = actor_row else {
            return Err(write_error("sessions.missing"));
        };

        // Strict per-row decode for actor session
        let actor_owner: Vec<u8> = actor_row
            .try_get("user_id")
            .map_err(|_| write_error("session.lock"))?;
        let actor_owner: [u8; 16] = actor_owner
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let actor_stored_epoch: Vec<u8> = actor_row
            .try_get("process_epoch")
            .map_err(|_| write_error("session.lock"))?;
        let actor_stored_epoch: [u8; 16] = actor_stored_epoch
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let actor_revoked: i64 = actor_row
            .try_get("revoked")
            .map_err(|_| write_error("session.lock"))?;

        verify_actor_session_row(
            actor_owner,
            actor.user.id,
            actor_stored_epoch,
            epoch,
            actor_revoked,
        )?;

        // If target_row is missing -> target_missing -> Ok(false)
        let Some(target_row) = target_row else {
            return Ok(false);
        };

        // Strict per-row decode for target session
        let target_owner: Vec<u8> = target_row
            .try_get("user_id")
            .map_err(|_| write_error("session.lock"))?;
        let target_owner: [u8; 16] = target_owner
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let target_stored_epoch: Vec<u8> = target_row
            .try_get("process_epoch")
            .map_err(|_| write_error("session.lock"))?;
        let target_stored_epoch: [u8; 16] = target_stored_epoch
            .try_into()
            .map_err(|_| write_error("session.lock"))?;
        let target_revoked: i64 = target_row
            .try_get("revoked")
            .map_err(|_| write_error("session.lock"))?;

        let should_revoke = decide_selected_target(
            target_owner,
            actor.user.id,
            target_stored_epoch,
            epoch,
            target_revoked,
        )?;

        if !should_revoke {
            return Ok(false);
        }

        let updated = sqlx::query(REVOKE_SESSION_SQL)
            .bind(target_digest.as_slice())
            .bind(actor.user.id.as_slice())
            .bind(epoch.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.revoke"))?;
        if updated.rows_affected() != 1 {
            return Err(write_error("sessions.count"));
        }

        insert_audit(&mut tx, &session_revoked_audit(actor.user.id)).await?;
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok(true)
    }

    async fn revoke_other_sessions(
        &self,
        actor: &Session,
        epoch: [u8; 16],
    ) -> Result<(u64, Vec<[u8; 32]>), ControllerError> {
        // Lock order:
        // 1. tx.begin
        // 2. lock_integrity_guard
        // 3. users row FOR UPDATE by actor.user.id via LOCK_USER_SQL
        // 4. sessions: scan-lock in token_hash byte order via LOCK_USER_SESSIONS_SQL
        let mut tx = self
            .db
            .0
            .begin()
            .await
            .map_err(|_| write_error("tx.begin"))?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;

        // Lock users row
        let user_row = sqlx::query(LOCK_USER_SQL)
            .bind(actor.user.id.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| write_error("users.lock"))?;
        let Some(user_row) = user_row else {
            return Err(write_error("users.missing"));
        };
        let fields = scan_user_fields(&user_row)?;
        let current_user = decode_user(
            fields.id,
            fields.username,
            fields.password_hash,
            fields.active,
            fields.is_admin,
            fields.must_change_password,
            fields.revision,
        )?;
        if !current_user.active {
            return Err(write_error("users.active"));
        }

        // Scan-lock sessions
        let rows = sqlx::query(LOCK_USER_SESSIONS_SQL)
            .bind(actor.user.id.as_slice())
            .bind(epoch.as_slice())
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.scan"))?;
        enforce_list_capacity(&rows)?;

        let mut current_seen = false;
        let mut to_revoke = Vec::new();

        for row in &rows {
            let digest_vec: Vec<u8> = row
                .try_get("token_hash")
                .map_err(|_| write_error("sessions.token_hash"))?;
            let digest: [u8; 32] = digest_vec
                .try_into()
                .map_err(|_| write_error("sessions.token_hash"))?;
            let stored_epoch_vec: Vec<u8> = row
                .try_get("process_epoch")
                .map_err(|_| write_error("sessions.process_epoch"))?;
            let stored_epoch: [u8; 16] = stored_epoch_vec
                .try_into()
                .map_err(|_| write_error("sessions.process_epoch"))?;
            let raw_revoked: i64 = row
                .try_get("revoked")
                .map_err(|_| write_error("sessions.revoked"))?;

            match decide_bulk_row(digest, actor.digest, stored_epoch, epoch, raw_revoked)? {
                BulkRowAction::ActorLive => {
                    current_seen = true;
                }
                BulkRowAction::Revoke => {
                    to_revoke.push(digest);
                }
                BulkRowAction::AlreadyRevoked => {}
            }
        }

        if !current_seen {
            return Err(write_error("sessions.missing"));
        }

        if to_revoke.is_empty() {
            return Ok((0, Vec::new()));
        }

        let updated = sqlx::query(REVOKE_OTHER_SESSIONS_SQL)
            .bind(actor.user.id.as_slice())
            .bind(epoch.as_slice())
            .bind(actor.digest.as_slice())
            .execute(&mut *tx)
            .await
            .map_err(|_| write_error("sessions.revoke"))?;

        let expected_count = to_revoke.len() as u64;
        if updated.rows_affected() != expected_count {
            return Err(write_error("sessions.count"));
        }

        insert_audit(&mut tx, &session_revoked_audit(actor.user.id)).await?;
        tx.commit().await.map_err(|_| write_error("tx.commit"))?;
        Ok((expected_count, to_revoke))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic Argon2id-shaped hash; not a real secret and never verifiable.
    const SYNTHETIC_HASH: &str =
        "$argon2id$v=19$m=65536,t=3,p=1$c2FsdHNhbHRzYWx0$abcDEF1234567890abcDEF1234567890abcdef";

    /// `(id, username, password_hash, active, is_admin, must_change_password,
    /// revision)` — the valid synthetic row from the task brief.
    fn valid_user_tuple() -> (Vec<u8>, String, String, i64, i64, i64, u64) {
        (
            vec![1u8; 16],
            "alice".to_string(),
            SYNTHETIC_HASH.to_string(),
            1,
            0,
            0,
            u64::MAX,
        )
    }

    fn sample_user() -> IdentityUser {
        let (id, username, password_hash, _active, _is_admin, _mcp, revision) = valid_user_tuple();
        IdentityUser {
            id: id.try_into().unwrap(),
            username,
            password_hash,
            active: true,
            is_admin: false,
            must_change_password: false,
            revision,
        }
    }

    /// The error must be a fixed redacted `Config`: stable shape, and none
    /// of the supplied input data may appear in the message.
    fn assert_fixed_redacted_config(err: &ControllerError, leaked: &[&str]) {
        match err {
            ControllerError::Config(msg) => {
                assert!(
                    msg.starts_with("identity auth read "),
                    "not the fixed redacted shape: {msg:?}"
                );
                // Only meaningful substrings: single characters also occur
                // inside the fixed English message itself.
                for needle in leaked.iter().filter(|n| n.len() >= 3) {
                    assert!(!msg.contains(needle), "error leaks input: {msg:?}");
                }
            }
            other => panic!("expected fixed Config, got {other:?}"),
        }
    }

    // -- decode_user -------------------------------------------------------

    #[test]
    fn decode_user_accepts_valid_synthetic_row() {
        let (id, username, password_hash, active, is_admin, mcp, revision) = valid_user_tuple();
        let user = decode_user(id, username, password_hash, active, is_admin, mcp, revision)
            .expect("valid synthetic row must decode");
        assert_eq!(user.id, [1u8; 16]);
        assert_eq!(user.username, "alice");
        assert_eq!(user.password_hash, SYNTHETIC_HASH);
        assert!(user.active);
        assert!(!user.is_admin);
        assert!(!user.must_change_password);
        assert_eq!(user.revision, u64::MAX);
    }

    #[test]
    fn decode_user_rejects_polluted_booleans_fixed_redacted() {
        for raw in [2, -1, 42, i64::MAX] {
            for field in 0..3u32 {
                let mut t = valid_user_tuple();
                match field {
                    0 => t.3 = raw,
                    1 => t.4 = raw,
                    _ => t.5 = raw,
                }
                let (id, username, password_hash, active, is_admin, mcp, revision) = t;
                let err = decode_user(id, username, password_hash, active, is_admin, mcp, revision)
                    .expect_err("polluted boolean must not decode");
                assert_fixed_redacted_config(&err, &["alice", SYNTHETIC_HASH]);
            }
        }
    }

    #[test]
    fn decode_user_rejects_wrong_id_length_fixed_redacted() {
        for len in [0usize, 1, 15, 17, 32] {
            let mut t = valid_user_tuple();
            t.0 = vec![7u8; len];
            let (id, username, password_hash, active, is_admin, mcp, revision) = t;
            let err = decode_user(id, username, password_hash, active, is_admin, mcp, revision)
                .expect_err("wrong id length must not decode");
            assert_fixed_redacted_config(&err, &["alice", SYNTHETIC_HASH]);
        }
    }

    #[test]
    fn decode_user_rejects_noncanonical_username_fixed_redacted() {
        for name in ["Alice", "ab", "a", "ali ce", "ali!ce", ""] {
            let mut t = valid_user_tuple();
            t.1 = name.to_string();
            let (id, username, password_hash, active, is_admin, mcp, revision) = t;
            let err = decode_user(id, username, password_hash, active, is_admin, mcp, revision)
                .expect_err("non-canonical username must not decode");
            assert_fixed_redacted_config(&err, &[name, SYNTHETIC_HASH]);
        }
        let mut t = valid_user_tuple();
        t.1 = "a".repeat(65);
        let (id, username, password_hash, active, is_admin, mcp, revision) = t;
        let err = decode_user(id, username, password_hash, active, is_admin, mcp, revision)
            .expect_err("oversized username must not decode");
        assert_fixed_redacted_config(&err, &["alice", SYNTHETIC_HASH]);
    }

    // -- strict_bool (pure revoked parser) ----------------------------------

    #[test]
    fn strict_bool_accepts_zero_and_one() {
        assert!(matches!(strict_bool(0, "sessions.revoked"), Ok(false)));
        assert!(matches!(strict_bool(1, "sessions.revoked"), Ok(true)));
    }

    #[test]
    fn strict_bool_rejects_polluted_values_fixed_redacted() {
        for raw in [2, -1, 255, i64::MAX, i64::MIN] {
            let err =
                strict_bool(raw, "sessions.revoked").expect_err("polluted value must not decode");
            assert_fixed_redacted_config(&err, &[]);
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth read sessions.revoked"),
                "must be the exact fixed code, got {err:?}"
            );
        }
    }

    // -- only_one (LIMIT 2 cardinality guard) ---------------------------------

    #[test]
    fn only_one_passes_through_zero_rows() {
        let rows: Vec<u8> = vec![];
        assert!(matches!(only_one(rows, "users.duplicate"), Ok(None)));
    }

    #[test]
    fn only_one_passes_through_single_row() {
        let rows: Vec<u8> = vec![42];
        assert!(matches!(only_one(rows, "users.duplicate"), Ok(Some(42))));
    }

    #[test]
    fn only_one_rejects_duplicate_rows_fixed_redacted() {
        let rows: Vec<u8> = vec![1, 2];
        let err = only_one(rows, "users.duplicate")
            .expect_err("two rows must be rejected, never first-row picked");
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity auth read users.duplicate"),
            "duplicate rows must give the exact fixed code, got {err:?}"
        );
    }

    #[test]
    fn only_one_rejects_duplicate_session_rows_fixed_redacted() {
        let rows: Vec<u8> = vec![1, 2];
        let err = only_one(rows, "sessions.duplicate")
            .expect_err("two session rows must be rejected, never first-row picked");
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity auth read sessions.duplicate"),
            "duplicate session rows must give the exact fixed code, got {err:?}"
        );
    }

    // -- read_error (pure redaction surface) ----------------------------------

    #[test]
    fn read_error_yields_exact_fixed_redacted_config() {
        let err = read_error("users.duplicate");
        assert_fixed_redacted_config(&err, &["alice", SYNTHETIC_HASH]);
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity auth read users.duplicate"),
            "must be the exact fixed code, got {err:?}"
        );
    }

    // -- decode_session ------------------------------------------------------

    #[test]
    fn decode_session_returns_user_when_active_and_unrevoked() {
        let (id, username, password_hash, active, is_admin, mcp, revision) = valid_user_tuple();
        let user = decode_session(
            id,
            username,
            password_hash,
            active,
            is_admin,
            mcp,
            revision,
            0,
        )
        .expect("clean row must decode")
        .expect("active + unrevoked must be Some");
        assert_eq!(user.username, "alice");
        assert!(user.active);
    }

    #[test]
    fn decode_session_returns_none_when_revoked() {
        let (id, username, password_hash, active, is_admin, mcp, revision) = valid_user_tuple();
        let result = decode_session(
            id,
            username,
            password_hash,
            active,
            is_admin,
            mcp,
            revision,
            1,
        )
        .expect("revoked = 1 is a known state, not corruption");
        assert!(result.is_none(), "revoked session must be None");
    }

    #[test]
    fn decode_session_returns_none_when_inactive() {
        let mut t = valid_user_tuple();
        t.3 = 0; // active = 0
        let (id, username, password_hash, active, is_admin, mcp, revision) = t;
        let result = decode_session(
            id,
            username,
            password_hash,
            active,
            is_admin,
            mcp,
            revision,
            0,
        )
        .expect("active = 0 is a known state, not corruption");
        assert!(result.is_none(), "inactive user must be None");
    }

    #[test]
    fn decode_session_rejects_polluted_revoked_not_missing() {
        for raw in [2, -1] {
            let (id, username, password_hash, active, is_admin, mcp, revision) = valid_user_tuple();
            let err = decode_session(
                id,
                username,
                password_hash,
                active,
                is_admin,
                mcp,
                revision,
                raw,
            )
            .expect_err("polluted revoked must be corruption, not missing");
            assert_fixed_redacted_config(&err, &["alice", SYNTHETIC_HASH]);
        }
    }

    // -- fixed SQL catalog ----------------------------------------------------

    #[test]
    fn sql_catalog_is_fixed_read_only_selects() {
        for (sql, expected_binds) in [(FIND_USER_SQL, 1usize), (FIND_SESSION_SQL, 2)] {
            let normalized = sql.trim().to_ascii_uppercase();
            assert_eq!(
                normalized.split_whitespace().next(),
                Some("SELECT"),
                "must be a single SELECT: {sql:?}"
            );
            assert!(!sql.contains(';'), "no statement separator: {sql:?}");
            let forbidden = [
                "SET", "USE", "UPDATE", "INSERT", "DELETE", "DROP", "ALTER", "TRUNCATE", "GRANT",
            ];
            for token in sql.split_whitespace() {
                let bare = token
                    .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                    .to_ascii_uppercase();
                assert!(
                    !forbidden.contains(&bare.as_str()),
                    "forbidden keyword token in fixed SQL: {bare:?} in {sql:?}"
                );
            }
            assert_eq!(
                sql.matches('?').count(),
                expected_binds,
                "bound key count: {sql:?}"
            );
            let compact = normalized.replace(char::is_whitespace, "");
            assert!(
                !compact.contains("REVOKED=FALSE"),
                "no revoked=FALSE filter anywhere: {sql:?}"
            );
        }
        let user_compact = FIND_USER_SQL
            .replace(char::is_whitespace, "")
            .to_ascii_uppercase();
        assert!(
            user_compact.contains("LIMIT2"),
            "FIND_USER_SQL must bound the fetch with LIMIT 2: {FIND_USER_SQL:?}"
        );
        for cast in [
            "CAST(U.ACTIVEASSIGNED)",
            "CAST(U.IS_ADMINASSIGNED)",
            "CAST(U.MUST_CHANGE_PASSWORDASSIGNED)",
        ] {
            assert!(
                user_compact.contains(cast),
                "missing {cast} in {FIND_USER_SQL:?}"
            );
        }
        let session_compact = FIND_SESSION_SQL
            .replace(char::is_whitespace, "")
            .to_ascii_uppercase();
        assert!(
            session_compact.contains("LIMIT2"),
            "FIND_SESSION_SQL must bound the fetch with LIMIT 2: {FIND_SESSION_SQL:?}"
        );
        for cast in [
            "CAST(U.ACTIVEASSIGNED)",
            "CAST(U.IS_ADMINASSIGNED)",
            "CAST(U.MUST_CHANGE_PASSWORDASSIGNED)",
            "CAST(S.REVOKEDASSIGNED)",
        ] {
            assert!(
                session_compact.contains(cast),
                "missing {cast} in {FIND_SESSION_SQL:?}"
            );
        }
    }

    // -- Task 2b: pure snapshot validation --------------------------------------

    #[test]
    fn snapshot_validation_pure() {
        let mut snap = sample_user();
        snap.revision = u64::MAX;
        let cur = snap.clone();
        assert!(validate_current_user(Some(&cur), &snap).is_ok()); // stub InvalidArgument -> RED
        assert!(matches!(
            validate_current_user(None, &snap),
            Err(ControllerError::InvalidArgument)
        ));
        let mut inactive = cur.clone();
        inactive.active = false;
        inactive.revision -= 1; // active 拒绝优先于 revision 冲突
        assert!(matches!(
            validate_current_user(Some(&inactive), &snap),
            Err(ControllerError::InvalidArgument)
        ));
        let mut wrong_id = cur.clone();
        wrong_id.id = [9; 16];
        assert!(matches!(
            validate_current_user(Some(&wrong_id), &snap),
            Err(ControllerError::InvalidArgument)
        ));
        let mut changed_hash = cur.clone();
        changed_hash.password_hash.push('x');
        assert!(matches!(
            validate_current_user(Some(&changed_hash), &snap),
            Err(ControllerError::RevisionConflict)
        ));
        let mut old_revision = cur.clone();
        old_revision.revision = u64::MAX - 1;
        assert!(matches!(
            validate_current_user(Some(&old_revision), &snap),
            Err(ControllerError::RevisionConflict)
        ));
        let mut zero = snap.clone();
        zero.revision = 0;
        let mut one = zero.clone();
        one.revision = 1;
        assert!(matches!(
            validate_current_user(Some(&one), &zero),
            Err(ControllerError::RevisionConflict)
        ));
    }

    // -- Task 2b: pinned audit projections ---------------------------------------

    #[test]
    fn login_failed_audit_isomorphic_for_both_shapes() {
        let unknown = login_failure_audit(None);
        assert_eq!(unknown, login_failure_audit(Some([9; 16]))); // wrong_audit stub -> next assertions RED
        assert_eq!(unknown.actor_kind, "system");
        assert_eq!(unknown.actor_user_id, None);
        assert_eq!(unknown.event_type, "auth.login.failed");
        assert_eq!(unknown.target_kind, Some("user"));
        assert_eq!(unknown.target_id, None);
        assert_eq!(unknown.params_redacted, "{}");
        assert_eq!(unknown.outcome, "failure");
    }

    #[test]
    fn login_succeeded_audit_pinned_projection() {
        let id = [7; 16];
        let success = login_succeeded_audit(id);
        assert_eq!(success.actor_kind, "user");
        assert_eq!(success.actor_user_id, Some(id));
        assert_eq!(success.event_type, "auth.login.succeeded");
        assert_eq!(success.target_kind, Some("user"));
        let encoded_id = hex::encode(id);
        assert_eq!(success.target_id.as_deref(), Some(encoded_id.as_str()));
        assert_eq!(success.params_redacted, "{}");
        assert_eq!(success.outcome, "success");
    }

    // -- Task 2b: write SQL gate ---------------------------------------------------

    #[test]
    fn write_sql_gate_fixed_strings() {
        // Step1 SQL 常量已正确：GREEN-by-construction，不计 RED
        let lock = LOCK_USER_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_uppercase();
        assert!(lock.starts_with("SELECT ") && lock.contains("WHERE ID = ? FOR UPDATE"));
        assert_eq!(lock.matches('?').count(), 1);
        assert!(!lock.contains(';'));
        let session = INSERT_SESSION_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("")
            .to_ascii_uppercase();
        assert!(session.contains("(TOKEN_HASH,USER_ID,PROCESS_EPOCH,CREATED_TIME,REVOKED)"));
        assert!(session.contains("UTC_TIMESTAMP(6)"));
        assert_eq!(session.matches('?').count(), 3);
        let audit = AUDIT_INSERT_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("")
            .to_ascii_uppercase();
        assert!(audit.contains("(ID,ACTOR_KIND,ACTOR_USER_ID,EVENT_TYPE,TARGET_KIND,TARGET_ID,PARAMS_REDACTED,OUTCOME,TIME_EVIDENCE,PROCESS_EPOCH,EVENT_SEQ)"));
        assert_eq!(audit.matches('?').count(), 11);
    }

    // -- Task 3: pure session validation ---------------------------------------

    #[test]
    fn change_password_session_strict() {
        // session owner/epoch/revoked 污染
        let e = [9u8; 16];
        assert!(matches!(
            validate_change_password_session([1; 16], [2; 16], e, e, 0),
            Err(ControllerError::InvalidArgument)
        )); // owner 错配
        assert!(matches!(
            validate_change_password_session([1; 16], [1; 16], [1; 16], e, 0),
            Err(ControllerError::InvalidArgument)
        )); // epoch 错配
        assert!(matches!(
            validate_change_password_session([1; 16], [1; 16], e, e, 1),
            Err(ControllerError::InvalidArgument)
        )); // 已撤
        for raw in [2i64, -1] {
            assert_fixed_redacted_config(
                &validate_change_password_session([1; 16], [1; 16], e, e, raw).unwrap_err(),
                &[],
            );
        } // 污染 -> 固定 Config
        assert!(validate_change_password_session([1; 16], [1; 16], e, e, 0).is_ok());
        // 干净 -> Ok
    }

    #[test]
    fn revision_overflow_checked_add() {
        // u64::MAX 溢出
        assert!(matches!(
            next_password_revision(u64::MAX),
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(next_password_revision(5).unwrap(), 6); // RED(桩恒 Err)
    }

    #[test]
    fn authz_epoch_overflow_checked_add() {
        assert!(matches!(
            next_authz_epoch(u64::MAX),
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(next_authz_epoch(7).unwrap(), 8); // RED(桩恒 Err)；新增纯函数，既有 `validate_current_user` 已审路径零改动
    }

    #[test]
    fn password_changed_audit_pinned() {
        let a = password_changed_audit([7; 16]); // RED(wrong_audit)
        assert_eq!(a.actor_kind, "user");
        assert_eq!(a.actor_user_id, Some([7; 16]));
        assert_eq!(a.event_type, "auth.password.changed");
        assert_eq!(a.target_kind, Some("user"));
        assert_eq!(a.target_id.as_deref(), Some(hex::encode([7; 16]).as_str()));
        assert_eq!(a.params_redacted, "{}");
        assert_eq!(a.outcome, "success");
    }

    #[test]
    fn session_revoked_audit_pinned() {
        let a = session_revoked_audit([7; 16]); // 同上，event_type "auth.session.revoked"
        assert_eq!(a.actor_kind, "user");
        assert_eq!(a.actor_user_id, Some([7; 16]));
        assert_eq!(a.event_type, "auth.session.revoked");
        assert_eq!(a.target_kind, Some("user"));
        assert_eq!(a.target_id.as_deref(), Some(hex::encode([7; 16]).as_str()));
        assert_eq!(a.params_redacted, "{}");
        assert_eq!(a.outcome, "success");
    }

    // -- Task 3: write SQL gate --------------------------------------------------

    #[test]
    fn change_password_sql_gate_fixed_strings() {
        // Step1 SQL 常量已正确：GREEN-by-construction，不计 RED
        let norm = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_uppercase()
        };
        let compact = |s: &str| s.replace(char::is_whitespace, "").to_ascii_uppercase();
        assert!(norm(LOCK_SESSION_SQL).ends_with("FOR UPDATE"));
        assert_eq!(LOCK_SESSION_SQL.matches('?').count(), 1);
        assert!(
            compact(UPDATE_USERS_SQL).contains("MUST_CHANGE_PASSWORD=FALSE")
                && compact(UPDATE_USERS_SQL).contains("WHEREID=?ANDREVISION=?")
        );
        assert_eq!(UPDATE_USERS_SQL.matches('?').count(), 4);
        assert!(compact(SELECT_USER_SESSIONS_SQL).contains("ORDERBYTOKEN_HASHFORUPDATE"));
        assert_eq!(SELECT_USER_SESSIONS_SQL.matches('?').count(), 1);
        assert!(compact(REVOKE_USER_SESSIONS_SQL).contains("SETREVOKED=TRUEWHEREUSER_ID=?"));
        assert_eq!(REVOKE_USER_SESSIONS_SQL.matches('?').count(), 1);
        assert!(compact(READ_AUTHZ_EPOCH_SQL).contains("FROMSCHEMA_METAWHERESINGLETON=1"));
        assert!(compact(BUMP_AUTHZ_EPOCH_SQL).contains("WHERESINGLETON=1ANDAUTHZ_EPOCH=?"));
        assert_eq!(BUMP_AUTHZ_EPOCH_SQL.matches('?').count(), 2);
        for s in [
            LOCK_SESSION_SQL,
            UPDATE_USERS_SQL,
            SELECT_USER_SESSIONS_SQL,
            REVOKE_USER_SESSIONS_SQL,
            READ_AUTHZ_EPOCH_SQL,
            BUMP_AUTHZ_EPOCH_SQL,
        ] {
            assert!(!s.contains(';'));
        }
    }

    // -- Task 4b: pure logout decision --------------------------------------------

    #[test]
    fn decide_logout_matching_unrevoked_proceeds() {
        // RED: the stub refuses everything, so this must fail on the
        // assertion before GREEN.
        let epoch = [3u8; 16];
        assert!(
            matches!(decide_logout([1; 16], [1; 16], epoch, epoch, 0), Ok(true)),
            "matching live session must proceed to revoke + audit"
        );
    }

    #[test]
    fn decide_logout_matching_revoked_is_idempotent() {
        // RED: the stub refuses everything, so this must fail on the
        // assertion before GREEN.
        let epoch = [3u8; 16];
        assert!(
            matches!(decide_logout([1; 16], [1; 16], epoch, epoch, 1), Ok(false)),
            "matching already-revoked session must be idempotent, no audit"
        );
    }

    #[test]
    fn decide_logout_wrong_owner_fails_closed_before_idempotence() {
        // Wrong owner must fail closed for both revoked states; the
        // `revoked = 1` case is the discriminator pinning owner-before-
        // idempotence (M-3).
        let epoch = [3u8; 16];
        for revoked in [0i64, 1] {
            assert!(
                matches!(
                    decide_logout([1; 16], [2; 16], epoch, epoch, revoked),
                    Err(ControllerError::InvalidArgument)
                ),
                "wrong owner must fail closed even when already revoked (revoked={revoked})"
            );
        }
    }

    #[test]
    fn decide_logout_wrong_epoch_fails_closed() {
        assert!(
            matches!(
                decide_logout([1; 16], [1; 16], [1; 16], [2; 16], 0),
                Err(ControllerError::InvalidArgument)
            ),
            "wrong stored process epoch must fail closed"
        );
    }

    #[test]
    fn decide_logout_polluted_revoked_precedes_owner_check() {
        // Strict bool runs before owner/epoch: even a wrong owner with a
        // polluted `revoked` is the fixed redacted Config, not
        // InvalidArgument.
        for raw in [2i64, -1] {
            let err = decide_logout([1; 16], [2; 16], [3; 16], [3; 16], raw)
                .expect_err("polluted revoked must not decode");
            assert_fixed_redacted_config(&err, &[]);
            assert!(
                matches!(
                    &err,
                    ControllerError::Config(m) if m == "identity auth read sessions.revoked"
                ),
                "strict bool must precede owner/epoch, got {err:?}"
            );
        }
    }

    #[test]
    fn logout_audit_pinned_projection() {
        // RED: the stub returns the deliberately wrong projection.
        let a = logout_audit([7; 16]);
        assert_eq!(a.actor_kind, "user");
        assert_eq!(a.actor_user_id, Some([7; 16]));
        assert_eq!(a.event_type, "auth.logout");
        assert_eq!(a.target_kind, Some("user"));
        assert_eq!(a.target_id.as_deref(), Some(hex::encode([7; 16]).as_str()));
        assert_eq!(a.params_redacted, "{}");
        assert_eq!(a.outcome, "success");
    }

    #[test]
    fn logout_sql_gate_fixed_strings() {
        // Step1 SQL 常量已正确：GREEN-by-construction，不计 RED
        let compact = REVOKE_SESSION_SQL
            .replace(char::is_whitespace, "")
            .to_ascii_uppercase();
        assert!(compact.starts_with("UPDATESESSIONS"));
        assert!(compact.contains(
            "SETREVOKED=TRUEWHERETOKEN_HASH=?ANDUSER_ID=?ANDPROCESS_EPOCH=?ANDREVOKED=FALSE"
        ));
        assert_eq!(REVOKE_SESSION_SQL.matches('?').count(), 3);
        assert!(!REVOKE_SESSION_SQL.contains(';'));
    }

    // -- Task 3 (read): fixed SQL catalog for the session list ----------------

    #[test]
    fn list_user_sessions_sql_catalog_fixed_read_only() {
        // Step1 SQL 常量已正确：GREEN-by-construction，不计 RED
        let normalized = LIST_USER_SESSIONS_SQL.trim().to_ascii_uppercase();
        assert_eq!(
            normalized.split_whitespace().next(),
            Some("SELECT"),
            "must be a single read-only SELECT: {LIST_USER_SESSIONS_SQL:?}"
        );
        assert!(
            !LIST_USER_SESSIONS_SQL.contains(';'),
            "no statement separator"
        );
        let forbidden = [
            "SET", "USE", "UPDATE", "INSERT", "DELETE", "DROP", "ALTER", "TRUNCATE", "GRANT",
            "FOR", "LOCK",
        ];
        for token in LIST_USER_SESSIONS_SQL.split_whitespace() {
            let bare = token
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_uppercase();
            assert!(
                !forbidden.contains(&bare.as_str()),
                "forbidden keyword token in fixed read-only SQL: {bare:?}"
            );
        }
        assert_eq!(
            LIST_USER_SESSIONS_SQL.matches('?').count(),
            2,
            "exactly two binds (user_id, process_epoch): {LIST_USER_SESSIONS_SQL:?}"
        );
        let compact = normalized.replace(char::is_whitespace, "");
        assert!(
            compact
                .contains("FROMSESSIONSWHEREUSER_ID=?ANDPROCESS_EPOCH=?ORDERBYTOKEN_HASHLIMIT8193"),
            "fixed WHERE/ORDER BY/LIMIT 8193 shape: {LIST_USER_SESSIONS_SQL:?}"
        );
        assert!(
            compact.contains("CAST(REVOKEDASSIGNED)"),
            "revoked must be projected CAST(... AS SIGNED): {LIST_USER_SESSIONS_SQL:?}"
        );
        assert!(
            !compact.contains("REVOKED=FALSE"),
            "no revoked=FALSE filter: pollution must be decoded, not hidden"
        );
    }

    // -- Task 3 (read): strict row decode --------------------------------------

    /// `(token_hash, user_id, process_epoch, created_time, revoked)` — the
    /// valid synthetic list row: 32-byte digest, 16-byte owner, 16-byte
    /// epoch, a microsecond `DATETIME(6)` stored in UTC, `revoked` 0.
    fn valid_stored_session_tuple() -> (Vec<u8>, Vec<u8>, Vec<u8>, chrono::NaiveDateTime, i64) {
        (
            vec![0xA5u8; 32],
            vec![0x11u8; 16],
            vec![0x22u8; 16],
            chrono::NaiveDateTime::new(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
                chrono::NaiveTime::from_hms_micro_opt(12, 34, 56, 789_123).unwrap(),
            ),
            0,
        )
    }

    #[test]
    fn decode_stored_session_accepts_single_clean_row() {
        for revoked in [0i64, 1] {
            let (digest, owner_id, process_epoch, created, raw) = {
                let mut t = valid_stored_session_tuple();
                t.4 = revoked;
                t
            };
            let s = decode_stored_session(digest, owner_id, process_epoch, created, raw)
                .expect("clean row must decode");
            assert_eq!(s.digest, [0xA5u8; 32]);
            assert_eq!(s.owner_id, [0x11u8; 16]);
            assert_eq!(s.process_epoch, [0x22u8; 16]);
            assert_eq!(s.revoked, revoked == 1);
            let expected: chrono::DateTime<chrono::Utc> =
                chrono::DateTime::from_naive_utc_and_offset(created, chrono::Utc);
            assert_eq!(s.created_time, expected);
        }
    }

    #[test]
    fn decode_stored_session_rejects_polluted_revoked_fixed_redacted() {
        for raw in [2i64, -1, 255, i64::MAX] {
            let (digest, owner_id, process_epoch, created, _) = valid_stored_session_tuple();
            let err = decode_stored_session(digest, owner_id, process_epoch, created, raw)
                .expect_err("polluted revoked must not decode, never be treated as missing");
            assert_fixed_redacted_config(&err, &[]);
            assert!(
                matches!(
                    &err,
                    ControllerError::Config(m) if m == "identity auth read sessions.revoked"
                ),
                "must be the exact fixed sessions.revoked code, got {err:?}"
            );
        }
    }

    #[test]
    fn decode_stored_session_rejects_wrong_binary_lengths_fixed_redacted() {
        for digest_len in [0usize, 1, 16, 31, 33, 64] {
            let (_, owner_id, process_epoch, created, revoked) = valid_stored_session_tuple();
            let digest = vec![0xA5u8; digest_len];
            let err = decode_stored_session(digest, owner_id, process_epoch, created, revoked)
                .expect_err("wrong digest length must not decode");
            assert!(
                matches!(
                    &err,
                    ControllerError::Config(m)
                        if m == "identity auth read sessions.token_hash"
                ),
                "digest length {digest_len} must give the exact fixed code, got {err:?}"
            );
        }
        for owner_len in [0usize, 15, 17, 32] {
            let (digest, _, process_epoch, created, revoked) = valid_stored_session_tuple();
            let owner_id = vec![0x11u8; owner_len];
            let err = decode_stored_session(digest, owner_id, process_epoch, created, revoked)
                .expect_err("wrong owner length must not decode");
            assert!(
                matches!(
                    &err,
                    ControllerError::Config(m) if m == "identity auth read sessions.user_id"
                ),
                "owner length {owner_len} must give the exact fixed code, got {err:?}"
            );
        }
        for epoch_len in [0usize, 15, 17, 32] {
            let (digest, owner_id, _, created, revoked) = valid_stored_session_tuple();
            let process_epoch = vec![0x22u8; epoch_len];
            let err = decode_stored_session(digest, owner_id, process_epoch, created, revoked)
                .expect_err("wrong epoch length must not decode");
            assert!(
                matches!(
                    &err,
                    ControllerError::Config(m)
                        if m == "identity auth read sessions.process_epoch"
                ),
                "epoch length {epoch_len} must give the exact fixed code, got {err:?}"
            );
        }
    }

    #[test]
    fn decode_stored_session_created_time_is_utc_with_microseconds() {
        let (digest, owner_id, process_epoch, created, revoked) = valid_stored_session_tuple();
        let s = decode_stored_session(digest, owner_id, process_epoch, created, revoked)
            .expect("clean row must decode");
        // Independent construction path: the `DATETIME(6)` value is
        // interpreted as UTC and its microsecond precision must survive.
        let expected = chrono::DateTime::parse_from_rfc3339("2026-10-05T12:34:56.789123+00:00")
            .expect("fixed fixture")
            .with_timezone(&chrono::Utc);
        assert_eq!(s.created_time, expected);
    }

    // -- Task 3 (read): capacity guard ------------------------------------------

    #[test]
    fn enforce_list_capacity_8192_ok_8193_resource_exhausted() {
        let empty: Vec<u8> = vec![];
        assert!(
            enforce_list_capacity(&empty).is_ok(),
            "zero rows are within the cap"
        );
        let at_cap: Vec<u8> = vec![0u8; 8192];
        assert!(
            enforce_list_capacity(&at_cap).is_ok(),
            "exactly 8192 rows are within the hard cap"
        );
        let over: Vec<u8> = vec![0u8; 8193];
        assert!(
            matches!(
                enforce_list_capacity(&over),
                Err(ControllerError::ResourceExhausted)
            ),
            "the 8193rd fetched row must be the fixed ResourceExhausted, never a truncated list"
        );
    }

    // -- Task 3 (write): decisions and SQL gates --------------------------------

    #[test]
    fn selected_target_decision_strict() {
        let e = [3u8; 16];
        assert!(matches!(
            decide_selected_target([1; 16], [1; 16], e, e, 0),
            Ok(true)
        )); // 活体目标 -> 撤销+审计
        assert!(matches!(
            decide_selected_target([1; 16], [1; 16], e, e, 1),
            Ok(false)
        )); // 已撤 -> 404 同形
        for revoked in [0i64, 1] {
            assert!(matches!(
                decide_selected_target([1; 16], [2; 16], e, e, revoked),
                Ok(false)
            )); // 他人 -> 404 同形
            assert!(matches!(
                decide_selected_target([1; 16], [1; 16], [9; 16], e, revoked),
                Ok(false)
            )); // 旧 epoch -> 404 同形
        }
        for raw in [2i64, -1] {
            let err = decide_selected_target([1; 16], [2; 16], e, e, raw)
                .expect_err("污染 revoked 必须先于 owner 判定");
            assert_fixed_redacted_config(&err, &[]);
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth read sessions.revoked")
            );
        }
    }

    #[test]
    fn actor_session_row_verification_strict() {
        let e = [3u8; 16];
        assert!(verify_actor_session_row([1; 16], [1; 16], e, e, 0).is_ok()); // 干净 -> 通过
        for (owner, epoch, revoked) in [([2; 16], e, 0i64), ([1; 16], [9; 16], 0), ([1; 16], e, 1)]
        {
            let err = verify_actor_session_row(owner, [1; 16], epoch, e, revoked)
                .expect_err("漂移的当前行必须 fail closed");
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth write sessions.actor")
            );
        }
        for raw in [2i64, -1] {
            let err = verify_actor_session_row([1; 16], [2; 16], e, e, raw)
                .expect_err("污染先于 owner 判定");
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth read sessions.revoked")
            );
        }
    }

    #[test]
    fn bulk_row_decision_strict() {
        let e = [3u8; 16];
        let (actor, other) = ([7u8; 32], [8u8; 32]);
        assert!(matches!(
            decide_bulk_row(actor, actor, e, e, 0),
            Ok(BulkRowAction::ActorLive)
        )); // 当前行保留
        assert!(matches!(
            decide_bulk_row(other, actor, e, e, 0),
            Ok(BulkRowAction::Revoke)
        )); // 他会话 -> 撤销
        assert!(matches!(
            decide_bulk_row(other, actor, e, e, 1),
            Ok(BulkRowAction::AlreadyRevoked)
        )); // 已撤 -> 跳过
        for (digest, revoked) in [(actor, 1i64), (other, 0), (actor, 0)] {
            let err = decide_bulk_row(digest, actor, [9; 16], e, revoked)
                .expect_err("epoch 漂移必须 fail closed");
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth write sessions.epoch")
            );
        }
        for raw in [2i64, -1] {
            let err = decide_bulk_row(other, actor, e, e, raw).expect_err("污染先于一切状态判定");
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "identity auth read sessions.revoked")
            );
        }
        let err = decide_bulk_row(actor, actor, e, e, 1).expect_err("当前行已撤必须 fail closed");
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity auth write sessions.actor")
        );
    }

    #[test]
    fn task3_write_sql_gate_fixed_strings() {
        let compact = |s: &str| s.replace(char::is_whitespace, "").to_ascii_uppercase();
        let scan = compact(LOCK_USER_SESSIONS_SQL);
        assert!(scan.starts_with("SELECT") && !LOCK_USER_SESSIONS_SQL.contains(';'));
        assert_eq!(LOCK_USER_SESSIONS_SQL.matches('?').count(), 2);
        assert!(
            scan.contains("CAST(REVOKEDASSIGNED)ASREVOKED")
                && scan.contains(
                    "WHEREUSER_ID=?ANDPROCESS_EPOCH=?ORDERBYTOKEN_HASHLIMIT8193FORUPDATE"
                )
                && !scan.contains("REVOKED=FALSE")
        );
        let bulk = compact(REVOKE_OTHER_SESSIONS_SQL);
        assert!(bulk.starts_with("UPDATESESSIONS") && !REVOKE_OTHER_SESSIONS_SQL.contains(';'));
        assert_eq!(REVOKE_OTHER_SESSIONS_SQL.matches('?').count(), 3);
        assert!(bulk.contains(
            "SETREVOKED=TRUEWHEREUSER_ID=?ANDPROCESS_EPOCH=?ANDTOKEN_HASH<>?ANDREVOKED=FALSE"
        ));
    }

    // -- trait boundary --------------------------------------------------------

    // No pool fixture is constructed in this test module: every write
    // method (`insert_session`, `record_login_failure`, `change_password`,
    // `revoke_session`, `revoke_selected_session`, `revoke_other_sessions`)
    // is a real transaction and must never be awaited against a live or
    // loopback pool here, and the read-only `list_user_sessions` is
    // covered the same way (pure decode/capacity helpers plus the fixed
    // SQL gate above). Their driver-layer behavior is verified by
    // compilation only; live-database acceptance (real MySQL/TiDB
    // physical lock order, atomicity, rollback and physical decode) waits
    // an ignored test on a confirmed isolated target with genuine backup
    // approval and is NOT claimed by these offline tests.
}
