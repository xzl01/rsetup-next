# Final Whole-Branch Review Package
Base: 6752403893a4e66dd3fbd9282d859398d9700370 (docs/spec+plan commit)
Head: 7db6429288f37796643afe477e698edd308e1da9

## Git Log
7db6429 chore: ignore SDD scratch workspace and untrack accidentally committed report
98735dc feat(app): unified multi-device storage TUI panel
4e61103 feat(app): wire MMC status into TUI app state
7f69454 feat(app): add unified storage TUI dictionary keys
5cb92c1 feat(app): add hardware mmc and storage CLI commands
6ec422f feat(core): aggregate MMC and unified storage status in Controller
aa5fa98 feat(core): add MMC_IOC_CMD ioctl fallback for EXT_CSD health data
9237ef6 fix(core): filter for primary mmc block device node only
e341310 feat(core): implement MmcManager and sysfs multi-device probing
505ae81 feat(core): implement MMC life time and EXT_CSD conversion parsers
ebc4f69 feat(core): define MMC and unified storage data models

## Git Diff --stat
 .gitignore                        |   3 +
 crates/rsetup-app/src/i18n.rs     | 103 ++++++++
 crates/rsetup-app/src/main.rs     | 339 ++++++++++++++++++++++++-
 crates/rsetup-app/src/tui.rs      | 515 ++++++++++++++++++++++++++++++--------
 crates/rsetup-core/src/actions.rs | 102 +++++++-
 crates/rsetup-core/src/lib.rs     |   7 +-
 crates/rsetup-core/src/mmc.rs     | 477 +++++++++++++++++++++++++++++++++++
 crates/rsetup-core/src/mmc/sys.rs | 336 +++++++++++++++++++++++++
 crates/rsetup-core/src/model.rs   |  92 +++++++
 9 files changed, 1865 insertions(+), 109 deletions(-)

## Git Diff (code files only)
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
diff --git a/crates/rsetup-app/src/main.rs b/crates/rsetup-app/src/main.rs
index 620694d..1fe3315 100644
--- a/crates/rsetup-app/src/main.rs
+++ b/crates/rsetup-app/src/main.rs
@@ -152,20 +152,30 @@ enum HardwareCommands {
     /// Inspect or set fan and thermal policy / 查看或设置风扇与温控策略
     Thermal {
         #[command(subcommand)]
         command: ThermalCommands,
     },
     /// Inspect NVMe storage devices and SMART health / 查看 NVMe 存储设备与 SMART 健康状态
     Nvme {
         #[arg(long)]
         json: bool,
     },
+    /// Inspect MMC/eMMC/SD storage devices and endurance health / 查看 MMC/eMMC/SD 存储设备与寿命健康状态
+    Mmc {
+        #[arg(long)]
+        json: bool,
+    },
+    /// Inspect unified storage (NVMe + MMC) summary / 查看统一存储（NVMe + MMC）汇总
+    Storage {
+        #[arg(long)]
+        json: bool,
+    },
 }
 
 #[derive(Debug, Subcommand)]
 enum OverlayCommands {
     Status {
         #[arg(long)]
         json: bool,
         /// Authorize reading protected EFI configuration / 授权读取 EFI 配置
         #[arg(long)]
         authorize: bool,
@@ -665,20 +675,36 @@ async fn main() -> Result<()> {
                 },
             },
             HardwareCommands::Nvme { json } => {
                 let status = controller.nvme_status()?;
                 if json {
                     println!("{}", serde_json::to_string_pretty(&status)?);
                 } else {
                     println!("{}", format_nvme_status(&status, locale));
                 }
             }
+            HardwareCommands::Mmc { json } => {
+                let status = controller.mmc_status()?;
+                if json {
+                    println!("{}", serde_json::to_string_pretty(&status)?);
+                } else {
+                    println!("{}", format_mmc_status(&status, locale));
+                }
+            }
+            HardwareCommands::Storage { json } => {
+                let status = controller.storage_status()?;
+                if json {
+                    println!("{}", serde_json::to_string_pretty(&status)?);
+                } else {
+                    println!("{}", format_storage_status(&status, locale));
+                }
+            }
         },
         Commands::Tui => tui::run(controller, locale)?,
         Commands::Serve { listen } => server::serve(controller, listen).await?,
         Commands::Doctor { json } => print_doctor(&controller, locale, json)?,
     }
     Ok(())
 }
 
 fn parse_fan_curve_point(value: &str) -> std::result::Result<FanCurvePoint, String> {
     let (temperature, speed) = value
@@ -1139,20 +1165,141 @@ fn format_nvme_status(status: &rsetup_core::NvmeStatus, locale: Locale) -> Strin
             } else {
                 dev.smart.warning_flags.join(", ")
             };
             out.push(format!("  Critical Warning: {}", warning_str));
         }
     }
 
     out.join("\n")
 }
 
+fn format_mmc_status(status: &rsetup_core::MmcStatus, locale: Locale) -> String {
+    let is_zh = locale == Locale::ZhCn;
+    if !status.initialized {
+        if let Some(msg) = &status.message {
+            if is_zh {
+                return format!("未检测到 MMC/SD 存储设备，模块未激活。（{}）", msg);
+            } else {
+                return format!(
+                    "No MMC/SD storage devices detected; module is uninitialized. ({})",
+                    msg
+                );
+            }
+        } else if is_zh {
+            return "未检测到 MMC/SD 存储设备，模块未激活。".into();
+        } else {
+            return "No MMC/SD storage devices detected; module is uninitialized.".into();
+        }
+    }
+
+    if status.devices.is_empty() {
+        return if is_zh {
+            "已检测到 MMC 控制器，但未发现可用设备。".into()
+        } else {
+            "MMC host detected, but no storage devices found.".into()
+        };
+    }
+
+    let mut out = Vec::new();
+    for (idx, dev) in status.devices.iter().enumerate() {
+        if idx > 0 {
+            out.push("".to_string());
+        }
+        let header = if is_zh {
+            format!("=== 存储设备: {} ({}) [{}] ===", dev.name, dev.block_path, dev.card_type)
+        } else {
+            format!(
+                "=== Storage Device: {} ({}) [{}] ===",
+                dev.name, dev.block_path, dev.card_type
+            )
+        };
+        out.push(header);
+
+        let size_str = format_bytes(dev.total_bytes);
+        if is_zh {
+            out.push(format!("  类型:             {}", dev.card_type));
+            out.push(format!("  型号:             {}", dev.model));
+            out.push(format!("  厂商:             {}", dev.manufacturer));
+            out.push(format!("  序列号:           {}", dev.serial));
+            out.push(format!("  固件版本:         {}", dev.firmware));
+            out.push(format!(
+                "  总容量:           {} ({} 字节)",
+                size_str, dev.total_bytes
+            ));
+            let pre_eol_str = match dev.health.pre_eol_info {
+                0 => "未定义".to_string(),
+                1 => "正常".to_string(),
+                2 => "预警 (80% 寿命)".to_string(),
+                3 => "紧急 (建议更换)".to_string(),
+                other => format!("未知 ({other})"),
+            };
+            let life_str = |value: Option<u8>| -> String {
+                match value {
+                    Some(p) => format!("{p}%"),
+                    None => "不支持".to_string(),
+                }
+            };
+            let life_a_str = life_str(dev.health.life_time_est_a_percent);
+            let life_b_str = life_str(dev.health.life_time_est_b_percent);
+            out.push(format!("  预 EOL 状态:       {pre_eol_str}"));
+            out.push(format!("  寿命估计 (A/B):   {life_a_str} / {life_b_str}"));
+            let warning_str = if dev.health.warning_flags.is_empty() {
+                "无".to_string()
+            } else {
+                dev.health.warning_flags.join(", ")
+            };
+            out.push(format!("  告警标志:         {warning_str}"));
+        } else {
+            out.push(format!("  Type:             {}", dev.card_type));
+            out.push(format!("  Model:            {}", dev.model));
+            out.push(format!("  Manufacturer:   {}", dev.manufacturer));
+            out.push(format!("  Serial Number:    {}", dev.serial));
+            out.push(format!("  Firmware:         {}", dev.firmware));
+            out.push(format!(
+                "  Total Capacity:   {} ({} bytes)",
+                size_str, dev.total_bytes
+            ));
+            let pre_eol_str = match dev.health.pre_eol_info {
+                0 => "Undefined".to_string(),
+                1 => "Normal".to_string(),
+                2 => "Warning (80% endurance)".to_string(),
+                3 => "Urgent (replace soon)".to_string(),
+                other => format!("Unknown ({other})"),
+            };
+            let life_str = |value: Option<u8>| -> String {
+                match value {
+                    Some(p) => format!("{p}%"),
+                    None => "N/A".to_string(),
+                }
+            };
+            let life_a_str = life_str(dev.health.life_time_est_a_percent);
+            let life_b_str = life_str(dev.health.life_time_est_b_percent);
+            out.push(format!("  Pre-EOL:            {pre_eol_str}"));
+            out.push(format!("  Life Time Est (A/B): {life_a_str} / {life_b_str}"));
+            let warning_str = if dev.health.warning_flags.is_empty() {
+                "None".to_string()
+            } else {
+                dev.health.warning_flags.join(", ")
+            };
+            out.push(format!("  Warning Flags:    {warning_str}"));
+        }
+    }
+
+    out.join("\n")
+}
+
+fn format_storage_status(status: &rsetup_core::StorageStatus, locale: Locale) -> String {
+    let nvme = format_nvme_status(&status.nvme, locale);
+    let mmc = format_mmc_status(&status.mmc, locale);
+    format!("{nvme}\n\n{mmc}")
+}
+
 fn percent(value: u64, total: u64) -> f32 {
     if total == 0 {
         0.0
     } else {
         value as f32 / total as f32 * 100.0
     }
 }
 
 fn print_json_or_debug<T>(value: &T, json: bool) -> Result<()>
 where
