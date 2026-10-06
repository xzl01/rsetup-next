use crate::{
    ControllerError,
    auth::{
        password::PasswordHasher,
        session::{SessionAliasKey, SessionClock, token_digest},
    },
};
use std::{sync::Arc, time::Instant};

#[derive(Clone, Debug)]
pub struct IdentityUser {
    pub id: [u8; 16],
    pub username: String,
    pub password_hash: String,
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub struct Session {
    pub user: IdentityUser,
    pub digest: [u8; 32],
}
pub struct Login {
    pub raw_token: String,
    pub session: Session,
}

#[derive(Clone, Debug)]
pub struct StoredSession {
    pub digest: [u8; 32],
    pub owner_id: [u8; 16],
    pub process_epoch: [u8; 16],
    pub created_time: chrono::DateTime<chrono::Utc>,
    pub revoked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionItem {
    pub id: String,
    pub current: bool,
    pub created_time: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPage {
    pub items: Vec<SessionItem>,
    pub next_cursor: Option<String>,
}

pub trait IdentityRepository: Send + Sync {
    fn find_user(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<IdentityUser>, ControllerError>> + Send;
    fn insert_session(
        &self,
        user: &IdentityUser,
        digest: [u8; 32],
        epoch: [u8; 16],
    ) -> impl Future<Output = Result<(), ControllerError>> + Send;
    fn find_session(
        &self,
        digest: [u8; 32],
        epoch: [u8; 16],
    ) -> impl Future<Output = Result<Option<IdentityUser>, ControllerError>> + Send;
    /// Atomically require an unrevoked session with this digest, user id and
    /// process epoch, an active user, and the supplied hash/revision; only then
    /// update the password and revoke that user's sessions in the same boundary.
    fn change_password(
        &self,
        session: &Session,
        epoch: [u8; 16],
        next_hash: &str,
    ) -> impl Future<Output = Result<(), ControllerError>> + Send;
    fn revoke_session(
        &self,
        session: &Session,
        epoch: [u8; 16],
    ) -> impl Future<Output = Result<(), ControllerError>> + Send;
    fn record_login_failure(
        &self,
        user_id: Option<[u8; 16]>,
    ) -> impl Future<Output = Result<(), ControllerError>> + Send;
    fn list_user_sessions(
        &self,
        _actor: &Session,
        _epoch: [u8; 16],
    ) -> impl Future<Output = Result<Vec<StoredSession>, ControllerError>> + Send {
        async {
            Err(ControllerError::Config(
                "identity session management not wired".into(),
            ))
        }
    }
    /// Atomically revalidate the already-resolved target row for the
    /// authenticated `actor` (owner/epoch/revoked) and mark it revoked in one
    /// boundary. `Ok(false)` means the target was not live in this process or
    /// its owner/epoch did not match; a missing target never reports success.
    /// No raw token is accepted — only the caller-authenticated actor and
    /// digest.
    fn revoke_selected_session(
        &self,
        _actor: &Session,
        _epoch: [u8; 16],
        _target_digest: [u8; 32],
    ) -> impl Future<Output = Result<bool, ControllerError>> + Send {
        async {
            Err(ControllerError::Config(
                "identity session management not wired".into(),
            ))
        }
    }
    /// Atomically revoke every other live session of the authenticated
    /// `actor`, returning the actual newly revoked count plus the digests
    /// newly revoked; the current digest is never included and a repeated
    /// call over an already-revoked set returns `(0, [])`.
    fn revoke_other_sessions(
        &self,
        _actor: &Session,
        _epoch: [u8; 16],
    ) -> impl Future<Output = Result<(u64, Vec<[u8; 32]>), ControllerError>> + Send {
        async {
            Err(ControllerError::Config(
                "identity session management not wired".into(),
            ))
        }
    }
}
pub struct AuthService<R: IdentityRepository> {
    repo: Arc<R>,
    clock: SessionClock,
    alias_key: SessionAliasKey,
    hasher: PasswordHasher,
    dummy_hash: String,
}
impl<R: IdentityRepository> AuthService<R> {
    pub fn new(repo: Arc<R>) -> Result<Self, ControllerError> {
        let hasher = PasswordHasher::new();
        let dummy_hash = hasher.hash("rsetup-identity-dummy-verification")?;
        Ok(Self {
            repo,
            clock: SessionClock::new(),
            alias_key: SessionAliasKey::new(),
            hasher,
            dummy_hash,
        })
    }

    pub async fn list_sessions(
        &self,
        actor: &Session,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<SessionPage, ControllerError> {
        // 1. actor must be active and not must_change_password
        if !actor.user.active {
            return Err(ControllerError::InvalidArgument);
        }
        if actor.user.must_change_password {
            return Err(ControllerError::PermissionDenied);
        }

        // 2. Validate limit 1..=200
        if !(1..=200).contains(&limit) {
            return Err(ControllerError::InvalidArgument);
        }

        // 3. Service checks actor.digest itself live before processing
        let now = Instant::now();
        if !self.clock.is_live(&actor.digest, now) {
            return Err(ControllerError::InvalidArgument);
        }

        // 4. Cursor validation if present:
        // Cursor is 132 lowercase hex = alias(32B, 64 hex) + limit(2B, 4 hex) + mac(32B, 64 hex).
        let cursor_after_alias = if let Some(cursor_str) = cursor {
            if cursor_str.len() != 132 {
                return Err(ControllerError::InvalidArgument);
            }
            if !cursor_str
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(ControllerError::InvalidArgument);
            }
            let alias_hex = &cursor_str[0..64];
            let limit_hex = &cursor_str[64..68];
            let mac_hex = &cursor_str[68..132];

            let mut alias_bytes = [0u8; 32];
            if hex::decode_to_slice(alias_hex, &mut alias_bytes).is_err() {
                return Err(ControllerError::InvalidArgument);
            }
            let mut limit_bytes = [0u8; 2];
            if hex::decode_to_slice(limit_hex, &mut limit_bytes).is_err() {
                return Err(ControllerError::InvalidArgument);
            }
            let cursor_limit = u16::from_be_bytes(limit_bytes) as usize;
            if cursor_limit != limit {
                return Err(ControllerError::InvalidArgument);
            }

            let mut expected_mac_payload = Vec::with_capacity(16 + 16 + 32 + 2);
            expected_mac_payload.extend_from_slice(&actor.user.id);
            expected_mac_payload.extend_from_slice(&self.clock.epoch);
            expected_mac_payload.extend_from_slice(&alias_bytes);
            expected_mac_payload.extend_from_slice(&limit_bytes);

            let expected_mac = self.alias_key.sign_cursor(&expected_mac_payload);
            let mut provided_mac = [0u8; 32];
            if hex::decode_to_slice(mac_hex, &mut provided_mac).is_err() {
                return Err(ControllerError::InvalidArgument);
            }

            // constant time compare MAC
            let mut diff = 0u8;
            for i in 0..32 {
                diff |= expected_mac[i] ^ provided_mac[i];
            }
            if diff != 0 {
                return Err(ControllerError::InvalidArgument);
            }

            Some((alias_hex.to_string(), alias_bytes))
        } else {
            None
        };

        // 5. Read sessions from repository
        let rows = self
            .repo
            .list_user_sessions(actor, self.clock.epoch)
            .await?;

        // 6. Hard limit capacity at 8192 (8193 -> ResourceExhausted)
        if rows.len() > 8192 {
            return Err(ControllerError::ResourceExhausted);
        }

        // 7. Validate each row: owner_id must match actor, process_epoch must match clock.epoch, not polluted
        struct Candidate {
            alias: String,
            current: bool,
            created_time: chrono::DateTime<chrono::Utc>,
        }
        let mut candidates = Vec::with_capacity(rows.len());

        for row in rows {
            if row.owner_id != actor.user.id || row.process_epoch != self.clock.epoch {
                return Err(ControllerError::Config("polluted session row".into()));
            }
            if row.revoked {
                continue;
            }
            if !self.clock.is_live(&row.digest, now) {
                continue;
            }
            let alias = self.alias_key.alias(&row.digest);
            let current = row.digest == actor.digest;
            candidates.push(Candidate {
                alias,
                current,
                created_time: row.created_time,
            });
        }

        // 8. Sort by public alias (lexicographical)
        candidates.sort_by(|a, b| a.alias.cmp(&b.alias));

        // 9. Apply cursor filtering
        let after_index = if let Some((ref start_alias, _)) = cursor_after_alias {
            match candidates.iter().position(|c| &c.alias == start_alias) {
                Some(pos) => pos + 1,
                None => {
                    // Start alias not found among current live items; find insertion point
                    candidates
                        .iter()
                        .position(|c| &c.alias > start_alias)
                        .unwrap_or(candidates.len())
                }
            }
        } else {
            0
        };

        let page_items: Vec<_> = candidates
            .into_iter()
            .skip(after_index)
            .take(limit + 1)
            .collect();
        let has_more = page_items.len() > limit;
        let returned_candidates = if has_more {
            &page_items[..limit]
        } else {
            &page_items[..]
        };

        let next_cursor = if has_more {
            let last_item = returned_candidates
                .last()
                .expect("must have last item if has_more");
            let mut alias_bytes = [0u8; 32];
            hex::decode_to_slice(&last_item.alias, &mut alias_bytes)
                .map_err(|_| ControllerError::Crypto)?;
            let limit_u16 = limit as u16;
            let limit_bytes = limit_u16.to_be_bytes();

            let mut mac_payload = Vec::with_capacity(16 + 16 + 32 + 2);
            mac_payload.extend_from_slice(&actor.user.id);
            mac_payload.extend_from_slice(&self.clock.epoch);
            mac_payload.extend_from_slice(&alias_bytes);
            mac_payload.extend_from_slice(&limit_bytes);

            let mac = self.alias_key.sign_cursor(&mac_payload);
            let cursor_str = format!(
                "{}{}{}",
                last_item.alias,
                hex::encode(limit_bytes),
                hex::encode(mac)
            );
            Some(cursor_str)
        } else {
            None
        };

        let items = returned_candidates
            .iter()
            .map(|c| SessionItem {
                id: c.alias.clone(),
                current: c.current,
                created_time: c
                    .created_time
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
            .collect();

        Ok(SessionPage { items, next_cursor })
    }
    pub async fn login(&self, username: &str, password: &str) -> Result<Login, ControllerError> {
        let found = self.repo.find_user(username).await?;
        let verified = match &found {
            Some(user) => self.hasher.verify(password, &user.password_hash)?,
            None => self.hasher.verify(password, &self.dummy_hash)?,
        };
        if !verified || !found.as_ref().is_some_and(|user| user.active) {
            self.repo
                .record_login_failure(found.as_ref().map(|user| user.id))
                .await?;
            return Err(ControllerError::InvalidArgument);
        }
        let user = found.ok_or(ControllerError::InvalidArgument)?;
        let mut raw = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
        let raw_token = hex::encode(raw);
        let digest = token_digest(raw_token.as_bytes());
        self.repo
            .insert_session(&user, digest, self.clock.epoch)
            .await?;
        self.clock.insert(digest, Instant::now());
        Ok(Login {
            raw_token,
            session: Session { user, digest },
        })
    }
    pub async fn authenticate(&self, raw: &str) -> Result<Session, ControllerError> {
        self.authenticate_with_now(raw, Instant::now).await
    }
    async fn authenticate_with_now(
        &self,
        raw: &str,
        now: impl Fn() -> Instant,
    ) -> Result<Session, ControllerError> {
        let digest = token_digest(raw.as_bytes());
        let user = self
            .repo
            .find_session(digest, self.clock.epoch)
            .await?
            .ok_or(ControllerError::InvalidArgument)?;
        if !user.active {
            return Err(ControllerError::InvalidArgument);
        }
        self.clock.check(digest, &self.clock.epoch, now())?;
        Ok(Session { user, digest })
    }
    pub async fn change_password(
        &self,
        session: &Session,
        current_password: &str,
        new_password: &str,
    ) -> Result<(), ControllerError> {
        if !self
            .hasher
            .verify(current_password, &session.user.password_hash)?
        {
            return Err(ControllerError::InvalidArgument);
        }
        crate::auth::password::validate_new_password(current_password, new_password)?;
        let next_hash = self.hasher.hash(new_password)?;
        self.repo
            .change_password(session, self.clock.epoch, &next_hash)
            .await
    }
    pub async fn logout(&self, session: &Session) -> Result<(), ControllerError> {
        self.repo.revoke_session(session, self.clock.epoch).await?;
        self.clock.remove(&session.digest);
        Ok(())
    }
    /// Task 3f pre-auth peek: does `digest` still exist and is live in the
    /// current process `SessionClock`?
    ///
    /// Pure in-memory: no repository access and no idle/absolute renewal
    /// (`SessionClock::is_live` only reads the deadlines map). A `true`
    /// result is only a *necessary* condition — the DB-authoritative
    /// `authenticate` must still run before anything is trusted. A `false`
    /// result fails closed and is indistinguishable from a dead session.
    pub fn is_live_session(&self, digest: &[u8; 32]) -> bool {
        self.clock.is_live(digest, Instant::now())
    }
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
    use super::*;
    use crate::auth::session::IDLE;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct AuthStateSnapshot {
        password_hash: String,
        active: bool,
        is_admin: bool,
        must_change_password: bool,
        revision: u64,
        sessions: Vec<([u8; 32], bool)>,
    }
    type TestStoredSession = (
        [u8; 32],
        bool,
        [u8; 16],
        [u8; 16],
        chrono::DateTime<chrono::Utc>,
    );
    /// Intentional failure injection kinds for the revocation methods.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RepoFailKind {
        /// Fixed database error (transport/commit boundary failure).
        Database,
        /// Fixed commit-unknown configuration error.
        CommitUnknown,
    }

    fn injected_error(kind: RepoFailKind) -> ControllerError {
        match kind {
            RepoFailKind::Database => ControllerError::Database(sqlx::Error::RowNotFound),
            RepoFailKind::CommitUnknown => ControllerError::Config("commit unknown".into()),
        }
    }

    /// Test-only kinds of concurrent target-row drift applied by the
    /// `revoke_selected_session` seam (I-2) between the list read and the
    /// in-lock revalidation check.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SelectedDriftKind {
        /// Owner id changed to a foreign user.
        Owner,
        /// Process epoch changed to a foreign epoch.
        Epoch,
        /// Row revoked concurrently.
        Revoked,
    }
    /// Test-only pending concurrent drift of one target row (I-2 seam).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct SelectedDrift {
        digest: [u8; 32],
        kind: SelectedDriftKind,
    }

    struct FakeRepo {
        state: Mutex<(IdentityUser, Vec<TestStoredSession>)>,
        failures: Mutex<Vec<Option<[u8; 16]>>>,
        audit_error: AtomicBool,
        lookup_error: AtomicBool,
        lookup_advance: Mutex<Option<Arc<Mutex<Instant>>>>,
        /// Fixed redacted audit event projection: only fixed event names,
        /// never a raw token, digest, password or public alias.
        audit: Mutex<Vec<String>>,
        selected_calls: AtomicUsize,
        bulk_calls: AtomicUsize,
        selected_fail: Mutex<Option<RepoFailKind>>,
        bulk_fail: Mutex<Option<RepoFailKind>>,
        /// When set, `revoke_other_sessions` returns this pair verbatim
        /// (without touching state) so service-side validation can be probed.
        bulk_override: Mutex<Option<(u64, Vec<[u8; 32]>)>>,
        /// I-2 test-only seam: when set, `revoke_selected_session` applies
        /// this concurrent target-row drift after `list_user_sessions`
        /// returned clean rows and before the in-lock revalidation check,
        /// so the repository recheck branch (owner/epoch mismatch or
        /// revoked -> `Ok(false)`) is reachable through the real service.
        selected_drift: Mutex<Option<SelectedDrift>>,
    }
    impl FakeRepo {
        fn auth_state(&self) -> AuthStateSnapshot {
            let state = self.state.lock().unwrap();
            let mut sessions: Vec<_> = state
                .1
                .iter()
                .map(|(hash, revoked, _, _, _)| (*hash, *revoked))
                .collect();
            sessions.sort();
            AuthStateSnapshot {
                password_hash: state.0.password_hash.clone(),
                active: state.0.active,
                is_admin: state.0.is_admin,
                must_change_password: state.0.must_change_password,
                revision: state.0.revision,
                sessions,
            }
        }
    }
    impl IdentityRepository for FakeRepo {
        async fn find_user(&self, username: &str) -> Result<Option<IdentityUser>, ControllerError> {
            let state = self.state.lock().unwrap();
            Ok((state.0.username == username).then(|| state.0.clone()))
        }
        async fn insert_session(
            &self,
            user: &IdentityUser,
            digest: [u8; 32],
            epoch: [u8; 16],
        ) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            if state.0.id != user.id || !state.0.active || !user.active {
                return Err(ControllerError::InvalidArgument);
            }
            if state.0.password_hash != user.password_hash || state.0.revision != user.revision {
                return Err(ControllerError::RevisionConflict);
            }
            let now = chrono::Utc::now();
            state.1.push((digest, false, epoch, user.id, now));
            Ok(())
        }
        async fn find_session(
            &self,
            digest: [u8; 32],
            epoch: [u8; 16],
        ) -> Result<Option<IdentityUser>, ControllerError> {
            if self.lookup_error.load(Ordering::SeqCst) {
                return Err(ControllerError::InvalidArgument);
            }
            if let Some(now) = self.lookup_advance.lock().unwrap().as_ref() {
                *now.lock().unwrap() += IDLE;
            }
            let state = self.state.lock().unwrap();
            Ok(state
                .1
                .iter()
                .any(|(h, r, e, user_id, _)| {
                    h == &digest && !r && e == &epoch && user_id == &state.0.id
                })
                .then(|| state.0.clone()))
        }
        async fn change_password(
            &self,
            session: &Session,
            epoch: [u8; 16],
            next_hash: &str,
        ) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            if state.0.revision != session.user.revision
                || state.0.password_hash != session.user.password_hash
            {
                return Err(ControllerError::RevisionConflict);
            }
            if !state
                .1
                .iter()
                .any(|(digest, revoked, stored_epoch, user_id, _)| {
                    digest == &session.digest
                        && !revoked
                        && stored_epoch == &epoch
                        && user_id == &session.user.id
                })
                || !state.0.active
                || state.0.id != session.user.id
            {
                return Err(ControllerError::InvalidArgument);
            }
            let next_revision = state
                .0
                .revision
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;
            state.0.password_hash = next_hash.into();
            state.0.revision = next_revision;
            state.0.must_change_password = false;
            for session in &mut state.1 {
                session.1 = true;
            }
            Ok(())
        }
        async fn revoke_session(
            &self,
            session: &Session,
            epoch: [u8; 16],
        ) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            for stored in &mut state.1 {
                if stored.0 != session.digest {
                    continue;
                }
                if stored.3 != session.user.id || stored.2 != epoch {
                    return Err(ControllerError::InvalidArgument);
                }
                stored.1 = true;
                return Ok(());
            }
            Ok(())
        }
        async fn record_login_failure(
            &self,
            user_id: Option<[u8; 16]>,
        ) -> Result<(), ControllerError> {
            if self.audit_error.load(Ordering::SeqCst) {
                return Err(ControllerError::Config("audit unavailable".into()));
            }
            self.failures.lock().unwrap().push(user_id);
            Ok(())
        }
        async fn list_user_sessions(
            &self,
            actor: &Session,
            epoch: [u8; 16],
        ) -> Result<Vec<StoredSession>, ControllerError> {
            let state = self.state.lock().unwrap();
            Ok(state
                .1
                .iter()
                .filter(|(_, _, e, user_id, _)| user_id == &actor.user.id && e == &epoch)
                .map(
                    |(digest, revoked, stored_epoch, user_id, created_time)| StoredSession {
                        digest: *digest,
                        owner_id: *user_id,
                        process_epoch: *stored_epoch,
                        created_time: *created_time,
                        revoked: *revoked,
                    },
                )
                .collect())
        }
        async fn revoke_selected_session(
            &self,
            actor: &Session,
            epoch: [u8; 16],
            target_digest: [u8; 32],
        ) -> Result<bool, ControllerError> {
            self.selected_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(kind) = *self.selected_fail.lock().unwrap() {
                return Err(injected_error(kind));
            }
            let mut state = self.state.lock().unwrap();
            for stored in state.1.iter_mut() {
                if stored.0 != target_digest {
                    continue;
                }
                // I-2 test-only seam: simulate a concurrent change of the
                // target row after `list_user_sessions` returned clean rows
                // and before the in-lock revalidation check, so the
                // repository recheck branch below is exercised through the
                // real service path. Applied once, to the exact digest only.
                if let Some(drift) = self.selected_drift.lock().unwrap().take() {
                    if drift.digest == stored.0 {
                        match drift.kind {
                            SelectedDriftKind::Owner => stored.3 = [0xff; 16],
                            SelectedDriftKind::Epoch => stored.2 = [0xee; 16],
                            SelectedDriftKind::Revoked => stored.1 = true,
                        }
                    }
                }
                // Revalidate owner/epoch/revoked in the same boundary:
                // mismatch or already revoked fails closed with `false`.
                if stored.3 != actor.user.id || stored.2 != epoch || stored.1 {
                    return Ok(false);
                }
                stored.1 = true;
                drop(state);
                self.audit
                    .lock()
                    .unwrap()
                    .push("identity.session_revoke_selected".into());
                return Ok(true);
            }
            Ok(false)
        }
        async fn revoke_other_sessions(
            &self,
            actor: &Session,
            epoch: [u8; 16],
        ) -> Result<(u64, Vec<[u8; 32]>), ControllerError> {
            self.bulk_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(override_result) = self.bulk_override.lock().unwrap().clone() {
                return Ok(override_result);
            }
            if let Some(kind) = *self.bulk_fail.lock().unwrap() {
                return Err(injected_error(kind));
            }
            let mut state = self.state.lock().unwrap();
            let mut digests = Vec::new();
            for stored in state.1.iter_mut() {
                if stored.3 == actor.user.id
                    && stored.2 == epoch
                    && !stored.1
                    && stored.0 != actor.digest
                {
                    stored.1 = true;
                    digests.push(stored.0);
                }
            }
            let count = digests.len() as u64;
            drop(state);
            // No audit for a no-op; only actual revocations are projected.
            if count > 0 {
                self.audit
                    .lock()
                    .unwrap()
                    .push("identity.session_revoke_others".into());
            }
            Ok((count, digests))
        }
    }
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
            let mut rows = self.inner.list_user_sessions(actor, epoch).await?;
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
    fn auth_fixture() -> (AuthService<FakeRepo>, Arc<FakeRepo>) {
        let hash = PasswordHasher::new().hash("old password").unwrap();
        let repo = Arc::new(FakeRepo {
            state: Mutex::new((
                IdentityUser {
                    id: [1; 16],
                    username: "alice".into(),
                    password_hash: hash,
                    active: true,
                    is_admin: false,
                    must_change_password: false,
                    revision: 1,
                },
                Vec::new(),
            )),
            failures: Mutex::new(Vec::new()),
            audit_error: AtomicBool::new(false),
            lookup_error: AtomicBool::new(false),
            lookup_advance: Mutex::new(None),
            audit: Mutex::new(Vec::new()),
            selected_calls: AtomicUsize::new(0),
            bulk_calls: AtomicUsize::new(0),
            selected_fail: Mutex::new(None),
            bulk_fail: Mutex::new(None),
            bulk_override: Mutex::new(None),
            selected_drift: Mutex::new(None),
        });
        (AuthService::new(repo.clone()).unwrap(), repo)
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
    #[tokio::test]
    async fn is_live_session_unknown_digest_is_false() {
        let (svc, _repo) = auth_fixture();
        assert!(!svc.is_live_session(&[0x7c; 32]));
    }
    #[tokio::test]
    async fn is_live_session_fresh_login_is_true_and_peek_renews_nothing() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let digest = login.session.digest;
        let now = Instant::now();
        assert!(
            svc.is_live_session(&digest),
            "a freshly logged-in digest must be live in this process clock"
        );
        // The peek must not extend the idle deadline: the same check the
        // authenticate path performs at `now + IDLE` must still fail.
        assert!(
            svc.clock
                .check(digest, &svc.clock.epoch, now + IDLE)
                .is_err(),
            "a live peek must not renew the idle deadline"
        );
    }
    #[tokio::test]
    async fn is_live_session_idle_expired_digest_is_false() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let digest = login.session.digest;
        let now = Instant::now();
        // Host-limited: a monotonic baseline below IDLE+1s cannot build a
        // genuine past instant (Task 3b fix1 clock limitation note); the
        // deadline semantics themselves are covered by the injectable-time
        // SessionClock tests in session.rs.
        let Some(expired_at) = now
            .checked_sub(IDLE)
            .and_then(|t| t.checked_sub(std::time::Duration::from_secs(1)))
        else {
            eprintln!(
                "clock-limited host: monotonic baseline < 30m1s; skipping host-dependent expired-peek case"
            );
            return;
        };
        svc.clock.insert(digest, expired_at);
        assert!(
            !svc.is_live_session(&digest),
            "an idle-expired digest must not be live"
        );
    }
    #[tokio::test]
    async fn slow_lookup_cannot_extend_session_past_old_idle_deadline() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let now = Instant::now();
        let manual_now = Arc::new(Mutex::new(now));
        svc.clock.insert(login.session.digest, now - IDLE / 2);
        *repo.lookup_advance.lock().unwrap() = Some(manual_now.clone());
        let result = svc
            .authenticate_with_now(&login.raw_token, || *manual_now.lock().unwrap())
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        *repo.lookup_advance.lock().unwrap() = None;
        assert!(
            svc.authenticate_with_now(&login.raw_token, || *manual_now.lock().unwrap())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn missing_session_does_not_extend_idle_on_recovery() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let now = Instant::now();
        svc.clock.insert(login.session.digest, now - IDLE / 2);
        repo.state.lock().unwrap().1[0].1 = true;
        assert!(svc.authenticate(&login.raw_token).await.is_err());
        repo.state.lock().unwrap().1[0].1 = false;
        assert!(
            svc.authenticate_with_now(&login.raw_token, || now + IDLE / 2)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn lookup_error_does_not_extend_idle_on_recovery() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let now = Instant::now();
        svc.clock.insert(login.session.digest, now - IDLE / 2);
        repo.lookup_error.store(true, Ordering::SeqCst);
        assert!(svc.authenticate(&login.raw_token).await.is_err());
        repo.lookup_error.store(false, Ordering::SeqCst);
        assert!(
            svc.authenticate_with_now(&login.raw_token, || now + IDLE / 2)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn inactive_user_cannot_authenticate_existing_session_or_extend_idle() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let now = Instant::now();
        svc.clock.insert(login.session.digest, now - IDLE / 2);
        repo.state.lock().unwrap().0.active = false;
        assert!(svc.authenticate(&login.raw_token).await.is_err());
        assert!(
            svc.clock
                .check(login.session.digest, &svc.clock.epoch, now + IDLE / 2)
                .is_err()
        );
    }
    #[tokio::test]
    async fn logged_out_session_snapshot_cannot_change_password() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        svc.logout(&login.session).await.unwrap();
        let before = repo.auth_state();
        let result = svc
            .change_password(&login.session, "old password", "new password")
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert_eq!(repo.auth_state(), before);
        assert!(svc.login("alice", "old password").await.is_ok());
    }
    #[tokio::test]
    async fn separately_revoked_snapshot_cannot_change_password_or_revoke_live_session() {
        let (svc, repo) = auth_fixture();
        let old = svc.login("alice", "old password").await.unwrap();
        let live = svc.login("alice", "old password").await.unwrap();
        repo.revoke_session(&old.session, svc.clock.epoch)
            .await
            .unwrap();
        let before = repo.auth_state();
        let result = svc
            .change_password(&old.session, "old password", "new password")
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert_eq!(repo.auth_state(), before);
        assert!(svc.authenticate(&live.raw_token).await.is_ok());
    }
    #[tokio::test]
    async fn inactive_snapshot_cannot_change_password() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        repo.state.lock().unwrap().0.active = false;
        let before = repo.auth_state();
        let result = svc
            .change_password(&login.session, "old password", "new password")
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert_eq!(repo.auth_state(), before);
    }
    #[tokio::test]
    async fn old_process_epoch_snapshot_cannot_change_password() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let restarted = AuthService::new(repo.clone()).unwrap();
        let before = repo.auth_state();
        let result = restarted
            .change_password(&login.session, "old password", "new password")
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert_eq!(repo.auth_state(), before);
    }
    #[tokio::test]
    async fn session_for_different_user_id_cannot_change_password() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        repo.state.lock().unwrap().1[0].3 = [2; 16];
        let before = repo.auth_state();
        let result = svc
            .change_password(&login.session, "old password", "new password")
            .await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert_eq!(repo.auth_state(), before);
    }
    #[tokio::test]
    async fn wrong_current_password_preserves_account_and_sessions() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let before = repo.auth_state();
        let result = svc
            .change_password(&login.session, "wrong password", "new password")
            .await;
        assert_eq!(repo.auth_state(), before);
        assert!(svc.authenticate(&login.raw_token).await.is_ok());
        assert!(result.is_err());
    }
    #[tokio::test]
    async fn password_change_revokes_existing_session() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        assert_ne!(
            repo.auth_state().sessions[0].0.as_slice(),
            login.raw_token.as_bytes()
        );
        svc.change_password(&login.session, "old password", "new password")
            .await
            .unwrap();
        assert!(svc.authenticate(&login.raw_token).await.is_err());
        assert!(svc.login("alice", "new password").await.is_ok());
    }
    #[tokio::test]
    async fn overflowing_password_revision_preserves_live_session_and_account() {
        let (svc, repo) = auth_fixture();
        repo.state.lock().unwrap().0.revision = u64::MAX;
        let old = svc.login("alice", "old password").await.unwrap();
        let live = svc.login("alice", "old password").await.unwrap();
        let before = repo.auth_state();
        assert!(matches!(
            svc.change_password(&old.session, "old password", "new password")
                .await,
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(repo.auth_state(), before);
        assert!(svc.authenticate(&live.raw_token).await.is_ok());
    }

    #[tokio::test]
    async fn concurrent_password_change_does_not_overwrite_new_hash_or_revoke_new_session() {
        let (svc, repo) = auth_fixture();
        let old = svc.login("alice", "old password").await.unwrap();
        let winner = svc.login("alice", "old password").await.unwrap();
        svc.change_password(&winner.session, "old password", "winner password")
            .await
            .unwrap();
        let newer = svc.login("alice", "winner password").await.unwrap();
        let before = repo.auth_state();
        let result = svc
            .change_password(&old.session, "old password", "loser password")
            .await;
        assert!(matches!(result, Err(ControllerError::RevisionConflict)));
        assert_eq!(repo.auth_state(), before);
        assert!(svc.authenticate(&newer.raw_token).await.is_ok());
        assert!(svc.login("alice", "winner password").await.is_ok());
    }
    #[tokio::test]
    async fn stale_login_snapshot_with_drifted_revision_never_creates_session() {
        let (svc, repo) = auth_fixture();
        let stale = repo.state.lock().unwrap().0.clone();
        repo.state.lock().unwrap().0.revision = stale.revision + 1;
        let result = repo.insert_session(&stale, [7; 32], svc.clock.epoch).await;
        assert!(matches!(result, Err(ControllerError::RevisionConflict)));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn stale_login_snapshot_with_drifted_hash_never_creates_session() {
        let (svc, repo) = auth_fixture();
        let mut stale = repo.state.lock().unwrap().0.clone();
        stale.password_hash = "other-hash".into();
        let result = repo.insert_session(&stale, [7; 32], svc.clock.epoch).await;
        assert!(matches!(result, Err(ControllerError::RevisionConflict)));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn inactive_snapshot_never_creates_session() {
        let (svc, repo) = auth_fixture();
        let mut stale = repo.state.lock().unwrap().0.clone();
        stale.active = false;
        let result = repo.insert_session(&stale, [7; 32], svc.clock.epoch).await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn inactive_current_row_rejects_stale_active_snapshot() {
        let (svc, repo) = auth_fixture();
        let stale = repo.state.lock().unwrap().0.clone();
        repo.state.lock().unwrap().0.active = false;
        assert!(matches!(
            repo.insert_session(&stale, [8; 32], svc.clock.epoch).await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn inactive_current_row_with_drifted_revision_rejects_stale_active_snapshot() {
        let (svc, repo) = auth_fixture();
        let stale = repo.state.lock().unwrap().0.clone();
        repo.state.lock().unwrap().0.active = false;
        repo.state.lock().unwrap().0.revision = stale.revision + 1;
        assert!(matches!(
            repo.insert_session(&stale, [8; 32], svc.clock.epoch).await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn matching_active_snapshot_creates_session() {
        let (svc, repo) = auth_fixture();
        let current = repo.state.lock().unwrap().0.clone();
        assert!(
            repo.insert_session(&current, [8; 32], svc.clock.epoch)
                .await
                .is_ok()
        );
        assert_eq!(repo.state.lock().unwrap().1.len(), 1);
    }
    #[tokio::test]
    async fn unknown_bad_password_and_inactive_user_record_only_anonymous_failures() {
        let (svc, repo) = auth_fixture();
        assert!(matches!(
            svc.login("ghost", "wrong").await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(matches!(
            svc.login("alice", "wrong").await,
            Err(ControllerError::InvalidArgument)
        ));
        repo.state.lock().unwrap().0.active = false;
        assert!(matches!(
            svc.login("alice", "old password").await,
            Err(ControllerError::InvalidArgument)
        ));
        assert_eq!(
            *repo.failures.lock().unwrap(),
            vec![None, Some([1; 16]), Some([1; 16])]
        );
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn unknown_account_really_runs_dummy_argon2_verify() {
        let (mut svc, repo) = auth_fixture();
        svc.dummy_hash = "synthetic-invalid-phc".into();
        let result = svc.login("ghost", "wrong").await;
        assert!(matches!(result, Err(ControllerError::Crypto)));
        assert!(repo.failures.lock().unwrap().is_empty());
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn audit_write_failure_is_not_swallowed_as_invalid_credentials() {
        let (svc, repo) = auth_fixture();
        repo.audit_error.store(true, Ordering::SeqCst);
        let result = svc.login("ghost", "wrong").await;
        assert!(matches!(
            result,
            Err(ControllerError::Config(ref message)) if message == "audit unavailable"
        ));
        assert!(repo.state.lock().unwrap().1.is_empty());
    }
    #[tokio::test]
    async fn successful_login_creates_single_session_and_records_no_failure() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let sessions = repo.state.lock().unwrap().1.clone();
        assert_eq!(sessions.len(), 1);
        assert!(!sessions[0].1);
        assert!(svc.authenticate(&login.raw_token).await.is_ok());
        assert!(repo.failures.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_sessions_two_sessions_and_current_marker() {
        let (svc, _repo) = auth_fixture();
        let _login1 = svc.login("alice", "old password").await.unwrap();
        let login2 = svc.login("alice", "old password").await.unwrap();

        let page = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(page.items.len(), 2);
        let cur = page
            .items
            .iter()
            .find(|i| i.current)
            .expect("must have current session");
        let other = page
            .items
            .iter()
            .find(|i| !i.current)
            .expect("must have other session");
        assert_ne!(cur.id, other.id);
        assert_eq!(cur.id.len(), 64);
        assert_eq!(other.id.len(), 64);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_sessions_does_not_contain_other_user_sessions() {
        let (svc, repo) = auth_fixture();
        let login_alice = svc.login("alice", "old password").await.unwrap();

        // inject a session for user [2; 16] into FakeRepo
        let other_digest = token_digest(b"other-user-raw-token");
        let now = chrono::Utc::now();
        repo.state
            .lock()
            .unwrap()
            .1
            .push((other_digest, false, svc.clock.epoch, [2; 16], now));

        let page = svc
            .list_sessions(&login_alice.session, None, 50)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].current);
    }

    #[tokio::test]
    async fn list_sessions_idle_expiry_without_renewal() {
        let (svc, _repo) = auth_fixture();
        let login1 = svc.login("alice", "old password").await.unwrap();
        let login2 = svc.login("alice", "old password").await.unwrap();

        // Move login1's clock back so it is 100 milliseconds before 30min idle boundary
        let now = Instant::now();
        svc.clock.insert(
            login1.session.digest,
            now - IDLE + std::time::Duration::from_millis(100),
        );

        let page = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(
            page.items.len(),
            2,
            "session before 30min idle boundary must be present"
        );

        // Now move login1 to exactly 30min idle boundary
        let now2 = Instant::now();
        svc.clock.insert(login1.session.digest, now2 - IDLE);
        let page2 = svc.list_sessions(&login2.session, None, 50).await.unwrap();
        assert_eq!(
            page2.items.len(),
            1,
            "session at or past 30min idle boundary must be absent"
        );
        assert!(page2.items[0].current);
    }

    #[tokio::test]
    async fn list_sessions_limits_and_pagination() {
        let (svc, _repo) = auth_fixture();
        let login1 = svc.login("alice", "old password").await.unwrap();
        let _login2 = svc.login("alice", "old password").await.unwrap();

        // limit 0 is invalid
        assert!(matches!(
            svc.list_sessions(&login1.session, None, 0).await,
            Err(ControllerError::InvalidArgument)
        ));
        // limit 201 is invalid (> 200)
        assert!(matches!(
            svc.list_sessions(&login1.session, None, 201).await,
            Err(ControllerError::InvalidArgument)
        ));
        // limit 1 on 2 items returns 1 item and a 132-hex cursor
        let page1 = svc.list_sessions(&login1.session, None, 1).await.unwrap();
        assert_eq!(page1.items.len(), 1);
        let cursor = page1.next_cursor.expect("should have next_cursor");
        assert_eq!(cursor.len(), 132);

        // Fetch second page using cursor
        let page2 = svc
            .list_sessions(&login1.session, Some(&cursor), 1)
            .await
            .unwrap();
        assert_eq!(page2.items.len(), 1);
        assert!(page2.next_cursor.is_none());
        assert_ne!(page1.items[0].id, page2.items[0].id);
        // items should be strictly ordered by alias
        assert!(page1.items[0].id < page2.items[0].id);

        // Fetch with limit 200
        let page_200 = svc.list_sessions(&login1.session, None, 200).await.unwrap();
        assert_eq!(page_200.items.len(), 2);
        assert!(page_200.next_cursor.is_none());

        // Fetch with default limit 50
        let page_50 = svc.list_sessions(&login1.session, None, 50).await.unwrap();
        assert_eq!(page_50.items.len(), 2);
        assert!(page_50.next_cursor.is_none());
    }

    #[tokio::test]
    async fn list_sessions_corrupted_foreign_or_tampered_cursor_is_invalid_argument() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();
        let _login2 = svc.login("alice", "old password").await.unwrap();

        let page = svc.list_sessions(&login.session, None, 1).await.unwrap();
        let cursor = page.next_cursor.unwrap();

        // Wrong length
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&cursor[..130]), 1)
                .await,
            Err(ControllerError::InvalidArgument)
        ));
        // Uppercase hex
        let upper_hex = cursor.to_ascii_uppercase();
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&upper_hex), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Non-hex
        let mut non_hex = cursor.clone();
        non_hex.replace_range(0..2, "zz");
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&non_hex), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Tampered MAC
        let mut tampered_mac = cursor.clone();
        let last_char = if tampered_mac.ends_with('0') {
            '1'
        } else {
            '0'
        };
        tampered_mac.pop();
        tampered_mac.push(last_char);
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&tampered_mac), 1)
                .await,
            Err(ControllerError::InvalidArgument)
        ));
        // Limit mismatch: cursor was minted with limit=1, pass limit=2
        assert!(matches!(
            svc.list_sessions(&login.session, Some(&cursor), 2).await,
            Err(ControllerError::InvalidArgument)
        ));
        // Foreign user
        let mut foreign_actor = login.session.clone();
        foreign_actor.user.id = [9; 16];
        assert!(matches!(
            svc.list_sessions(&foreign_actor, Some(&cursor), 1).await,
            Err(ControllerError::InvalidArgument)
        ));
    }

    #[tokio::test]
    async fn list_sessions_capacity_8192_passes_8193_resource_exhausted() {
        let (svc, repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // Exactly 8192 rows (bounded at 8192) passes without ResourceExhausted
        let now = chrono::Utc::now();
        let mut sessions_8192 = Vec::with_capacity(8192);
        for i in 0..8192u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_be_bytes());
            sessions_8192.push((d, false, svc.clock.epoch, login.session.user.id, now));
        }
        repo.state.lock().unwrap().1 = sessions_8192;

        let result = svc.list_sessions(&login.session, None, 50).await;
        // Alice's login session digest is not in the synthetic loop, so candidates may be empty or filtered by clock, but it must NOT be ResourceExhausted!
        assert!(result.is_ok());

        // 8193 rows -> ResourceExhausted
        let mut sessions_8193 = Vec::with_capacity(8193);
        for i in 0..8193u32 {
            let mut d = [0u8; 32];
            d[..4].copy_from_slice(&i.to_be_bytes());
            sessions_8193.push((d, false, svc.clock.epoch, login.session.user.id, now));
        }
        repo.state.lock().unwrap().1 = sessions_8193;

        let result = svc.list_sessions(&login.session, None, 50).await;
        assert!(matches!(result, Err(ControllerError::ResourceExhausted)));
    }

    #[tokio::test]
    async fn list_sessions_rejects_inactive_actor_session_digest() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // Expire the actor's session digest in clock
        let now = Instant::now();
        svc.clock.insert(login.session.digest, now - IDLE);

        let result = svc.list_sessions(&login.session, None, 50).await;
        assert!(matches!(result, Err(ControllerError::InvalidArgument)));
    }

    #[tokio::test]
    async fn list_sessions_rejects_must_change_password_and_inactive() {
        let (svc, _repo) = auth_fixture();
        let login = svc.login("alice", "old password").await.unwrap();

        // must_change_password
        let mut force_pw_session = login.session.clone();
        force_pw_session.user.must_change_password = true;
        assert!(matches!(
            svc.list_sessions(&force_pw_session, None, 50).await,
            Err(ControllerError::PermissionDenied)
        ));

        // inactive
        let mut inactive_session = login.session.clone();
        inactive_session.user.active = false;
        assert!(matches!(
            svc.list_sessions(&inactive_session, None, 50).await,
            Err(ControllerError::InvalidArgument)
        ));
    }

    #[tokio::test]
    async fn list_sessions_default_trait_implementation_rejects_with_config() {
        struct DefaultRepo;
        impl IdentityRepository for DefaultRepo {
            async fn find_user(
                &self,
                _username: &str,
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn insert_session(
                &self,
                _user: &IdentityUser,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn find_session(
                &self,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn change_password(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
                _next_hash: &str,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn revoke_session(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn record_login_failure(
                &self,
                _user_id: Option<[u8; 16]>,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
        }

        let repo = Arc::new(DefaultRepo);
        let actor = Session {
            user: IdentityUser {
                id: [1; 16],
                username: "alice".into(),
                password_hash: "hash".into(),
                active: true,
                is_admin: false,
                must_change_password: false,
                revision: 1,
            },
            digest: [2; 32],
        };
        let res = repo.list_user_sessions(&actor, [3; 16]).await;
        assert!(matches!(
            res,
            Err(ControllerError::Config(ref msg)) if msg == "identity session management not wired"
        ));
    }

    #[tokio::test]
    async fn list_sessions_rejects_polluted_repo_row() {
        struct PollutedRepo {
            polluted_rows: Vec<StoredSession>,
        }
        impl IdentityRepository for PollutedRepo {
            async fn find_user(
                &self,
                _username: &str,
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn insert_session(
                &self,
                _user: &IdentityUser,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn find_session(
                &self,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn change_password(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
                _next_hash: &str,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn revoke_session(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn record_login_failure(
                &self,
                _user_id: Option<[u8; 16]>,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn list_user_sessions(
                &self,
                _actor: &Session,
                _epoch: [u8; 16],
            ) -> Result<Vec<StoredSession>, ControllerError> {
                Ok(self.polluted_rows.clone())
            }
        }

        let (svc_orig, _repo) = auth_fixture();
        let login = svc_orig.login("alice", "old password").await.unwrap();
        let now = chrono::Utc::now();

        // 1. Polluted epoch
        let repo_bad_epoch = Arc::new(PollutedRepo {
            polluted_rows: vec![StoredSession {
                digest: login.session.digest,
                owner_id: login.session.user.id,
                process_epoch: [0xee; 16], // wrong epoch
                created_time: now,
                revoked: false,
            }],
        });
        let svc_bad_epoch = AuthService {
            repo: repo_bad_epoch,
            clock: svc_orig.clock,
            alias_key: svc_orig.alias_key,
            hasher: PasswordHasher::new(),
            dummy_hash: svc_orig.dummy_hash.clone(),
        };
        let res = svc_bad_epoch.list_sessions(&login.session, None, 50).await;
        assert!(
            matches!(res, Err(ControllerError::Config(ref msg)) if msg == "polluted session row")
        );

        // 2. Polluted owner
        let repo_bad_owner = Arc::new(PollutedRepo {
            polluted_rows: vec![StoredSession {
                digest: login.session.digest,
                owner_id: [0xff; 16], // wrong owner
                process_epoch: svc_bad_epoch.clock.epoch,
                created_time: now,
                revoked: false,
            }],
        });
        let svc_bad_owner = AuthService {
            repo: repo_bad_owner,
            clock: svc_bad_epoch.clock,
            alias_key: svc_bad_epoch.alias_key,
            hasher: PasswordHasher::new(),
            dummy_hash: svc_bad_epoch.dummy_hash,
        };
        let res2 = svc_bad_owner.list_sessions(&login.session, None, 50).await;
        assert!(
            matches!(res2, Err(ControllerError::Config(ref msg)) if msg == "polluted session row")
        );
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

    #[tokio::test]
    async fn revoke_default_trait_methods_reject_with_fixed_config() {
        struct RevokeDefaultRepo;
        impl IdentityRepository for RevokeDefaultRepo {
            async fn find_user(
                &self,
                _username: &str,
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn insert_session(
                &self,
                _user: &IdentityUser,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn find_session(
                &self,
                _digest: [u8; 32],
                _epoch: [u8; 16],
            ) -> Result<Option<IdentityUser>, ControllerError> {
                unimplemented!()
            }
            async fn change_password(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
                _next_hash: &str,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn revoke_session(
                &self,
                _session: &Session,
                _epoch: [u8; 16],
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
            async fn record_login_failure(
                &self,
                _user_id: Option<[u8; 16]>,
            ) -> Result<(), ControllerError> {
                unimplemented!()
            }
        }

        let repo = Arc::new(RevokeDefaultRepo);
        let actor = Session {
            user: IdentityUser {
                id: [1; 16],
                username: "alice".into(),
                password_hash: "hash".into(),
                active: true,
                is_admin: false,
                must_change_password: false,
                revision: 1,
            },
            digest: [2; 32],
        };
        let res = repo.revoke_selected_session(&actor, [3; 16], [4; 32]).await;
        assert!(matches!(
            res,
            Err(ControllerError::Config(ref msg)) if msg == "identity session management not wired"
        ));
        let res2 = repo.revoke_other_sessions(&actor, [3; 16]).await;
        assert!(matches!(
            res2,
            Err(ControllerError::Config(ref msg)) if msg == "identity session management not wired"
        ));
    }
}
