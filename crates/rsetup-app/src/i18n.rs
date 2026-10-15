use clap::ValueEnum;
use rsetup_core::{ActionError, ActionRun, ActionStatus, RiskLevel, ServiceState, SourceError};
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LocaleArg {
    Auto,
    En,
    #[value(name = "zh-CN", alias = "zh", alias = "zh-cn", alias = "zh_CN")]
    ZhCn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    En,
    ZhCn,
}

impl Locale {
    pub fn mirror_benchmark(self, result: &rsetup_core::MirrorBenchmark) -> String {
        let mut lines = vec![format!(
            "{}{}",
            result.provider_id,
            if result.synthetic {
                if self.is_zh() {
                    " · 模拟测速"
                } else {
                    " · Simulated test"
                }
            } else {
                ""
            }
        )];
        for probe in &result.probes {
            let kind = if probe.kind == rsetup_core::SourceKind::Radxa {
                "Radxa"
            } else if self.is_zh() {
                "系统"
            } else {
                "System"
            };
            let measurement = match (probe.latency_ms, probe.bytes_per_second) {
                (Some(latency), Some(speed))
                    if probe.status == rsetup_core::MirrorProbeStatus::Ok =>
                {
                    format!(
                        "{latency:.0} ms · {:.1} KiB/s · {} B",
                        speed / 1024.0,
                        probe.downloaded_bytes
                    )
                }
                _ => self.mirror_probe_status(probe.status).into(),
            };
            lines.push(format!("{kind}: {measurement}\n  {}", probe.url));
        }
        if result.probes.is_empty() {
            lines.push(
                if self.is_zh() {
                    "无可测试的源条目"
                } else {
                    "No applicable source entries"
                }
                .into(),
            );
        }
        lines.join("\n")
    }