@@ -1213,21 +1360,23 @@ fn decode_base64(value: &str) -> Option<Vec<u8>> {
             output.push(bits as u8);
         }
     }
     Some(output)
 }
 
 #[cfg(test)]
 mod tests {
     use super::*;
     use clap::Parser;
-    use rsetup_core::{NvmeDevice, NvmeSmartLog, NvmeStatus};
+    use rsetup_core::{
+        MmcDevice, MmcHealth, MmcStatus, NvmeDevice, NvmeSmartLog, NvmeStatus, StorageStatus,
+    };
 
     #[test]
     fn hardware_cli_parses_nvme_subcommand_and_flags() {
         let cli = Cli::try_parse_from(["rsetup-next", "hardware", "nvme"]).expect("parse nvme");
         match cli.command {
             Some(Commands::Hardware {
                 command: HardwareCommands::Nvme { json },
             }) => {
                 assert!(!json);
             }
@@ -1303,20 +1452,208 @@ mod tests {
         assert!(out_zh.contains("Radxa NVMe SSD 256GB"), "zh should contain model");
         assert!(out_zh.contains("42"), "zh should contain temperature 42");
         assert!(out_zh.contains("RADXA2026NVME01"), "zh should contain serial");
 
         let out_en = format_nvme_status(&status, Locale::En);
         assert!(out_en.contains("nvme0"), "en should contain nvme0");
         assert!(out_en.contains("Radxa NVMe SSD 256GB"), "en should contain model");
         assert!(out_en.contains("42"), "en should contain temperature 42");
         assert!(out_en.contains("Temperature"), "en should contain label Temperature");
     }
+
+    #[test]
+    fn hardware_cli_parses_mmc_subcommand_and_flags() {
+        let cli = Cli::try_parse_from(["rsetup-next", "hardware", "mmc"]).expect("parse mmc");
+        match cli.command {
+            Some(Commands::Hardware {
+                command: HardwareCommands::Mmc { json },
+            }) => {
+                assert!(!json);
+            }
+            other => panic!("unexpected command parsed: {:?}", other),
+        }
+
+        let cli_json =
+            Cli::try_parse_from(["rsetup-next", "hardware", "mmc", "--json"]).expect("parse mmc json");
+        match cli_json.command {
+            Some(Commands::Hardware {
+                command: HardwareCommands::Mmc { json },
+            }) => {
+                assert!(json);
+            }
+            other => panic!("unexpected command parsed: {:?}", other),
+        }
+    }
+
+    #[test]
+    fn hardware_cli_parses_storage_subcommand_and_flags() {
+        let cli = Cli::try_parse_from(["rsetup-next", "hardware", "storage"]).expect("parse storage");
+        match cli.command {
+            Some(Commands::Hardware {
+                command: HardwareCommands::Storage { json },
+            }) => {
+                assert!(!json);
+            }
+            other => panic!("unexpected command parsed: {:?}", other),
+        }
+
+        let cli_json = Cli::try_parse_from([
+            "rsetup-next",
+            "hardware",
+            "storage",
+            "--json",
+        ])
+        .expect("parse storage json");
+        match cli_json.command {
+            Some(Commands::Hardware {
+                command: HardwareCommands::Storage { json },
+            }) => {
+                assert!(json);
+            }
+            other => panic!("unexpected command parsed: {:?}", other),
+        }
+    }
+
+    #[test]
+    fn format_mmc_status_uninitialized_en_and_zh() {
+        let uninit = MmcStatus {
+            initialized: false,
+            devices: Vec::new(),
+            message: Some("No MMC/SD devices detected in system".into()),
+        };
+
+        let formatted_zh = format_mmc_status(&uninit, Locale::ZhCn);
+        assert!(
+            formatted_zh.contains("未检测到 MMC/SD"),
+            "ZH output: {formatted_zh}"
+        );
+
+        let formatted_en = format_mmc_status(&uninit, Locale::En);
+        assert!(
+            formatted_en.contains("No MMC/SD"),
+            "EN output: {formatted_en}"
+        );
+    }
+
+    #[test]
+    fn format_mmc_status_initialized_with_devices() {
+        let emmc = MmcDevice {
+            name: "mmc0:0001".into(),
+            block_path: "/dev/mmcblk0".into(),
+            card_type: "MMC".into(),
+            model: "DG4064".into(),
+            manufacturer: "0x45".into(),
+            serial: "0x12345678".into(),
+            firmware: "0x00".into(),
+            total_bytes: 64_000_000_000,
+            health: MmcHealth {
+                pre_eol_info: 1,
+                life_time_est_a_percent: Some(10),
+                life_time_est_b_percent: None,
+                warning_flags: Vec::new(),
+            },
+        };
+        let sd = MmcDevice {
+            name: "mmc1:0001".into(),
+            block_path: "/dev/mmcblk1".into(),
+            card_type: "SD".into(),
+            model: "SU08G".into(),
+            manufacturer: "0x1B".into(),
+            serial: "0x5A4F".into(),
+            firmware: "1.0".into(),
+            total_bytes: 8_000_000_000,
+            health: MmcHealth {
+                pre_eol_info: 1,
+                life_time_est_a_percent: None,
+                life_time_est_b_percent: None,
+                warning_flags: Vec::new(),
+            },
+        };
+        let status = MmcStatus {
+            initialized: true,
+            devices: vec![emmc, sd],
+            message: None,
+        };
+
+        let out_zh = format_mmc_status(&status, Locale::ZhCn);
+        assert!(out_zh.contains("mmc0:0001"), "zh should contain mmc0:0001");
+        assert!(out_zh.contains("/dev/mmcblk0"), "zh should contain block path");
+        assert!(out_zh.contains("MMC"), "zh should contain card type MMC");
+        assert!(out_zh.contains("SD"), "zh should contain card type SD");
+        assert!(out_zh.contains("10%"), "zh should contain life estimate 10%");
+        assert!(out_zh.contains("不支持"), "zh should contain N/A marker");
+
+        let out_en = format_mmc_status(&status, Locale::En);
+        assert!(out_en.contains("mmc0:0001"), "en should contain mmc0:0001");
+        assert!(out_en.contains("N/A"), "en should contain N/A marker");
+        assert!(out_en.contains("Normal"), "en should contain Normal pre-EOL");
+    }
+
+    #[test]
+    fn format_storage_status_contains_both_sections() {
+        let nvme = NvmeStatus {
+            initialized: true,
+            devices: vec![NvmeDevice {
+                name: "nvme0".into(),
+                path: "/dev/nvme0".into(),
+                model: "Radxa NVMe SSD 256GB".into(),
+                serial: "RADXA2026NVME01".into(),
+                firmware: "1.0.0".into(),
+                total_bytes: 256_060_514_304,
+                smart: NvmeSmartLog {
+                    critical_warning: 0,
+                    warning_flags: Vec::new(),
+                    temperature_c: 42.0,
+                    available_spare_percent: 100,
+                    spare_threshold_percent: 10,
+                    percentage_used: 3,
+                    data_read_bytes: 1024 * 1024 * 1024 * 50, // 50 GiB
+                    data_written_bytes: 1024 * 1024 * 1024 * 30, // 30 GiB
+                    host_read_commands: 1000,
+                    host_write_commands: 500,
+                    power_on_hours: 120,
+                    unsafe_shutdowns: 1,
+                    media_errors: 0,
+                    num_err_log_entries: 0,
+                },
+            }],
+            message: None,
+        };
+        let mmc = MmcStatus {
+            initialized: true,
+            devices: vec![MmcDevice {
+                name: "mmc0:0001".into(),
+                block_path: "/dev/mmcblk0".into(),
+                card_type: "MMC".into(),
+                model: "DG4064".into(),
+                manufacturer: "0x45".into(),
+                serial: "0x12345678".into(),
+                firmware: "0x00".into(),
+                total_bytes: 64_000_000_000,
+                health: MmcHealth {
+                    pre_eol_info: 1,
+                    life_time_est_a_percent: Some(10),
+                    life_time_est_b_percent: None,
+                    warning_flags: Vec::new(),
+                },
+            }],
+            message: None,
+        };
+        let status = StorageStatus { nvme, mmc };
+
+        let out = format_storage_status(&status, Locale::En);
+        assert!(out.contains("nvme0"), "storage output should contain nvme0");
+        assert!(
+            out.contains("mmc0:0001"),
+            "storage output should contain mmc0:0001"
+        );
+    }
 }
 
 #[cfg(test)]
 mod decoding_tests {
     use super::decode_base64;
 
     #[test]
     fn base64_requires_canonical_padding() {
         for invalid in ["AB=C", "Zg==AAAA", "Zh==", "Zm9=", "====", "A===", "abc"] {
             assert!(decode_base64(invalid).is_none(), "{invalid}");
diff --git a/crates/rsetup-app/src/tui.rs b/crates/rsetup-app/src/tui.rs
index 9a422e4..5a2b615 100644
--- a/crates/rsetup-app/src/tui.rs
+++ b/crates/rsetup-app/src/tui.rs
@@ -75,20 +75,21 @@ struct App {
     actions: Vec<ActionSpec>,
     selected: usize,
     confirm_pending: bool,
     last_run: Option<ActionRun>,
     source_status: SourceStatus,
     source_plan: Option<SourcePlan>,
     source_picker: bool,
     source_selected: usize,
     notice: Option<String>,
     pub(crate) nvme_status: rsetup_core::NvmeStatus,
+    pub(crate) mmc_status: rsetup_core::MmcStatus,
     benchmark_rx: Option<
         std::sync::mpsc::Receiver<Result<rsetup_core::MirrorBenchmark, rsetup_core::SourceError>>,
     >,
     benchmarks: std::collections::BTreeMap<String, rsetup_core::MirrorBenchmark>,
 }
 
 impl App {
     fn new(controller: Controller, locale: Locale) -> Result<Self> {
         let snapshot = controller.snapshot()?;
         let actions = controller.actions();
@@ -100,34 +101,40 @@ impl App {
             .iter()
             .position(|provider| {
                 Some(provider.id.as_str()) == source_status.current_system_provider.as_deref()
             })
             .unwrap_or(0);
         let nvme_status = controller.nvme_status().unwrap_or_else(|_| rsetup_core::NvmeStatus {
             initialized: false,
             devices: vec![],
             message: Some("Failed to query NVMe status".into()),
         });
+        let mmc_status = controller.mmc_status().unwrap_or_else(|_| rsetup_core::MmcStatus {
+            initialized: false,
+            devices: vec![],
+            message: Some("Failed to query MMC status".into()),
+        });
         Ok(Self {
             controller,
             locale,
             snapshot,
             actions,
             selected: 0,
             confirm_pending: false,
             last_run: None,
             source_status,
             source_plan: None,
             source_picker: false,
             source_selected,
             notice: None,
             nvme_status,
+            mmc_status,
             benchmark_rx: None,
             benchmarks: Default::default(),
         })
     }
 
     fn next(&mut self) {
         if self.source_picker {
             self.source_selected = (self.source_selected + 1)
                 .min(self.source_status.providers.len().saturating_sub(1));
             self.confirm_pending = false;
@@ -196,20 +203,28 @@ impl App {
             .source_status()
             .map_err(|error| anyhow!(self.locale.source_error(&error)))?;
         self.nvme_status = self
             .controller
             .nvme_status()
             .unwrap_or_else(|_| rsetup_core::NvmeStatus {
                 initialized: false,
                 devices: vec![],
                 message: Some("Failed to query NVMe status".into()),
             });
+        self.mmc_status = self
+            .controller
+            .mmc_status()
+            .unwrap_or_else(|_| rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("Failed to query MMC status".into()),
+            });
         if self.source_picker {
             self.update_source_plan();
         }
         Ok(())
     }
 
     fn request_run(&mut self) {
         let Some(action) = self.actions.get(self.selected) else {
             return;
         };
@@ -361,35 +376,65 @@ fn render_header(frame: &mut Frame, app: &App, area: Rect) {
         Paragraph::new(line).block(
             Block::default()
                 .borders(Borders::BOTTOM)
                 .border_style(Style::default().fg(MUTED)),
         ),
         area,
     );
 }
 
 fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
+    // Unified storage card: NVMe devices take 3 content rows each, MMC/SD
+    // devices 2, with one blank row between devices. Long lines wrap inside
+    // the card's inner width (the device line alone wraps to two rows on a
+    // 100-column terminal), so measure the wrapped row count at the card's
+    // width to size the card for what will actually render.
     let has_nvme_devices = app.nvme_status.initialized && !app.nvme_status.devices.is_empty();
-    // The telemetry card needs four content rows plus its two border rows: the
-    // device line alone wraps to two rows on a 100-column terminal, so a shorter
-    // card clips the spare/threshold line instead of showing it.
-    let desired_nvme_height = if has_nvme_devices { 6 } else { 3 };
+    let has_mmc_devices = app.mmc_status.initialized && !app.mmc_status.devices.is_empty();
+    let nvme_count = if has_nvme_devices { app.nvme_status.devices.len() } else { 0 };
+    let mmc_count = if has_mmc_devices { app.mmc_status.devices.len() } else { 0 };
+    let inner_width = (area.width.saturating_sub(2)) as usize;
+    let nvme_rows = if has_nvme_devices {
+        app.nvme_status
+            .devices
+            .iter()
+            .map(|dev| wrapped_rows(&nvme_device_lines(app, dev, inner_width), inner_width))
+            .sum()
+    } else {
+        0
+    };
+    let mmc_rows = if has_mmc_devices {
+        app.mmc_status
+            .devices
+            .iter()
+            .map(|dev| wrapped_rows(&mmc_device_lines(app, dev), inner_width))
+            .sum()
+    } else {
+        0
+    };
+    let device_count = nvme_count + mmc_count;
+    let content_rows = if device_count == 0 {
+        0
+    } else {
+        nvme_rows + mmc_rows + (device_count - 1)
+    };
+    let desired_height = if device_count == 0 { 3 } else { 2 + content_rows };
     // On short viewports keep the two cards above and the service list below
-    // intact rather than growing the telemetry card past the available room.
-    let nvme_headroom = area.height.saturating_sub(6 + 6 + 4);
-    let nvme_height = desired_nvme_height.min(nvme_headroom).max(3);
+    // intact rather than growing the storage card past the available room.
+    let headroom = area.height.saturating_sub(6 + 6 + 4) as usize;
+    let height = desired_height.min(headroom).max(3);
     let rows = Layout::default()
         .direction(Direction::Vertical)
         .constraints([
             Constraint::Length(6),
             Constraint::Length(6),
-            Constraint::Length(nvme_height),
+            Constraint::Length(height as u16),
             Constraint::Min(4),
         ])
         .split(area);
     let cpu = app.snapshot.metrics.cpu_percent.clamp(0.0, 100.0) as u16;
     let memory = percent(
         app.snapshot.metrics.memory_used_bytes,
         app.snapshot.metrics.memory_total_bytes,
     ) as u16;
     let gauges = Layout::default()
         .direction(Direction::Horizontal)
@@ -437,21 +482,21 @@ fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
         temp
     );
     frame.render_widget(
         Paragraph::new(identity)
             .style(Style::default().fg(BONE))
             .block(instrument(app.locale.text("device_core")))
             .wrap(Wrap { trim: true }),
         rows[1],
     );
 
-    render_nvme_summary(frame, app, rows[2]);
+    render_storage_summary(frame, app, rows[2]);
 
     let services = app
         .snapshot
         .services
         .iter()
         .map(|service| {
             format!(
                 "{}  ·  {}  ·  {}",
                 app.locale.service_state(service.state),
                 app.locale.service_label(&service.id, &service.label),
@@ -462,120 +507,260 @@ fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
         .join("\n");
     frame.render_widget(
         Paragraph::new(services)
             .style(Style::default().fg(MUTED))
             .block(instrument(app.locale.text("service_signals")))
             .wrap(Wrap { trim: true }),
         rows[3],
     );
 }
 
-fn render_nvme_summary(frame: &mut Frame, app: &App, area: Rect) {
-    let block = instrument(app.locale.text("nvme_telemetry"));
-    if !app.nvme_status.initialized || app.nvme_status.devices.is_empty() {
-        let msg = Paragraph::new(app.locale.text("nvme_not_detected"))
+fn render_storage_summary(frame: &mut Frame, app: &App, area: Rect) {
+    let block = instrument(app.locale.text("storage_telemetry"));
+
+    let nvme = if app.nvme_status.initialized {
+        app.nvme_status.devices.as_slice()
+    } else {
+        &[]
+    };
+    let mmc = if app.mmc_status.initialized {
+        app.mmc_status.devices.as_slice()
+    } else {
+        &[]
+    };
+
+    if nvme.is_empty() && mmc.is_empty() {
+        let msg = Paragraph::new(app.locale.text("storage_not_detected"))
             .style(Style::default().fg(MUTED))
             .block(block)
             .wrap(Wrap { trim: true });
         frame.render_widget(msg, area);
         return;
     }
 
-    let mut lines = Vec::new();
-    for (idx, dev) in app.nvme_status.devices.iter().enumerate() {
+    // NVMe devices all come first, then the MMC/SD devices. When the card is
+    // too short for all of them, trailing devices are dropped and a "more
+    // devices" hint row is shown instead. Rows are counted after wrapping at
+    // the card's inner width, because the budget is in rendered rows.
+    enum Device<'a> {
+        Nvme(&'a rsetup_core::NvmeDevice),
+        Mmc(&'a rsetup_core::MmcDevice),
+    }
+    let devices: Vec<Device<'_>> = nvme
+        .iter()
+        .map(Device::Nvme)
+        .chain(mmc.iter().map(Device::Mmc))
+        .collect();
+    let budget = area.height.saturating_sub(2) as usize;
+    let inner_width = area.width.saturating_sub(2) as usize;
+    let mut lines: Vec<Line<'static>> = Vec::new();
+    let mut used_rows = 0usize;
+    let mut more = 0usize;
+    for (idx, dev) in devices.iter().enumerate() {
+        let dev_lines = match dev {
+            Device::Nvme(d) => nvme_device_lines(app, d, inner_width),
+            Device::Mmc(d) => mmc_device_lines(app, d),
+        };
+        // A blank row separates consecutive devices; charge it to the second one.
+        let rows = wrapped_rows(&dev_lines, inner_width) + if idx > 0 { 1 } else { 0 };
+        if used_rows + rows > budget {
+            more = devices.len() - idx;
+            break;
+        }
         if idx > 0 {
             lines.push(Line::from(""));
         }
-        let size_str = crate::format_bytes(dev.total_bytes);
-        // Line 1: dev.name, dev.path, model, capacity
-        lines.push(Line::from(vec![
-            Span::styled(format!("{} ", dev.name), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
-            Span::styled(format!("({}) · ", dev.path), Style::default().fg(MUTED)),
-            Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
-            Span::styled(size_str, Style::default().fg(AMBER)),
-        ]));
-
-        // Line 2: Health state, temperature, used endurance, available spare
-        let is_healthy = dev.smart.critical_warning == 0 && dev.smart.warning_flags.is_empty();
-        let (status_text, status_color) = if is_healthy {
-            (app.locale.text("nvme_healthy"), SIGNAL)
-        } else {
-            (app.locale.text("nvme_warning"), CORAL)
-        };
-        let status_label = if app.locale.is_zh() { "状态: " } else { "Health: " };
-        let temp_label = if app.locale.is_zh() { "温度: " } else { "Temp: " };
-        let spare_label = if app.locale.is_zh() { "备用: " } else { "Spare: " };
-        let endurance_label = if app.locale.is_zh() { "已用寿命: " } else { "Used Endurance: " };
-        // "Used Endurance:" is long enough to push the mandatory available-spare
-        // value past the 59 columns the mission panel gets on a 100-column
-        // terminal, so fall back to the short spelling when the full one does
-        // not fit next to the warning flags.
-        let short_endurance_label = if app.locale.is_zh() { endurance_label } else { "Used: " };
-        let inner_width = area.width.saturating_sub(2) as usize;
-
-        let build_telemetry_line = |endurance_label: &'static str| -> Line<'static> {
-            let mut spans = vec![
-                Span::styled(status_label, Style::default().fg(MUTED)),
-                Span::styled(status_text, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
-            ];
-            if !is_healthy && !dev.smart.warning_flags.is_empty() {
-                spans.push(Span::styled(
-                    format!(" ({})", dev.smart.warning_flags.join(", ")),
-                    Style::default().fg(CORAL),
-                ));
-            }
-            spans.extend(vec![
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(temp_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{:.1} °C", dev.smart.temperature_c), Style::default().fg(BONE)),
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(endurance_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{}%", dev.smart.percentage_used), Style::default().fg(BONE)),
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(spare_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{}%", dev.smart.available_spare_percent), Style::default().fg(BONE)),
-            ]);
-            Line::from(spans)
-        };
-        let full_line = build_telemetry_line(endurance_label);
-        let line2 = if full_line.width() > inner_width {
-            build_telemetry_line(short_endurance_label)
+        lines.extend(dev_lines);
+        used_rows += rows;
+    }
+    if more > 0 && budget - used_rows >= 1 {
+        let more_line = if app.locale.is_zh() {
+            format!("... {} {}", app.locale.text("storage_more_devices"), more)
         } else {
-            full_line
+            format!("... {} {}", more, app.locale.text("storage_more_devices"))
         };
-        lines.push(line2);
-
-        // Line 3: Data read & written plus the available-spare threshold
-        let io_label = if app.locale.is_zh() { "读写: " } else { "I/O: " };
-        let threshold_label = if app.locale.is_zh() { "阈值" } else { "threshold" };
-        let read_str = crate::format_bytes(dev.smart.data_read_bytes);
-        let write_str = crate::format_bytes(dev.smart.data_written_bytes);
-
-        lines.push(Line::from(vec![
-            Span::styled(io_label, Style::default().fg(MUTED)),
-            Span::styled(format!("Read {read_str} / Written {write_str}"), Style::default().fg(BONE)),
-            Span::styled(" · ", Style::default().fg(MUTED)),
-            Span::styled(
-                format!("{threshold_label} {}%", dev.smart.spare_threshold_percent),
-                Style::default().fg(BONE),
-            ),
-        ]));
+        lines.push(Line::from(vec![Span::styled(
+            more_line,
+            Style::default().fg(MUTED),
+        )]));
     }
 
     frame.render_widget(
         Paragraph::new(lines)
             .block(block)
             .wrap(Wrap { trim: true }),
         area,
     );
 }
 
+fn wrapped_rows(lines: &[Line<'static>], inner_width: usize) -> usize {
+    // Ratatui wraps each line at the paragraph's inner width; count the
+    // rendered rows the way the paragraph will, so the card height and the
+    // truncation budget agree with what actually fits.
+    if inner_width == 0 {
+        return lines.len().max(1);
+    }
+    lines
+        .iter()
+        .map(|line| {
+            if line.width() == 0 {
+                1
+            } else {
+                (line.width().saturating_sub(1) / inner_width) + 1
+            }
+        })
+        .sum()
+}
+
+fn nvme_device_lines(
+    app: &App,
+    dev: &rsetup_core::NvmeDevice,
+    inner_width: usize,
+) -> Vec<Line<'static>> {
+    let size_str = crate::format_bytes(dev.total_bytes);
+    // Line 1: dev.name, dev.path, model, capacity
+    let line1 = Line::from(vec![
+        Span::styled(format!("{} ", dev.name), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
+        Span::styled(format!("({}) · ", dev.path), Style::default().fg(MUTED)),
+        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
+        Span::styled(size_str, Style::default().fg(AMBER)),
+    ]);
+
+    // Line 2: Health state, temperature, used endurance, available spare
+    let is_healthy = dev.smart.critical_warning == 0 && dev.smart.warning_flags.is_empty();
+    let (status_text, status_color) = if is_healthy {
+        (app.locale.text("nvme_healthy"), SIGNAL)
+    } else {
+        (app.locale.text("nvme_warning"), CORAL)
+    };
+    let status_label = if app.locale.is_zh() { "状态: " } else { "Health: " };
+    let temp_label = if app.locale.is_zh() { "温度: " } else { "Temp: " };
+    let spare_label = if app.locale.is_zh() { "备用: " } else { "Spare: " };
+    let endurance_label = if app.locale.is_zh() { "已用寿命: " } else { "Used Endurance: " };
+    // "Used Endurance:" is long enough to push the mandatory available-spare
+    // value past the 59 columns the mission panel gets on a 100-column
+    // terminal, so fall back to the short spelling when the full one does
+    // not fit next to the warning flags.
+    let short_endurance_label = if app.locale.is_zh() { endurance_label } else { "Used: " };
+
+    let build_telemetry_line = |endurance_label: &'static str| -> Line<'static> {
+        let mut spans = vec![
+            Span::styled(status_label, Style::default().fg(MUTED)),
+            Span::styled(status_text, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
+        ];
+        if !is_healthy && !dev.smart.warning_flags.is_empty() {
+            spans.push(Span::styled(
+                format!(" ({})", dev.smart.warning_flags.join(", ")),
+                Style::default().fg(CORAL),
+            ));
+        }
+        spans.extend(vec![
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(temp_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{:.1} °C", dev.smart.temperature_c), Style::default().fg(BONE)),
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(endurance_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{}%", dev.smart.percentage_used), Style::default().fg(BONE)),
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(spare_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{}%", dev.smart.available_spare_percent), Style::default().fg(BONE)),
+        ]);
+        Line::from(spans)
+    };
+    let full_line = build_telemetry_line(endurance_label);
+    let line2 = if full_line.width() > inner_width {
+        build_telemetry_line(short_endurance_label)
+    } else {
+        full_line
+    };
+
+    // Line 3: Data read & written plus the available-spare threshold
+    let io_label = if app.locale.is_zh() { "读写: " } else { "I/O: " };
+    let threshold_label = if app.locale.is_zh() { "阈值" } else { "threshold" };
+    let read_str = crate::format_bytes(dev.smart.data_read_bytes);
+    let write_str = crate::format_bytes(dev.smart.data_written_bytes);
+
+    let line3 = Line::from(vec![
+        Span::styled(io_label, Style::default().fg(MUTED)),
+        Span::styled(format!("Read {read_str} / Written {write_str}"), Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(
+            format!("{threshold_label} {}%", dev.smart.spare_threshold_percent),
+            Style::default().fg(BONE),
+        ),
+    ]);
+
+    vec![line1, line2, line3]
+}
+
+fn mmc_device_lines(app: &App, dev: &rsetup_core::MmcDevice) -> Vec<Line<'static>> {
+    // Line 1: [type] block_path · model · manufacturer · capacity
+    let type_label = if dev.card_type == "MMC" {
+        app.locale.text("storage_emmc")
+    } else {
+        app.locale.text("storage_sd")
+    };
+    let line1 = Line::from(vec![
+        Span::styled(format!("[{}] ", type_label), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
+        Span::styled(format!("{} · ", dev.block_path), Style::default().fg(BONE)),
+        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
+        Span::styled(format!("{} · ", dev.manufacturer), Style::default().fg(MUTED)),
+        Span::styled(crate::format_bytes(dev.total_bytes), Style::default().fg(AMBER)),
+    ]);
+
+    // Line 2: Health state, SLC/MLC life estimates, pre-EOL warning
+    let health_label = format!("{}: ", app.locale.text("storage_health"));
+    let life_a_label = format!("{}: ", app.locale.text("storage_life_a"));
+    let life_b_label = format!("{}: ", app.locale.text("storage_life_b"));
+    let pre_eol_label = format!("{}: ", app.locale.text("storage_pre_eol"));
+    let (health_text, health_color) =
+        if !dev.health.warning_flags.is_empty() || dev.health.pre_eol_info == 3 {
+            (app.locale.text("storage_critical"), CORAL)
+        } else if dev.health.pre_eol_info == 2 {
+            (app.locale.text("storage_warning"), AMBER)
+        } else {
+            (app.locale.text("storage_healthy"), SIGNAL)
+        };
+    let life_a = dev
+        .health
+        .life_time_est_a_percent
+        .map(|p| format!("{p}%"))
+        .unwrap_or_else(|| app.locale.text("storage_na").to_string());
+    let life_b = dev
+        .health
+        .life_time_est_b_percent
+        .map(|p| format!("{p}%"))
+        .unwrap_or_else(|| app.locale.text("storage_na").to_string());
+    let eol = match dev.health.pre_eol_info {
+        0 => app.locale.text("storage_eol_undefined"),
+        1 => app.locale.text("storage_eol_normal"),
+        2 => app.locale.text("storage_eol_warning"),
+        3 => app.locale.text("storage_eol_urgent"),
+        _ => app.locale.text("storage_eol_undefined"),
+    };
+    let line2 = Line::from(vec![
+        Span::styled(health_label, Style::default().fg(MUTED)),
+        Span::styled(health_text, Style::default().fg(health_color).add_modifier(Modifier::BOLD)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(life_a_label, Style::default().fg(MUTED)),
+        Span::styled(life_a, Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(life_b_label, Style::default().fg(MUTED)),
+        Span::styled(life_b, Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(pre_eol_label, Style::default().fg(MUTED)),
+        Span::styled(eol, Style::default().fg(BONE)),
+    ]);
+
+    vec![line1, line2]
+}
+
 fn render_actions(frame: &mut Frame, app: &mut App, area: Rect) {
     let rows = Layout::default()
         .direction(Direction::Vertical)
         .constraints([Constraint::Min(10), Constraint::Length(8)])
         .split(area);
     if app.source_picker {
         render_source_picker(frame, app, rows);
         return;
     }
     let items: Vec<ListItem> = app
@@ -849,67 +1034,79 @@ mod tests {
     fn test_tui_app_loads_and_refreshes_nvme_status() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let mut app = App::new(controller, Locale::En).expect("init app");
         assert!(app.nvme_status.initialized);
         assert_eq!(app.nvme_status.devices.len(), 1);
         app.refresh().expect("refresh app");
         assert!(app.nvme_status.initialized);
     }
 
     #[test]
-    fn test_render_nvme_summary_demo() {
+    fn test_tui_app_loads_and_refreshes_mmc_status() {
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::En).expect("init app");
+        // Demo mode returns 2 MMC devices (eMMC + SD).
+        assert!(app.mmc_status.initialized);
+        assert_eq!(app.mmc_status.devices.len(), 2);
+        app.refresh().expect("refresh app");
+        assert!(app.mmc_status.initialized);
+        assert_eq!(app.mmc_status.devices.len(), 2);
+    }
+
+    #[test]
+    fn test_render_storage_summary_demo() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let app = App::new(controller, Locale::ZhCn).expect("init app");
         let backend = ratatui::backend::TestBackend::new(80, 25);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 80, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         let norm_text = text.split_whitespace().collect::<Vec<_>>().join("");
         assert!(text.contains("Radxa M.2 NVMe SSD 512GB") || text.contains("nvme0"));
         assert!(text.contains("38.5") || text.contains("温度"));
         // Available spare and its threshold must both be on screen, not clipped.
         assert!(
             norm_text.contains("备用:100%"),
             "Expected available spare '备用: 100%' in buffer, got: {text:?}"
         );
         assert!(
             norm_text.contains("阈值10%"),
             "Expected spare threshold '阈值 10%' in buffer, got: {text:?}"
         );
     }
 
     #[test]
-    fn test_render_nvme_summary_fits_available_spare() {
+    fn test_render_storage_summary_fits_available_spare() {
         // Real device geometry: left panel is 61% of a 100-column terminal,
-        // so the NVMe box has 61 display columns (59 inner columns).
+        // so the storage box has 61 display columns (59 inner columns).
         for (locale, spare_label, threshold_label) in
             [(Locale::ZhCn, "备用", "阈值"), (Locale::En, "Spare", "threshold")]
         {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, locale).expect("init app");
             let mut dev = app.nvme_status.devices[0].clone();
             // Mirror the real Rock 5B device: 100% spare against a 1% threshold.
             dev.smart.available_spare_percent = 100;
             dev.smart.spare_threshold_percent = 1;
             app.nvme_status.devices = vec![dev];
 
             let backend = ratatui::backend::TestBackend::new(61, 6);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
-                    render_nvme_summary(frame, &app, Rect::new(0, 0, 61, 6));
+                    render_storage_summary(frame, &app, Rect::new(0, 0, 61, 6));
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
 
             assert!(
                 norm_text.contains(&format!("{spare_label}:100%")),
                 "Expected available spare '{spare_label}: 100%' inside 59 columns, got: {raw_text:?}"
             );
@@ -918,29 +1115,29 @@ mod tests {
                 "Expected spare threshold '1%' (label '{threshold_label}') in buffer, got: {raw_text:?}"
             );
             assert!(
                 norm_text.contains(threshold_label),
                 "Expected threshold label '{threshold_label}' in buffer, got: {raw_text:?}"
             );
         }
     }
 
     #[test]
-    fn test_render_nvme_summary_uses_full_endurance_label_when_wide() {
+    fn test_render_storage_summary_uses_full_endurance_label_when_wide() {
         let render_norm = |locale: Locale, width: u16| -> String {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let app = App::new(controller, locale).expect("init app");
             let backend = ratatui::backend::TestBackend::new(width, 6);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
-                    render_nvme_summary(frame, &app, Rect::new(0, 0, width, 6));
+                    render_storage_summary(frame, &app, Rect::new(0, 0, width, 6));
                 })
                 .expect("draw");
             terminal
                 .backend()
                 .buffer()
                 .content()
                 .iter()
                 .map(|c| c.symbol())
                 .collect::<String>()
                 .split_whitespace()
@@ -970,134 +1167,246 @@ mod tests {
         let backend = ratatui::backend::TestBackend::new(100, 30);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 render(frame, &mut app);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
-        assert!(norm_text.contains("NVMe存储遥测"), "Expected 'NVMe 存储遥测' in buffer, got: {raw_text}");
+        assert!(norm_text.contains("存储状态"), "Expected '存储状态' in buffer, got: {raw_text}");
         assert!(raw_text.contains("Radxa M.2 NVMe SSD 512GB"), "Expected 'Radxa M.2 NVMe SSD 512GB' in buffer");
         assert!(norm_text.contains("正常"), "Expected '正常' in buffer");
         assert!(raw_text.contains("38.5 °C"), "Expected '38.5 °C' in buffer");
         // Regression for the live Rock 5B screenshot: on a 100x30 terminal the
         // telemetry card must show the compacted health line (including the
         // available spare) and the read/write line with its threshold, instead
         // of clipping the wrapped overflow row.
         assert!(
             norm_text.contains("状态:正常·温度:38.5°C·已用寿命:2%·备用:100%"),
             "Expected compacted telemetry line 2 in buffer, got: {raw_text}"
         );
         assert!(
             norm_text.contains("读写:Read1.14TiB/Written791.62GiB·阈值10%"),
             "Expected telemetry line 3 with spare threshold in buffer, got: {raw_text}"
         );
     }
 
     #[test]
-    fn test_render_full_tui_uninitialized_nvme() {
+    fn test_render_full_tui_no_storage_devices() {
         // Test in Chinese
         {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, Locale::ZhCn).expect("init app");
             app.nvme_status = rsetup_core::NvmeStatus {
                 initialized: false,
                 devices: vec![],
                 message: Some("No NVMe".into()),
             };
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("No MMC".into()),
+            };
             let backend = ratatui::backend::TestBackend::new(100, 30);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
                     render(frame, &mut app);
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
             assert!(
-                norm_text.contains("未检测到NVMe存储设备，模块未激活"),
-                "Expected Chinese uninitialized message in buffer, got: {raw_text}"
+                norm_text.contains("未检测到NVMe或MMC存储设备"),
+                "Expected Chinese no-storage message in buffer, got: {raw_text}"
             );
         }
 
         // Test in English
         {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, Locale::En).expect("init app");
             app.nvme_status = rsetup_core::NvmeStatus {
                 initialized: false,
                 devices: vec![],
                 message: Some("No NVMe".into()),
             };
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("No MMC".into()),
+            };
             let backend = ratatui::backend::TestBackend::new(100, 30);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
                     render(frame, &mut app);
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             assert!(
-                raw_text.contains("No NVMe storage devices detected; module is uninitialized."),
-                "Expected English uninitialized message in buffer, got: {raw_text}"
+                raw_text.contains("No NVMe or MMC storage devices detected."),
+                "Expected English no-storage message in buffer, got: {raw_text}"
             );
         }
     }
 
     #[test]
     fn test_render_tui_small_viewport() {
         let viewports = [(60, 18), (40, 12)];
         for (w, h) in viewports {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, Locale::ZhCn).expect("init app");
             let backend = ratatui::backend::TestBackend::new(w, h);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             let res = terminal.draw(|frame| {
                 render(frame, &mut app);
             });
             assert!(res.is_ok(), "Rendering failed on viewport {w}x{h}");
         }
     }
 
     #[test]
-    fn test_render_nvme_summary_warning_state() {
+    fn test_render_storage_summary_warning_state() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let mut app = App::new(controller, Locale::ZhCn).expect("init app");
         let mut dev = app.nvme_status.devices[0].clone();
         dev.smart.critical_warning = 0x03;
         dev.smart.warning_flags = vec!["spare_below_threshold".into(), "temperature_exceeded".into()];
         app.nvme_status.devices = vec![dev];
 
         let backend = ratatui::backend::TestBackend::new(100, 10);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 100, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
         assert!(norm_text.contains("告警"), "Expected '告警' in buffer, got: {raw_text}");
         assert!(raw_text.contains("spare_below_threshold"), "Expected 'spare_below_threshold' in buffer");
         assert!(raw_text.contains("temperature_exceeded"), "Expected 'temperature_exceeded' in buffer");
 
         // Also test English locale
         app.locale = Locale::En;
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 100, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let text_en = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         assert!(text_en.contains("Warning"), "Expected 'Warning' in buffer, got: {text_en}");
         assert!(text_en.contains("spare_below_threshold"), "Expected 'spare_below_threshold' in buffer");
     }
+
+    #[test]
+    fn test_render_storage_summary_mmc_only() {
+        // MMC devices only: NVMe uninitialized, no not-detected notice shown.
+        // The full TUI at 100x20 clamps the card to its minimum height, so the
+        // card is rendered directly on a large area to check its contents.
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::ZhCn).expect("init app");
+        app.nvme_status = rsetup_core::NvmeStatus {
+            initialized: false,
+            devices: vec![],
+            message: None,
+        };
+        let backend = ratatui::backend::TestBackend::new(100, 20);
+        let mut terminal = Terminal::new(backend).expect("init test terminal");
+        terminal
+            .draw(|frame| {
+                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
+        assert!(raw_text.contains("FE4MB4"), "Expected eMMC model in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk0"), "Expected eMMC path in buffer, got: {raw_text}");
+        assert!(raw_text.contains("SC64G"), "Expected SD model in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk1"), "Expected SD path in buffer, got: {raw_text}");
+        assert!(raw_text.contains("10%"), "Expected eMMC life value in buffer, got: {raw_text}");
+        assert!(norm_text.contains("存储状态"), "Expected card title in buffer, got: {raw_text}");
+
+        // English locale: type labels and the N/A life values for the SD card.
+        app.locale = Locale::En;
+        terminal
+            .draw(|frame| {
+                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_en = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        assert!(raw_en.contains("eMMC"), "Expected 'eMMC' label in buffer, got: {raw_en}");
+        assert!(raw_en.contains("SD Card"), "Expected 'SD Card' label in buffer, got: {raw_en}");
+        assert!(raw_en.contains("N/A"), "Expected 'N/A' life for SD card, got: {raw_en}");
+    }
+
+    #[test]
+    fn test_render_storage_summary_multi_device_all_visible_when_tall() {
+        // On a tall terminal the card has room for all demo devices:
+        // 1 NVMe + 2 MMC must all be visible without truncation.
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::En).expect("init app");
+        let backend = ratatui::backend::TestBackend::new(100, 40);
+        let mut terminal = Terminal::new(backend).expect("init test terminal");
+        terminal
+            .draw(|frame| {
+                render(frame, &mut app);
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        assert!(raw_text.contains("nvme0"), "Expected NVMe device in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk0"), "Expected eMMC device in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk1"), "Expected SD device in buffer, got: {raw_text}");
+    }
+
+    #[test]
+    fn test_render_storage_summary_empty() {
+        for locale in [Locale::ZhCn, Locale::En] {
+            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+            let mut app = App::new(controller, locale).expect("init app");
+            app.nvme_status = rsetup_core::NvmeStatus {
+                initialized: false,
+                devices: vec![],
+                message: None,
+            };
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: None,
+            };
+            let backend = ratatui::backend::TestBackend::new(100, 20);
+            let mut terminal = Terminal::new(backend).expect("init test terminal");
+            terminal
+                .draw(|frame| {
+                    render_storage_summary(frame, &app, Rect::new(0, 0, 80, 10));
+                })
+                .expect("draw");
+            let buffer = terminal.backend().buffer();
+            let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+            let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
+            if locale == Locale::ZhCn {
+                assert!(
+                    norm_text.contains("未检测到NVMe或MMC存储设备"),
+                    "Expected Chinese no-storage message in buffer, got: {raw_text}"
+                );
+            } else {
+                assert!(
+                    raw_text.contains("No NVMe or MMC storage devices detected."),
+                    "Expected English no-storage message in buffer, got: {raw_text}"
+                );
+            }
+        }
+    }
 }
 
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
diff --git a/crates/rsetup-core/src/lib.rs b/crates/rsetup-core/src/lib.rs
index b7d1c99..c8508fe 100644
--- a/crates/rsetup-core/src/lib.rs
+++ b/crates/rsetup-core/src/lib.rs
@@ -2,37 +2,40 @@ mod actions;
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
+pub mod mmc;
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
+pub use mmc::{MmcError, MmcManager};
 pub use model::{
     ActionRun, ActionSpec, ActionStatus, ActivityEvent, Alert, AlertLevel, Capability,
-    DeviceIdentity, DeviceSnapshot, MetricSet, NetworkInterface, NvmeDevice, NvmeSmartLog,
-    NvmeStatus, ProbeMode, RiskLevel, ServiceState, ServiceSummary, StorageMetric,
+    DeviceIdentity, DeviceSnapshot, MetricSet, MmcDevice, MmcHealth, MmcStatus, NetworkInterface,
+    NvmeDevice, NvmeSmartLog, NvmeStatus, ProbeMode, RiskLevel, ServiceState, ServiceSummary,
+    StorageMetric, StorageStatus,
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
diff --git a/crates/rsetup-core/src/mmc.rs b/crates/rsetup-core/src/mmc.rs
new file mode 100644
index 0000000..f105e43
--- /dev/null
+++ b/crates/rsetup-core/src/mmc.rs
@@ -0,0 +1,477 @@
+use crate::model::MmcStatus;
+use std::path::{Path, PathBuf};
+use thiserror::Error;
+
+pub mod sys;
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
+/// MMC/SD device manager.
+#[derive(Debug, Clone)]
+pub struct MmcManager {
+    status: MmcStatus,
+    sysfs_root: PathBuf,
+}
+
+impl Default for MmcManager {
+    fn default() -> Self {
+        Self::new()
+    }
+}
+
+impl MmcManager {
+    /// Probe the system for MMC/SD devices and initialize manager status.
+    pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self {
+        let root = sysfs_root
+            .map(|p| p.to_path_buf())
+            .unwrap_or_else(|| PathBuf::from("/"));
+        let devices = Self::probe_sysfs(&root);
+
+        let status = if devices.is_empty() {
+            MmcStatus {
+                initialized: false,
+                devices: Vec::new(),
+                message: Some("No MMC/SD devices detected in system".into()),
+            }
+        } else {
+            let mmc_devices = devices
+                .into_iter()
+                .filter_map(|name| sys::read_device_sysfs(&root, &name).ok())
+                .collect();
+            MmcStatus {
+                initialized: true,
+                devices: mmc_devices,
+                message: None,
+            }
+        };
+
+        Self {
+            status,
+            sysfs_root: root,
+        }
+    }
+
+    /// Create default instance probing `/`.
+    pub fn new() -> Self {
+        Self::probe_and_init(None)
+    }
+
+    /// Return sysfs root path.
+    pub fn sysfs_root(&self) -> &Path {
+        &self.sysfs_root
+    }
+
+    pub fn status(&self) -> MmcStatus {
+        self.status.clone()
+    }
+
+    pub fn is_initialized(&self) -> bool {
+        self.status.initialized
+    }
+
+    /// Probe `sys/bus/mmc/devices` under the given root.
+    pub fn probe_sysfs(root: &Path) -> Vec<String> {
+        let mmc_bus_dir = if root == Path::new("/") {
+            PathBuf::from("/sys/bus/mmc/devices")
+        } else {
+            root.join("sys/bus/mmc/devices")
+        };
+
+        let mut devices = Vec::new();
+        if let Ok(entries) = std::fs::read_dir(&mmc_bus_dir) {
+            for entry in entries.flatten() {
+                let file_name = entry.file_name();
+                let name = file_name.to_string_lossy().to_string();
+                let dev_dir = entry.path();
+                let type_file = dev_dir.join("type");
+                if let Some(card_type) = sys::read_trimmed_attr(&type_file) {
+                    if card_type == "MMC" || card_type == "SD" {
+                        devices.push(name);
+                    }
+                }
+            }
+        }
+        devices.sort();
+        devices
+    }
+}
+
+fn parse_hex_or_dec_u8(s: &str) -> Option<u8> {
+    let s = s.trim();
+    if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
+        u8::from_str_radix(hex_str, 16).ok()
+    } else {
+        s.parse::<u8>().ok()
+    }
+}
+
+/// Map a raw EXT_CSD / sysfs life-time estimate byte to a percentage.
+///
+/// Per the eMMC specification: `0x00` is "Not defined" and `0x01..=0x0A`
+/// represent 10%..100% wear. `0x0B` means the estimated lifetime has been
+/// exceeded (reported as 101 to distinguish it from an exact 100%); all
+/// other values are reserved/undefined and yield `None`.
+pub fn map_life_time_byte_to_percent(byte_val: u8) -> Option<u8> {
+    match byte_val {
+        0x00 => None,
+        0x01..=0x0A => Some(byte_val * 10),
+        0x0B => Some(101),
+        _ => None,
+    }
+}
+
+pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>) {
+    let tokens: Vec<&str> = s.split_whitespace().collect();
+    if tokens.len() != 2 {
+        return (None, None);
+    }
+
+    let val_a = parse_hex_or_dec_u8(tokens[0])
+        .and_then(map_life_time_byte_to_percent);
+    let val_b = parse_hex_or_dec_u8(tokens[1])
+        .and_then(map_life_time_byte_to_percent);
+
+    (val_a, val_b)
+}
+
+pub fn parse_pre_eol_info_str(s: &str) -> u8 {
+    let val = parse_hex_or_dec_u8(s);
+    match val {
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
+        0x13 => "Micron",
+        0x45 => "SanDisk",
+        0x70 => "Kingston",
+        0x11 => "Toshiba",
+        0xfe => "Micron",
+        _ => "Unknown",
+    }
+}
+
+pub fn format_manufacturer(manfid_str: &str) -> String {
+    let s = manfid_str.trim();
+    let parsed_val = if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
+        u32::from_str_radix(hex_str, 16).ok()
+    } else {
+        s.parse::<u32>().ok()
+    };
+
+    if let Some(val) = parsed_val {
+        let name = parse_manfid_to_name(val);
+        if name != "Unknown" {
+            format!("{} ({})", name, s)
+        } else {
+            s.to_string()
+        }
+    } else {
+        s.to_string()
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
+
+    if life_a.map(|v| v >= 100).unwrap_or(false) {
+        flags.push("life_time_typ_a_exceeded".to_string());
+    }
+    if life_b.map(|v| v >= 100).unwrap_or(false) {
+        flags.push("life_time_typ_b_exceeded".to_string());
+    }
+
+    flags
+}
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn test_parse_life_time() {
+        // Valid pairs
+        assert_eq!(parse_life_time_str("0x01 0x02"), (Some(10), Some(20)));
+        assert_eq!(parse_life_time_str("0x01\n0x02"), (Some(10), Some(20)));
+        assert_eq!(parse_life_time_str("1 2"), (Some(10), Some(20)));
+        assert_eq!(parse_life_time_str("0x0A 0x05"), (Some(100), Some(50)));
+
+        // Exceeded
+        assert_eq!(parse_life_time_str("0x0B 0x0B"), (Some(101), Some(101)));
+
+        // Invalid values / 0x00 (Not defined) / 0x0C (Reserved)
+        assert_eq!(parse_life_time_str("0x00 0x0C"), (None, None));
+        assert_eq!(parse_life_time_str("0x01 0x00"), (Some(10), None));
+        assert_eq!(parse_life_time_str("0x00 0x02"), (None, Some(20)));
+
+        // Empty or invalid format
+        assert_eq!(parse_life_time_str(""), (None, None));
+        assert_eq!(parse_life_time_str("not_a_number"), (None, None));
+        assert_eq!(parse_life_time_str("0x01"), (None, None));
+    }
+
+    #[test]
+    fn test_parse_pre_eol_info() {
+        assert_eq!(parse_pre_eol_info_str("0x01"), 1);
+        assert_eq!(parse_pre_eol_info_str("1"), 1);
+        assert_eq!(parse_pre_eol_info_str("0x02"), 2);
+        assert_eq!(parse_pre_eol_info_str("2"), 2);
+        assert_eq!(parse_pre_eol_info_str("0x03"), 3);
+        assert_eq!(parse_pre_eol_info_str("3"), 3);
+
+        // Invalid / unknown
+        assert_eq!(parse_pre_eol_info_str("0x00"), 0);
+        assert_eq!(parse_pre_eol_info_str("0x04"), 0);
+        assert_eq!(parse_pre_eol_info_str(""), 0);
+        assert_eq!(parse_pre_eol_info_str("foo"), 0);
+    }
+
+    #[test]
+    fn test_format_manufacturer() {
+        assert_eq!(format_manufacturer("0x000015"), "Samsung (0x000015)");
+        assert_eq!(format_manufacturer("0x15"), "Samsung (0x15)");
+        assert_eq!(format_manufacturer("0x000090"), "SK Hynix (0x000090)");
+        assert_eq!(format_manufacturer("0x000013"), "Micron (0x000013)");
+        assert_eq!(format_manufacturer("0x000045"), "SanDisk (0x000045)");
+        assert_eq!(format_manufacturer("0x000070"), "Kingston (0x000070)");
+        assert_eq!(format_manufacturer("0x000011"), "Toshiba (0x000011)");
+        assert_eq!(format_manufacturer("0x0000fe"), "Micron (0x0000fe)");
+        assert_eq!(format_manufacturer("0x000099"), "0x000099");
+        assert_eq!(format_manufacturer("invalid"), "invalid");
+    }
+
+    #[test]
+    fn test_generate_warning_flags() {
+        // Normal
+        assert_eq!(
+            generate_warning_flags(1, Some(50), Some(60)),
+            Vec::<String>::new()
+        );
+
+        // Pre EOL warning
+        assert_eq!(
+            generate_warning_flags(2, Some(50), Some(60)),
+            vec!["pre_eol_warning".to_string()]
+        );
+
+        // Pre EOL urgent
+        assert_eq!(
+            generate_warning_flags(3, Some(50), Some(60)),
+            vec!["pre_eol_urgent".to_string()]
+        );
+
+        // Typ A exceeded
+        assert_eq!(
+            generate_warning_flags(1, Some(100), Some(60)),
+            vec!["life_time_typ_a_exceeded".to_string()]
+        );
+        assert_eq!(
+            generate_warning_flags(1, Some(101), Some(60)),
+            vec!["life_time_typ_a_exceeded".to_string()]
+        );
+
+        // Typ B exceeded
+        assert_eq!(
+            generate_warning_flags(1, Some(50), Some(100)),
+            vec!["life_time_typ_b_exceeded".to_string()]
+        );
+
+        // All flags
+        assert_eq!(
+            generate_warning_flags(3, Some(100), Some(101)),
+            vec![
+                "pre_eol_urgent".to_string(),
+                "life_time_typ_a_exceeded".to_string(),
+                "life_time_typ_b_exceeded".to_string()
+            ]
+        );
+    }
+
+    #[test]
+    fn test_mmc_probing_no_devices() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-none-{}", uuid::Uuid::new_v4()));
+        let manager = MmcManager::probe_and_init(Some(&root));
+        assert!(!manager.is_initialized());
+        let status = manager.status();
+        assert!(!status.initialized);
+        assert_eq!(status.devices.len(), 0);
+        assert_eq!(
+            status.message,
+            Some("No MMC/SD devices detected in system".into())
+        );
+        let _ = std::fs::remove_dir_all(&root);
+    }
+
+    #[test]
+    fn test_mmc_probing_multi_devices() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-multi-{}", uuid::Uuid::new_v4()));
+
+        // mmc0:0001: MMC
+        let dev0_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
+        std::fs::create_dir_all(&dev0_dir).expect("create dev0_dir");
+        std::fs::write(dev0_dir.join("type"), "MMC\n").unwrap();
+        std::fs::write(dev0_dir.join("name"), "FE4MB4\n").unwrap();
+        std::fs::write(dev0_dir.join("manfid"), "0x000015\n").unwrap();
+        std::fs::write(dev0_dir.join("serial"), "0x12345678\n").unwrap();
+        std::fs::write(dev0_dir.join("life_time"), "0x01 0x01\n").unwrap();
+        std::fs::write(dev0_dir.join("pre_eol_info"), "0x01\n").unwrap();
+        let blk0_dir = dev0_dir.join("block/mmcblk0");
+        std::fs::create_dir_all(&blk0_dir).expect("create blk0_dir");
+        let class_blk0 = root.join("sys/class/block/mmcblk0");
+        std::fs::create_dir_all(&class_blk0).expect("create class_blk0");
+        std::fs::write(class_blk0.join("size"), "122142720\n").unwrap();
+
+        // mmc1:59b4: SD
+        let dev1_dir = root.join("sys/bus/mmc/devices/mmc1:59b4");
+        std::fs::create_dir_all(&dev1_dir).expect("create dev1_dir");
+        std::fs::write(dev1_dir.join("type"), "SD\n").unwrap();
+        std::fs::write(dev1_dir.join("name"), "SC64G\n").unwrap();
+        std::fs::write(dev1_dir.join("manfid"), "0x000045\n").unwrap();
+        std::fs::write(dev1_dir.join("serial"), "0x87654321\n").unwrap();
+        let blk1_dir = dev1_dir.join("block/mmcblk1");
+        std::fs::create_dir_all(&blk1_dir).expect("create blk1_dir");
+        let class_blk1 = root.join("sys/class/block/mmcblk1");
+        std::fs::create_dir_all(&class_blk1).expect("create class_blk1");
+        std::fs::write(class_blk1.join("size"), "124735488\n").unwrap();
+
+        // mmc2:0001: SDIO (should be ignored)
+        let dev2_dir = root.join("sys/bus/mmc/devices/mmc2:0001");
+        std::fs::create_dir_all(&dev2_dir).expect("create dev2_dir");
+        std::fs::write(dev2_dir.join("type"), "SDIO\n").unwrap();
+        std::fs::write(dev2_dir.join("name"), "WIFI\n").unwrap();
+
+        let manager = MmcManager::probe_and_init(Some(&root));
+        assert!(manager.is_initialized());
+        assert_eq!(manager.sysfs_root(), root.as_path());
+        let status = manager.status();
+        assert!(status.initialized);
+        assert_eq!(status.message, None);
+        assert_eq!(status.devices.len(), 2);
+
+        let d0 = &status.devices[0];
+        assert_eq!(d0.name, "mmc0:0001");
+        assert_eq!(d0.card_type, "MMC");
+        assert_eq!(d0.model, "FE4MB4");
+        assert_eq!(d0.manufacturer, "Samsung (0x000015)");
+        assert_eq!(d0.serial, "0x12345678");
+        assert_eq!(d0.block_path, "/dev/mmcblk0");
+        assert_eq!(d0.total_bytes, 62537072640);
+        assert_eq!(d0.health.pre_eol_info, 1);
+        assert_eq!(d0.health.life_time_est_a_percent, Some(10));
+        assert_eq!(d0.health.life_time_est_b_percent, Some(10));
+        assert!(d0.health.warning_flags.is_empty());
+
+        let d1 = &status.devices[1];
+        assert_eq!(d1.name, "mmc1:59b4");
+        assert_eq!(d1.card_type, "SD");
+        assert_eq!(d1.model, "SC64G");
+        assert_eq!(d1.manufacturer, "SanDisk (0x000045)");
+        assert_eq!(d1.serial, "0x87654321");
+        assert_eq!(d1.block_path, "/dev/mmcblk1");
+        assert_eq!(d1.total_bytes, 124735488 * 512);
+
+        let _ = std::fs::remove_dir_all(&root);
+    }
+
+    #[test]
+    fn test_map_life_time_byte_to_percent() {
+        assert_eq!(map_life_time_byte_to_percent(0x00), None);
+        assert_eq!(map_life_time_byte_to_percent(0x01), Some(10));
+        assert_eq!(map_life_time_byte_to_percent(0x05), Some(50));
+        assert_eq!(map_life_time_byte_to_percent(0x0A), Some(100));
+        assert_eq!(map_life_time_byte_to_percent(0x0B), Some(101));
+        assert_eq!(map_life_time_byte_to_percent(0x0C), None);
+    }
+
+    #[test]
+    fn test_sysfs_fallback_to_ioctl_when_attributes_missing() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-fallback-{}", uuid::Uuid::new_v4()));
+        let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
+        std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
+        std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
+        std::fs::write(dev_dir.join("name"), "FALLBACK\n").unwrap();
+        std::fs::write(dev_dir.join("manfid"), "0x000015\n").unwrap();
+        std::fs::write(dev_dir.join("serial"), "0xDEADBEEF\n").unwrap();
+        // Deliberately NO life_time / pre_eol_info / fwrev / prv / hwrev files:
+        // this forces the ioctl fallback path in read_device_sysfs.
+        let blk_dir = dev_dir.join("block/mmcblk0");
+        std::fs::create_dir_all(&blk_dir).expect("create blk_dir");
+        let class_blk0 = root.join("sys/class/block/mmcblk0");
+        std::fs::create_dir_all(&class_blk0).expect("create class_blk0");
+        std::fs::write(class_blk0.join("size"), "122142720\n").unwrap();
+
+        // On the test machine there is no /dev/mmcblk0, so the ioctl fallback
+        // fails; the device must still be built normally without panicking:
+        // health stays 0/None, firmware stays empty, block detection is intact.
+        let dev = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev.block_path, "/dev/mmcblk0");
+        assert_eq!(dev.total_bytes, 122_142_720u64 * 512);
+        assert_eq!(dev.health.pre_eol_info, 0);
+        assert_eq!(dev.health.life_time_est_a_percent, None);
+        assert_eq!(dev.health.life_time_est_b_percent, None);
+        assert!(dev.health.warning_flags.is_empty());
+        assert_eq!(dev.firmware, "");
+
+        let _ = std::fs::remove_dir_all(&root);
+    }
+
+    #[test]
+    fn test_mmc_primary_block_device_selection() {
+        let root = std::env::temp_dir().join(format!("rsetup-mmc-blk-filter-{}", uuid::Uuid::new_v4()));
+        let dev_dir = root.join("sys/bus/mmc/devices/mmc0:0001");
+        std::fs::create_dir_all(&dev_dir).expect("create dev_dir");
+        std::fs::write(dev_dir.join("type"), "MMC\n").unwrap();
+        std::fs::write(dev_dir.join("name"), "TEST_MMC\n").unwrap();
+
+        let block_dir = dev_dir.join("block");
+        // Create boot0 as the ONLY file first to see if it mistakenly matches
+        std::fs::create_dir_all(block_dir.join("mmcblk0boot0")).unwrap();
+
+        let class_blk0boot0 = root.join("sys/class/block/mmcblk0boot0");
+        std::fs::create_dir_all(&class_blk0boot0).unwrap();
+        std::fs::write(class_blk0boot0.join("size"), "10\n").unwrap();
+
+        // If only boot0 is present, it shouldn't match mmcblk0
+        let dev = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev.block_path, "");
+
+        // Now add boot0, boot1, rpmb, p1, and mmcblk0
+        std::fs::create_dir_all(block_dir.join("mmcblk0boot1")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0rpmb")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0p1")).unwrap();
+        std::fs::create_dir_all(block_dir.join("mmcblk0")).unwrap();
+
+        let class_blk0 = root.join("sys/class/block/mmcblk0");
+        std::fs::create_dir_all(&class_blk0).unwrap();
+        std::fs::write(class_blk0.join("size"), "1000\n").unwrap();
+
+        let dev2 = sys::read_device_sysfs(&root, "mmc0:0001").expect("read_device_sysfs");
+        assert_eq!(dev2.block_path, "/dev/mmcblk0");
+        assert_eq!(dev2.total_bytes, 1000 * 512);
+
+        let _ = std::fs::remove_dir_all(&root);
+    }
+}
diff --git a/crates/rsetup-core/src/mmc/sys.rs b/crates/rsetup-core/src/mmc/sys.rs
new file mode 100644
index 0000000..ecc00d0
--- /dev/null
+++ b/crates/rsetup-core/src/mmc/sys.rs
@@ -0,0 +1,336 @@
+use super::{
+    format_manufacturer, generate_warning_flags, map_life_time_byte_to_percent,
+    parse_life_time_str, parse_pre_eol_info_str, MmcError,
+};
+use crate::model::{MmcDevice, MmcHealth};
+use std::fs;
+use std::path::{Path, PathBuf};
+
+/// Mirror of the kernel's `struct mmc_ioc_cmd`
+/// (`include/uapi/linux/mmc/ioctl.h`), field-for-field. The kernel requires
+/// this struct to be exactly 72 bytes with `data_ptr` 8-byte aligned, the
+/// same on 32- and 64-bit, which `#[repr(C)]` with the trailing `u64` gives
+/// us here.
+#[repr(C)]
+struct MmcIocCmd {
+    write_flag: libc::c_int,
+    is_acmd: libc::c_int,
+    opcode: u32,
+    arg: u32,
+    response: [u32; 4],
+    flags: u32,
+    blksz: u32,
+    blocks: u32,
+    postsleep_min_us: u32,
+    postsleep_max_us: u32,
+    data_timeout_ns: u32,
+    cmd_timeout_ms: u32,
+    _pad: u32,
+    data_ptr: u64,
+}
+
+/// `MMC_BLOCK_MAJOR` from `include/uapi/linux/major.h`.
+const MMC_BLOCK_MAJOR: u8 = 179;
+
+// _IOWR(MMC_BLOCK_MAJOR, 0, struct mmc_ioc_cmd)
+// dir = _IOC_READ | _IOC_WRITE = 3, size = 72 (0x48), type = 179 (0xB3), nr = 0
+// 3 << 30 | 72 << 16 | 0xB3 << 8 | 0x00 = 0xc048b300
+const MMC_IOC_CMD: libc::c_ulong =
+    3u64 << 30 | 72u64 << 16 | (MMC_BLOCK_MAJOR as u64) << 8;
+
+/// CMD6 — SEND EXT_CSD (include/linux/mmc/core.h).
+const MMC_OPCODE_SEND_EXT_CSD: u32 = 8;
+
+/// R1 response type: (1<<0) | (1<<2) | (1<<4).
+const MMC_RSP_R1: u32 = 0x15;
+
+/// Command carries an associated data transfer (include/linux/mmc/core.h).
+const MMC_CMD_ADTC: u32 = 1 << 5;
+
+// EXT_CSD byte offsets (include/linux/mmc/mmc.h)
+const EXT_CSD_REV: usize = 192;
+const EXT_CSD_SEC_CNT: usize = 212;
+const EXT_CSD_FIRMWARE_VERSION: usize = 254;
+const EXT_CSD_PRE_EOL_INFO: usize = 267;
+const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A: usize = 268;
+const EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B: usize = 269;
+
+/// Parsed subset of the 512-byte EXT_CSD register (CMD6).
+#[derive(Debug, Clone, PartialEq, Eq)]
+pub struct MmcExtCsd {
+    pub rev: u8,
+    /// Capacity in 512-byte sectors (EXT_CSD[212..216], little-endian).
+    pub sec_count: u32,
+    /// ASCII firmware revision, NUL-padded (EXT_CSD[254..262]).
+    pub firmware_version: [u8; 8],
+    pub pre_eol_info: u8,
+    /// Raw byte value: 0x00 undefined .. 0x0B exceeded.
+    pub life_time_est_typ_a: u8,
+    pub life_time_est_typ_b: u8,
+}
+
+/// Parse a 512-byte EXT_CSD buffer. Pure safe code — no device access.
+pub fn parse_ext_csd(buf: &[u8; 512]) -> MmcExtCsd {
+    let mut firmware_version = [0u8; 8];
+    firmware_version.copy_from_slice(&buf[EXT_CSD_FIRMWARE_VERSION..EXT_CSD_FIRMWARE_VERSION + 8]);
+
+    MmcExtCsd {
+        rev: buf[EXT_CSD_REV],
+        sec_count: u32::from_le_bytes([
+            buf[EXT_CSD_SEC_CNT],
+            buf[EXT_CSD_SEC_CNT + 1],
+            buf[EXT_CSD_SEC_CNT + 2],
+            buf[EXT_CSD_SEC_CNT + 3],
+        ]),
+        firmware_version,
+        pre_eol_info: buf[EXT_CSD_PRE_EOL_INFO],
+        life_time_est_typ_a: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A],
+        life_time_est_typ_b: buf[EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B],
+    }
+}
+
+/// Read the full 512-byte EXT_CSD register from an MMC block device node
+/// (e.g. `/dev/mmcblk0`) via the direct `MMC_IOC_CMD` ioctl. Read-only;
+/// same style as `nvme/sys.rs::read_smart_log_raw`.
+pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError> {
+    use std::ffi::CString;
+
+    let c_path = CString::new(dev_path)
+        .map_err(|e| MmcError::Io(format!("Invalid device path {}: {}", dev_path, e)))?;
+
+    // Open read-only
+    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
+    if fd < 0 {
+        let err = std::io::Error::last_os_error();
+        return Err(MmcError::Io(format!("Failed to open {}: {}", dev_path, err)));
+    }
+
+    let mut buf = [0u8; 512];
+
+    let mut cmd = MmcIocCmd {
+        write_flag: 0, // read
+        is_acmd: 0,
+        opcode: MMC_OPCODE_SEND_EXT_CSD,
+        arg: 0, // EXT_CSD read starts at offset 0
+        response: [0; 4],
+        flags: MMC_RSP_R1 | MMC_CMD_ADTC,
+        blksz: 512,
+        blocks: 1,
+        postsleep_min_us: 0,
+        postsleep_max_us: 0,
+        data_timeout_ns: 0,
+        cmd_timeout_ms: 0,
+        _pad: 0,
+        data_ptr: buf.as_mut_ptr() as u64,
+    };
+
+    let ret = unsafe { libc::ioctl(fd, MMC_IOC_CMD, &mut cmd) };
+    let ioctl_err = if ret < 0 {
+        Some(std::io::Error::last_os_error())
+    } else {
+        None
+    };
+
+    unsafe { libc::close(fd) };
+
+    if let Some(err) = ioctl_err {
+        return Err(MmcError::Io(format!(
+            "MMC_IOC_CMD failed on {}: {}",
+            dev_path, err
+        )));
+    }
+
+    Ok(buf)
+}
+
+/// Read a trimmed string from a file if it exists.
+pub fn read_trimmed_attr(path: &Path) -> Option<String> {
+    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
+}
+
+/// Read block device size in bytes from sysfs.
+pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64 {
+    let size_file = if sysfs_root == Path::new("/") {
+        PathBuf::from(format!("/sys/class/block/{}/size", block_name))
+    } else {
+        sysfs_root.join(format!("sys/class/block/{}/size", block_name))
+    };
+
+    if let Ok(size_str) = fs::read_to_string(&size_file) {
+        if let Ok(blocks) = size_str.trim().parse::<u64>() {
+            return blocks.saturating_mul(512);
+        }
+    }
+    0
+}
+
+/// Check if a block device name is a primary mmcblk device (e.g. "mmcblk0", "mmcblk1"),
+/// excluding partitions ("mmcblk0p1"), boot partitions ("mmcblk0boot0"), rpmb ("mmcblk0rpmb"), etc.
+pub fn is_primary_mmcblk(name: &str) -> bool {
+    if let Some(suffix) = name.strip_prefix("mmcblk") {
+        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
+    } else {
+        false
+    }
+}
+
+/// Read sysfs MMC/SD device info and construct an MmcDevice.
+pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError> {
+    let dev_dir = if sysfs_root == Path::new("/") {
+        PathBuf::from(format!("/sys/bus/mmc/devices/{}", dev_name))
+    } else {
+        sysfs_root.join(format!("sys/bus/mmc/devices/{}", dev_name))
+    };
+
+    if !dev_dir.exists() {
+        return Err(MmcError::Io(format!(
+            "MMC device directory does not exist: {}",
+            dev_dir.display()
+        )));
+    }
+
+    let card_type = read_trimmed_attr(&dev_dir.join("type")).unwrap_or_default();
+    if card_type == "SDIO" {
+        return Err(MmcError::NotSupported("SDIO device ignored".to_string()));
+    }
+
+    let model = read_trimmed_attr(&dev_dir.join("name")).unwrap_or_default();
+    let manfid_raw = read_trimmed_attr(&dev_dir.join("manfid")).unwrap_or_default();
+    let manufacturer = format_manufacturer(&manfid_raw);
+    let serial = read_trimmed_attr(&dev_dir.join("serial")).unwrap_or_default();
+
+    let mut firmware = read_trimmed_attr(&dev_dir.join("fwrev"))
+        .or_else(|| read_trimmed_attr(&dev_dir.join("prv")))
+        .or_else(|| read_trimmed_attr(&dev_dir.join("hwrev")))
+        .unwrap_or_default();
+
+    let life_time_str = read_trimmed_attr(&dev_dir.join("life_time")).unwrap_or_default();
+    let (mut life_a, mut life_b) = parse_life_time_str(&life_time_str);
+
+    let pre_eol_str = read_trimmed_attr(&dev_dir.join("pre_eol_info")).unwrap_or_default();
+    let mut pre_eol = parse_pre_eol_info_str(&pre_eol_str);
+
+    // Determine block device
+    // Check dev_dir/block or entries matching block/mmcblk*
+    let mut block_name: Option<String> = None;
+
+    let block_dir = dev_dir.join("block");
+    if block_dir.exists() {
+        if let Ok(entries) = fs::read_dir(&block_dir) {
+            for entry in entries.flatten() {
+                let name = entry.file_name().to_string_lossy().to_string();
+                if is_primary_mmcblk(&name) {
+                    block_name = Some(name);
+                    break;
+                }
+            }
+        }
+    }
+
+    // Fallback: check dev_dir entries directly for primary mmcblk*
+    if block_name.is_none() {
+        if let Ok(entries) = fs::read_dir(&dev_dir) {
+            for entry in entries.flatten() {
+                let name = entry.file_name().to_string_lossy().to_string();
+                if is_primary_mmcblk(&name) {
+                    block_name = Some(name);
+                    break;
+                }
+            }
+        }
+    }
+
+    let (block_path, total_bytes) = if let Some(blk) = block_name {
+        let size = read_block_device_size(sysfs_root, &blk);
+        (format!("/dev/{}", blk), size)
+    } else {
+        (String::new(), 0)
+    };
+
+    // Fallback: when sysfs exposes no life time / pre-EOL info at all, try
+    // reading EXT_CSD directly from the block device via MMC_IOC_CMD. On
+    // failure, silently keep the sysfs results (unwrap_or_default semantics)
+    // — no error logging, device probing is unaffected.
+    if life_a.is_none() && life_b.is_none() && pre_eol == 0 && !block_path.is_empty() {
+        if let Ok(buf) = read_ext_csd_raw(&block_path) {
+            let ext = parse_ext_csd(&buf);
+            pre_eol = ext.pre_eol_info;
+            life_a = map_life_time_byte_to_percent(ext.life_time_est_typ_a);
+            life_b = map_life_time_byte_to_percent(ext.life_time_est_typ_b);
+
+            if firmware.is_empty() {
+                let end = ext
+                    .firmware_version
+                    .iter()
+                    .position(|&b| b == 0)
+                    .unwrap_or(ext.firmware_version.len());
+                firmware = String::from_utf8_lossy(&ext.firmware_version[..end])
+                    .into_owned();
+            }
+        }
+    }
+
+    let warning_flags = generate_warning_flags(pre_eol, life_a, life_b);
+
+    let health = MmcHealth {
+        pre_eol_info: pre_eol,
+        life_time_est_a_percent: life_a,
+        life_time_est_b_percent: life_b,
+        warning_flags,
+    };
+
+    Ok(MmcDevice {
+        name: dev_name.to_string(),
+        block_path,
+        card_type,
+        model,
+        manufacturer,
+        serial,
+        firmware,
+        total_bytes,
+        health,
+    })
+}
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn test_mmc_ioc_cmd_struct_size() {
+        assert_eq!(std::mem::size_of::<MmcIocCmd>(), 72);
+        // `data_ptr` must sit at offset 64 for 8-byte alignment (kernel
+        // comment: the struct must be the same size on 32/64-bit).
+        assert_eq!(
+            std::mem::offset_of!(MmcIocCmd, data_ptr),
+            64,
+            "MmcIocCmd.data_ptr must be 8-byte aligned at offset 64"
+        );
+    }
+
+    #[test]
+    fn test_parse_ext_csd() {
+        let mut buf = [0u8; 512];
+        buf[192] = 8;
+        buf[212..216].copy_from_slice(&122_142_720u32.to_le_bytes());
+        let fw: &[u8] = b"0x01\0\0\0\0";
+        buf[254..262].copy_from_slice(fw);
+        buf[267] = 0x02;
+        buf[268] = 0x05;
+        buf[269] = 0x06;
+
+        let ext = parse_ext_csd(&buf);
+        assert_eq!(ext.rev, 8);
+        assert_eq!(ext.sec_count, 122_142_720);
+        assert_eq!(ext.firmware_version, [b'0', b'x', b'0', b'1', 0, 0, 0, 0]);
+        assert_eq!(ext.pre_eol_info, 0x02);
+        assert_eq!(ext.life_time_est_typ_a, 0x05);
+        assert_eq!(ext.life_time_est_typ_b, 0x06);
+    }
+
+    #[test]
+    fn test_read_ext_csd_raw_nonexistent_device() {
+        let res = read_ext_csd_raw("/dev/nonexistent_mmc_device_xyz");
+        assert!(matches!(res, Err(MmcError::Io(_))));
+    }
+}
diff --git a/crates/rsetup-core/src/model.rs b/crates/rsetup-core/src/model.rs
index aee2e29..a846c33 100644
--- a/crates/rsetup-core/src/model.rs
+++ b/crates/rsetup-core/src/model.rs
@@ -205,10 +205,102 @@ pub struct NvmeSmartLog {
     pub percentage_used: u8,
     pub data_read_bytes: u64,
     pub data_written_bytes: u64,
     pub host_read_commands: u64,
     pub host_write_commands: u64,
     pub power_on_hours: u64,
     pub unsafe_shutdowns: u64,
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
+
+#[cfg(test)]
+mod tests {
+    use super::*;
+
+    #[test]
+    fn mmc_and_storage_models_serialize_and_deserialize() {
+        let health = MmcHealth {
+            pre_eol_info: 1,
+            life_time_est_a_percent: Some(10),
+            life_time_est_b_percent: Some(20),
+            warning_flags: vec!["urgent".to_string()],
+        };
+
+        let device = MmcDevice {
+            name: "mmcblk0".to_string(),
+            block_path: "/dev/mmcblk0".to_string(),
+            card_type: "eMMC".to_string(),
+            model: "DG4064".to_string(),
+            manufacturer: "0x45".to_string(),
+            serial: "0x12345678".to_string(),
+            firmware: "0x00".to_string(),
+            total_bytes: 64000000000,
+            health: health.clone(),
+        };
+
+        let mmc_status = MmcStatus {
+            initialized: true,
+            devices: vec![device.clone()],
+            message: None,
+        };
+
+        let nvme_status = NvmeStatus {
+            initialized: true,
+            devices: vec![],
+            message: None,
+        };
+
+        let storage_status = StorageStatus {
+            nvme: nvme_status.clone(),
+            mmc: mmc_status.clone(),
+        };
+
+        let json = serde_json::to_string(&storage_status).expect("serialize storage_status");
+        assert!(json.contains("\"lifeTimeEstAPercent\":10"));
+        assert!(json.contains("\"blockPath\":\"/dev/mmcblk0\""));
+        assert!(json.contains("\"mmc\":{"));
+        assert!(json.contains("\"nvme\":{"));
+
+        let deserialized: StorageStatus = serde_json::from_str(&json).expect("deserialize storage_status");
+        assert_eq!(deserialized, storage_status);
+    }
+}
+
