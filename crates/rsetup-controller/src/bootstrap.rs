use crate::{ControllerError, DbPool};

pub trait BootstrapSecretSink: Send + Sync {
    fn emit(&self, username: &str, password: &str);
}

use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
use rand::{RngCore, rngs::OsRng};

pub async fn bootstrap_admin(
    db: &DbPool,
    sink: &dyn BootstrapSecretSink,
) -> Result<(), ControllerError> {
    let (secret, hash) = generate_secret()?;
    let mut tx = db.0.begin().await?;
    let initialized: bool =
        sqlx::query_scalar("SELECT initialized FROM schema_meta WHERE singleton = 1 FOR UPDATE")
            .fetch_one(&mut *tx)
            .await?;
    if initialized {
        tx.commit().await?;
        return Ok(());
    }
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = 'admin'")
        .fetch_one(&mut *tx)
        .await?;
    if existing != 0 {
        return Err(ControllerError::Config(
            "admin exists without initialized marker; manual recovery required".into(),
        ));
    }
    sqlx::query("INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) VALUES (?, 'admin', 'admin', ?, TRUE, TRUE, TRUE, 1, UTC_TIMESTAMP(6))")
        .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).bind(hash).execute(&mut *tx).await?;
    sqlx::query("UPDATE schema_meta SET initialized = TRUE WHERE singleton = 1")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    sink.emit("admin", &secret);
    Ok(())
}

fn generate_secret() -> Result<(String, String), ControllerError> {
    let mut entropy = [0u8; 32];
    OsRng.fill_bytes(&mut entropy);
    let secret = hex::encode(entropy);
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|_| ControllerError::Crypto)?
        .to_string();
    Ok((secret, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::{Argon2, PasswordHash, PasswordVerifier};

    #[test]
    fn bootstrap_secret_has_entropy_and_argon2id_hash() {
        let (secret, hash) = generate_secret().unwrap();
        assert!(secret.len() >= 32, "at least 128 bits represented in hex");
        let parsed = PasswordHash::new(&hash).unwrap();
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .unwrap();
    }
}
