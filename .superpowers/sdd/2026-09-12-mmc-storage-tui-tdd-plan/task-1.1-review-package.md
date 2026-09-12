# Task 1.1 Review Package
Base: 6752403893a4e66dd3fbd9282d859398d9700370
Head: ebc4f698623b89bfa1ed7bf3f139b491612f6c95

## Git Log
ebc4f69 feat(core): define MMC and unified storage data models

## Git Diff
diff --git a/crates/rsetup-core/src/lib.rs b/crates/rsetup-core/src/lib.rs
index f1ae6e6..0652a9e 100644
--- a/crates/rsetup-core/src/lib.rs
+++ b/crates/rsetup-core/src/lib.rs
@@ -19,4 +19,5 @@ pub use model::{
     ActionRun, ActionSpec, ActionStatus, ActivityEvent, BenchmarkHistoryEntry, BenchmarkRunRecord,
     BenchmarkScope, CoolingDevice, CoolingDeviceType, DeviceIdentity, DeviceMetrics,
     DeviceSnapshot, FanCurveConfig, FanCurvePoint, FanCurvePolicy, FanCurveProfile,
-    FanCurveRequest, FanCurveStatus, HardwareError, MirrorBenchmark, NetworkInterface, NvmeDevice,
-    NvmeSmartLog, NvmeStatus, OverlayStatus, OverlayTarget, PackageInventory, RiskLevel,
-    ServiceSignal, ServiceState, SourceCandidate, SourceError, SourceFamily, SourcePlan,
-    SourcePreference, SourceProvider, SourceStatus, SpiFlashImage, SpiFlashInstallPlan,
-    SpiFlashStatus, SpiFlashTarget, StorageMetric, ThermalSensor, ThermalStatus,
+    FanCurveRequest, FanCurveStatus, HardwareError, MirrorBenchmark, MmcDevice, MmcHealth,
+    MmcStatus, NetworkInterface, NvmeDevice, NvmeSmartLog, NvmeStatus, OverlayStatus, OverlayTarget,
+    PackageInventory, RiskLevel, ServiceSignal, ServiceState, SourceCandidate, SourceError,
+    SourceFamily, SourcePlan, SourcePreference, SourceProvider, SourceStatus, SpiFlashImage,
+    SpiFlashInstallPlan, SpiFlashStatus, SpiFlashTarget, StorageMetric, StorageStatus,
+    ThermalSensor, ThermalStatus,
 };
diff --git a/crates/rsetup-core/src/model.rs b/crates/rsetup-core/src/model.rs
index cdff1d7..e2a106f 100644
--- a/crates/rsetup-core/src/model.rs
+++ b/crates/rsetup-core/src/model.rs
@@ -214,3 +214,40 @@ pub struct NvmeSmartLog {
     pub media_errors: u64,
     pub num_err_log_entries: u64,
 }
+
+#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
+#[serde(rename_all = "camelCase")]
+pub struct MmcHealth {
+    pub pre_eol_info: u8,
+    pub life_time_est_a_percent: Option<u8>,
+    pub life_time_est_b_percent: Option<u8>,
+    pub warning_flags: Vec<String>,
+}
+
+#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
+#[serde(rename_all = "camelCase")]
+pub struct MmcDevice {
+    pub name: String,
+    pub block_path: String,
+    pub card_type: String,
+    pub model: String,
+    pub manufacturer: String,
+    pub serial: String,
+    pub firmware: String,
+    pub total_bytes: u64,
+    pub health: MmcHealth,
+}
+
+#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
+#[serde(rename_all = "camelCase")]
+pub struct MmcStatus {
+    pub initialized: bool,
+    pub devices: Vec<MmcDevice>,
+    pub message: Option<String>,
+}
+
+#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
+#[serde(rename_all = "camelCase")]
+pub struct StorageStatus {
+    pub nvme: NvmeStatus,
+    pub mmc: MmcStatus,
+}
