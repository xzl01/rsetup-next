//! 身份认证与会话仓储契约。

use super::model::{IdentityUser, Session, StoredSession};
use crate::ControllerError;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
