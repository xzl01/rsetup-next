use rsetup_controller::{
    BootstrapSecretSink, ControllerConfig, DbPool, bootstrap_admin, build_router, migrate,
};

struct LogSecretSink(std::path::PathBuf);
impl BootstrapSecretSink for LogSecretSink {
    fn emit(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), rsetup_controller::ControllerError> {
        write_private_secret(&self.0, username, password).map_err(|error| {
            rsetup_controller::ControllerError::Config(format!(
                "unable to record first-boot secret in private file: {error}; administrator recovery requires separate reviewed procedure"
            ))
        })
    }
}

fn write_private_secret(
    path: &std::path::Path,
    username: &str,
    password: &str,
) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    // create_new + 0600 protect the file, not its parent: deploy under a private directory.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{username}: {password}")?;
    file.sync_all()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ControllerConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    migrate(&db).await?;
    bootstrap_admin(
        &db,
        &LogSecretSink(
            std::env::var_os("CONTROLLER_BOOTSTRAP_SECRET_LOG")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "controller-bootstrap-secret.log".into()),
        ),
    )
    .await?;
    let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
    axum::serve(listener, build_router(db)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_sink_reports_actual_create_new_failure_without_overwrite() {
        let path =
            std::env::temp_dir().join(format!("rsetup-secret-failure-{}", uuid::Uuid::new_v4()));
        let sink = LogSecretSink(path.clone());
        write_private_secret(&path, "admin", "first").unwrap();
        let result = sink.emit("admin", "second");
        assert!(result.is_err(), "sink must report write failure");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "admin: first\n");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn first_boot_secret_is_private_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("rsetup-controller-secret-{}", uuid::Uuid::new_v4()));
        write_private_secret(&path, "admin", "first").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_private_secret(&path, "admin", "second").is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("first"));
        std::fs::remove_file(path).unwrap();
    }
}
