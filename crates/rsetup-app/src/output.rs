use crate::i18n::Locale;

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

pub(crate) fn format_nvme_status(status: &rsetup_core::NvmeStatus, locale: Locale) -> String {
    let is_zh = locale == Locale::ZhCn;
    if !status.initialized {
        if let Some(msg) = &status.message {
            if is_zh {
                return format!("未检测到 NVMe 存储设备，模块未激活。（{}）", msg);
            } else {
                return format!(
                    "No NVMe storage devices detected; module is uninitialized. ({})",
                    msg
                );
            }
        } else if is_zh {
            return "未检测到 NVMe 存储设备，模块未激活。".into();
        } else {
            return "No NVMe storage devices detected; module is uninitialized.".into();
        }
    }

    if status.devices.is_empty() {
        return if is_zh {
            "已检测到 NVMe 控制器，但未发现可用命名空间/设备。".into()
        } else {
            "NVMe controller detected, but no storage devices/namespaces found.".into()
        };
    }

    let mut out = Vec::new();
    for (idx, dev) in status.devices.iter().enumerate() {
        if idx > 0 {
            out.push("".to_string());
        }
        let header = if is_zh {
            format!("=== NVMe 设备: {} ({}) ===", dev.name, dev.path)
        } else {
            format!("=== NVMe Device: {} ({}) ===", dev.name, dev.path)
        };
        out.push(header);

        let size_str = format_bytes(dev.total_bytes);
        if is_zh {
            out.push(format!("  型号:             {}", dev.model));
            out.push(format!("  序列号:           {}", dev.serial));
            out.push(format!("  固件版本:         {}", dev.firmware));
            out.push(format!(
                "  总容量:           {} ({} 字节)",
                size_str, dev.total_bytes
            ));

            if let Some(smart) = &dev.smart {
                out.push(format!("  当前温度:         {:.1} °C", smart.temperature_c));
                out.push(format!(
                    "  备用空间/阈值:    {}% / {}%",
                    smart.available_spare_percent, smart.spare_threshold_percent
                ));
                out.push(format!("  已使用寿命:       {}%", smart.percentage_used));
                out.push(format!(
                    "  数据读写量:       读取 {} / 写入 {}",
                    format_bytes(smart.data_read_bytes),
                    format_bytes(smart.data_written_bytes)
                ));
                out.push(format!(
                    "  通电时间/不安全关机: {} 小时 / {} 次",
                    smart.power_on_hours, smart.unsafe_shutdowns
                ));
                out.push(format!(
                    "  错误计数:         介质错误 {} / 错误日志项 {}",
                    smart.media_errors, smart.num_err_log_entries
                ));
                let warning_str = if smart.warning_flags.is_empty() {
                    "无".to_string()
                } else {
                    smart.warning_flags.join(", ")
                };
                out.push(format!("  告警状态:         {}", warning_str));
            } else {
                let reason = match &dev.telemetry.error {
                    Some(err) => match err.kind {
                        rsetup_core::TelemetryErrorKind::PermissionDenied => {
                            format!("不可读取 (权限不足, code {})", err.code.unwrap_or(13))
                        }
                        rsetup_core::TelemetryErrorKind::NvmeStatus => {
                            format!("不可读取 (协议状态码 {})", err.code.unwrap_or(0))
                        }
                        rsetup_core::TelemetryErrorKind::Io => {
                            if let Some(code) = err.code {
                                format!("不可读取 (I/O 错误, code {})", code)
                            } else {
                                "不可读取 (I/O 错误)".to_string()
                            }
                        }
                    },
                    None => "不可读取".to_string(),
                };
                out.push(format!("  健康状态:         {}", reason));
            }
        } else {
            out.push(format!("  Model:            {}", dev.model));
            out.push(format!("  Serial Number:    {}", dev.serial));
            out.push(format!("  Firmware:         {}", dev.firmware));
            out.push(format!(
                "  Total Capacity:   {} ({} bytes)",
                size_str, dev.total_bytes
            ));

            if let Some(smart) = &dev.smart {
                out.push(format!("  Temperature:      {:.1} °C", smart.temperature_c));
                out.push(format!(
                    "  Available Spare:  {}% (threshold: {}%)",
                    smart.available_spare_percent, smart.spare_threshold_percent
                ));
                out.push(format!("  Percentage Used:  {}%", smart.percentage_used));
                out.push(format!(
                    "  Data Read/Write:  Read {} / Written {}",
                    format_bytes(smart.data_read_bytes),
                    format_bytes(smart.data_written_bytes)
                ));
                out.push(format!(
                    "  Power-on/Shutdown: {} hrs / {} unsafe shutdowns",
                    smart.power_on_hours, smart.unsafe_shutdowns
                ));
                out.push(format!(
                    "  Error Counts:     Media errors: {} / Error entries: {}",
                    smart.media_errors, smart.num_err_log_entries
                ));
                let warning_str = if smart.warning_flags.is_empty() {
                    "None".to_string()
                } else {
                    smart.warning_flags.join(", ")
                };
                out.push(format!("  Critical Warning: {}", warning_str));
            } else {
                let reason = match &dev.telemetry.error {
                    Some(err) => match err.kind {
                        rsetup_core::TelemetryErrorKind::PermissionDenied => {
                            format!(
                                "Unavailable (Permission denied, code {})",
                                err.code.unwrap_or(13)
                            )
                        }
                        rsetup_core::TelemetryErrorKind::NvmeStatus => {
                            format!("Unavailable (Protocol status {})", err.code.unwrap_or(0))
                        }
                        rsetup_core::TelemetryErrorKind::Io => {
                            if let Some(code) = err.code {
                                format!("Unavailable (I/O error, code {})", code)
                            } else {
                                "Unavailable (I/O error)".to_string()
                            }
                        }
                    },
                    None => "Unavailable".to_string(),
                };
                out.push(format!("  Health:           {}", reason));
            }
        }
    }

    out.join("\n")
}

