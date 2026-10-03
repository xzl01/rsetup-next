use crate::{
    ControllerError,
    auth::{
        password::PasswordHasher,
        session::{SessionClock, token_digest},
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

pub trait IdentityRepository: Send + Sync {
    fn find_user(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<IdentityUser>, ControllerError>> + Send;
    fn insert_session(
        &self,
        user_id: [u8; 16],
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
        digest: [u8; 32],
    ) -> impl Future<Output = Result<(), ControllerError>> + Send;
}
pub struct AuthService<R: IdentityRepository> {
    repo: Arc<R>,
    clock: SessionClock,
    hasher: PasswordHasher,
}
impl<R: IdentityRepository> AuthService<R> {
    pub fn new(repo: Arc<R>) -> Self {
        Self {
            repo,
            clock: SessionClock::new(),
            hasher: PasswordHasher::new(),
        }
    }
    pub async fn login(&self, username: &str, password: &str) -> Result<Login, ControllerError> {
        let user = self
            .repo
            .find_user(username)
            .await?
            .ok_or(ControllerError::InvalidArgument)?;
        if !user.active || !self.hasher.verify(password, &user.password_hash)? {
            return Err(ControllerError::InvalidArgument);
        }
        let mut raw = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
        let raw_token = hex::encode(raw);
        let digest = token_digest(raw_token.as_bytes());
        self.repo
            .insert_session(user.id, digest, self.clock.epoch)
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
        self.repo.revoke_session(session.digest).await?;
        self.clock.remove(&session.digest);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::IDLE;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
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
    type StoredSession = ([u8; 32], bool, [u8; 16], [u8; 16]);
    struct FakeRepo {
        state: Mutex<(IdentityUser, Vec<StoredSession>)>,
        lookup_error: AtomicBool,
        lookup_advance: Mutex<Option<Arc<Mutex<Instant>>>>,
    }
    impl FakeRepo {
        fn auth_state(&self) -> AuthStateSnapshot {
            let state = self.state.lock().unwrap();
            let mut sessions: Vec<_> = state
                .1
                .iter()
                .map(|(hash, revoked, _, _)| (*hash, *revoked))
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
            user_id: [u8; 16],
            digest: [u8; 32],
            epoch: [u8; 16],
        ) -> Result<(), ControllerError> {
            self.state
                .lock()
                .unwrap()
                .1
                .push((digest, false, epoch, user_id));
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
                .any(|(h, r, e, user_id)| {
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
                .any(|(digest, revoked, stored_epoch, user_id)| {
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
        async fn revoke_session(&self, digest: [u8; 32]) -> Result<(), ControllerError> {
            for session in &mut self.state.lock().unwrap().1 {
                if session.0 == digest {
                    session.1 = true
                }
            }
            Ok(())
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
            lookup_error: AtomicBool::new(false),
            lookup_advance: Mutex::new(None),
        });
        (AuthService::new(repo.clone()), repo)
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
        repo.revoke_session(old.session.digest).await.unwrap();
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
        let restarted = AuthService::new(repo.clone());
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
}
