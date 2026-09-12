# Task 1.2 Review Package
Base: ebc4f698623b89bfa1ed7bf3f139b491612f6c95
Head: 505ae81e05a39626bb36cb68bc8614cefe94fa21

## Git Log
505ae81 feat(core): implement MMC life time and EXT_CSD conversion parsers

## Git Diff
diff --git a/crates/rsetup-core/src/lib.rs b/crates/rsetup-core/src/lib.rs
index 0652a9e..e0081d5 100644
--- a/crates/rsetup-core/src/lib.rs
+++ b/crates/rsetup-core/src/lib.rs
@@ -10,6 +10,7 @@ pub mod fan_curve;
 pub mod hardware;
 pub mod i18n;
 pub mod led;
+pub mod mmc;
 pub mod model;
 pub mod nvme;
 pub mod overlay;
@@ -19,10 +20,10 @@ pub use model::{
     ActionRun, ActionSpec, ActionStatus, ActivityEvent, BenchmarkHistoryEntry, BenchmarkRunRecord,
     BenchmarkScope, CoolingDevice, CoolingDeviceType, DeviceIdentity, DeviceMetrics,
     DeviceSnapshot, FanCurveConfig, FanCurvePoint, FanCurvePolicy, FanCurveProfile,
-    FanCurveRequest, FanCurveStatus, HardwareError, MirrorBenchmark, MmcDevice, MmcHealth,
-    MmcStatus, NetworkInterface, NvmeDevice, NvmeSmartLog, NvmeStatus, OverlayStatus, OverlayTarget,
-    PackageInventory, RiskLevel, ServiceSignal, ServiceState, SourceCandidate, SourceError,
-    SourceFamily, SourcePlan, SourcePreference, SourceProvider, SourceStatus, SpiFlashImage,
-    SpiFlashInstallPlan, SpiFlashStatus, SpiFlashTarget, StorageMetric, StorageStatus,
-    ThermalSensor, ThermalStatus,
+    FanCurveRequest, FanCurveStatus, HardwareError, MirrorBenchmark, MmcDevice, MmcError,
+    MmcHealth, MmcStatus, NetworkInterface, NvmeDevice, NvmeSmartLog, NvmeStatus, OverlayStatus,
+    OverlayTarget, PackageInventory, RiskLevel, ServiceSignal, ServiceState, SourceCandidate,
+    SourceError, SourceFamily, SourcePlan, SourcePreference, SourceProvider, SourceStatus,
+    SpiFlashImage, SpiFlashInstallPlan, SpiFlashStatus, SpiFlashTarget, StorageMetric,
+    StorageStatus, ThermalSensor, ThermalStatus,
 };