pub(crate) fn format_mmc_status(status: &rsetup_core::MmcStatus, locale: Locale) -> String {
    let is_zh = locale == Locale::ZhCn;
    if !status.initialized {
        if let Some(msg) = &status.message {
            if is_zh {
                return format!("未检测到 MMC/SD 存储设备，模块未激活。（{}）", msg);
            } else {
                return format!(
                    "No MMC/SD storage devices detected; module is uninitialized. ({})",
                    msg
                );
            }
        } else if is_zh {
            return "未检测到 MMC/SD 存储设备，模块未激活。".into();
        } else {
            return "No MMC/SD storage devices detected; module is uninitialized.".into();
        }
    }

    if status.devices.is_empty() {
        return if is_zh {
            "已检测到 MMC 控制器，但未发现可用设备。".into()
        } else {
            "MMC host detected, but no storage devices found.".into()
        };
    }

    let mut out = Vec::new();
    for (idx, dev) in status.devices.iter().enumerate() {
        if idx > 0 {
            out.push("".to_string());
        }
        let header = if is_zh {
            format!(
                "=== 存储设备: {} ({}) [{}] ===",
                dev.name, dev.block_path, dev.card_type
            )
        } else {
            format!(
                "=== Storage Device: {} ({}) [{}] ===",
                dev.name, dev.block_path, dev.card_type
            )
        };
        out.push(header);

        let size_str = format_bytes(dev.total_bytes);
        if is_zh {
            out.push(format!("  类型:             {}", dev.card_type));
            out.push(format!("  型号:             {}", dev.model));
            out.push(format!("  厂商:             {}", dev.manufacturer));
            out.push(format!("  序列号:           {}", dev.serial));
            out.push(format!("  固件版本:         {}", dev.firmware));
            out.push(format!(
                "  总容量:           {} ({} 字节)",
                size_str, dev.total_bytes
            ));
            match dev.telemetry.state {
                rsetup_core::TelemetryReadState::Unavailable => {
                    let err_reason = locale.storage_telemetry_error(dev.telemetry.error.as_ref());
                    out.push(format!("  健康状态:         不可读取 ({err_reason})"));
                }
                rsetup_core::TelemetryReadState::Unsupported => {
                    out.push("  健康状态:         不支持健康指标".to_string());
                }
                rsetup_core::TelemetryReadState::Available => {
                    let pre_eol_str = match dev.health.pre_eol_info {
                        0 => "未定义".to_string(),
                        1 => "正常".to_string(),
                        2 => "预警 (80% 寿命)".to_string(),
                        3 => "紧急 (建议更换)".to_string(),
                        other => format!("未知 ({other})"),
                    };
                    let life_str_zh = |value: Option<u8>| -> String {
                        match value {
                            Some(100) => "90–100%".to_string(),
                            Some(101) => ">100%".to_string(),
                            Some(p) => format!("{p}%"),
                            None => "不支持".to_string(),
                        }
                    };
                    let life_a_str = life_str_zh(dev.health.life_time_est_a_percent);
                    let life_b_str = life_str_zh(dev.health.life_time_est_b_percent);
                    out.push(format!("  预 EOL 状态:       {pre_eol_str}"));
                    out.push(format!("  寿命估计 (A/B):   {life_a_str} / {life_b_str}"));
                    let warning_str = if dev.health.warning_flags.is_empty() {
                        "无".to_string()
                    } else {
                        dev.health.warning_flags.join(", ")
                    };
                    out.push(format!("  告警标志:         {warning_str}"));
                }
            }
        } else {
            out.push(format!("  Type:             {}", dev.card_type));
            out.push(format!("  Model:            {}", dev.model));
            out.push(format!("  Manufacturer:   {}", dev.manufacturer));
            out.push(format!("  Serial Number:    {}", dev.serial));
            out.push(format!("  Firmware:         {}", dev.firmware));
            out.push(format!(
                "  Total Capacity:   {} ({} bytes)",
                size_str, dev.total_bytes
            ));
            match dev.telemetry.state {
                rsetup_core::TelemetryReadState::Unavailable => {
                    let err_reason = locale.storage_telemetry_error(dev.telemetry.error.as_ref());
                    out.push(format!("  Health:           Unavailable ({err_reason})"));
                }
                rsetup_core::TelemetryReadState::Unsupported => {
                    out.push("  Health:           Health telemetry unsupported".to_string());
                }
                rsetup_core::TelemetryReadState::Available => {
                    let pre_eol_str = match dev.health.pre_eol_info {
                        0 => "Undefined".to_string(),
                        1 => "Normal".to_string(),
                        2 => "Warning (80% endurance)".to_string(),
                        3 => "Urgent (replace soon)".to_string(),
                        other => format!("Unknown ({other})"),
                    };
                    let life_str_en = |value: Option<u8>| -> String {
                        match value {
                            Some(100) => "90–100%".to_string(),
                            Some(101) => ">100%".to_string(),
                            Some(p) => format!("{p}%"),
                            None => "N/A".to_string(),
                        }
                    };
                    let life_a_str = life_str_en(dev.health.life_time_est_a_percent);
                    let life_b_str = life_str_en(dev.health.life_time_est_b_percent);
                    out.push(format!("  Pre-EOL:            {pre_eol_str}"));
                    out.push(format!(
                        "  Life Time Est (A/B): {life_a_str} / {life_b_str}"
                    ));
                    let warning_str = if dev.health.warning_flags.is_empty() {
                        "None".to_string()
                    } else {
                        dev.health.warning_flags.join(", ")
                    };
                    out.push(format!("  Warning Flags:    {warning_str}"));
                }
            }
        }
    }

    out.join("\n")
}

