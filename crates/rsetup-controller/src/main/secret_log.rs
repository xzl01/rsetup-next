//! 首次启动管理员凭据落盘实现。
//!
//! 提供 [`LogSecretSink`] 将随机初始口令原子写入受 0600 权限保护的本地文件，
//! 仅在初次建库启动（bootstrap）时由生产入口调用。

use std::path::PathBuf;

use rsetup_controller::{BootstrapSecretSink, ControllerError};

pub(super) struct LogSecretSink(pub(super) PathBuf);

impl BootstrapSecretSink for LogSecretSink {
    fn emit(&self, username: &str, password: &str) -> Result<(), ControllerError> {
        write_private_secret(&self.0, username, password).map_err(|error| {
            ControllerError::Config(format!(
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
