use crate::{
    ControllerError,
    auth::{
        password::PasswordHasher,
        session::{SessionAliasKey, SessionClock, token_digest},
    },
};
use std::sync::Arc;

mod model;
mod pagination;
mod repository;
mod revoke;
mod session_ops;
#[cfg(test)]
mod tests_support;

pub use self::model::{IdentityUser, Login, Session, SessionItem, SessionPage, StoredSession};
pub use self::repository::IdentityRepository;

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
}
