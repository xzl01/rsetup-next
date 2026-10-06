# Controller auth logout: two serial TDD slices (2026-10-03)

**Goal:** `POST /auth/logout` ultimately revokes exactly the authenticated session, audits actual revocation once, and does not claim success from a stale discovery read.
**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` §§3/5; `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md` §§2.1/2.3/4. Existing `Session{user,digest}`/`SessionClock.epoch` and SQLx `lock_integrity_guard` must be reused.
**Global:** Rust 1.85, SQLx 0.8.6, offline locked; no real DB/secret/network in these source tasks, no production router until all six repository methods are implemented/reviewed. `authz_epoch` is NOT incremented by a mere logout (no account/grant change); wrong call costs a needless global cache invalidation, omission for an actual account change is forbidden (change_password handles it separately). If future spec explicitly includes session-only revocations in authz_epoch, scope and test that change as a separate task; never silently change this boundary. Full DB transaction/lock acceptance waits confirmed isolated MySQL/TiDB targets and original backup/migration gates. Never fabricate a live waiter edge.

## Task 4a: Pass the authenticated snapshot to the repository

**Files:** `crates/rsetup-controller/src/auth/service.rs` (trait, `AuthService::logout`, FakeRepo and tests); `crates/rsetup-controller/src/auth/sqlx_repo.rs` (only update the still-failing `revoke_session` trait stub signature and its fixed-reject test). No other source.

**Produces:** `IdentityRepository::revoke_session(&self, session: &Session, epoch:[u8;16]) -> impl Future<Output=Result<(),ControllerError>> + Send`. Add trait doc: strictly recheck stored owner/process epoch/revoked under guard→users→sessions locks; missing/already revoked is idempotent for the same owner; no raw token crosses the boundary. `AuthService::logout(&Session)` calls it with `self.clock.epoch`, then removes memory clock entry **only after** repository success. No raw cookie/token crosses this interface.

- [ ] Start with a compiling signature adaptation: FakeRepo uses its former digest-only behavior via `session.digest`, ignores `session.user` and `epoch` intentionally; SQLx remains `Err(writes_not_ready())`. Update the single existing direct FakeRepo call and SQLx fixed-reject test to the new signature. This is a stub, not GREEN.
- [ ] Before implementation, add real tests in service.rs test module (fixture `auth_fixture()` already exists):
  ```rust
  #[tokio::test] async fn wrong_owner_cannot_revoke_valid_session() {
      let (svc, repo)=auth_fixture(); let login=svc.login("alice","old password").await.unwrap();
      let mut forged=login.session.clone(); forged.user.id=[8;16];
      assert!(matches!(repo.revoke_session(&forged,svc.clock.epoch).await,Err(ControllerError::InvalidArgument)));
      assert!(svc.authenticate(&login.raw_token).await.is_ok());
  }
  #[tokio::test] async fn wrong_process_epoch_cannot_revoke_valid_session() {
      let (svc, repo)=auth_fixture(); let login=svc.login("alice","old password").await.unwrap();
      let wrong=[0;16]; assert_ne!(wrong,svc.clock.epoch);
      assert!(matches!(repo.revoke_session(&login.session,wrong).await,Err(ControllerError::InvalidArgument)));
      assert!(svc.authenticate(&login.raw_token).await.is_ok());
  }
  ```
  A deliberately digest-only FakeRepo passes through and revokes the row, so both new tests fail on **assertions** rather than types. Synthetic credentials only. Existing logout tests remain GREEN.
- [ ] GREEN: FakeRepo under one state mutex checks stored digest/user id/process epoch, returns fixed `InvalidArgument` on owner/epoch mismatch; a missing or already revoked matching session is idempotent `Ok(())` with no other row changed. Existing `AuthService::logout` performs `clock.remove` after `?`. Keep change_password/login behavior and all existing tests unchanged.
- [ ] Run `CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo +1.85.0 test --offline --locked -p rsetup-controller --lib auth::service::`, then all nonignored package tests, fmt and Clippy. Independent task review must verify trait call sites (including SQLx stub) and no raw token logging. No real DB claim.

## Task 4b: Implement the SQLx logout transaction (after 4a review)

**Files:** only `crates/rsetup-controller/src/auth/sqlx_repo.rs` and its module tests. Consume `revoke_session(&Session, epoch)` from 4a; other five repository methods frozen.

**Lock order and decision:** begin→`lock_integrity_guard`→users by `session.user.id FOR UPDATE` (strict row/boolean decoding; do **not** require old hash/revision: a concurrent password change must not prevent cleanup)→sessions by `session.digest FOR UPDATE`, compare stored `user_id`/`process_epoch` to the supplied authenticated session/epoch; `CAST(revoked AS SIGNED)` must decode exactly 0/1. Missing/already revoked session is idempotent `Ok(())` **without** audit when the user row exists (also if that user is now inactive). A live session with an inactive current user is an integrity violation and fails closed; it cannot be attributed to a valid user actor. Wrong owner/epoch or corrupt boolean likewise fails closed. Missing `users` row after the guard is fixed `InvalidArgument` (the spec preserves user rows, so this is corruption); do not report logout complete. For a live matching session and active user, `UPDATE sessions SET revoked=TRUE WHERE token_hash=? AND user_id=? AND process_epoch=? AND revoked=FALSE`, require `rows_affected==1`; a different count is a fixed redacted `Config("identity auth write sessions.revoke")` and rollback, never `Ok`/success audit. Insert fixed `auth.logout` (`actor_kind=user`, actor_user_id/target user id only, params_redacted='{}', outcome=success) in the same transaction; commit. No `schema_meta.authz_epoch` bump on logout.

**Compiling RED stub and behavioral tests:** add `decide_logout(owner, expected_owner, stored_epoch, expected_epoch, raw_revoked)->Result<bool,ControllerError>` returning fixed `InvalidArgument` and `logout_audit(user_id)->AuditInsert` returning a fully populated wrong projection. Test matching unrevoked→`Ok(true)` (RED), matching revoked→`Ok(false)` (RED), wrong owner/epoch→InvalidArgument, **wrong owner + already revoked**→InvalidArgument (owner check precedes idempotence), raw2/-1→fixed redacted Config (strict bool first), fixed event projection fields (`auth.logout`, hex ID, `{}`, no credential). SQL constants for users/session lock/update may be correct in stub; their structure tests are GREEN-by-construction, **not** TDD RED. Then minimal transaction code and tests GREEN. Update the `sqlx_repo.rs` module docs: `revoke_session` is no longer a fixed reject; the six-method production repository can be wired only after independent code review. Do NOT write a test that awaits real methods on `offline_pool()` (it opens a loopback socket).

**Verification boundary:** Rust1.85 offline locked targeted + full nonignored package + fmt/Clippy; review source lock sequence and fixed audit/error paths. A live MySQL/TiDB ignored test later must verify successful revocation+audit commit, idempotent no extra audit, wrong owner/epoch rollback and concurrent admin deactivation; pure tests/compile do **not** prove DB atomicity or actual lock wait. Only after Task4b and its review may the HTTP router's production wiring gate be considered.
