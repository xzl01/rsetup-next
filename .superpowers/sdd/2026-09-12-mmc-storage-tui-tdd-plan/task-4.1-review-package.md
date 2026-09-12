# Task 4.1 Review Package
Base: 5cb92c1187969ee48208f99b76705924056d334e
Head: 7f694544e06e700dd38f0391661ddb502d0b1561

## Git Log
7f69454 feat(app): add unified storage TUI dictionary keys

## Git Diff --stat
 crates/rsetup-app/src/i18n.rs | 103 ++++++++++++++++++++++++++++++++++++++++++
 1 file changed, 103 insertions(+)

## Git Diff
diff --git a/crates/rsetup-app/src/i18n.rs b/crates/rsetup-app/src/i18n.rs
index 4b04560..b40f8cc 100644
--- a/crates/rsetup-app/src/i18n.rs
+++ b/crates/rsetup-app/src/i18n.rs
@@ -181,20 +181,49 @@ impl Locale {
             (Self::ZhCn, "source_files_short") => "个文件",
             (Self::ZhCn, "source_rolled_back") => "软件包刷新失败，已自动回滚。",
             (Self::ZhCn, "source_plan_ready") => "预演完成，未修改系统文件。",
             (Self::ZhCn, "nvme_telemetry") => "NVMe 存储遥测",
             (Self::ZhCn, "nvme_healthy") => "正常",
             (Self::ZhCn, "nvme_warning") => "告警",
             (Self::ZhCn, "nvme_endurance") => "已用寿命",
             (Self::ZhCn, "nvme_spare") => "可用备用",
             (Self::ZhCn, "nvme_io") => "累计读写",
             (Self::ZhCn, "nvme_not_detected") => "未检测到 NVMe 存储设备，模块未激活",
+            (Self::ZhCn, "storage_telemetry") => "存储状态",
+            (Self::ZhCn, "storage_not_detected") => "未检测到 NVMe 或 MMC 存储设备",
+            (Self::ZhCn, "storage_nvme") => "NVMe",
+            (Self::ZhCn, "storage_emmc") => "eMMC",
+            (Self::ZhCn, "storage_sd") => "SD 卡",
+            (Self::ZhCn, "storage_healthy") => "正常",
+            (Self::ZhCn, "storage_warning") => "告警",
+            (Self::ZhCn, "storage_critical") => "故障",
+            (Self::ZhCn, "storage_health") => "状态",
+            (Self::ZhCn, "storage_temperature") => "温度",
+            (Self::ZhCn, "storage_endurance") => "已用寿命",
+            (Self::ZhCn, "storage_spare") => "可用备用",
+            (Self::ZhCn, "storage_io") => "累计读写",
+            (Self::ZhCn, "storage_capacity") => "格式化容量",
+            (Self::ZhCn, "storage_model") => "设备型号",
+            (Self::ZhCn, "storage_serial") => "序列号",
+            (Self::ZhCn, "storage_manufacturer") => "厂商",
+            (Self::ZhCn, "storage_firmware") => "固件",
+            (Self::ZhCn, "storage_life_a") => "SLC 寿命",
+            (Self::ZhCn, "storage_life_b") => "MLC 寿命",
+            (Self::ZhCn, "storage_pre_eol") => "预警",
+            (Self::ZhCn, "storage_eol_normal") => "正常",
+            (Self::ZhCn, "storage_eol_warning") => "预警(80%)",
+            (Self::ZhCn, "storage_eol_urgent") => "紧急",
+            (Self::ZhCn, "storage_eol_undefined") => "未定义",
+            (Self::ZhCn, "storage_na") => "不支持",
+            (Self::ZhCn, "storage_more_devices") => "更多设备",
+            (Self::ZhCn, "storage_read") => "读",
+            (Self::ZhCn, "storage_written") => "写",
             (_, "live_linux_only") => "live execution is only supported on Linux SBC hosts",
             (_, "not_available") => "n/a",
             (_, "synthetic_data") => "SYNTHETIC DATA",
             (_, "network_interfaces") => "network interface(s)",
             (_, "capability_signals") => "capability signal(s)",
             (_, "alerts") => "alert(s)",
             (_, "root") => "root",
             (_, "planned_steps") => "Planned steps",
             (_, "raw_output") => "Raw output",
             (_, "probe") => "probe",
@@ -253,20 +282,51 @@ impl Locale {
             (_, "source_plan_ready") => "Dry run complete; no system file was changed.",
             (_, "nvme_telemetry") => "NVMe Storage Telemetry",
             (_, "nvme_healthy") => "Healthy",
             (_, "nvme_warning") => "Warning",
             (_, "nvme_endurance") => "Used Endurance",
             (_, "nvme_spare") => "Available Spare",
             (_, "nvme_io") => "Data Read/Written",
             (_, "nvme_not_detected") => {
                 "No NVMe storage devices detected; module is uninitialized."
             }
+            (_, "storage_telemetry") => "Storage Devices",
+            (_, "storage_not_detected") => {
+                "No NVMe or MMC storage devices detected."
+            }
+            (_, "storage_nvme") => "NVMe",
+            (_, "storage_emmc") => "eMMC",
+            (_, "storage_sd") => "SD Card",
+            (_, "storage_healthy") => "Healthy",
+            (_, "storage_warning") => "Warning",
+            (_, "storage_critical") => "Critical",
+            (_, "storage_health") => "Health",
+            (_, "storage_temperature") => "Temp",
+            (_, "storage_endurance") => "Used Endurance",
+            (_, "storage_spare") => "Available Spare",
+            (_, "storage_io") => "Data Read/Written",
+            (_, "storage_capacity") => "Capacity",
+            (_, "storage_model") => "Model",
+            (_, "storage_serial") => "Serial",
+            (_, "storage_manufacturer") => "Manufacturer",
+            (_, "storage_firmware") => "Firmware",
+            (_, "storage_life_a") => "SLC Life",
+            (_, "storage_life_b") => "MLC Life",
+            (_, "storage_pre_eol") => "Pre-EOL",
+            (_, "storage_eol_normal") => "Normal",
+            (_, "storage_eol_warning") => "Warning(80%)",
+            (_, "storage_eol_urgent") => "Urgent",
+            (_, "storage_eol_undefined") => "Undefined",
+            (_, "storage_na") => "N/A",
+            (_, "storage_more_devices") => "more device(s)",
+            (_, "storage_read") => "Read",
+            (_, "storage_written") => "Written",
             _ => "",
         }
     }
 
     pub fn risk(self, risk: RiskLevel) -> &'static str {
         match (self, risk) {
             (Self::ZhCn, RiskLevel::Safe) => "安全",
             (Self::ZhCn, RiskLevel::Guarded) => "需确认",
             (Self::ZhCn, RiskLevel::High) => "高风险",
             (Self::ZhCn, RiskLevel::Critical) => "严重风险",
@@ -594,20 +654,63 @@ mod tests {
                 "No NVMe storage devices detected; module is uninitialized.",
             ),
         ];
 
         for (key, zh, en) in expected {
             assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
             assert_eq!(Locale::En.text(key), en, "En translation for {key}");
         }
     }
 