pub(crate) fn format_storage_status(status: &rsetup_core::StorageStatus, locale: Locale) -> String {
    let nvme = format_nvme_status(&status.nvme, locale);
    let mmc = format_mmc_status(&status.mmc, locale);
    format!("{nvme}\n\n{mmc}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsetup_core::{
        Controller, ExecutionPolicy, MmcDevice, MmcHealth, MmcStatus, NvmeDevice, NvmeSmartLog,
        NvmeStatus, ProbeMode, StorageStatus,
    };

    #[test]
    fn format_nvme_status_uninitialized_en_and_zh() {
        let uninit = NvmeStatus {
            initialized: false,
            devices: Vec::new(),
            message: Some("No NVMe controller detected in system".into()),
        };

        let formatted_zh = format_nvme_status(&uninit, Locale::ZhCn);
        assert!(
            formatted_zh.contains("未检测到 NVMe 存储设备") || formatted_zh.contains("未激活"),
            "ZH output: {formatted_zh}"
        );

        let formatted_en = format_nvme_status(&uninit, Locale::En);
        assert!(
            formatted_en.contains("No NVMe") || formatted_en.contains("uninitialized"),
            "EN output: {formatted_en}"
        );
    }

    #[test]
    fn format_nvme_status_initialized_with_device() {
        let status = NvmeStatus {
            initialized: true,
            devices: vec![NvmeDevice {
                name: "nvme0".into(),
                path: "/dev/nvme0".into(),
                model: "Radxa NVMe SSD 256GB".into(),
                serial: "RADXA2026NVME01".into(),
                firmware: "1.0.0".into(),
                total_bytes: 256_060_514_304,
                smart: Some(NvmeSmartLog {
                    critical_warning: 0,
                    warning_flags: Vec::new(),
                    temperature_c: 42.0,
                    available_spare_percent: 100,
                    spare_threshold_percent: 10,
                    percentage_used: 3,
                    data_read_bytes: 1024 * 1024 * 1024 * 50, // 50 GiB
                    data_written_bytes: 1024 * 1024 * 1024 * 30, // 30 GiB
                    host_read_commands: 1000,
                    host_write_commands: 500,
                    power_on_hours: 120,
                    unsafe_shutdowns: 1,
                    media_errors: 0,
                    num_err_log_entries: 0,
                }),
                telemetry: rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Available,
                    error: None,
                },
                health_state: rsetup_core::HealthState::Healthy,
            }],
            message: None,
        };

        let out_zh = format_nvme_status(&status, Locale::ZhCn);
        assert!(out_zh.contains("nvme0"), "zh should contain nvme0");
        assert!(
            out_zh.contains("Radxa NVMe SSD 256GB"),
            "zh should contain model"
        );
        assert!(out_zh.contains("42"), "zh should contain temperature 42");
        assert!(
            out_zh.contains("RADXA2026NVME01"),
            "zh should contain serial"
        );

        let out_en = format_nvme_status(&status, Locale::En);
        assert!(out_en.contains("nvme0"), "en should contain nvme0");
        assert!(
            out_en.contains("Radxa NVMe SSD 256GB"),
            "en should contain model"
        );
        assert!(out_en.contains("42"), "en should contain temperature 42");
        assert!(
            out_en.contains("Temperature"),
            "en should contain label Temperature"
        );
    }

    #[test]
    fn format_mmc_status_uninitialized_en_and_zh() {
        let uninit = MmcStatus {
            initialized: false,
            devices: Vec::new(),
            message: Some("No MMC/SD devices detected in system".into()),
        };

        let formatted_zh = format_mmc_status(&uninit, Locale::ZhCn);
        assert!(
            formatted_zh.contains("未检测到 MMC/SD"),
            "ZH output: {formatted_zh}"
        );

        let formatted_en = format_mmc_status(&uninit, Locale::En);
        assert!(
            formatted_en.contains("No MMC/SD"),
            "EN output: {formatted_en}"
        );
    }

    #[test]
    fn format_mmc_status_initialized_with_devices() {
        let emmc = MmcDevice {
            name: "mmc0:0001".into(),
            block_path: "/dev/mmcblk0".into(),
            card_type: "MMC".into(),
            model: "DG4064".into(),
            manufacturer: "0x45".into(),
            serial: "0x12345678".into(),
            firmware: "0x00".into(),
            total_bytes: 64_000_000_000,
            health: MmcHealth {
                pre_eol_info: 1,
                life_time_est_a_percent: Some(10),
                life_time_est_b_percent: None,
                warning_flags: Vec::new(),
            },
            telemetry: rsetup_core::TelemetryStatus {
                state: rsetup_core::TelemetryReadState::Available,
                error: None,
            },
            health_state: rsetup_core::HealthState::Healthy,
        };
        let sd = MmcDevice {
            name: "mmc1:0001".into(),
            block_path: "/dev/mmcblk1".into(),
            card_type: "SD".into(),
            model: "SU08G".into(),
            manufacturer: "0x1B".into(),
            serial: "0x5A4F".into(),
            firmware: "1.0".into(),
            total_bytes: 8_000_000_000,
            health: MmcHealth {
                pre_eol_info: 0,
                life_time_est_a_percent: None,
                life_time_est_b_percent: None,
                warning_flags: Vec::new(),
            },
            telemetry: rsetup_core::TelemetryStatus {
                state: rsetup_core::TelemetryReadState::Unsupported,
                error: None,
            },
            health_state: rsetup_core::HealthState::Unknown,
        };
        let status = MmcStatus {
            initialized: true,
            devices: vec![emmc, sd],
            message: None,
        };

        let out_zh = format_mmc_status(&status, Locale::ZhCn);
        assert!(out_zh.contains("mmc0:0001"), "zh should contain mmc0:0001");
        assert!(
            out_zh.contains("/dev/mmcblk0"),
            "zh should contain block path"
        );
        assert!(out_zh.contains("MMC"), "zh should contain card type MMC");
        assert!(out_zh.contains("SD"), "zh should contain card type SD");
        assert!(
            out_zh.contains("10%"),
            "zh should contain life estimate 10%"
        );
        assert!(out_zh.contains("不支持"), "zh should contain N/A marker");

        let out_en = format_mmc_status(&status, Locale::En);
        assert!(out_en.contains("mmc0:0001"), "en should contain mmc0:0001");
        assert!(out_en.contains("N/A"), "en should contain N/A marker");
        assert!(
            out_en.contains("Normal"),
            "en should contain Normal pre-EOL"
        );
    }

    #[test]
    fn format_storage_status_contains_both_sections() {
        let nvme = NvmeStatus {
            initialized: true,
            devices: vec![NvmeDevice {
                name: "nvme0".into(),
                path: "/dev/nvme0".into(),
                model: "Radxa NVMe SSD 256GB".into(),
                serial: "RADXA2026NVME01".into(),
                firmware: "1.0.0".into(),
                total_bytes: 256_060_514_304,
                smart: Some(NvmeSmartLog {
                    critical_warning: 0,
                    warning_flags: Vec::new(),
                    temperature_c: 42.0,
                    available_spare_percent: 100,
                    spare_threshold_percent: 10,
                    percentage_used: 3,
                    data_read_bytes: 1024 * 1024 * 1024 * 50, // 50 GiB
                    data_written_bytes: 1024 * 1024 * 1024 * 30, // 30 GiB
                    host_read_commands: 1000,
                    host_write_commands: 500,
                    power_on_hours: 120,
                    unsafe_shutdowns: 1,
                    media_errors: 0,
                    num_err_log_entries: 0,
                }),
                telemetry: rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Available,
                    error: None,
                },
                health_state: rsetup_core::HealthState::Healthy,
            }],
            message: None,
        };
        let mmc = MmcStatus {
            initialized: true,
            devices: vec![MmcDevice {
                name: "mmc0:0001".into(),
                block_path: "/dev/mmcblk0".into(),
                card_type: "MMC".into(),
                model: "DG4064".into(),
                manufacturer: "0x45".into(),
                serial: "0x12345678".into(),
                firmware: "0x00".into(),
                total_bytes: 64_000_000_000,
                health: MmcHealth {
                    pre_eol_info: 1,
                    life_time_est_a_percent: Some(10),
                    life_time_est_b_percent: None,
                    warning_flags: Vec::new(),
                },
                telemetry: rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Available,
                    error: None,
                },
                health_state: rsetup_core::HealthState::Healthy,
            }],
            message: None,
        };
        let status = StorageStatus { nvme, mmc };

        let out = format_storage_status(&status, Locale::En);
        assert!(out.contains("nvme0"), "storage output should contain nvme0");
        assert!(
            out.contains("mmc0:0001"),
            "storage output should contain mmc0:0001"
        );
    }

    #[test]
    fn format_nvme_and_mmc_status_handles_unavailable_and_special_percentages() {
        let unavail_nvme = NvmeStatus {
            initialized: true,
            devices: vec![NvmeDevice {
                name: "nvme0".into(),
                path: "/dev/nvme0".into(),
                model: "SAMSUNG 980".into(),
                serial: "S123".into(),
                firmware: "1.0".into(),
                total_bytes: 500_000_000_000,
                smart: None,
                telemetry: rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Unavailable,
                    error: Some(rsetup_core::TelemetryError {
                        kind: rsetup_core::TelemetryErrorKind::PermissionDenied,
                        code: Some(13),
                    }),
                },
                health_state: rsetup_core::HealthState::Unknown,
            }],
            message: None,
        };
        let out_zh = format_nvme_status(&unavail_nvme, Locale::ZhCn);
        assert!(out_zh.contains("SAMSUNG 980"));
        assert!(out_zh.contains("不可读取"));
        assert!(out_zh.contains("权限不足"));
        assert!(!out_zh.contains("0.0 °C"));

        let out_en = format_nvme_status(&unavail_nvme, Locale::En);
        assert!(out_en.contains("SAMSUNG 980"));
        assert!(out_en.contains("Unavailable"));
        assert!(out_en.contains("Permission denied"));
        assert!(!out_en.contains("0.0 °C"));

        let special_mmc = MmcStatus {
            initialized: true,
            devices: vec![MmcDevice {
                name: "mmc0:0001".into(),
                block_path: "/dev/mmcblk0".into(),
                card_type: "MMC".into(),
                model: "EMMC".into(),
                manufacturer: "0x15".into(),
                serial: "123".into(),
                firmware: "1".into(),
                total_bytes: 32_000_000_000,
                health: MmcHealth {
                    pre_eol_info: 2,
                    life_time_est_a_percent: Some(100),
                    life_time_est_b_percent: Some(101),
                    warning_flags: vec!["pre_eol_warning".into()],
                },
                telemetry: rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Available,
                    error: None,
                },
                health_state: rsetup_core::HealthState::Warning,
            }],
            message: None,
        };
        let mmc_zh = format_mmc_status(&special_mmc, Locale::ZhCn);
        assert!(mmc_zh.contains("90–100%"));
        assert!(mmc_zh.contains(">100%"));

        let mmc_en = format_mmc_status(&special_mmc, Locale::En);
        assert!(mmc_en.contains("90–100%"));
        assert!(mmc_en.contains(">100%"));
    }

    #[test]
    fn mmc_unavailable_preserves_reason_in_both_cli_commands() {
        use rsetup_core::{
            HealthState, MmcHealth, TelemetryError, TelemetryErrorKind, TelemetryReadState,
            TelemetryStatus,
        };
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut storage = controller.storage_status().unwrap();
        storage.mmc.devices.truncate(1);
        let dev = &mut storage.mmc.devices[0];
        dev.health = MmcHealth::default();
        dev.health_state = HealthState::Unknown;
        dev.telemetry = TelemetryStatus {
            state: TelemetryReadState::Unavailable,
            error: Some(TelemetryError {
                kind: TelemetryErrorKind::PermissionDenied,
                code: Some(13),
            }),
        };
        for (locale, reason) in [
            (Locale::En, "Permission denied (13)"),
            (Locale::ZhCn, "权限不足 (13)"),
        ] {
            let mmc = format_mmc_status(&storage.mmc, locale);
            let combined = format_storage_status(&storage, locale);
            assert!(mmc.contains(reason), "{mmc}");
            assert!(combined.contains(reason), "{combined}");
            assert!(!mmc.lines().any(|line| {
                (line.contains("Warning Flags:") && line.trim_end().ends_with("None"))
                    || (line.contains("告警标志:") && line.trim_end().ends_with('无'))
            }));
        }
    }
}
