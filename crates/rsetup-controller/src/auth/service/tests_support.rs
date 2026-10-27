use super::*;
use crate::auth::session::IDLE;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AuthStateSnapshot {
    password_hash: String,
    active: bool,
    is_admin: bool,
    must_change_password: bool,
    revision: u64,
    pub(super) sessions: Vec<([u8; 32], bool)>,
}

pub(super) type TestStoredSession = (
    [u8; 32],
    bool,
    [u8; 16],
    [u8; 16],
    chrono::DateTime<chrono::Utc>,
);

/// Intentional failure injection kinds for the revocation methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RepoFailKind {
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
pub(super) enum SelectedDriftKind {
    /// Owner id changed to a foreign user.
    Owner,
    /// Process epoch changed to a foreign epoch.
    Epoch,
    /// Row revoked concurrently.
    Revoked,
}

/// Test-only pending concurrent drift of one target row (I-2 seam).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectedDrift {
    pub(super) digest: [u8; 32],
    pub(super) kind: SelectedDriftKind,
}

pub(super) struct FakeRepo {
    pub(super) state: Mutex<(IdentityUser, Vec<TestStoredSession>)>,
    pub(super) failures: Mutex<Vec<Option<[u8; 16]>>>,
    pub(super) audit_error: AtomicBool,
    pub(super) lookup_error: AtomicBool,
    pub(super) lookup_advance: Mutex<Option<Arc<Mutex<Instant>>>>,
    /// Fixed redacted audit event projection: only fixed event names,
    /// never a raw token, digest, password or public alias.
    pub(super) audit: Mutex<Vec<String>>,
    pub(super) selected_calls: AtomicUsize,
    pub(super) bulk_calls: AtomicUsize,
    pub(super) selected_fail: Mutex<Option<RepoFailKind>>,
    pub(super) bulk_fail: Mutex<Option<RepoFailKind>>,
    /// When set, `revoke_other_sessions` returns this pair verbatim
    /// (without touching state) so service-side validation can be probed.
    pub(super) bulk_override: Mutex<Option<(u64, Vec<[u8; 32]>)>>,
    /// I-2 test-only seam: when set, `revoke_selected_session` applies
    /// this concurrent target-row drift after `list_user_sessions`
    /// returned clean rows and before the in-lock revalidation check,
    /// so the repository recheck branch (owner/epoch mismatch or
    /// revoked -> `Ok(false)`) is reachable through the real service.
    pub(super) selected_drift: Mutex<Option<SelectedDrift>>,
}

impl FakeRepo {
    pub(super) fn auth_state(&self) -> AuthStateSnapshot {
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

    async fn record_login_failure(&self, user_id: Option<[u8; 16]>) -> Result<(), ControllerError> {
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
            if let Some(drift) = self.selected_drift.lock().unwrap().take()
                && drift.digest == stored.0
            {
                match drift.kind {
                    SelectedDriftKind::Owner => stored.3 = [0xff; 16],
                    SelectedDriftKind::Epoch => stored.2 = [0xee; 16],
                    SelectedDriftKind::Revoked => stored.1 = true,
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

pub(super) fn auth_fixture() -> (AuthService<FakeRepo>, Arc<FakeRepo>) {
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
