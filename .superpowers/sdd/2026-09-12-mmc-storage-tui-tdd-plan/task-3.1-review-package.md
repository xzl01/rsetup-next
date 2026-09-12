# Task 3.1 Review Package
Base: aa5fa98da8ed8d91c231a7f1327900334e703533
Head: 6ec422f33280045a11a3e6c856a744089f0eb266

## Git Log
6ec422f feat(core): aggregate MMC and unified storage status in Controller

## Git Diff --stat
 crates/rsetup-core/src/actions.rs | 102 ++++++++++++++++++++++++++++++++++++--
 1 file changed, 99 insertions(+), 3 deletions(-)

## Git Diff
diff --git a/crates/rsetup-core/src/actions.rs b/crates/rsetup-core/src/actions.rs
index 57990c4..58befd1 100644
--- a/crates/rsetup-core/src/actions.rs
+++ b/crates/rsetup-core/src/actions.rs
@@ -1,14 +1,14 @@
 use crate::{
-    ActionRun, ActionSpec, ActionStatus, ActivityEvent, NvmeDevice, NvmeManager, NvmeSmartLog,
-    NvmeStatus, ProbeMode, RiskLevel, SourceApplyResult, SourceError, SourcePlan, SourceStatus,
-    collect_snapshot,
+    ActionRun, ActionSpec, ActionStatus, ActivityEvent, MmcDevice, MmcHealth, MmcManager,
+    MmcStatus, NvmeDevice, NvmeManager, NvmeSmartLog, NvmeStatus, ProbeMode, RiskLevel,
+    SourceApplyResult, SourceError, SourcePlan, SourceStatus, StorageStatus, collect_snapshot,
     fan_curve::{
         FanCurveApplyResult, FanCurveManager, FanCurvePlan, FanCurveRequest, FanCurveStatus,
         FanCurveTick,
     },
     hardware::{
         GpioStatus, HardwareError, HardwareManager, LedStatus, OverlayApplyResult, OverlayPlan,
         OverlayStatus, RgbLedConfig, ThermalStatus, VideoFrame, VideoStatus,
     },
     sources::{SourceManager, source_run},
     spi_flash::{
@@ -71,20 +71,21 @@ pub struct Controller {
     mode: ProbeMode,
     policy: ExecutionPolicy,
     synthetic: bool,
     runs: Arc<RwLock<VecDeque<ActionRun>>>,
     activity: Arc<RwLock<VecDeque<ActivityEvent>>>,
     sources: Arc<SourceManager>,
     hardware: Arc<HardwareManager>,
     spi_flash: Arc<SpiFlashManager>,
     fan_curve: Arc<FanCurveManager>,
     nvme: Arc<NvmeManager>,
+    mmc: Arc<MmcManager>,
     overlay_cache: Arc<RwLock<Option<OverlayStatus>>>,
 }
 
 impl Controller {
     pub fn new(mode: ProbeMode, policy: ExecutionPolicy) -> Self {
         let synthetic = mode == ProbeMode::Demo || !cfg!(target_os = "linux");
         let mut activity = VecDeque::new();
         activity.push_back(ActivityEvent {
             id: Uuid::new_v4().to_string(),
             at: Utc::now(),
@@ -106,20 +107,21 @@ impl Controller {
             mode,
             policy,
             synthetic,
             runs: Arc::new(RwLock::new(VecDeque::new())),
             activity: Arc::new(RwLock::new(activity)),
             sources: Arc::new(SourceManager::new(synthetic)),
             hardware: Arc::new(HardwareManager::new(synthetic)),
             spi_flash: Arc::new(SpiFlashManager::new(synthetic)),
             fan_curve: Arc::new(FanCurveManager::new(synthetic)),
             nvme: Arc::new(NvmeManager::new()),
+            mmc: Arc::new(MmcManager::new()),
             overlay_cache: Arc::new(RwLock::new(None)),
         }
     }
 
     pub fn from_environment() -> Self {
         let mode = match env::var("RSETUP_MODE").ok().as_deref() {
             Some("demo") => ProbeMode::Demo,
             Some("live") => ProbeMode::Live,
             _ => ProbeMode::Auto,
         };
@@ -510,20 +512,34 @@ impl Controller {
         self.fan_curve.status()
     }
 
     pub fn nvme_status(&self) -> Result<NvmeStatus, HardwareError> {
         if self.synthetic {
             return Ok(demo_nvme_status());
         }
         Ok(self.nvme.status())
     }
 
+    pub fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
+        if self.synthetic {
+            return Ok(demo_mmc_status());
+        }
+        Ok(self.mmc.status())
+    }
+
+    pub fn storage_status(&self) -> Result<StorageStatus, HardwareError> {
+        Ok(StorageStatus {
+            nvme: self.nvme_status()?,
+            mmc: self.mmc_status()?,
+        })
+    }
+
     pub fn plan_fan_curve(&self, request: &FanCurveRequest) -> Result<FanCurvePlan, HardwareError> {
         self.fan_curve.plan(request)
     }
 
     pub fn apply_fan_curve(
         &self,
         request: &FanCurveRequest,
         plan_token: &str,
         confirmed: bool,
     ) -> Result<FanCurveApplyResult, HardwareError> {
@@ -1831,20 +1847,62 @@ fn demo_nvme_status() -> NvmeStatus {
                 power_on_hours: 120,
                 unsafe_shutdowns: 1,
                 media_errors: 0,
                 num_err_log_entries: 0,
             },
         }],
         message: None,
     }
 }
 
+fn demo_mmc_status() -> MmcStatus {
+    MmcStatus {
+        initialized: true,
+        devices: vec![
+            MmcDevice {
+                name: "mmc0:0001".into(),
+                block_path: "/dev/mmcblk0".into(),
+                card_type: "MMC".into(),
+                model: "FE4MB4".into(),
+                manufacturer: "Samsung (0x000015)".into(),
+                serial: "0x12345678".into(),
+                firmware: "0x01".into(),
+                total_bytes: 62_537_072_640, // ~58.2 GiB
+                health: MmcHealth {
+                    pre_eol_info: 1,
+                    life_time_est_a_percent: Some(10),
+                    life_time_est_b_percent: Some(10),
+                    warning_flags: Vec::new(),
+                },
+            },
+            MmcDevice {
+                name: "mmc1:59b4".into(),
+                block_path: "/dev/mmcblk1".into(),
+                card_type: "SD".into(),
+                model: "SC64G".into(),
+                manufacturer: "SanDisk (0x000045)".into(),
+                serial: "0x87654321".into(),
+                firmware: "0x01".into(),
+                total_bytes: 64_026_691_584, // ~59.6 GiB
+                // SD cards carry no life-time estimate; pre_eol 0 means undefined.
+                health: MmcHealth {
+                    pre_eol_info: 0,
+                    life_time_est_a_percent: None,
+                    life_time_est_b_percent: None,
+                    warning_flags: Vec::new(),
+                },
+            },
+        ],
+        message: None,
+    }
+}
+
 #[cfg(test)]
 mod tests {
     use super::*;
     use crate::{FanCurveConfig, FanCurvePoint};
 
     #[test]
     fn guarded_action_requires_confirmation() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let result = controller.execute("service.ssh-enable", false);
         assert!(matches!(result, Err(ActionError::ConfirmationRequired(_))));
@@ -2008,20 +2066,58 @@ mod tests {
     #[test]
     fn test_controller_nvme_status_live() {
         let controller = Controller::new(ProbeMode::Auto, ExecutionPolicy::DryRun);
         let status = controller.nvme_status().expect("nvme status in live/auto mode");
         // Whether initialized is true or false depends on host, but it must not panic and must return Ok.
         if !status.initialized {
             assert!(status.message.is_some());
         }
     }
 
+    #[test]
+    fn test_controller_mmc_status_demo() {
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let status = controller.mmc_status().expect("mmc status in demo mode");
+        assert!(status.initialized);
+        assert_eq!(status.devices.len(), 2);
+
+        let emmc = &status.devices[0];
+        assert_eq!(emmc.card_type, "MMC");
+        assert_eq!(emmc.block_path, "/dev/mmcblk0");
+        assert_eq!(emmc.total_bytes, 62_537_072_640);
+        assert_eq!(emmc.health.life_time_est_a_percent, Some(10));
+
+        let sd = &status.devices[1];
+        assert_eq!(sd.card_type, "SD");
+        assert_eq!(sd.health.life_time_est_a_percent, None);
+    }
+
+    #[test]
+    fn test_controller_mmc_status_live() {
+        let controller = Controller::new(ProbeMode::Live, ExecutionPolicy::DryRun);
+        let status = controller.mmc_status().expect("mmc status in live mode");
+        // Whether initialized is true or false depends on host, but it must not panic and must return Ok.
+        if !status.initialized {
+            assert!(status.message.is_some());
+        }
+    }
+
+    #[test]
+    fn test_controller_storage_status_demo() {
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let storage = controller.storage_status().expect("storage status in demo mode");
+        assert!(storage.nvme.initialized);
+        assert_eq!(storage.nvme.devices.len(), 1);
+        assert!(storage.mmc.initialized);
+        assert_eq!(storage.mmc.devices.len(), 2);
+    }
+
     #[test]
     fn demo_catalog_contains_migrated_service_lifecycle_actions() {
         let actions = action_catalog(true);
         let identifiers = actions
             .iter()
             .map(|action| action.id.as_str())
             .collect::<std::collections::HashSet<_>>();
         for expected in [
             "service.ssh-install",
             "service.ssh-enable",
