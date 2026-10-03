use crate::ControllerError;
use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher as _, PasswordVerifier as _, Version,
    password_hash::SaltString,
};
use rand::rngs::OsRng;

#[derive(Default)]
pub struct PasswordHasher;
impl PasswordHasher {
    pub fn new() -> Self {
        Self
    }

    fn argon2() -> Result<Argon2<'static>, ControllerError> {
        let params = Params::new(65536, 3, 1, None).map_err(|_| ControllerError::Crypto)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }

    pub fn hash(&self, secret: &str) -> Result<String, ControllerError> {
        Self::argon2()?
            .hash_password(secret.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|hash| hash.to_string())
            .map_err(|_| ControllerError::Crypto)
    }

    pub fn verify(&self, secret: &str, hash: &str) -> Result<bool, ControllerError> {
        let parsed = PasswordHash::new(hash).map_err(|_| ControllerError::Crypto)?;
        match Self::argon2()?.verify_password(secret.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(_) => Err(ControllerError::Crypto),
        }
    }
}

pub fn validate_new_password(old: &str, new: &str) -> Result<(), ControllerError> {
    if new == old || !(12..=128).contains(&new.chars().count()) || new.len() > 512 {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_hash_rejects_wrong_secret() {
        let h = PasswordHasher::new();
        let hash = h.hash("correct password").unwrap();
        assert!(h.verify("correct password", &hash).unwrap());
        assert!(!h.verify("wrong password", &hash).unwrap());
        let parsed = PasswordHash::new(&hash).unwrap();
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.params.get("m").unwrap().decimal().unwrap(), 65536);
        assert_eq!(parsed.params.get("t").unwrap().decimal().unwrap(), 3);
        assert_eq!(parsed.params.get("p").unwrap().decimal().unwrap(), 1);
    }

    #[test]
    fn password_policy_enforces_unicode_chars_bytes_and_old_secret() {
        assert!(validate_new_password("abcdefghijkl", "a new password").is_ok());
        assert!(validate_new_password("not the same!", "not the same!").is_err());
        assert!(validate_new_password("old password", "elevenchars").is_err());
        assert!(validate_new_password("old password", &"界".repeat(129)).is_err());
        assert!(validate_new_password("old password", &"😀".repeat(128)).is_ok());
        assert!(validate_new_password("old password", &"😀".repeat(129)).is_err());
    }
}
