use crate::hardware::HardwareError;
use crate::mmc::MmcManager;
use crate::model::{MmcStatus, NvmeStatus};
use crate::nvme::NvmeManager;
use std::path::Path;

pub trait StorageReader: Send + Sync {
    fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>;
    fn mmc_status(&self) -> Result<MmcStatus, HardwareError>;
}

pub struct SystemStorageReader {
    nvme: NvmeManager,
    mmc: MmcManager,
}

impl SystemStorageReader {
    pub fn new(nvme_root: Option<&Path>, mmc_root: Option<&Path>) -> Self {
        Self {
            nvme: NvmeManager::probe_and_init(nvme_root),
            mmc: MmcManager::probe_and_init(mmc_root),
        }
    }
}

impl Default for SystemStorageReader {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl StorageReader for SystemStorageReader {
    fn nvme_status(&self) -> Result<NvmeStatus, HardwareError> {
        self.nvme
            .status()
            .map_err(|e| HardwareError::Io(e.to_string()))
    }

    fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
        self.mmc
            .status()
            .map_err(|e| HardwareError::Io(e.to_string()))
    }
}