diff --git a/crates/rsetup-core/src/mmc.rs b/crates/rsetup-core/src/mmc.rs
new file mode 100644
index 0000000..fcaea79
--- /dev/null
+++ b/crates/rsetup-core/src/mmc.rs
@@ -0,0 +1,189 @@
+use thiserror::Error;
+
+#[derive(Debug, Error, PartialEq, Eq)]
+pub enum MmcError {
+    #[error("Invalid buffer length: expected {expected}, got {actual}")]
+    InvalidBufferLength { expected: usize, actual: usize },
+    #[error("Device not supported: {0}")]
+    NotSupported(String),
+    #[error("I/O error: {0}")]
+    Io(String),
+}
+
+fn parse_single_byte_hex_or_dec(part: &str) -> Option<u8> {
+    let s = part.trim();
+    if s.is_empty() {
+        return None;
+    }
+    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
+        u8::from_str_radix(hex, 16).ok()
+    } else {
+        s.parse::<u8>().ok()
+    }
+}
+
+fn map_life_time_byte_to_percent(byte_val: u8) -> Option<u8> {
+    match byte_val {
+        0x00 => None,
+        0x01..=0x0A => Some(byte_val.saturating_mul(10)),
+        0x0B => Some(101),
+        _ => None,
+    }
+}
+
+pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>) {
+    let parts: Vec<&str> = s.split_whitespace().collect();
+    if parts.len() < 2 {
+        return (None, None);
+    }
+    let val_a = parse_single_byte_hex_or_dec(parts[0]).and_then(map_life_time_byte_to_percent);
+    let val_b = parse_single_byte_hex_or_dec(parts[1]).and_then(map_life_time_byte_to_percent);
+    (val_a, val_b)
+}
+
+pub fn parse_pre_eol_info_str(s: &str) -> u8 {
+    match parse_single_byte_hex_or_dec(s) {
+        Some(1) => 1,
+        Some(2) => 2,
+        Some(3) => 3,
+        _ => 0,
+    }
+}
+
+pub fn parse_manfid_to_name(manfid: u32) -> &'static str {
+    match manfid {
+        0x15 => "Samsung",
+        0x90 => "SK Hynix",
+        0x13 | 0xfe => "Micron",
+        0x45 => "SanDisk",
+        0x70 => "Kingston",
+        0x11 => "Toshiba",
+        _ => "Unknown",
+    }
+}
+
+pub fn format_manufacturer(manfid_str: &str) -> String {
+    let s = manfid_str.trim();
+    let num = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
+        u32::from_str_radix(hex, 16).ok()
+    } else {
+        s.parse::<u32>().ok()
+    };
+
+    match num {
+        Some(id) => {
+            let name = parse_manfid_to_name(id);
+            if name != "Unknown" {
+                format!("{} ({:#08x})", name, id)
+            } else {
+                format!("{:#08x}", id)
+            }
+        }
+        None => s.to_string(),
+    }
+}
+
+pub fn generate_warning_flags(
+    pre_eol: u8,
+    life_a: Option<u8>,
+    life_b: Option<u8>,
+) -> Vec<String> {
+    let mut flags = Vec::new();
+    if pre_eol == 2 {
+        flags.push("pre_eol_warning".to_string());
+    } else if pre_eol == 3 {
+        flags.push("pre_eol_urgent".to_string());
+    }
+    if life_a.map(|v| v >= 100).unwrap_or(false) {
+        flags.push("life_time_typ_a_exceeded".to_string());
+    }
+    if life_b.map(|v| v >= 100).unwrap_or(false) {
+        flags.push("life_time_typ_b_exceeded".to_string());
+    }
+    flags
+}
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn test_parse_life_time() {
+        assert_eq!(parse_life_time_str("0x01 0x02"), (Some(10), Some(20)));
+        assert_eq!(parse_life_time_str("1 2"), (Some(10), Some(20)));
+        assert_eq!(parse_life_time_str("0x0B 0x01"), (Some(101), Some(10)));
+        assert_eq!(parse_life_time_str("0x00 0x05"), (None, Some(50)));
+        assert_eq!(parse_life_time_str("0x0C 0x01"), (None, Some(10)));
+        assert_eq!(parse_life_time_str("0x01"), (None, None));
+        assert_eq!(parse_life_time_str(""), (None, None));
+        assert_eq!(parse_life_time_str("invalid format"), (None, None));
+    }
+
+    #[test]
+    fn test_parse_pre_eol_info() {
+        assert_eq!(parse_pre_eol_info_str("0x01"), 1);
+        assert_eq!(parse_pre_eol_info_str("1"), 1);
+        assert_eq!(parse_pre_eol_info_str("0x02"), 2);
+        assert_eq!(parse_pre_eol_info_str("0x03"), 3);
+        assert_eq!(parse_pre_eol_info_str("0x00"), 0);
+        assert_eq!(parse_pre_eol_info_str("0x04"), 0);
+        assert_eq!(parse_pre_eol_info_str("invalid"), 0);
+    }
+
+    #[test]
+    fn test_format_manufacturer() {
+        assert_eq!(format_manufacturer("0x000015"), "Samsung (0x000015)");
+        assert_eq!(format_manufacturer("0x15"), "Samsung (0x000015)");
+        assert_eq!(format_manufacturer("0x000090"), "SK Hynix (0x000090)");
+        assert_eq!(format_manufacturer("0x000099"), "0x000099");
+        assert_eq!(format_manufacturer("invalid"), "invalid");
+    }
+
+    #[test]
+    fn test_generate_warning_flags() {
+        assert!(generate_warning_flags(1, Some(10), Some(20)).is_empty());
+        assert_eq!(
+            generate_warning_flags(2, Some(10), Some(20)),
+            vec!["pre_eol_warning".to_string()]
+        );
+        assert_eq!(
+            generate_warning_flags(3, Some(10), Some(20)),
+            vec!["pre_eol_urgent".to_string()]
+        );
+        assert_eq!(
+            generate_warning_flags(1, Some(101), Some(20)),
+            vec!["life_time_typ_a_exceeded".to_string()]
+        );
+        assert_eq!(
+            generate_warning_flags(2, Some(100), Some(101)),
+            vec![
+                "pre_eol_warning".to_string(),
+                "life_time_typ_a_exceeded".to_string(),
+                "life_time_typ_b_exceeded".to_string()
+            ]
+        );
+    }
+}
