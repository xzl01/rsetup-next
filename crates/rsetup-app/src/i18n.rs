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
            (Self::ZhCn, "board_model_probe") => "开发板型号探测",
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
            (Self::ZhCn, "confirm_help") => "此操作可能修改开发板。按 Y 继续，按 N 取消。",
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
            (_, "board_model_probe") => "board model probe",
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
                "This operation may change the board. Press Y to continue or N to cancel."
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
            _ => "",
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
            "system.inspect" => "重新读取开发板身份、健康状态、服务与已检测硬件。",
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
            "system.reboot" => "停止服务并立即重启本机开发板。",
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
            SourceError::Authorization(detail) => format!("管理员授权失败：{detail}"),
            SourceError::Io(detail) => format!("无法管理 APT 软件源：{detail}"),
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
    fn chinese_action_copy_is_keyed_by_stable_id() {
        assert_eq!(
            Locale::ZhCn.action_title("system.reboot", "Reboot device"),
            "重启设备"
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
