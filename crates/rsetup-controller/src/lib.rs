pub mod auth;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod error;
pub mod model;

pub use bootstrap::{BootstrapSecretSink, bootstrap_admin};
pub use config::ControllerConfig;
pub use db::{AdmissionStore, DbPool, migrate};
pub use error::ControllerError;
pub use model::{AdmissionSnapshot, AdmissionState, ReviewDecision};

pub fn parse_device_id(s: &str) -> Result<[u8; 32], ControllerError> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ControllerError::InvalidArgument);
    }
    hex::decode(s)
        .map_err(|_| ControllerError::InvalidArgument)?
        .try_into()
        .map_err(|_| ControllerError::InvalidArgument)
}

pub fn build_router(state: DbPool) -> axum::Router {
    use axum::{http::StatusCode, routing::get};
    async fn healthz() -> StatusCode {
        StatusCode::OK
    }
    async fn readyz(axum::extract::State(db): axum::extract::State<DbPool>) -> StatusCode {
        if sqlx::query("SELECT 1").execute(&db.0).await.is_ok() {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
    axum::Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::parse_device_id;

    #[test]
    fn device_id_rejects_serial_and_noncanonical_hex() {
        assert!(parse_device_id(&"ab".repeat(32)).is_ok());
        assert!(parse_device_id("serial-001").is_err());
        assert!(parse_device_id(&"AB".repeat(32)).is_err());
    }

    #[tokio::test]
    async fn healthz_does_not_claim_database_readiness() {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use tower::ServiceExt;
        let db = crate::DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .unwrap(),
        );
        let router = crate::build_router(db);
        let health = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ready = router
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
