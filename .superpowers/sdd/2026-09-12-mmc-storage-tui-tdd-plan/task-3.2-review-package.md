# Task 3.2 Review Package
Base: 6ec422f33280045a11a3e6c856a744089f0eb266
Head: 5cb92c1187969ee48208f99b76705924056d334e

## Git Log
5cb92c1 feat(app): add hardware mmc and storage CLI commands

## Git Diff --stat
 crates/rsetup-app/src/main.rs | 339 +++++++++++++++++++++++++++++++++++++++++-
 1 file changed, 338 insertions(+), 1 deletion(-)

## Git Diff
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