+    #[test]
+    fn test_storage_tui_dictionary_keys() {
+        let expected = [
+            ("storage_telemetry", "存储状态", "Storage Devices"),
+            (
+                "storage_not_detected",
+                "未检测到 NVMe 或 MMC 存储设备",
+                "No NVMe or MMC storage devices detected.",
+            ),
+            ("storage_nvme", "NVMe", "NVMe"),
+            ("storage_emmc", "eMMC", "eMMC"),
+            ("storage_sd", "SD 卡", "SD Card"),
+            ("storage_healthy", "正常", "Healthy"),
+            ("storage_warning", "告警", "Warning"),
+            ("storage_critical", "故障", "Critical"),
+            ("storage_health", "状态", "Health"),
+            ("storage_temperature", "温度", "Temp"),
+            ("storage_endurance", "已用寿命", "Used Endurance"),
+            ("storage_spare", "可用备用", "Available Spare"),
+            ("storage_io", "累计读写", "Data Read/Written"),
+            ("storage_capacity", "格式化容量", "Capacity"),
+            ("storage_model", "设备型号", "Model"),
+            ("storage_serial", "序列号", "Serial"),
+            ("storage_manufacturer", "厂商", "Manufacturer"),
+            ("storage_firmware", "固件", "Firmware"),
+            ("storage_life_a", "SLC 寿命", "SLC Life"),
+            ("storage_life_b", "MLC 寿命", "MLC Life"),
+            ("storage_pre_eol", "预警", "Pre-EOL"),
+            ("storage_eol_normal", "正常", "Normal"),
+            ("storage_eol_warning", "预警(80%)", "Warning(80%)"),
+            ("storage_eol_urgent", "紧急", "Urgent"),
+            ("storage_eol_undefined", "未定义", "Undefined"),
+            ("storage_na", "不支持", "N/A"),
+            ("storage_more_devices", "更多设备", "more device(s)"),
+            ("storage_read", "读", "Read"),
+            ("storage_written", "写", "Written"),
+        ];
+        for (key, zh, en) in expected {
+            assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
+            assert_eq!(Locale::En.text(key), en, "En translation for {key}");
+        }
+    }
+
     #[test]
     fn chinese_action_copy_is_keyed_by_stable_id() {
         assert_eq!(
             Locale::ZhCn.action_title("system.reboot", "Reboot device"),
             "重启设备"
         );
     }
 
     #[test]
     fn confirmation_copy_names_sbc_settings_and_network_risk() {