    pub fn mirror_probe_status(self, status: rsetup_core::MirrorProbeStatus) -> &'static str {
        use rsetup_core::MirrorProbeStatus::*;
        match (self.is_zh(), status) {
            (true, Ok) => "可用",
            (false, Ok) => "Available",
            (true, Timeout) => "连接超时",
            (false, Timeout) => "Timed out",
            (true, HttpError) => "索引不可访问",
            (false, HttpError) => "Index unavailable",
            (true, NetworkError) => "网络或 TLS 错误",
            (false, NetworkError) => "Network or TLS error",
            (true, InvalidIndex) => "返回内容不是软件包索引",
            (false, InvalidIndex) => "Not a package index",
            (true, SampleTooLarge) => "样本超出大小限制",
            (false, SampleTooLarge) => "Sample exceeds limit",
            (true, CurlMissing) => "请安装 curl 后重试",
            (false, CurlMissing) => "Install curl and retry",
        }
    }
    pub fn resolve(requested: LocaleArg) -> Self {
        match requested {
            LocaleArg::En => Self::En,
            LocaleArg::ZhCn => Self::ZhCn,
            LocaleArg::Auto => ["RSETUP_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|name| env::var(name).ok())
                .find_map(|value| Self::candidate(&value))
                .unwrap_or(Self::En),
        }
    }

    fn candidate(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(Self::from_language_tag(value))
        }
    }

    fn from_language_tag(value: &str) -> Self {
        if value.trim().to_ascii_lowercase().starts_with("zh") {
            Self::ZhCn
        } else {
            Self::En
        }
    }

    pub fn is_zh(self) -> bool {
        self == Self::ZhCn
    }

    pub fn text(self, key: &str) -> &'static str {
        match (self, key) {
            (Self::ZhCn, "live_linux_only") => "实时执行仅支持 Linux SBC 主机",
            (Self::ZhCn, "not_available") => "不可用",
            (Self::ZhCn, "synthetic_data") => "模拟数据",
            (Self::ZhCn, "network_interfaces") => "个网络接口",
            (Self::ZhCn, "capability_signals") => "个能力信号",
            (Self::ZhCn, "alerts") => "个提醒",
            (Self::ZhCn, "root") => "管理员权限",
            (Self::ZhCn, "planned_steps") => "计划步骤",
            (Self::ZhCn, "raw_output") => "原始输出",
            (Self::ZhCn, "probe") => "设备探测",
            (Self::ZhCn, "execution") => "执行策略",
            (Self::ZhCn, "native_actions") => "内置操作执行层",
            (Self::ZhCn, "device_tree") => "设备树",
            (Self::ZhCn, "synthetic_provider") => "模拟数据提供器",
            (Self::ZhCn, "live_provider") => "Linux 实时数据提供器",
            (Self::ZhCn, "dry_run_enabled") => "已启用预演保护",
            (Self::ZhCn, "live_changes_enabled") => "已允许实时变更",
            (Self::ZhCn, "native_actions_ready") => "由 rsetup-next 控制中心直接提供",
            (Self::ZhCn, "board_model_probe") => "SBC 型号探测",
            (Self::ZhCn, "ready") => "就绪",
            (Self::ZhCn, "unavailable") => "缺失",
            (Self::ZhCn, "mission_control") => "SBC 控制中心",
            (Self::ZhCn, "demo_dry") => "演示 / 预演",
            (Self::ZhCn, "local_live") => "本机 / 实时数据",
            (Self::ZhCn, "cpu_load") => "处理器负载",
            (Self::ZhCn, "memory_bus") => "内存占用",
            (Self::ZhCn, "device_core") => "设备概况",
            (Self::ZhCn, "service_signals") => "服务状态",
            (Self::ZhCn, "guided_operations") => "引导式操作",
            (Self::ZhCn, "task_brief") => "操作说明",
            (Self::ZhCn, "confirm_change") => "确认变更",
            (Self::ZhCn, "confirm_help") => {
                "此操作会更改 SBC 系统设置，并可能带来网络安全风险。按 Y 接受风险并继续，按 N 取消。"
            }
            (Self::ZhCn, "last_result") => "最近结果",
            (Self::ZhCn, "steps_short") => "步",
            (Self::ZhCn, "no_operation") => "未选择操作",
            (Self::ZhCn, "select") => "选择",
            (Self::ZhCn, "run") => "运行",
            (Self::ZhCn, "refresh") => "刷新",
            (Self::ZhCn, "exit") => "退出",
            (Self::ZhCn, "synthetic_blocked") => "模拟遥测 · 真实变更已阻止",
            (Self::ZhCn, "device_stable") => "设备连接稳定",
            (Self::ZhCn, "no_sensor") => "无传感器",
            (Self::ZhCn, "uptime") => "运行时间",
            (Self::ZhCn, "thermal") => "温度",
            (Self::ZhCn, "load_average") => "系统负载",
            (Self::ZhCn, "system_sources") => "系统软件源",
            (Self::ZhCn, "radxa_sources") => "Radxa 软件源",
            (Self::ZhCn, "managed_source_files") => "受管源文件",
            (Self::ZhCn, "mirror_providers") => "可选镜像",
            (Self::ZhCn, "selected_mirror") => "所选镜像",
            (Self::ZhCn, "no_source_changes") => "当前受管软件源无需修改。",
            (Self::ZhCn, "replacements") => "处替换",
            (Self::ZhCn, "backup_files") => "备份文件",
            (Self::ZhCn, "source_picker") => "选择软件源镜像",
            (Self::ZhCn, "source_plan") => "变更预览",
            (Self::ZhCn, "source_plan_token") => "计划令牌",
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
            (Self::ZhCn, "storage_telemetry") => "存储状态",
            (Self::ZhCn, "storage_not_detected") => "未检测到 NVMe 或 MMC 存储设备",
            (Self::ZhCn, "storage_nvme") => "NVMe",
            (Self::ZhCn, "storage_emmc") => "eMMC",
            (Self::ZhCn, "storage_sd") => "SD 卡",
            (Self::ZhCn, "storage_healthy") => "正常",
            (Self::ZhCn, "storage_warning") => "告警",
            (Self::ZhCn, "storage_critical") => "故障",
            (Self::ZhCn, "storage_health") => "状态",
            (Self::ZhCn, "storage_temperature") => "温度",
            (Self::ZhCn, "storage_endurance") => "已用寿命",
            (Self::ZhCn, "storage_spare") => "可用备用",
            (Self::ZhCn, "storage_io") => "累计读写",
            (Self::ZhCn, "storage_capacity") => "格式化容量",
            (Self::ZhCn, "storage_model") => "设备型号",
            (Self::ZhCn, "storage_serial") => "序列号",
            (Self::ZhCn, "storage_manufacturer") => "厂商",
            (Self::ZhCn, "storage_firmware") => "固件",
            (Self::ZhCn, "storage_life_a") => "SLC 寿命",
            (Self::ZhCn, "storage_life_b") => "MLC 寿命",
            (Self::ZhCn, "storage_pre_eol") => "预警",
            (Self::ZhCn, "storage_eol_normal") => "正常",
            (Self::ZhCn, "storage_eol_warning") => "预警(80%)",
            (Self::ZhCn, "storage_eol_urgent") => "紧急",
            (Self::ZhCn, "storage_eol_undefined") => "未定义",
            (Self::ZhCn, "storage_na") => "不支持",
            (Self::ZhCn, "storage_more_devices") => "更多设备",
            (Self::ZhCn, "storage_read") => "读",
            (Self::ZhCn, "storage_written") => "写",
            (Self::ZhCn, "storage_telemetry_unavailable") => "不可读取",
            (Self::ZhCn, "storage_telemetry_unsupported") => "不支持健康指标",
            (Self::ZhCn, "storage_permission_denied") => "权限不足",
            (Self::ZhCn, "storage_read_failed") => "读取失败",
            (Self::ZhCn, "storage_protocol_error") => "设备协议错误",
            (Self::ZhCn, "storage_nvme_enum_failed") => "NVMe 设备枚举失败",
            (Self::ZhCn, "storage_mmc_enum_failed") => "MMC 设备枚举失败",
            (Self::ZhCn, "storage_unknown") => "未知",
            (Self::ZhCn, "install_plan_title") => "系统镜像写入预览",
            (Self::ZhCn, "install_target") => "目标设备",
            (Self::ZhCn, "install_capacity") => "容量",
            (Self::ZhCn, "install_removable") => "可移动介质",
            (Self::ZhCn, "install_mounted") => "已挂载",
            (Self::ZhCn, "install_running_system") => "承载正在运行的系统",
            (Self::ZhCn, "install_destroyed_partitions") => "将被清除的现有分区",
            (Self::ZhCn, "install_no_partitions") => "无现有分区",
            (Self::ZhCn, "install_partition_mounted_at") => "挂载于",
            (Self::ZhCn, "install_image") => "镜像",
            (Self::ZhCn, "install_image_format") => "格式",
            (Self::ZhCn, "install_image_size") => "文件大小",
            (Self::ZhCn, "install_image_uncompressed") => "写入大小",
            (Self::ZhCn, "install_image_sha256") => "SHA-256（前 1 MiB）",
            (Self::ZhCn, "install_partition_table") => "分区表",
            (Self::ZhCn, "install_partition_table_unknown") => "未读取（压缩镜像）",
            (Self::ZhCn, "install_requires_root") => "写入需要管理员授权",
            (Self::ZhCn, "install_dry_run_notice") => {
                "预演模式：不会写入任何设备；加 --live-execution 才会真正写入。"
            }
            (Self::ZhCn, "install_confirm_prompt") => "请键入目标设备名以确认写入",
            (Self::ZhCn, "install_confirm_hint") => "必须与上面的目标设备名完全一致；留空即放弃。",
            (Self::ZhCn, "install_not_a_terminal") => {
                "标准输入不是终端：可以用 --acknowledge <设备名>，或把设备名通过管道输入。"
            }
            (Self::ZhCn, "install_written") => "写入完成",
            (Self::ZhCn, "install_planned") => "已计划（未写入）",
            (Self::ZhCn, "install_bytes_written") => "已写入字节",
            (Self::ZhCn, "install_verification") => "分区表校验",
            (Self::ZhCn, "install_verification_matched") => "重读的分区表与镜像一致",
            (Self::ZhCn, "install_verification_mismatched") => "重读的分区表与镜像不一致",
            (Self::ZhCn, "install_verification_unsupported") => "当前主机无法重读分区表",
            (Self::ZhCn, "install_verification_compressed_not_compared") => {
                "压缩镜像的布局未在写入前解析，因此未比较重读的分区表"
            }
            (Self::ZhCn, "install_synthetic_notice") => "演示目标：未写入任何真实设备",
            (Self::ZhCn, "install_panel_title") => "系统镜像安装",
            (Self::ZhCn, "install_step_targets") => "1 选择目标设备",
            (Self::ZhCn, "install_step_image") => "2 镜像路径（绝对路径）",
            (Self::ZhCn, "install_step_confirm") => "3 键入目标设备名确认",
            (Self::ZhCn, "install_field_image") => "镜像路径",
            (Self::ZhCn, "install_field_confirm") => "键入设备名",
            (Self::ZhCn, "install_not_planned") => "请先按 Enter 载入写入计划",
            (Self::ZhCn, "install_disabled") => "已禁用：键入的设备名尚未匹配目标",
            (Self::ZhCn, "install_ready") => "可以写入：按 Enter 执行（不可回滚）",
            (Self::ZhCn, "install_dry_run_ready") => {
                "预演模式：按 Enter 只会模拟写入，不会触碰设备"
            }
            (Self::ZhCn, "install_no_targets") => "未检测到可写入的块设备",
            (Self::ZhCn, "install_open_hint") => "I 系统镜像安装",
            (Self::ZhCn, "install_panel_footer") => {
                "TAB 切换字段  J/K 选择设备  ENTER 载入计划或执行  BACKSPACE 删除  ESC 返回"
            }
            (Self::ZhCn, "install_plan_hint") => "选择设备后按 Enter 载入计划",
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
            (_, "execution") => "execution",
            (_, "native_actions") => "native-actions",
            (_, "device_tree") => "device-tree",
            (_, "synthetic_provider") => "synthetic demo provider",
            (_, "live_provider") => "live Linux provider",
            (_, "dry_run_enabled") => "dry-run guard enabled",
            (_, "live_changes_enabled") => "LIVE changes enabled",
            (_, "native_actions_ready") => "built into the rsetup-next control plane",
            (_, "board_model_probe") => "SBC model probe",
            (_, "ready") => "OK",
            (_, "unavailable") => "--",
            (_, "mission_control") => "SBC CONTROL CENTER",
            (_, "demo_dry") => "DEMO / DRY",
            (_, "local_live") => "LOCAL / LIVE DATA",
            (_, "cpu_load") => "CPU LOAD",
            (_, "memory_bus") => "MEMORY",
            (_, "device_core") => "DEVICE CORE",
            (_, "service_signals") => "SERVICE SIGNALS",
            (_, "guided_operations") => "GUIDED OPERATIONS",
            (_, "task_brief") => "TASK BRIEF",
            (_, "confirm_change") => "CONFIRM CHANGE",
            (_, "confirm_help") => {
                "This operation changes SBC system settings and may introduce network security risks. Press Y to accept the risks and continue, or N to cancel."
            }
            (_, "last_result") => "LAST RESULT",
            (_, "steps_short") => "step(s)",
            (_, "no_operation") => "No operation selected",
            (_, "select") => "select",
            (_, "run") => "run",
            (_, "refresh") => "refresh",
            (_, "exit") => "exit",
            (_, "synthetic_blocked") => "SYNTHETIC TELEMETRY · CHANGES BLOCKED",
            (_, "device_stable") => "DEVICE LINK STABLE",
            (_, "no_sensor") => "NO SENSOR",
            (_, "uptime") => "UP",
            (_, "thermal") => "THERMAL",
            (_, "load_average") => "LOAD",
            (_, "system_sources") => "System sources",
            (_, "radxa_sources") => "Radxa sources",
            (_, "managed_source_files") => "Managed source files",
            (_, "mirror_providers") => "Mirror providers",
            (_, "selected_mirror") => "Selected mirror",
            (_, "no_source_changes") => "No managed APT source entry needs to change.",
            (_, "replacements") => "replacement(s)",
            (_, "backup_files") => "Backup files",
            (_, "source_picker") => "SELECT PACKAGE MIRROR",
            (_, "source_plan") => "CHANGE PREVIEW",
            (_, "source_plan_token") => "Plan token",
            (_, "source_files_short") => "file(s)",
            (_, "source_rolled_back") => {
                "Package refresh failed and the source files were rolled back."
            }
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
            (_, "storage_telemetry") => "Storage Devices",
            (_, "storage_not_detected") => "No NVMe or MMC storage devices detected.",
            (_, "storage_nvme") => "NVMe",
            (_, "storage_emmc") => "eMMC",
            (_, "storage_sd") => "SD Card",
            (_, "storage_healthy") => "Healthy",
            (_, "storage_warning") => "Warning",
            (_, "storage_critical") => "Critical",
            (_, "storage_health") => "Health",
            (_, "storage_temperature") => "Temp",
            (_, "storage_endurance") => "Used Endurance",
            (_, "storage_spare") => "Available Spare",
            (_, "storage_io") => "Data Read/Written",
            (_, "storage_capacity") => "Capacity",
            (_, "storage_model") => "Model",
            (_, "storage_serial") => "Serial",
            (_, "storage_manufacturer") => "Manufacturer",
            (_, "storage_firmware") => "Firmware",
            (_, "storage_life_a") => "SLC Life",
            (_, "storage_life_b") => "MLC Life",
            (_, "storage_pre_eol") => "Pre-EOL",
            (_, "storage_eol_normal") => "Normal",
            (_, "storage_eol_warning") => "Warning(80%)",
            (_, "storage_eol_urgent") => "Urgent",
            (_, "storage_eol_undefined") => "Undefined",
            (_, "storage_na") => "N/A",
            (_, "storage_more_devices") => "more device(s)",
            (_, "storage_read") => "Read",
            (_, "storage_written") => "Written",
            (_, "storage_telemetry_unavailable") => "Unavailable",
            (_, "storage_telemetry_unsupported") => "Health telemetry unsupported",
            (_, "storage_permission_denied") => "Permission denied",
            (_, "storage_read_failed") => "Read failed",
            (_, "storage_protocol_error") => "Device protocol error",
            (_, "storage_nvme_enum_failed") => "NVMe device enumeration failed",
            (_, "storage_mmc_enum_failed") => "MMC device enumeration failed",
            (_, "storage_unknown") => "Unknown",
            (_, "install_plan_title") => "System image write preview",
            (_, "install_target") => "Target device",
            (_, "install_capacity") => "Capacity",
            (_, "install_removable") => "Removable media",
            (_, "install_mounted") => "Mounted",
            (_, "install_running_system") => "Hosts the running system",
            (_, "install_destroyed_partitions") => "Existing partitions destroyed",
            (_, "install_no_partitions") => "No existing partitions",
            (_, "install_partition_mounted_at") => "mounted at",
            (_, "install_image") => "Image",
            (_, "install_image_format") => "Format",
            (_, "install_image_size") => "File size",
            (_, "install_image_uncompressed") => "Write size",
            (_, "install_image_sha256") => "SHA-256 (first 1 MiB)",
            (_, "install_partition_table") => "Partition table",
            (_, "install_partition_table_unknown") => "not read (compressed image)",
            (_, "install_requires_root") => "Administrator authorization required",
            (_, "install_dry_run_notice") => {
                "Dry run: nothing is written; add --live-execution to write."
            }
            (_, "install_confirm_prompt") => "Type the target device name to confirm",
            (_, "install_confirm_hint") => {
                "It must match the target device name exactly; leave it empty to cancel."
            }
            (_, "install_not_a_terminal") => {
                "Standard input is not a terminal: pass --acknowledge <device>, or pipe the device name in."
            }
            (_, "install_written") => "Write complete",
            (_, "install_planned") => "Planned, not written",
            (_, "install_bytes_written") => "Bytes written",
            (_, "install_verification") => "Partition table verification",
            (_, "install_verification_matched") => "the re-read partition table matches the image",
            (_, "install_verification_mismatched") => {
                "the re-read partition table DOES NOT match the image"
            }
            (_, "install_verification_unsupported") => "this host cannot re-read partition tables",
            (_, "install_verification_compressed_not_compared") => {
                "the image is compressed, so its layout was not parsed before writing and the re-read table was not compared"
            }
            (_, "install_synthetic_notice") => "Synthetic target: no real device was written",
            (_, "install_panel_title") => "System image install",
            (_, "install_step_targets") => "1 Choose the target device",
            (_, "install_step_image") => "2 Image path (absolute)",
            (_, "install_step_confirm") => "3 Type the target device name to confirm",
            (_, "install_field_image") => "Image path",
            (_, "install_field_confirm") => "Type the device name",
            (_, "install_not_planned") => "Press Enter to load the write plan first",
            (_, "install_disabled") => {
                "Disabled: the typed device name does not match the target yet"
            }
            (_, "install_ready") => "Ready: press Enter to write (no rollback)",
            (_, "install_dry_run_ready") => {
                "Dry run: Enter only simulates the write and never touches the device"
            }
            (_, "install_no_targets") => "No writable block device was detected",
            (_, "install_open_hint") => "I system image install",
            (_, "install_panel_footer") => {
                "TAB field  J/K device  ENTER plan or write  BACKSPACE delete  ESC back"
            }
            (_, "install_plan_hint") => "Press Enter to load the plan for the selected device",
            _ => "",
        }
    }

    pub fn storage_telemetry_error(&self, error: Option<&rsetup_core::TelemetryError>) -> String {
        use rsetup_core::TelemetryErrorKind;
        let Some(error) = error else {
            return self.text("storage_telemetry_unavailable").to_string();
        };
        let key = match error.kind {
            TelemetryErrorKind::PermissionDenied => "storage_permission_denied",
            TelemetryErrorKind::Io => "storage_read_failed",
            TelemetryErrorKind::NvmeStatus => "storage_protocol_error",
        };
        match error.code {
            Some(code) => format!("{} ({code})", self.text(key)),
            None => self.text(key).to_string(),
        }
    }

    pub fn risk(self, risk: RiskLevel) -> &'static str {
        match (self, risk) {
            (Self::ZhCn, RiskLevel::Safe) => "安全",
            (Self::ZhCn, RiskLevel::Guarded) => "需确认",
            (Self::ZhCn, RiskLevel::High) => "高风险",
            (Self::ZhCn, RiskLevel::Critical) => "严重风险",
            (_, RiskLevel::Safe) => "safe",
            (_, RiskLevel::Guarded) => "guarded",
            (_, RiskLevel::High) => "high",
            (_, RiskLevel::Critical) => "critical",
        }
    }

    pub fn service_state(self, state: ServiceState) -> &'static str {
        match (self, state) {
            (Self::ZhCn, ServiceState::Active) => "运行中",
            (Self::ZhCn, ServiceState::Inactive) => "未运行",
            (Self::ZhCn, ServiceState::Failed) => "失败",
            (Self::ZhCn, ServiceState::Unknown) => "未知",
            (_, ServiceState::Active) => "ACTIVE",
            (_, ServiceState::Inactive) => "INACTIVE",
            (_, ServiceState::Failed) => "FAILED",
            (_, ServiceState::Unknown) => "UNKNOWN",
        }
    }

    pub fn action_title(self, id: &str, fallback: &str) -> String {
        if !self.is_zh() {
            return fallback.into();
        }
        match id {
            "system.inspect" => "运行系统检查",
            "system.update" => "更新操作系统",
            "system.change-sources" => "切换软件源",
            "service.ssh-install" => "安装远程终端",
            "service.ssh-enable" => "启用远程终端",
            "service.ssh-disable" => "停用远程终端",
            "service.ssh-regenerate-host-keys" => "重新生成 SSH 主机密钥",
            "service.ssh-remove" => "移除远程终端",
            "network.restart" => "重启网络管理器",
            "service.docker-install" => "安装容器运行时",
            "service.docker-enable" => "启用容器运行时",
            "service.docker-disable" => "停用容器运行时",
            "service.docker-remove" => "移除容器运行时",
            "storage.expand-root" => "扩展根文件系统",
            "power.enable-sleep" => "启用睡眠与休眠",
            "power.disable-sleep" => "禁用睡眠与休眠",
            "system.reboot" => "重启设备",
            _ => fallback,
        }
        .into()
    }

    pub fn action_description(self, id: &str, fallback: &str) -> String {
        if !self.is_zh() {
            return fallback.into();
        }
        match id {
            "system.inspect" => "重新读取 SBC 身份、健康状态、服务与已检测硬件。",
            "system.update" => "刷新软件包索引并升级已安装的软件包。",
            "system.change-sources" => {
                "选择可信的 Debian、Ubuntu 与 Radxa 镜像；执行前预览并备份，刷新失败时自动回滚。"
            }
            "service.ssh-install" => "安装 OpenSSH 服务端软件包，不改变当前启用状态。",
            "service.ssh-enable" => "启用并启动 SSH；请先确认远程登录账户使用安全凭据。",
            "service.ssh-disable" => "停止 SSH 服务并禁止其自动启动。",
            "service.ssh-regenerate-host-keys" => "替换本机 SSH 服务身份并生成一组新密钥。",
            "service.ssh-remove" => "从本机移除 OpenSSH 服务端软件包。",
            "network.restart" => "重启 NetworkManager 并重新检查本机网络接口。",
            "service.docker-install" => "安装发行版提供的 Docker 软件包，不自动启用服务。",
            "service.docker-enable" => "启用并启动 Docker 服务。",
            "service.docker-disable" => "停止 Docker 服务并禁止其自动启动。",
            "service.docker-remove" => "移除 Docker 软件包，保留现有容器数据。",
            "storage.expand-root" => "将支持的根文件系统扩展至可用存储空间。",
            "power.enable-sleep" => "恢复 systemd 睡眠与休眠目标。",
            "power.disable-sleep" => "屏蔽系统睡眠与休眠目标，让 SBC 持续在线。",
            "system.reboot" => "停止服务并立即重启本机 SBC。",
            _ => fallback,
        }
        .into()
    }

    pub fn action_steps(self, id: &str, fallback: &[String]) -> Vec<String> {
        if !self.is_zh() {
            return fallback.to_vec();
        }
        let steps: &[&str] = match id {
            "system.inspect" => &[
                "读取操作系统与设备树身份",
                "检查存储、网络、温度与服务",
                "重新计算提醒",
            ],
            "system.update" => &[
                "刷新软件包元数据",
                "升级已安装的软件包",
                "列出被保留的软件包",
            ],
            "system.change-sources" => &[
                "检测受管 APT 软件源条目",
                "只预览已识别的 Debian、Ubuntu 与 Radxa 地址变更",
                "备份并原子替换受影响文件",
                "刷新软件包元数据，失败时恢复原文件",
            ],
            "service.ssh-install" => &["安装 OpenSSH 服务端软件包", "刷新检测到的 SSH 服务状态"],
            "service.ssh-enable" => &["检查 SSH 服务与账户安全", "设置开机启用", "启动 SSH 服务"],
            "service.ssh-disable" => &["停止 SSH 服务", "禁止自动启动", "检查服务状态"],
            "service.ssh-regenerate-host-keys" => &[
                "删除现有 SSH 主机密钥文件",
                "生成一组新的主机密钥",
                "刷新检测到的 SSH 服务状态",
            ],
            "service.ssh-remove" => &["移除 OpenSSH 服务端软件包", "刷新检测到的 SSH 服务状态"],
            "network.restart" => &["记录活动接口", "重启 NetworkManager", "等待网络接口恢复"],
            "service.docker-install" => &["安装 Docker 软件包", "刷新检测到的 Docker 服务状态"],
            "service.docker-enable" => {
                &["设置 Docker 开机启用", "启动 Docker 服务", "检查服务状态"]
            }
            "service.docker-disable" => &["停止 Docker 服务及容器", "禁止自动启动", "检查服务状态"],
            "service.docker-remove" => &["移除 Docker 软件包", "保留 /var/lib/docker 中的现有数据"],
            "storage.expand-root" => &[
                "确定根块设备",
                "验证 ext4 或 btrfs",
                "扩展文件系统",
                "检查扩展后容量",
            ],
            "power.enable-sleep" => &["取消屏蔽睡眠目标", "重新加载 systemd", "检查目标状态"],
            "power.disable-sleep" => &["屏蔽睡眠目标", "重新加载 systemd", "检查目标状态"],
            "system.reboot" => &["写回待处理数据", "停止服务", "请求系统重启"],
            _ => return fallback.to_vec(),
        };
        steps.iter().map(|value| (*value).into()).collect()
    }

    pub fn action_unavailable_reason(self, reason: &str) -> String {
        if !self.is_zh() {
            return reason.into();
        }
        match reason {
            "OpenSSH server is already installed." => "OpenSSH 服务端已安装。".into(),
            "Docker is already installed." => "Docker 已安装。".into(),
            "SSH is already enabled and running." => "SSH 已启用并正在运行。".into(),
            "SSH is already disabled and stopped." => "SSH 已停用并停止运行。".into(),
            "Docker is already enabled and running." => "Docker 已启用并正在运行。".into(),
            "Docker is already disabled and stopped." => "Docker 已停用并停止运行。".into(),
            "Sleep and hibernate targets are already enabled." => "睡眠与休眠目标已启用。".into(),
            "Sleep and hibernate targets are already disabled." => "睡眠与休眠目标已停用。".into(),
            "Neither resize2fs nor btrfs is installed." => "未安装 resize2fs 或 btrfs。".into(),
            _ => {
                if let Some(package) = reason
                    .strip_prefix("Package ")
                    .and_then(|value| value.strip_suffix(" is not installed."))
                {
                    return format!("未安装软件包 {package}。");
                }
                if let Some(unit) = reason
                    .strip_prefix("Systemd unit ")
                    .and_then(|value| value.strip_suffix(" is not installed."))
                {
                    return format!("未安装 systemd 单元 {unit}。");
                }
                if let Some(commands) = reason
                    .strip_prefix("Missing required command(s): ")
                    .and_then(|value| value.strip_suffix('.'))
                {
                    return format!("缺少必要命令：{commands}。");
                }
                reason.into()
            }
        }
    }

    pub fn service_label(self, id: &str, fallback: &str) -> String {
        if !self.is_zh() {
            return fallback.into();
        }
        match id {
            "ssh.service" => "远程终端",
            "NetworkManager.service" => "网络管理器",
            "docker.service" => "容器运行时",
            _ => fallback,
        }
        .into()
    }

    pub fn service_detail(self, detail: &str) -> String {
        if !self.is_zh() {
            return detail.into();
        }
        if let Some(port) = detail.strip_prefix("Listening on ") {
            return format!("监听端口 {port}");
        }
        if let Some(count) = detail.strip_suffix(" interfaces managed") {
            return format!("已托管 {count} 个接口");
        }
        match detail {
            "Installed · stopped" => "已安装 · 已停止",
            "systemd state unavailable" => "systemd 状态不可用",
            _ => detail,
        }
        .into()
    }

    pub fn run_summary(self, run: &ActionRun) -> String {
        if !self.is_zh() {
            return run.summary.clone();
        }
        if run.action_id == "system.change-sources" && run.synthetic {
            return "预演完成；未修改任何 APT 软件源文件。".into();
        }
        match (run.synthetic, run.status) {
            (true, _) => "预演完成；未修改任何系统状态。".into(),
            (_, ActionStatus::Succeeded) => "操作已成功完成。".into(),
            (_, ActionStatus::Failed) => "操作执行失败。".into(),
            (_, ActionStatus::Planned) => "操作已进入计划。".into(),
            (_, ActionStatus::Running) => "操作正在执行。".into(),
        }
    }

    pub fn action_error(self, error: &ActionError) -> String {
        if !self.is_zh() {
            return error.to_string();
        }
        match error {
            ActionError::Unknown(id) => format!("未知操作：{id}"),
            ActionError::Unavailable(title) => {
                format!("操作不可用：{}", self.known_action_title(title))
            }
            ActionError::ConfirmationRequired(title) => {
                format!("操作“{}”需要确认", self.known_action_title(title))
            }
            ActionError::RootRequired(title) => {
                format!("操作“{}”需要管理员权限", self.known_action_title(title))
            }
            ActionError::AuthorizationCanceled => "已取消管理员授权。".into(),
            ActionError::Authorization(title, detail) => format!(
                "操作“{}”的管理员授权失败：{detail}",
                self.known_action_title(title)
            ),
            ActionError::InputRequired(title) => {
                format!("操作“{}”需要先选择镜像", self.known_action_title(title))
            }
            ActionError::Launch(detail) => format!("无法启动操作：{detail}"),
        }
    }

    pub fn source_error(self, error: &SourceError) -> String {
        if !self.is_zh() {
            return error.to_string();
        }
        match error {
            SourceError::UnknownProvider(id) => format!("未知镜像：{id}"),
            SourceError::Unsupported(detail) => format!("当前系统无法管理 APT 软件源：{detail}"),
            SourceError::ConfirmationRequired => "切换软件源前需要明确确认".into(),
            SourceError::PlanRequired => "请先预览软件源变更，再使用返回的计划令牌应用".into(),
            SourceError::StalePlan => "软件源文件已在预览后变化，请重新预览".into(),
            SourceError::RootRequired => "切换软件源需要管理员权限".into(),
            SourceError::AuthorizationCanceled => "已取消管理员授权。".into(),
            SourceError::Authorization(detail) => format!("管理员授权失败：{detail}"),
            SourceError::Io(detail) => format!("无法管理 APT 软件源：{detail}"),
        }
    }

    /// Localized message for a system-install failure. The stable code that goes
    /// with it is locale-independent and lives in `main.rs`, so every surface
    /// reports the same code.
    pub fn install_error(self, error: &rsetup_core::SystemInstallError) -> String {
        use rsetup_core::SystemInstallError;
        if !self.is_zh() {
            return error.to_string();
        }
        match error {
            SystemInstallError::Unsupported(detail) => {
                format!("当前系统无法安装系统镜像：{detail}")
            }
            SystemInstallError::InvalidInput(detail) => format!("无效的安装请求：{detail}"),
            SystemInstallError::ConfirmationRequired => {
                "写入系统镜像前需要键入目标设备名确认".into()
            }
            SystemInstallError::AcknowledgementMismatch {
                expected,
                target,
                provided,
            } => format!(
                "输入的设备名与目标不一致：要写入 {target} 必须键入“{expected}”，收到的是“{provided}”"
            ),
            SystemInstallError::PlanRequired => "请先预览写入计划，再使用返回的计划令牌".into(),
            SystemInstallError::StalePlan => "目标或镜像在预览后发生变化，请重新预览".into(),
            SystemInstallError::RootRequired => "写入系统镜像需要管理员权限".into(),
            SystemInstallError::Conflict(detail) => format!("目标无法写入：{detail}"),
            SystemInstallError::RunningSystem(path) => {
                format!("拒绝写入 {path}：它承载着正在运行的根或启动文件系统")
            }
            SystemInstallError::Io(detail) => format!("写入系统镜像失败：{detail}"),
        }
    }

    pub fn install_warning(self, warning: &str) -> String {
        match (self, warning) {
            (Self::ZhCn, "writing_an_image_replaces_every_partition_on_the_target") => {
                "写入会替换目标设备上的全部分区。".into()
            }
            (Self::ZhCn, "the_write_cannot_be_rolled_back") => "此写入无法回滚。".into(),
            (Self::ZhCn, "the_target_is_removable_media") => {
                "目标是可移动介质，请确认它就是要覆盖的设备。".into()
            }
            (Self::ZhCn, "the_image_is_compressed_and_is_decompressed_while_writing") => {
                "镜像是压缩的，会在写入时解压（需要 xz）。".into()
            }
            (_, "writing_an_image_replaces_every_partition_on_the_target") => {
                "Writing replaces every partition on the target.".into()
            }
            (_, "the_write_cannot_be_rolled_back") => "This write cannot be rolled back.".into(),
            (_, "the_target_is_removable_media") => {
                "The target is removable media; confirm it is the device to overwrite.".into()
            }
            (_, "the_image_is_compressed_and_is_decompressed_while_writing") => {
                "The image is compressed and is decompressed while writing (requires xz).".into()
            }
            _ => warning.into(),
        }
    }

    pub fn install_device_kind(self, kind: rsetup_core::BlockDeviceKind) -> &'static str {
        use rsetup_core::BlockDeviceKind;
        match kind {
            BlockDeviceKind::Ufs => "UFS",
            BlockDeviceKind::Emmc => "eMMC",
            BlockDeviceKind::Sd => {
                if self.is_zh() {
                    "SD 卡"
                } else {
                    "SD card"
                }
            }
            BlockDeviceKind::Nvme => "NVMe",
            BlockDeviceKind::Other => {
                if self.is_zh() {
                    "其它"
                } else {
                    "Other"
                }
            }
        }
    }

    pub fn source_warning(self, warning: &str) -> String {
        match (self, warning) {
            (Self::ZhCn, "radxa_only") => {
                "此镜像只会修改 Radxa 软件源，系统软件源保持不变。".into()
            }
            (Self::ZhCn, "system_only") => {
                "此镜像只会修改系统软件源，Radxa 软件源保持不变。".into()
            }
            (Self::ZhCn, "already_selected") => "受管软件源已经使用该镜像。".into(),
            (Self::ZhCn, "no_managed_sources") => "未检测到可安全管理的软件源条目。".into(),
            (Self::ZhCn, "dry_run") => "当前为预演模式，不会写入系统文件。".into(),
            (_, "radxa_only") => {
                "This provider changes Radxa sources only; system sources stay unchanged.".into()
            }
            (_, "system_only") => {
                "This provider changes system sources only; Radxa sources stay unchanged.".into()
            }
            (_, "already_selected") => "Managed sources already use this provider.".into(),
            (_, "no_managed_sources") => "No safely managed source entry was detected.".into(),
            (_, "dry_run") => "Dry-run mode is active; no system file will be written.".into(),
            _ => warning.into(),
        }
    }

    fn known_action_title(self, fallback: &str) -> String {
        let id = match fallback {
            "Run system inspection" => "system.inspect",
            "Update operating system" => "system.update",
            "Change package mirrors" => "system.change-sources",
            "Install remote shell" => "service.ssh-install",
            "Enable remote shell" => "service.ssh-enable",
            "Disable remote shell" => "service.ssh-disable",
            "Regenerate SSH host keys" => "service.ssh-regenerate-host-keys",
            "Remove remote shell" => "service.ssh-remove",
            "Restart network manager" => "network.restart",
            "Install container runtime" => "service.docker-install",
            "Enable container runtime" => "service.docker-enable",
            "Disable container runtime" => "service.docker-disable",
            "Remove container runtime" => "service.docker-remove",
            "Expand root filesystem" => "storage.expand-root",
            "Enable sleep and hibernate" => "power.enable-sleep",
            "Disable sleep and hibernate" => "power.disable-sleep",
            "Reboot device" => "system.reboot",
            _ => return fallback.into(),
        };
        self.action_title(id, fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nvme_tui_dictionary_keys() {
        let expected = [
            ("nvme_telemetry", "NVMe 存储遥测", "NVMe Storage Telemetry"),
            ("nvme_healthy", "正常", "Healthy"),
            ("nvme_warning", "告警", "Warning"),
            ("nvme_endurance", "已用寿命", "Used Endurance"),
            ("nvme_spare", "可用备用", "Available Spare"),
            ("nvme_io", "累计读写", "Data Read/Written"),
            (
                "nvme_not_detected",
                "未检测到 NVMe 存储设备，模块未激活",
                "No NVMe storage devices detected; module is uninitialized.",
            ),
        ];

        for (key, zh, en) in expected {
            assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
            assert_eq!(Locale::En.text(key), en, "En translation for {key}");
        }
    }

    /// The system-install vocabulary must exist in both locales: a key that is
    /// only in one of them renders as an empty string on that surface.
    #[test]
    fn test_system_install_dictionary_keys() {
        let expected = [
            (
                "install_plan_title",
                "系统镜像写入预览",
                "System image write preview",
            ),
            ("install_target", "目标设备", "Target device"),
            ("install_capacity", "容量", "Capacity"),
            ("install_removable", "可移动介质", "Removable media"),
            ("install_mounted", "已挂载", "Mounted"),
            (
                "install_running_system",
                "承载正在运行的系统",
                "Hosts the running system",
            ),
            (
                "install_destroyed_partitions",
                "将被清除的现有分区",
                "Existing partitions destroyed",
            ),
            (
                "install_no_partitions",
                "无现有分区",
                "No existing partitions",
            ),
            ("install_partition_mounted_at", "挂载于", "mounted at"),
            ("install_image", "镜像", "Image"),
            ("install_image_format", "格式", "Format"),
            ("install_image_size", "文件大小", "File size"),
            ("install_image_uncompressed", "写入大小", "Write size"),
            (
                "install_image_sha256",
                "SHA-256（前 1 MiB）",
                "SHA-256 (first 1 MiB)",
            ),
            ("install_partition_table", "分区表", "Partition table"),
            (
                "install_partition_table_unknown",
                "未读取（压缩镜像）",
                "not read (compressed image)",
            ),
            (
                "install_requires_root",
                "写入需要管理员授权",
                "Administrator authorization required",
            ),
            (
                "install_dry_run_notice",
                "预演模式：不会写入任何设备；加 --live-execution 才会真正写入。",
                "Dry run: nothing is written; add --live-execution to write.",
            ),
            (
                "install_confirm_prompt",
                "请键入目标设备名以确认写入",
                "Type the target device name to confirm",
            ),
            (
                "install_confirm_hint",
                "必须与上面的目标设备名完全一致；留空即放弃。",
                "It must match the target device name exactly; leave it empty to cancel.",
            ),
            (
                "install_not_a_terminal",
                "标准输入不是终端：可以用 --acknowledge <设备名>，或把设备名通过管道输入。",
                "Standard input is not a terminal: pass --acknowledge <device>, or pipe the device name in.",
            ),
            ("install_written", "写入完成", "Write complete"),
            ("install_planned", "已计划（未写入）", "Planned, not written"),
            ("install_bytes_written", "已写入字节", "Bytes written"),
            (
                "install_verification",
                "分区表校验",
                "Partition table verification",
            ),
            (
                "install_verification_matched",
                "重读的分区表与镜像一致",
                "the re-read partition table matches the image",
            ),
            (
                "install_verification_mismatched",
                "重读的分区表与镜像不一致",
                "the re-read partition table DOES NOT match the image",
            ),
            (
                "install_verification_unsupported",
                "当前主机无法重读分区表",
                "this host cannot re-read partition tables",
            ),
            (
                "install_verification_compressed_not_compared",
                "压缩镜像的布局未在写入前解析，因此未比较重读的分区表",
                "the image is compressed, so its layout was not parsed before writing and the re-read table was not compared",
            ),
            (
                "install_synthetic_notice",
                "演示目标：未写入任何真实设备",
                "Synthetic target: no real device was written",
            ),
            (
                "install_panel_title",
                "系统镜像安装",
                "System image install",
            ),
            (
                "install_step_targets",
                "1 选择目标设备",
                "1 Choose the target device",
            ),
            (
                "install_step_image",
                "2 镜像路径（绝对路径）",
                "2 Image path (absolute)",
            ),
            (
                "install_step_confirm",
                "3 键入目标设备名确认",
                "3 Type the target device name to confirm",
            ),
            ("install_field_image", "镜像路径", "Image path"),
            (
                "install_field_confirm",
                "键入设备名",
                "Type the device name",
            ),
            (
                "install_not_planned",
                "请先按 Enter 载入写入计划",
                "Press Enter to load the write plan first",
            ),
            (
                "install_disabled",
                "已禁用：键入的设备名尚未匹配目标",
                "Disabled: the typed device name does not match the target yet",
            ),
            (
                "install_ready",
                "可以写入：按 Enter 执行（不可回滚）",
                "Ready: press Enter to write (no rollback)",
            ),
            (
                "install_dry_run_ready",
                "预演模式：按 Enter 只会模拟写入，不会触碰设备",
                "Dry run: Enter only simulates the write and never touches the device",
            ),
            (
                "install_no_targets",
                "未检测到可写入的块设备",
                "No writable block device was detected",
            ),
            (
                "install_open_hint",
                "I 系统镜像安装",
                "I system image install",
            ),
            (
                "install_panel_footer",
                "TAB 切换字段  J/K 选择设备  ENTER 载入计划或执行  BACKSPACE 删除  ESC 返回",
                "TAB field  J/K device  ENTER plan or write  BACKSPACE delete  ESC back",
            ),
            (
                "install_plan_hint",
                "选择设备后按 Enter 载入计划",
                "Press Enter to load the plan for the selected device",
            ),
        ];
        for (key, zh, en) in expected {
            assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
            assert_eq!(Locale::En.text(key), en, "En translation for {key}");
        }

        for warning in [
            "writing_an_image_replaces_every_partition_on_the_target",
            "the_write_cannot_be_rolled_back",
            "the_target_is_removable_media",
            "the_image_is_compressed_and_is_decompressed_while_writing",
        ] {
            let zh = Locale::ZhCn.install_warning(warning);
            let en = Locale::En.install_warning(warning);
            assert!(
                !zh.is_empty() && zh != warning,
                "ZhCn warning for {warning}"
            );
            assert!(!en.is_empty() && en != warning, "En warning for {warning}");
            assert_ne!(zh, en, "the warning is localized for {warning}");
        }

        use rsetup_core::BlockDeviceKind;
        assert_eq!(
            Locale::ZhCn.install_device_kind(BlockDeviceKind::Sd),
            "SD 卡"
        );
        assert_eq!(
            Locale::En.install_device_kind(BlockDeviceKind::Sd),
            "SD card"
        );
        assert_eq!(
            Locale::ZhCn.install_device_kind(BlockDeviceKind::Ufs),
            "UFS"
        );
        assert_eq!(
            Locale::En.install_device_kind(BlockDeviceKind::Other),
            "Other"
        );
    }

    #[test]
    fn test_storage_tui_dictionary_keys() {
        let expected = [
            ("storage_telemetry", "存储状态", "Storage Devices"),
            (
                "storage_not_detected",
                "未检测到 NVMe 或 MMC 存储设备",
                "No NVMe or MMC storage devices detected.",
            ),
            ("storage_nvme", "NVMe", "NVMe"),
            ("storage_emmc", "eMMC", "eMMC"),
            ("storage_sd", "SD 卡", "SD Card"),
            ("storage_healthy", "正常", "Healthy"),
            ("storage_warning", "告警", "Warning"),
            ("storage_critical", "故障", "Critical"),
            ("storage_health", "状态", "Health"),
            ("storage_temperature", "温度", "Temp"),
            ("storage_endurance", "已用寿命", "Used Endurance"),
            ("storage_spare", "可用备用", "Available Spare"),
            ("storage_io", "累计读写", "Data Read/Written"),
            ("storage_capacity", "格式化容量", "Capacity"),
            ("storage_model", "设备型号", "Model"),
            ("storage_serial", "序列号", "Serial"),
            ("storage_manufacturer", "厂商", "Manufacturer"),
            ("storage_firmware", "固件", "Firmware"),
            ("storage_life_a", "SLC 寿命", "SLC Life"),
            ("storage_life_b", "MLC 寿命", "MLC Life"),
            ("storage_pre_eol", "预警", "Pre-EOL"),
            ("storage_eol_normal", "正常", "Normal"),
            ("storage_eol_warning", "预警(80%)", "Warning(80%)"),
            ("storage_eol_urgent", "紧急", "Urgent"),
            ("storage_eol_undefined", "未定义", "Undefined"),
            ("storage_na", "不支持", "N/A"),
            ("storage_more_devices", "更多设备", "more device(s)"),
            ("storage_read", "读", "Read"),
            ("storage_written", "写", "Written"),
            ("storage_telemetry_unavailable", "不可读取", "Unavailable"),
            (
                "storage_telemetry_unsupported",
                "不支持健康指标",
                "Health telemetry unsupported",
            ),
            ("storage_permission_denied", "权限不足", "Permission denied"),
            ("storage_read_failed", "读取失败", "Read failed"),
            (
                "storage_protocol_error",
                "设备协议错误",
                "Device protocol error",
            ),
            (
                "storage_nvme_enum_failed",
                "NVMe 设备枚举失败",
                "NVMe device enumeration failed",
            ),
            (
                "storage_mmc_enum_failed",
                "MMC 设备枚举失败",
                "MMC device enumeration failed",
            ),
            ("storage_unknown", "未知", "Unknown"),
        ];
        for (key, zh, en) in expected {
            assert_eq!(Locale::ZhCn.text(key), zh, "ZhCn translation for {key}");
            assert_eq!(Locale::En.text(key), en, "En translation for {key}");
        }
    }

    #[test]
    fn storage_telemetry_error_formatting() {
        use rsetup_core::{TelemetryError, TelemetryErrorKind};

        assert_eq!(Locale::ZhCn.storage_telemetry_error(None), "不可读取");
        assert_eq!(Locale::En.storage_telemetry_error(None), "Unavailable");

        let perm = TelemetryError {
            kind: TelemetryErrorKind::PermissionDenied,
            code: Some(13),
        };
        assert_eq!(
            Locale::ZhCn.storage_telemetry_error(Some(&perm)),
            "权限不足 (13)"
        );
        assert_eq!(
            Locale::En.storage_telemetry_error(Some(&perm)),
            "Permission denied (13)"
        );

        let io_no_code = TelemetryError {
            kind: TelemetryErrorKind::Io,
            code: None,
        };
        assert_eq!(
            Locale::ZhCn.storage_telemetry_error(Some(&io_no_code)),
            "读取失败"
        );
        assert_eq!(
            Locale::En.storage_telemetry_error(Some(&io_no_code)),
            "Read failed"
        );

        let proto = TelemetryError {
            kind: TelemetryErrorKind::NvmeStatus,
            code: Some(2),
        };
        assert_eq!(
            Locale::ZhCn.storage_telemetry_error(Some(&proto)),
            "设备协议错误 (2)"
        );
        assert_eq!(
            Locale::En.storage_telemetry_error(Some(&proto)),
            "Device protocol error (2)"
        );
    }

    #[test]
    fn chinese_action_copy_is_keyed_by_stable_id() {
        assert_eq!(
            Locale::ZhCn.action_title("system.reboot", "Reboot device"),
            "重启设备"
        );
    }

    #[test]
    fn confirmation_copy_names_sbc_settings_and_network_risk() {
        assert_eq!(
            Locale::ZhCn.text("confirm_help"),
            "此操作会更改 SBC 系统设置，并可能带来网络安全风险。按 Y 接受风险并继续，按 N 取消。"
        );
    }

    #[test]
    fn chinese_action_availability_preserves_package_name() {
        assert_eq!(
            Locale::ZhCn.action_unavailable_reason("Package openssh-server is not installed."),
            "未安装软件包 openssh-server。"
        );
    }

    #[test]
    fn unknown_copy_falls_back_to_provider_text() {
        assert_eq!(
            Locale::ZhCn.action_title("vendor.custom", "Vendor action"),
            "Vendor action"
        );
    }

    #[test]
    fn language_tags_resolve_chinese_variants() {
        assert_eq!(Locale::from_language_tag("zh_CN.UTF-8"), Locale::ZhCn);
        assert_eq!(Locale::from_language_tag("en_US.UTF-8"), Locale::En);
    }

    #[test]
    fn auto_and_empty_candidates_defer_to_the_next_locale() {
        let locale = ["auto", "", "zh_CN.UTF-8"]
            .into_iter()
            .find_map(Locale::candidate);
        assert_eq!(locale, Some(Locale::ZhCn));
    }

    #[test]
    fn chinese_service_details_preserve_provider_values() {
        assert_eq!(
            Locale::ZhCn.service_detail("2 interfaces managed"),
            "已托管 2 个接口"
        );
        assert_eq!(Locale::ZhCn.service_detail("vendor state"), "vendor state");
    }

    #[test]
    fn chinese_action_errors_localize_known_action_titles() {
        assert_eq!(
            Locale::ZhCn.action_error(&ActionError::RootRequired("Update operating system".into())),
            "操作“更新操作系统”需要管理员权限"
        );
    }
}
