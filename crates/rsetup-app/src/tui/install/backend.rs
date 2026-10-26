use rsetup_core::{
    BlockDeviceBackend, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallError, SystemInstallManager, SystemInstallPartition, SystemInstallTarget,
    SystemMount,
};

/// A write that only counts bytes: the panel can show the real device identity
/// and still let the core judge the confirmation while nothing reaches a device.
/// The CLI keeps an equivalent private wrapper in `main.rs`; hoisting one shared
/// implementation is a follow-up because `main.rs` is out of scope here.
struct DryRunWriter {
    inner: std::sync::Arc<dyn BlockDeviceBackend>,
}

impl DryRunWriter {
    fn new(inner: std::sync::Arc<dyn BlockDeviceBackend>) -> Self {
        Self { inner }
    }
}

impl BlockDeviceBackend for DryRunWriter {
    fn targets(&self) -> std::result::Result<Vec<SystemInstallTarget>, SystemInstallError> {
        self.inner.targets()
    }

    fn mounts(&self) -> std::result::Result<Vec<SystemMount>, SystemInstallError> {
        self.inner.mounts()
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        _chunk: &[u8],
    ) -> std::result::Result<(), SystemInstallError> {
        Ok(())
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> std::result::Result<Vec<SystemInstallPartition>, SystemInstallError> {
        Err(SystemInstallError::Unsupported(
            "dry run: the partition table was not re-read".into(),
        ))
    }
}

/// Live inspection on Linux, the labelled demo provider elsewhere; a real write
/// additionally needs the controller's live policy, exactly like the CLI needs
/// `--live-execution`.
pub(in crate::tui) fn system_install_manager(synthetic: bool, live: bool) -> SystemInstallManager {
    let backend: std::sync::Arc<dyn BlockDeviceBackend> = if synthetic {
        std::sync::Arc::new(SyntheticBackend::default())
    } else {
        std::sync::Arc::new(SystemBlockBackend::new(None))
    };
    let backend: std::sync::Arc<dyn BlockDeviceBackend> = if live {
        backend
    } else {
        std::sync::Arc::new(DryRunWriter::new(backend))
    };
    SystemInstallManager::with_backend(backend, std::sync::Arc::new(SystemImageOpener), synthetic)
}
