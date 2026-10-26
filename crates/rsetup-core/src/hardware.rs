//! Hardware management facade: shared error and manager types plus the per-domain module tree.
use std::path::PathBuf;
use thiserror::Error;

mod gpio;
mod led;
mod overlay;
mod shared;
mod thermal;
mod video;

pub use self::gpio::{GpioChip, GpioConnector, GpioPin, GpioStatus};
pub use self::led::{LedDevice, LedSavedState, LedStatus, RgbLedConfig, RgbLedGroup};
pub use self::overlay::{
    OverlayApplyResult, OverlayBootChange, OverlayBootConfig, OverlayChange, OverlayEntry,
    OverlayPlan, OverlayStatus,
};
pub(crate) use self::overlay::{read_overlays, uefi_boot_mode};
pub(crate) use self::shared::{display_path, fingerprint, hardware_run};
pub use self::thermal::{CoolingDevice, ThermalStatus, ThermalZone};
pub use self::video::{VideoDevice, VideoFrame, VideoStatus};

#[derive(Debug, Error)]
pub enum HardwareError {
    #[error("hardware tool is unavailable: {0}")]
    Unsupported(String),
    #[error("invalid hardware selection: {0}")]
    InvalidInput(String),
    #[error("hardware resource conflict: {0}")]
    Conflict(String),
    #[error("confirmation is required before changing hardware configuration")]
    ConfirmationRequired,
    #[error("preview the hardware change before applying it")]
    PlanRequired,
    #[error("the hardware state changed after preview; create a fresh plan")]
    StalePlan,
    #[error("changing hardware configuration requires root privileges")]
    RootRequired,
    #[error("administrator authorization canceled")]
    AuthorizationCanceled,
    #[error("administrator authorization failed: {0}")]
    Authorization(String),
    #[error("unable to manage hardware: {0}")]
    Io(String),
}

#[derive(Debug, Clone)]
pub(crate) struct HardwareManager {
    root: PathBuf,
    synthetic: bool,
}

impl HardwareManager {
    pub(crate) fn new(synthetic: bool) -> Self {
        Self {
            root: PathBuf::from("/"),
            synthetic,
        }
    }

    #[cfg(test)]
    pub(crate) fn at_root(root: PathBuf) -> Self {
        Self {
            root,
            synthetic: false,
        }
    }
}
