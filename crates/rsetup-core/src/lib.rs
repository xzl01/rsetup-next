mod actions;
mod fan_curve;
mod hardware;
mod model;
mod pinout;
mod probe;
mod sources;
mod spi_flash;

pub use actions::{ActionError, Controller, ExecutionPolicy};
pub use fan_curve::{
    FanCurveApplyResult, FanCurveConfig, FanCurveDevice, FanCurvePlan, FanCurvePoint,
    FanCurveRequest, FanCurveResolvedPoint, FanCurveStatus, FanCurveTick, FanCurveZone,
};
pub use hardware::{
    CoolingDevice, GpioChip, GpioConnector, GpioPin, GpioStatus, HardwareError, LedDevice,
    LedSavedState, LedStatus, OverlayApplyResult, OverlayChange, OverlayEntry, OverlayPlan,
    OverlayStatus, RgbLedConfig, RgbLedGroup, ThermalStatus, ThermalZone, VideoDevice, VideoFrame,
    VideoStatus,
};
pub use model::{
    ActionRun, ActionSpec, ActionStatus, ActivityEvent, Alert, AlertLevel, Capability,
    DeviceIdentity, DeviceSnapshot, MetricSet, NetworkInterface, ProbeMode, RiskLevel,
    ServiceState, ServiceSummary, StorageMetric,
};
pub use probe::collect_snapshot;
pub use sources::{
    MirrorProvider, SourceApplyResult, SourceError, SourceFileChange, SourceFileSummary,
    SourceKind, SourcePlan, SourceStatus, provider_catalog,
};
pub use spi_flash::{
    SpiBootComponent, SpiBootImage, SpiFlashApplyResult, SpiFlashDevice, SpiFlashPlan,
    SpiFlashRequest, SpiFlashStatus,
};
