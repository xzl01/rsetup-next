use crate::{ControllerError, DbPool, auth::password::PasswordHasher};

pub trait BootstrapSecretSink: Send + Sync {
    fn emit(&self, username: &str, password: &str) -> Result<(), ControllerError>;
}

use rand::{RngCore, rngs::OsRng};

pub async fn bootstrap_admin(
    db: &DbPool,
    sink: &dyn BootstrapSecretSink,
) -> Result<(), ControllerError> {
    let (secret, hash) = generate_secret()?;
    let mut tx = db.0.begin().await?;
    crate::integrity::lock_integrity_guard(&mut tx).await?;
    let initialized_raw: i64 = sqlx::query_scalar(
        "SELECT CAST(initialized AS SIGNED) FROM schema_meta WHERE singleton = 1 FOR UPDATE",
    )
    .fetch_one(&mut *tx)
    .await?;
    if parse_initialized(initialized_raw)? {
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
    sink.emit("admin", &secret)
}

fn parse_initialized(raw: i64) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ControllerError::Config(
            "identity data meta.initialized".into(),
        )),
    }
}

fn generate_secret() -> Result<(String, String), ControllerError> {
    let mut entropy = [0u8; 32];
    OsRng.fill_bytes(&mut entropy);
    let secret = hex::encode(entropy);
    let hash = PasswordHasher::new().hash(&secret)?;
    Ok((secret, hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::{Argon2, PasswordHash, PasswordVerifier};

    #[test]
    fn bootstrap_initialized_accepts_exact_boolean_values() {
        assert!(!parse_initialized(0).unwrap());
        assert!(parse_initialized(1).unwrap());
    }

    #[test]
    fn bootstrap_initialized_rejects_invalid_values_without_echoing_them() {
        for raw in [2, -1, i64::MIN, i64::MAX] {
            let error = parse_initialized(raw).unwrap_err();
            assert!(matches!(error, ControllerError::Config(_)));
            assert_eq!(
                error.to_string(),
                "configuration: identity data meta.initialized"
            );
            assert!(!error.to_string().contains(&raw.to_string()));
        }
    }

    #[test]
    fn bootstrap_secret_has_entropy_and_argon2id_hash() {
        let (secret, hash) = generate_secret().unwrap();
        assert!(secret.len() >= 32, "at least 128 bits represented in hex");
        let parsed = PasswordHash::new(&hash).unwrap();
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.params.get("m").unwrap().decimal().unwrap(), 65536);
        assert_eq!(parsed.params.get("t").unwrap().decimal().unwrap(), 3);
        assert_eq!(parsed.params.get("p").unwrap().decimal().unwrap(), 1);
        Argon2::default()
            .verify_password(secret.as_bytes(), &parsed)
            .unwrap();
    }
}
