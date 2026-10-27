use super::*;
use std::time::Instant;

impl<R: IdentityRepository> AuthService<R> {
    /// Revoke the session identified by its public alias `id` for the
    /// authenticated `actor`. Returns `Ok(true)` when the actor's own
    /// session was revoked (plain logout shape) and `Ok(false)` when another
    /// session was revoked.
    pub async fn revoke_by_alias(
        &self,
        actor: &Session,
        id: &str,
    ) -> Result<bool, ControllerError> {
        // Guards before any query: actor qualification and this process
        // clock liveness of the actor digest.
        if !actor.user.active {
            return Err(ControllerError::InvalidArgument);
        }
        if actor.user.must_change_password {
            return Err(ControllerError::PermissionDenied);
        }
        let now = Instant::now();
        if !self.clock.is_live(&actor.digest, now) {
            return Err(ControllerError::InvalidArgument);
        }
        // Public alias format: exactly 64 lowercase hex.
        if id.len() != 64 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(ControllerError::InvalidArgument);
        }
        // Own alias: reuse the existing logout (repo revalidation + clock
        // removal) and report self revocation.
        if self.alias_key.matches(&actor.digest, id) {
            self.logout(actor).await?;
            return Ok(true);
        }
        // Another session: read owner/current-epoch rows through the Task2a
        // trait, enforce the 8192 cap and the fixed pollution errors, then
        // match keyed aliases without leaking digests.
        let rows = self
            .repo
            .list_user_sessions(actor, self.clock.epoch)
            .await?;
        if rows.len() > 8192 {
            return Err(ControllerError::ResourceExhausted);
        }
        // Validate every row's owner/epoch before selecting a target,
        // matching list_sessions' full row validation: a polluted row after
        // the target must still fail closed, not be skipped by the match.
        for row in &rows {
            if row.owner_id != actor.user.id || row.process_epoch != self.clock.epoch {
                return Err(ControllerError::Config("polluted session row".into()));
            }
        }
        let mut target: Option<[u8; 32]> = None;
        for row in &rows {
            if row.revoked || !self.clock.is_live(&row.digest, now) {
                continue;
            }
            if self.alias_key.matches(&row.digest, id) {
                target = Some(row.digest);
                break;
            }
        }
        // Cross-user, not found and revoked rows all collapse to NotFound.
        let target = target.ok_or(ControllerError::NotFound)?;
        let revoked = self
            .repo
            .revoke_selected_session(actor, self.clock.epoch, target)
            .await?;
        if !revoked {
            return Err(ControllerError::NotFound);
        }
        // Remove the target clock entry only after confirmed repository
        // Ok(true); any propagated Database/Config error preserved both
        // clocks and never claimed success.
        self.clock.remove(&target);
        Ok(false)
    }

    /// Revoke every other live session of the authenticated `actor` and
    /// return the actual number newly revoked; the current session stays
    /// live.
    pub async fn revoke_others(&self, actor: &Session) -> Result<u64, ControllerError> {
        if !actor.user.active {
            return Err(ControllerError::InvalidArgument);
        }
        if actor.user.must_change_password {
            return Err(ControllerError::PermissionDenied);
        }
        let now = Instant::now();
        if !self.clock.is_live(&actor.digest, now) {
            return Err(ControllerError::InvalidArgument);
        }
        let (count, digests) = self
            .repo
            .revoke_other_sessions(actor, self.clock.epoch)
            .await?;
        // Strict validation before any clock removal; mismatch is a fixed
        // redacted Config and preserves all clocks.
        if digests.len() as u64 != count {
            return Err(ControllerError::Config(
                "bulk revocation count mismatch".into(),
            ));
        }
        if digests.len() > 8192 {
            return Err(ControllerError::Config(
                "bulk revocation capacity exceeded".into(),
            ));
        }
        let mut unique = std::collections::HashSet::with_capacity(digests.len());
        for digest in &digests {
            if !unique.insert(*digest) {
                return Err(ControllerError::Config(
                    "bulk revocation duplicate digest".into(),
                ));
            }
        }
        if unique.contains(&actor.digest) {
            return Err(ControllerError::Config(
                "bulk revocation includes current digest".into(),
            ));
        }
        // Confirmed: remove only the returned other digests. authz_epoch is
        // not bumped (permissions unchanged); idle/absolute of the current
        // entry are untouched.
        for digest in &digests {
            self.clock.remove(digest);
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::{
        FakeRepo, RepoFailKind, SelectedDrift, SelectedDriftKind, auth_fixture,
    };
    use super::*;
    use crate::auth::session::IDLE;
    use std::sync::{Mutex, atomic::Ordering};

    /// Test-only wrapper: every `IdentityRepository` method delegates to the
    /// inner `FakeRepo`, but `list_user_sessions` appends raw `extra_rows`
    /// after the real rows, bypassing the fake's same-user/same-epoch filter
    /// so the service's own row validation is what gets exercised.
    struct PollutedListRepo {
        inner: Arc<FakeRepo>,
        extra_rows: Mutex<Vec<StoredSession>>,
    }
    impl IdentityRepository for PollutedListRepo {
        async fn find_user(&self, username: &str) -> Result<Option<IdentityUser>, ControllerError> {
            self.inner.find_user(username).await
        }
        async fn insert_session(
            &self,
            user: &IdentityUser,
            digest: [u8; 32],
            epoch: [u8; 16],
        ) -> Result<(), ControllerError> {
            self.inner.insert_session(user, digest, epoch).await
        }
        async fn find_session(
            &self,
            digest: [u8; 32],
            epoch: [u8; 16],
        ) -> Result<Option<IdentityUser>, ControllerError> {
            self.inner.find_session(digest, epoch).await
        }
        async fn change_password(
            &self,
            session: &Session,
            epoch: [u8; 16],
            next_hash: &str,
        ) -> Result<(), ControllerError> {
            self.inner.change_password(session, epoch, next_hash).await
        }
        async fn revoke_session(
            &self,
            session: &Session,
            epoch: [u8; 16],
        ) -> Result<(), ControllerError> {
            self.inner.revoke_session(session, epoch).await
        }
        async fn record_login_failure(
            &self,
            user_id: Option<[u8; 16]>,
        ) -> Result<(), ControllerError> {
            self.inner.record_login_failure(user_id).await
        }
        async fn list_user_sessions(
            &self,
            actor: &Session,
            epoch: [u8; 16],
        ) -> Result<Vec<StoredSession>, ControllerError> {
            let mut rows: Vec<StoredSession> = self.inner.list_user_sessions(actor, epoch).await?;
            rows.extend(self.extra_rows.lock().unwrap().iter().cloned());
            Ok(rows)
        }
        async fn revoke_selected_session(
            &self,
            actor: &Session,
            epoch: [u8; 16],
            target_digest: [u8; 32],
        ) -> Result<bool, ControllerError> {
            self.inner
                .revoke_selected_session(actor, epoch, target_digest)
                .await
        }
        async fn revoke_other_sessions(
            &self,
            actor: &Session,
            epoch: [u8; 16],
        ) -> Result<(u64, Vec<[u8; 32]>), ControllerError> {
            self.inner.revoke_other_sessions(actor, epoch).await
        }
    }
    /// Build the real service over a `PollutedListRepo` that appends
    /// `polluted` after the real list rows, sharing `base`'s clock and alias
    /// key.
    fn polluted_row_service(
        base: AuthService<FakeRepo>,
        repo: Arc<FakeRepo>,
        polluted: StoredSession,
    ) -> AuthService<PollutedListRepo> {
        let wrapper = Arc::new(PollutedListRepo {
            inner: repo,
            extra_rows: Mutex::new(vec![polluted]),
        });
        AuthService {
            repo: wrapper,
            clock: base.clock,
            alias_key: base.alias_key,
            hasher: PasswordHasher::new(),
            dummy_hash: base.dummy_hash.clone(),
        }
    }
    /// Assert that no audit event projection leaks a raw token, digest,
    /// password or public alias (fixed redacted names only).
    fn assert_audit_redacted(audit: &[String], secrets: &[String]) {
        for event in audit {
            for secret in secrets {
                assert!(
                    !event.contains(secret),
                    "audit projection must not leak a secret"
                );
            }
        }
    }

    // ---------- Task 2b: target/bulk revocation (FakeRepo, internal only) ----------

    #[tokio::test]
    async fn revoke_by_alias_other_session_revokes_only_target_and_counts_one_call() {
        let (svc, repo) = auth_fixture();
        let other = svc.login("alice", "old password").await.unwrap();
        let login = svc.login("alice", "old password").await.unwrap();
        let other_alias = svc.alias_key.alias(&other.session.digest);

        let result = svc.revoke_by_alias(&login.session, &other_alias).await;
        assert!(
            matches!(result, Ok(false)),
            "revoking another session must return false"
        );
        assert!(
            svc.authenticate(&other.raw_token).await.is_err(),
            "revoked other session must no longer authenticate (401 shape)"
        );
        assert!(
            svc.authenticate(&login.raw_token).await.is_ok(),
            "current session must stay live"
        );
        assert_eq!(
            repo.selected_calls.load(Ordering::SeqCst),
            1,
            "exactly one repository target revoke call"
        );
        let state = repo.state.lock().unwrap().1.clone();
        let target = state
            .iter()
            .find(|(digest, _, _, _, _)| *digest == other.session.digest)
            .unwrap();
        assert!(target.1, "target row must be revoked");
        let current = state
            .iter()
            .find(|(digest, _, _, _, _)| *digest == login.session.digest)
            .unwrap();
        assert!(!current.1, "current row must remain unrevoked");
        let audit = repo.audit.lock().unwrap().clone();
        assert_eq!(audit, vec!["identity.session_revoke_selected".to_string()]);
        assert_audit_redacted(
            &audit,
            &[
                other.raw_token.clone(),
                hex::encode(other.session.digest),
                other_alias.clone(),
                "old password".to_string(),
            ],
        );
    }

    #[tokio::test]
    async fn revoke_by_alias_self_session_logs_out_current_and_leaves_other_live() {
        let (svc, repo) = auth_fixture();
        let other = svc.login("alice", "old password").await.unwrap();
        let login = svc.login("alice", "old password").await.unwrap();
        let self_alias = svc.alias_key.alias(&login.session.digest);

        let result = svc.revoke_by_alias(&login.session, &self_alias).await;
        assert!(
            matches!(result, Ok(true)),
            "revoking the current session must return true"
        );
        assert!(
            svc.authenticate(&login.raw_token).await.is_err(),
            "current session must no longer authenticate after self revoke"
        );
        assert!(
            !svc.is_live_session(&login.session.digest),
            "no phantom clock entry may remain for the revoked current session"
        );
        assert!(
            svc.authenticate(&other.raw_token).await.is_ok(),
            "unrelated other session must remain live"
        );
        let state = repo.state.lock().unwrap().1.clone();
        let current = state
            .iter()
            .find(|(digest, _, _, _, _)| *digest == login.session.digest)
            .unwrap();
        assert!(
            current.1,
            "current row must be revoked via the existing logout path"
        );
        let other_row = state
            .iter()
            .find(|(digest, _, _, _, _)| *digest == other.session.digest)
            .unwrap();
        assert!(!other_row.1, "other row must remain unrevoked");
        assert_eq!(
            repo.selected_calls.load(Ordering::SeqCst),
            0,
            "self path reuses logout, not the target revoke"
        );
    }

    #[tokio::test]
    async fn revoke_by_alias_unknown_foreign_expired_and_revoked_aliases_are_indistinguishable_not_found()
     {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let expired = svc.login("alice", "old password").await.unwrap();
        let revoked = svc.login("alice", "old password").await.unwrap();
        let now = Instant::now();
        svc.clock.insert(expired.session.digest, now - IDLE);
        repo.revoke_session(&revoked.session, svc.clock.epoch)
            .await
            .unwrap();

        let foreign_digest = token_digest(b"foreign-user-raw-token");
        let foreign_alias = svc.alias_key.alias(&foreign_digest);
        repo.state.lock().unwrap().1.push((
            foreign_digest,
            false,
            svc.clock.epoch,
            [2; 16],
            chrono::Utc::now(),
        ));
        let expired_alias = svc.alias_key.alias(&expired.session.digest);
        let revoked_alias = svc.alias_key.alias(&revoked.session.digest);
        let unknown_alias = "0".repeat(64);

        for alias in [
            foreign_alias.as_str(),
            expired_alias.as_str(),
            revoked_alias.as_str(),
            unknown_alias.as_str(),
        ] {
            assert!(
                matches!(
                    svc.revoke_by_alias(&login.session, alias).await,
                    Err(ControllerError::NotFound)
                ),
                "foreign/expired/revoked/unknown aliases must be the same NotFound"
            );
        }
        assert!(
            svc.is_live_session(&revoked.session.digest),
            "a stale clock entry for a DB-revoked row must be preserved on NotFound"
        );
        assert_eq!(
            repo.selected_calls.load(Ordering::SeqCst),
            0,
            "not-found must not call the target revoke"
        );
        let state = repo.state.lock().unwrap().1.clone();
        for digest in [login.session.digest, expired.session.digest, foreign_digest] {
            let row = state.iter().find(|(d, _, _, _, _)| *d == digest).unwrap();
            assert!(!row.1, "unmatched rows must remain unrevoked");
        }
        assert!(repo.audit.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_by_alias_malformed_alias_is_invalid_argument_without_repo_change() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let valid_alias = svc.alias_key.alias(&login.session.digest);
        let before = repo.auth_state();
        let too_long = format!("{valid_alias}0");
        let upper = valid_alias.to_ascii_uppercase();
        let non_hex = format!("g{}", &valid_alias[1..]);

        for id in [&valid_alias[..63], &too_long, &upper, &non_hex] {
            assert!(
                matches!(
                    svc.revoke_by_alias(&login.session, id).await,
                    Err(ControllerError::InvalidArgument)
                ),
                "malformed alias must be InvalidArgument (400 shape)"
            );
        }
        assert_eq!(
            repo.auth_state(),
            before,
            "malformed must not touch the repository"
        );
        assert_eq!(repo.selected_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn revoke_by_alias_denies_unqualified_actor_before_any_repository_change() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let other = svc.login("alice", "old password").await.unwrap();
        let other_alias = svc.alias_key.alias(&other.session.digest);
        let before = repo.auth_state();

        let mut force_password = login.session.clone();
        force_password.user.must_change_password = true;
        assert!(
            matches!(
                svc.revoke_by_alias(&force_password, &other_alias).await,
                Err(ControllerError::PermissionDenied)
            ),
            "forced password change must be denied (403 shape)"
        );

        let mut inactive = login.session.clone();
        inactive.user.active = false;
        assert!(
            matches!(
                svc.revoke_by_alias(&inactive, &other_alias).await,
                Err(ControllerError::InvalidArgument)
            ),
            "inactive actor must be denied"
        );

        let dead_now = Instant::now();
        svc.clock.insert(login.session.digest, dead_now - IDLE);
        assert!(
            matches!(
                svc.revoke_by_alias(&login.session, &other_alias).await,
                Err(ControllerError::InvalidArgument)
            ),
            "actor whose clock entry is dead must be denied"
        );

        assert_eq!(
            repo.auth_state(),
            before,
            "denials must happen before any repository change"
        );
        assert_eq!(repo.selected_calls.load(Ordering::SeqCst), 0);
        assert!(repo.audit.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoke_by_alias_forged_owner_or_epoch_fails_closed() {
        // Forged owner on the target row.
        {
            let (svc, repo) = auth_fixture();
            let login = svc.login("alice", "old password").await.unwrap();
            let target = svc.login("alice", "old password").await.unwrap();
            let target_alias = svc.alias_key.alias(&target.session.digest);
            repo.state
                .lock()
                .unwrap()
                .1
                .iter_mut()
                .find(|(digest, _, _, _, _)| *digest == target.session.digest)
                .unwrap()
                .3 = [2; 16];
            assert!(
                matches!(
                    svc.revoke_by_alias(&login.session, &target_alias).await,
                    Err(ControllerError::NotFound)
                ),
                "forged owner must fail closed as NotFound, never success"
            );
            assert!(
                svc.is_live_session(&target.session.digest),
                "fail-closed must preserve the target clock entry"
            );
        }
        // Forged process epoch on the target row.
        {
            let (svc, repo) = auth_fixture();
            let login = svc.login("alice", "old password").await.unwrap();
            let target = svc.login("alice", "old password").await.unwrap();
            let target_alias = svc.alias_key.alias(&target.session.digest);
            repo.state
                .lock()
                .unwrap()
                .1
                .iter_mut()
                .find(|(digest, _, _, _, _)| *digest == target.session.digest)
                .unwrap()
                .2 = [0xee; 16];
            assert!(
                matches!(
                    svc.revoke_by_alias(&login.session, &target_alias).await,
                    Err(ControllerError::NotFound)
                ),
                "forged epoch must fail closed as NotFound, never success"
            );
            assert!(svc.is_live_session(&target.session.digest));
        }
    }

    /// I-2: `FakeRepo::list_user_sessions` prefilters by owner/epoch, so a
    /// forged owner/epoch row never reaches the service and the repository
    /// revalidation branch inside `revoke_selected_session`
    /// (`owner/epoch mismatch or revoked -> Ok(false)`) was unreachable from
    /// a real `revoke_by_alias` call. The test-only seam applies a
    /// concurrent target-row drift after the clean list read and before the
    /// in-lock revalidation check: the real service must match the target on
    /// the first list, then fold the repository `Ok(false)` into the fixed
    /// NotFound, preserve both clocks, claim no success audit, and make
    /// exactly one selected call.
    #[tokio::test]
    async fn revoke_by_alias_concurrent_drift_before_revalidation_is_fixed_not_found() {
        for kind in [
            SelectedDriftKind::Owner,
            SelectedDriftKind::Epoch,
            SelectedDriftKind::Revoked,
        ] {
            let (svc, repo) = auth_fixture();
            let login = svc.login("alice", "old password").await.unwrap();
            let target = svc.login("alice", "old password").await.unwrap();
            let target_alias = svc.alias_key.alias(&target.session.digest);
            let before = repo.auth_state();
            *repo.selected_drift.lock().unwrap() = Some(SelectedDrift {
                digest: target.session.digest,
                kind,
            });

            let result = svc.revoke_by_alias(&login.session, &target_alias).await;
            assert!(
                matches!(result, Err(ControllerError::NotFound)),
                "drifted target must fail closed as fixed NotFound, never success"
            );
            assert!(
                svc.is_live_session(&login.session.digest),
                "fail-closed must preserve the current clock entry"
            );
            assert!(
                svc.is_live_session(&target.session.digest),
                "fail-closed must preserve the target clock entry"
            );
            assert_eq!(
                repo.selected_calls.load(Ordering::SeqCst),
                1,
                "the list must match first and the repository recheck must run exactly once"
            );
            assert!(
                repo.audit.lock().unwrap().is_empty(),
                "a recheck-failed revoke must claim no success audit"
            );
            let state = repo.state.lock().unwrap().1.clone();
            let current_row = state
                .iter()
                .find(|(digest, _, _, _, _)| *digest == login.session.digest)
                .unwrap();
            assert!(!current_row.1, "the current row must remain unrevoked");
            let target_row = state
                .iter()
                .find(|(digest, _, _, _, _)| *digest == target.session.digest)
                .unwrap();
            match kind {
                SelectedDriftKind::Owner => {
                    assert_eq!(
                        target_row.3, [0xff; 16],
                        "the owner drift must land in storage"
                    );
                    assert!(
                        !target_row.1,
                        "the recheck must fail before marking the row revoked"
                    );
                    assert_eq!(
                        repo.auth_state(),
                        before,
                        "an owner drift must not count as a revocation"
                    );
                }
                SelectedDriftKind::Epoch => {
                    assert_eq!(
                        target_row.2, [0xee; 16],
                        "the epoch drift must land in storage"
                    );
                    assert!(
                        !target_row.1,
                        "the recheck must fail before marking the row revoked"
                    );
                    assert_eq!(
                        repo.auth_state(),
                        before,
                        "an epoch drift must not count as a revocation"
                    );
                }
                SelectedDriftKind::Revoked => {
                    assert!(
                        target_row.1,
                        "the concurrent revocation must land in storage"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn revoke_by_alias_polluted_row_after_target_fails_closed() {
        // A polluted row AFTER the valid target row: the target matches
        // first, so only a full row validation (as in list_sessions) can
        // still fail closed before the target revoke is selected.
        // Polluted owner, correct epoch.
        {
            let (base, repo) = auth_fixture();
            let login = base.login("alice", "old password").await.unwrap();
            let target = base.login("alice", "old password").await.unwrap();
            let target_alias = base.alias_key.alias(&target.session.digest);
            let before = repo.auth_state();
            let epoch = base.clock.epoch;
            let svc = polluted_row_service(
                base,
                repo.clone(),
                StoredSession {
                    digest: token_digest(b"polluted-owner-row"),
                    owner_id: [0xff; 16], // wrong owner
                    process_epoch: epoch,
                    created_time: chrono::Utc::now(),
                    revoked: false,
                },
            );
            let result = svc.revoke_by_alias(&login.session, &target_alias).await;
            assert!(
                matches!(
                    result,
                    Err(ControllerError::Config(ref msg)) if msg == "polluted session row"
                ),
                "a polluted owner row after the target must fail closed as fixed Config, never success"
            );
            assert_eq!(
                repo.selected_calls.load(Ordering::SeqCst),
                0,
                "full row validation must run before the target revoke call"
            );
            assert!(
                svc.is_live_session(&target.session.digest),
                "fail-closed must preserve the target clock entry"
            );
            assert!(
                svc.is_live_session(&login.session.digest),
                "fail-closed must preserve the current clock entry"
            );
            assert_eq!(
                repo.auth_state(),
                before,
                "fail-closed must not change repository state"
            );
            assert!(repo.audit.lock().unwrap().is_empty());
        }
        // Polluted process epoch, correct owner.
        {
            let (base, repo) = auth_fixture();
            let login = base.login("alice", "old password").await.unwrap();
            let target = base.login("alice", "old password").await.unwrap();
            let target_alias = base.alias_key.alias(&target.session.digest);
            let before = repo.auth_state();
            let owner = login.session.user.id;
            let svc = polluted_row_service(
                base,
                repo.clone(),
                StoredSession {
                    digest: token_digest(b"polluted-epoch-row"),
                    owner_id: owner,
                    process_epoch: [0xee; 16], // wrong process epoch
                    created_time: chrono::Utc::now(),
                    revoked: false,
                },
            );
            let result = svc.revoke_by_alias(&login.session, &target_alias).await;
            assert!(
                matches!(
                    result,
                    Err(ControllerError::Config(ref msg)) if msg == "polluted session row"
                ),
                "a polluted epoch row after the target must fail closed as fixed Config, never success"
            );
            assert_eq!(
                repo.selected_calls.load(Ordering::SeqCst),
                0,
                "full row validation must run before the target revoke call"
            );
            assert!(
                svc.is_live_session(&target.session.digest),
                "fail-closed must preserve the target clock entry"
            );
            assert!(
                svc.is_live_session(&login.session.digest),
                "fail-closed must preserve the current clock entry"
            );
            assert_eq!(
                repo.auth_state(),
                before,
                "fail-closed must not change repository state"
            );
            assert!(repo.audit.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn revoke_by_alias_repo_errors_preserve_every_clock_and_claim_no_success() {
        // Database error.
        {
            let (svc, repo) = auth_fixture();
            let login = svc.login("alice", "old password").await.unwrap();
            let target = svc.login("alice", "old password").await.unwrap();
            let target_alias = svc.alias_key.alias(&target.session.digest);
            let before = repo.auth_state();
            *repo.selected_fail.lock().unwrap() = Some(RepoFailKind::Database);
            let result = svc.revoke_by_alias(&login.session, &target_alias).await;
            assert!(
                matches!(result, Err(ControllerError::Database(_))),
                "database error must propagate, never claimed as success"
            );
            assert_eq!(
                repo.auth_state(),
                before,
                "uncertain error must not change repository state"
            );
            assert!(svc.authenticate(&login.raw_token).await.is_ok());
            assert!(
                svc.authenticate(&target.raw_token).await.is_ok(),
                "target must still authenticate after an uncertain error"
            );
            assert!(svc.is_live_session(&login.session.digest));
            assert!(svc.is_live_session(&target.session.digest));
        }
        // Commit-unknown error.
        {
            let (svc, repo) = auth_fixture();
            let login = svc.login("alice", "old password").await.unwrap();
            let target = svc.login("alice", "old password").await.unwrap();
            let target_alias = svc.alias_key.alias(&target.session.digest);
            let before = repo.auth_state();
            *repo.selected_fail.lock().unwrap() = Some(RepoFailKind::CommitUnknown);
            let result = svc.revoke_by_alias(&login.session, &target_alias).await;
            assert!(
                matches!(
                    result,
                    Err(ControllerError::Config(ref msg)) if msg == "commit unknown"
                ),
                "commit-unknown error must propagate as fixed Config"
            );
            assert_eq!(repo.auth_state(), before);
            assert!(
                svc.authenticate(&target.raw_token).await.is_ok(),
                "target must still authenticate after a commit-unknown error"
            );
            assert!(repo.audit.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn revoke_others_revokes_all_others_keeps_current_and_repeat_returns_zero() {
        let (svc, repo) = auth_fixture();
        let first = svc.login("alice", "old password").await.unwrap();
        let second = svc.login("alice", "old password").await.unwrap();
        let current = svc.login("alice", "old password").await.unwrap();

        let result = svc.revoke_others(&current.session).await;
        assert!(
            matches!(result, Ok(2)),
            "two other sessions must be revoked"
        );
        assert!(svc.authenticate(&first.raw_token).await.is_err());
        assert!(svc.authenticate(&second.raw_token).await.is_err());
        assert!(
            svc.authenticate(&current.raw_token).await.is_ok(),
            "current session must stay live"
        );
        assert!(svc.is_live_session(&current.session.digest));

        let again = svc.revoke_others(&current.session).await;
        assert!(
            matches!(again, Ok(0)),
            "replay must report zero new revocations"
        );
        assert!(svc.authenticate(&current.raw_token).await.is_ok());
        assert_eq!(repo.bulk_calls.load(Ordering::SeqCst), 2);
        let audit = repo.audit.lock().unwrap().clone();
        assert_eq!(
            audit,
            vec!["identity.session_revoke_others".to_string()],
            "a no-op repeat must not be audited"
        );
        assert_audit_redacted(
            &audit,
            &[
                first.raw_token.clone(),
                second.raw_token.clone(),
                hex::encode(first.session.digest),
                hex::encode(second.session.digest),
                svc.alias_key.alias(&first.session.digest),
                svc.alias_key.alias(&second.session.digest),
                "old password".to_string(),
            ],
        );
    }

    #[tokio::test]
    async fn revoke_others_inconsistent_repo_result_is_fixed_config_with_zero_clock_removals() {
        // Count/digest-length mismatch.
        {
            let (svc, repo) = auth_fixture();
            let first = svc.login("alice", "old password").await.unwrap();
            let second = svc.login("alice", "old password").await.unwrap();
            let current = svc.login("alice", "old password").await.unwrap();
            let before = repo.auth_state();
            *repo.bulk_override.lock().unwrap() =
                Some((3, vec![first.session.digest, second.session.digest]));
            assert!(
                matches!(
                    svc.revoke_others(&current.session).await,
                    Err(ControllerError::Config(ref msg))
                        if msg == "bulk revocation count mismatch"
                ),
                "count mismatch must be a fixed redacted Config"
            );
            assert!(svc.is_live_session(&first.session.digest));
            assert!(svc.is_live_session(&second.session.digest));
            assert!(svc.is_live_session(&current.session.digest));
            assert_eq!(repo.auth_state(), before);
        }
        // Duplicate digest in the returned collection.
        {
            let (svc, repo) = auth_fixture();
            let first = svc.login("alice", "old password").await.unwrap();
            let second = svc.login("alice", "old password").await.unwrap();
            let current = svc.login("alice", "old password").await.unwrap();
            let before = repo.auth_state();
            *repo.bulk_override.lock().unwrap() =
                Some((2, vec![first.session.digest, first.session.digest]));
            assert!(
                matches!(
                    svc.revoke_others(&current.session).await,
                    Err(ControllerError::Config(ref msg))
                        if msg == "bulk revocation duplicate digest"
                ),
                "duplicate digest must be a fixed redacted Config"
            );
            assert!(svc.is_live_session(&first.session.digest));
            assert!(svc.is_live_session(&second.session.digest));
            assert!(svc.is_live_session(&current.session.digest));
            assert_eq!(repo.auth_state(), before);
        }
        // Current actor digest in the returned collection.
        {
            let (svc, repo) = auth_fixture();
            let first = svc.login("alice", "old password").await.unwrap();
            let second = svc.login("alice", "old password").await.unwrap();
            let current = svc.login("alice", "old password").await.unwrap();
            let before = repo.auth_state();
            *repo.bulk_override.lock().unwrap() = Some((1, vec![current.session.digest]));
            assert!(
                matches!(
                    svc.revoke_others(&current.session).await,
                    Err(ControllerError::Config(ref msg))
                        if msg == "bulk revocation includes current digest"
                ),
                "current digest in the result must be a fixed redacted Config"
            );
            assert!(svc.is_live_session(&first.session.digest));
            assert!(svc.is_live_session(&second.session.digest));
            assert!(svc.is_live_session(&current.session.digest));
            assert_eq!(repo.auth_state(), before);
        }
        // More than 8192 digests.
        {
            let (svc, repo) = auth_fixture();
            let current = svc.login("alice", "old password").await.unwrap();
            let before = repo.auth_state();
            let mut digests = Vec::with_capacity(8193);
            for i in 0..8193u32 {
                let mut d = [0u8; 32];
                d[..4].copy_from_slice(&i.to_be_bytes());
                digests.push(d);
            }
            *repo.bulk_override.lock().unwrap() = Some((8193, digests));
            assert!(
                matches!(
                    svc.revoke_others(&current.session).await,
                    Err(ControllerError::Config(ref msg))
                        if msg == "bulk revocation capacity exceeded"
                ),
                "over-capacity digest collection must be a fixed redacted Config"
            );
            assert!(svc.is_live_session(&current.session.digest));
            assert_eq!(repo.auth_state(), before);
        }
    }

    #[tokio::test]
    async fn revoke_others_retry_after_repository_failure_keeps_current_and_completes() {
        let (svc, repo) = auth_fixture();
        let first = svc.login("alice", "old password").await.unwrap();
        let second = svc.login("alice", "old password").await.unwrap();
        let current = svc.login("alice", "old password").await.unwrap();

        *repo.bulk_fail.lock().unwrap() = Some(RepoFailKind::Database);
        assert!(
            matches!(
                svc.revoke_others(&current.session).await,
                Err(ControllerError::Database(_))
            ),
            "uncertain failure must propagate, never claimed as success"
        );
        assert!(
            svc.authenticate(&current.raw_token).await.is_ok(),
            "current session must survive an uncertain failure"
        );
        assert!(svc.authenticate(&first.raw_token).await.is_ok());
        assert!(svc.authenticate(&second.raw_token).await.is_ok());
        assert!(repo.audit.lock().unwrap().is_empty());

        *repo.bulk_fail.lock().unwrap() = None;
        let retry = svc.revoke_others(&current.session).await;
        assert!(
            matches!(retry, Ok(2)),
            "retry after failure must revoke the two others exactly once"
        );
        assert!(svc.authenticate(&first.raw_token).await.is_err());
        assert!(svc.authenticate(&second.raw_token).await.is_err());
        assert!(
            svc.authenticate(&current.raw_token).await.is_ok(),
            "current session must still be live after the successful retry"
        );
    }

    #[tokio::test]
    async fn revoke_by_alias_8193_rows_is_resource_exhausted_before_target_enumeration() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let now = chrono::Utc::now();
        let mut rows = Vec::with_capacity(8193);
        for i in 0..8192u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_be_bytes());
            rows.push((d, false, svc.clock.epoch, login.session.user.id, now));
        }
        // Plus the actor's own row: 8193 rows total.
        rows.push((
            login.session.digest,
            false,
            svc.clock.epoch,
            login.session.user.id,
            now,
        ));
        let some_alias = svc.alias_key.alias(&rows[0].0);
        repo.state.lock().unwrap().1 = rows;

        let result = svc.revoke_by_alias(&login.session, &some_alias).await;
        assert!(
            matches!(result, Err(ControllerError::ResourceExhausted)),
            "8193 rows must be ResourceExhausted (503 shape)"
        );
        assert_eq!(
            repo.selected_calls.load(Ordering::SeqCst),
            0,
            "capacity must fail before target enumeration"
        );
        assert!(repo.audit.lock().unwrap().is_empty());
    }
}
