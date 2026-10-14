mod actions;
mod deviceinfo_probe;
mod efi_overlay;
mod fan_curve;
mod hardware;
mod health;
pub mod mmc;
mod model;
pub mod nvme;
mod pinout;
mod probe;
mod sources;
mod spi_flash;
pub mod storage;
mod system_install;
mod transaction;
mod video;

pub use actions::{ActionError, Controller, ExecutionPolicy};
pub use fan_curve::{
    FanCurveApplyResult, FanCurveConfig, FanCurveDevice, FanCurvePlan, FanCurvePoint,
    FanCurveRequest, FanCurveResolvedPoint, FanCurveStatus, FanCurveTick, FanCurveZone,
};
pub use hardware::{
    CoolingDevice, GpioChip, GpioConnector, GpioPin, GpioStatus, HardwareError, LedDevice,
    LedSavedState, LedStatus, OverlayApplyResult, OverlayBootChange, OverlayBootConfig,
    OverlayChange, OverlayEntry, OverlayPlan, OverlayStatus, RgbLedConfig, RgbLedGroup,
    ThermalStatus, ThermalZone, VideoDevice, VideoFrame, VideoStatus,
};
pub use health::{mmc_health_state, nvme_health_state};
pub use mmc::{MmcError, MmcManager};
pub use model::{
    ActionRun, ActionSpec, ActionStatus, ActivityEvent, Alert, AlertLevel, BlockDeviceKind,
    Capability, DeviceIdentity, DeviceSnapshot, HealthState, InstallImageFormat, MetricSet,
    MmcDevice, MmcHealth, MmcStatus, NetworkInterface, NvmeDevice, NvmeSmartLog, NvmeStatus,
    ProbeMetadata, ProbeMode, RiskLevel, ServiceState, ServiceSummary, StorageMetric,
    StorageStatus, SystemInstallApplyResult, SystemInstallImage, SystemInstallPartition,
    SystemInstallPlan, SystemInstallRequest, SystemInstallTarget, SystemInstallVerification,
    SystemInstallVerificationState, TelemetryError, TelemetryErrorKind, TelemetryReadState,
    TelemetryStatus,
};
pub use nvme::{NvmeError, NvmeManager};
pub use probe::collect_snapshot;
pub use sources::{
    MirrorBenchmark, MirrorProbe, MirrorProbeStatus, MirrorProvider, SourceApplyResult,
    SourceError, SourceFileChange, SourceFileSummary, SourceKind, SourcePlan, SourceStatus,
    provider_catalog,
};
pub use spi_flash::{
    SpiBootComponent, SpiBootImage, SpiFlashApplyResult, SpiFlashDevice, SpiFlashPlan,
    SpiFlashRequest, SpiFlashStatus,
};
pub use storage::{StorageReader, SystemStorageReader};
pub use system_install::{
    BlockDeviceBackend, ImageOpener, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallError, SystemInstallManager, SystemMount,
};
