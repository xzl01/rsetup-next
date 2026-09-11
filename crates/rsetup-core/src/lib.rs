mod actions;
mod efi_overlay;
mod fan_curve;
mod hardware;
mod model;
mod pinout;
mod probe;
mod sources;
mod spi_flash;
mod transaction;
mod video;
pub mod nvme;

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
pub use model::{
    ActionRun, ActionSpec, ActionStatus, ActivityEvent, Alert, AlertLevel, Capability,
    DeviceIdentity, DeviceSnapshot, MetricSet, MmcDevice, MmcHealth, MmcStatus, NetworkInterface,
    NvmeDevice, NvmeSmartLog, NvmeStatus, ProbeMode, RiskLevel, ServiceState, ServiceSummary,
    StorageMetric, StorageStatus,
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
