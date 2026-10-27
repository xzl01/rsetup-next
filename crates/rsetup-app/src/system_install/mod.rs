use rsetup_core::{
    BlockDeviceBackend, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallError, SystemInstallManager, SystemInstallRequest, SystemInstallTarget,
    SystemMount,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) mod command;
pub(crate) mod privileged;
pub(crate) mod render;

pub(crate) use command::run_system_command;
#[cfg(test)]
pub(crate) use privileged::PRIVILEGED_HELPER;
pub(crate) use privileged::{
    InstallApplyError, effective_uid, install_needs_privilege, run_privileged_system_install,
    system_install_error_code,
};

/// The two CLI switches the install path needs, captured before the command
/// value is consumed.
#[derive(Clone, Copy)]
pub(crate) struct InstallOptions {
    /// `--live-execution`: a real write is permitted.
    pub(crate) live: bool,
    /// Live inspection on Linux, the labelled demo provider elsewhere — the same
    /// resolution the rest of the CLI uses for its probe mode.
    pub(crate) synthetic: bool,
}

fn system_install_backend(options: InstallOptions) -> Arc<dyn BlockDeviceBackend> {
    if options.synthetic {
        Arc::new(SyntheticBackend::default())
    } else {
        Arc::new(SystemBlockBackend::new(None))
    }
}

/// Without `--live-execution` the install runs against a dry-run writer: the
/// real device identity is shown and the acknowledgement is judged by the core,
/// but no byte reaches a device.
pub(in crate::system_install) fn system_install_manager(
    options: InstallOptions,
) -> SystemInstallManager {
    let backend = system_install_backend(options);
    let backend: Arc<dyn BlockDeviceBackend> = if options.live {
        backend
    } else {
        Arc::new(DryRunBackend::new(backend))
    };
    SystemInstallManager::with_backend(backend, Arc::new(SystemImageOpener), options.synthetic)
}

/// Reports the real target identity but never writes and never asks the kernel
/// to re-read a partition table.
struct DryRunBackend {
    inner: Arc<dyn BlockDeviceBackend>,
    written: Mutex<u64>,
}

impl DryRunBackend {
    fn new(inner: Arc<dyn BlockDeviceBackend>) -> Self {
        Self {
            inner,
            written: Mutex::new(0),
        }
    }
}

impl BlockDeviceBackend for DryRunBackend {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        self.inner.targets()
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        self.inner.mounts()
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        *self.written.lock().expect("dry-run counter") += chunk.len() as u64;
        Ok(())
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> Result<Vec<rsetup_core::SystemInstallPartition>, SystemInstallError> {
        Err(SystemInstallError::Unsupported(
            "dry run: the partition table was not re-read".into(),
        ))
    }

    fn is_dry_run(&self) -> bool {
        true
    }
}

pub(in crate::system_install) fn system_install_request(
    target: &str,
    image: &Path,
) -> SystemInstallRequest {
    SystemInstallRequest {
        target_id: target.to_owned(),
        image_path: Some(image.display().to_string()),
        image_id: None,
    }
}

#[cfg(test)]
pub(in crate::system_install) mod fixtures {
    use super::*;
    use std::{fs, path::PathBuf};

    /// Scratch directory under `target/`, removed when the test ends so image
    /// sized fixtures never accumulate.
    pub(in crate::system_install) struct InstallScratch(PathBuf);

    impl std::ops::Deref for InstallScratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for InstallScratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(in crate::system_install) fn install_scratch(name: &str) -> InstallScratch {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/system-install-cli-tests")
            .join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(&root).expect("create scratch directory");
        InstallScratch(root)
    }

    /// A minimal image with a valid MBR: two sectors of table, one partition.
    pub(in crate::system_install) fn mbr_image() -> Vec<u8> {
        let mut image = vec![0u8; 2 * 1024 * 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x83;
        image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
        image
    }

    pub(in crate::system_install) fn demo_install_manager(live: bool) -> SystemInstallManager {
        system_install_manager(InstallOptions {
            live,
            synthetic: true,
        })
    }

    pub(in crate::system_install) fn install_request_for(
        target: &str,
        image: &Path,
    ) -> SystemInstallRequest {
        system_install_request(target, image)
    }
}
