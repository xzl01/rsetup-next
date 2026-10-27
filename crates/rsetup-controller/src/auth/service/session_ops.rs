use super::*;
use std::time::Instant;

impl<R: IdentityRepository> AuthService<R> {
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

    /// 权威（仓储侧）会话核验：`digest` 在**本进程 epoch 内**是否仍对应一条
    /// 未撤销、且所属用户仍 active 的会话；是则返回该用户**此刻**的权限事实。
    ///
    /// 与 [`AuthService::authenticate`] 的差别只在输入与副作用：不接受原始
    /// 令牌（长连接不保存凭据，只握有 digest），不写时钟、不续期、不刷新任何
    /// deadline；读的是同一条权威会话视图（`find_session` 已把 `revoked` 与
    /// 用户行内连接并判定），因此「DB 里撤销会话」「DB 里停用用户」「DB 里给
    /// 管理员降权」都会在这里如实暴露，而进程时钟看不到这些变化。
    ///
    /// 闲置/绝对超时仍由 [`AuthService::is_live_session`] 的进程时钟把关，
    /// 两者合起来才是长连接逐帧复核的完整门禁；本方法是纯只读，绝不改变
    /// 会话生命周期。
    pub async fn session_authority(
        &self,
        digest: &[u8; 32],
    ) -> Result<Option<IdentityUser>, ControllerError> {
        let user = self.repo.find_session(*digest, self.clock.epoch).await?;
        Ok(user.filter(|user| user.active))
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
}

#[cfg(test)]
mod tests {
    use super::super::tests_support::auth_fixture;
    use super::*;
    use crate::auth::session::IDLE;
    use std::sync::{Mutex, atomic::Ordering};

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
}
