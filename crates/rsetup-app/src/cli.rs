use clap::{Parser, Subcommand};
use rsetup_core::FanCurvePoint;
use std::{io::IsTerminal, net::SocketAddr, path::PathBuf};

use crate::i18n::LocaleArg;

#[derive(Debug, Parser)]
#[command(
    name = "rsetup-next",
    version,
    about = "One control plane for your SBC / 一站式 SBC 控制中心"
)]
pub(crate) struct Cli {
    /// Use clearly labelled synthetic board telemetry / 使用明确标注的模拟数据
    #[arg(long, global = true)]
    pub(crate) demo: bool,

    /// Permit the fixed action catalog to change this Linux host / 允许操作修改 Linux 主机
    #[arg(long, global = true)]
    pub(crate) live_execution: bool,

    /// Display language: auto, en, or zh-CN / 显示语言：auto、en 或 zh-CN
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "auto",
        value_name = "LANG"
    )]
    pub(crate) lang: LocaleArg,

    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Print the current SBC snapshot / 显示当前 SBC 状态
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List available guided operations / 列出可用的引导式操作
    Actions {
        #[arg(long)]
        json: bool,
    },
    /// Run one guided operation / 运行一个引导式操作
    Run {
        action: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect, preview, or change APT mirrors / 查看、预览或切换 APT 软件源
    Sources {
        #[command(subcommand)]
        command: SourceCommands,
    },
    /// Inspect and manage SBC hardware / 查看和管理 SBC 硬件
    Hardware {
        #[command(subcommand)]
        command: HardwareCommands,
    },
    /// Open the interactive terminal control center / 打开交互式终端控制中心
    Tui,
    /// Serve the browser control center and JSON API / 启动浏览器控制中心与 JSON API
    Serve {
        #[arg(long, default_value = "127.0.0.1:8788")]
        listen: SocketAddr,
    },
    /// Inspect runtime, privilege, and hardware readiness / 检查运行环境与硬件就绪状态
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Inspect block devices and install a system image / 查看块设备并安装系统镜像
    System {
        #[command(subcommand)]
        command: SystemCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SystemCommands {
    /// List the block devices an image could be written to / 列出可写入镜像的块设备
    Targets {
        #[arg(long)]
        json: bool,
    },
    /// Preview what writing an image would destroy / 预览写入镜像将清除的内容
    Plan {
        #[arg(long)]
        target: String,
        #[arg(long)]
        image: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Write an image after typing the target device name / 键入目标设备名后写入镜像
    Install {
        #[arg(long)]
        target: String,
        #[arg(long)]
        image: PathBuf,
        /// Supply the typed device name instead of prompting / 非交互：直接给出键入的设备名
        #[arg(long, value_name = "DEVICE")]
        acknowledge: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SourceCommands {
    /// Sample mirror index latency and speed, without changing sources / 软件源测速，不修改配置
    Benchmark {
        /// Test one mirror; omit to test all / 指定镜像，省略则测试全部
        #[arg(long)]
        mirror: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show detected source files and current providers / 显示已检测源文件与当前镜像
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Preview the exact managed entries that would change / 预览将被修改的受管条目
    Plan {
        mirror: String,
        #[arg(long)]
        json: bool,
    },
    /// Apply a mirror after explicit confirmation / 明确确认后应用镜像
    Apply {
        mirror: String,
        /// Token returned by `sources plan` / `sources plan` 返回的计划令牌
        #[arg(long, value_name = "TOKEN")]
        plan_token: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum HardwareCommands {
    /// Manage device-tree overlays / 管理设备树叠加层
    Overlays {
        #[command(subcommand)]
        command: OverlayCommands,
    },
    /// Show the 40-pin GPIO map / 显示 40 针 GPIO 映射
    Gpio {
        #[arg(long)]
        json: bool,
        /// Authorize reading protected EFI configuration / 授权读取 EFI 配置
        #[arg(long)]
        authorize: bool,
    },
    /// Manage SPI boot flash / 管理 SPI 启动闪存
    SpiFlash {
        #[command(subcommand)]
        command: SpiFlashCommands,
    },
    /// Inspect or control Linux LED class devices / 查看或控制 Linux LED 设备
    Leds {
        #[command(subcommand)]
        command: LedCommands,
    },
    /// Inspect cameras or capture a test frame / 查看摄像头或抓取测试帧
    Video {
        #[command(subcommand)]
        command: VideoCommands,
    },
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
    /// Inspect MMC/eMMC/SD storage devices and endurance health / 查看 MMC/eMMC/SD 存储设备与寿命健康状态
    Mmc {
        #[arg(long)]
        json: bool,
    },
    /// Inspect unified storage (NVMe + MMC) summary / 查看统一存储（NVMe + MMC）汇总
    Storage {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum OverlayCommands {
    Status {
        #[arg(long)]
        json: bool,
        /// Authorize reading protected EFI configuration / 授权读取 EFI 配置
        #[arg(long)]
        authorize: bool,
    },
    Plan {
        #[arg(long = "enable")]
        selected_ids: Vec<String>,
        #[arg(long)]
        json: bool,
        /// Authorize reading protected EFI configuration / 授权读取 EFI 配置
        #[arg(long)]
        authorize: bool,
    },
    Apply {
        #[arg(long = "enable")]
        selected_ids: Vec<String>,
        #[arg(long, value_name = "TOKEN")]
        plan_token: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SpiFlashCommands {
    /// Show detected SPI NOR targets and installed boot images / 显示 SPI NOR 与已安装引导镜像
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Preview an install or erase operation / 预览写入或擦除操作
    Plan {
        #[arg(value_parser = ["install", "erase"])]
        operation: String,
        target: String,
        #[arg(long)]
        image: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Apply a previously previewed operation / 执行已预览的操作
    Apply {
        #[arg(value_parser = ["install", "erase"])]
        operation: String,
        target: String,
        #[arg(long)]
        image: Option<String>,
        #[arg(long, value_name = "TOKEN")]
        plan_token: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum VideoCommands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Capture {
        device: String,
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ThermalCommands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Set {
        policy: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Configure a temperature-driven pwm-fan curve / 配置温度驱动的风扇曲线
    FanCurve {
        #[command(subcommand)]
        command: FanCurveCommands,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum FanCurveCommands {
    /// Show detected curve targets and saved configuration / 显示曲线目标与已保存配置
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Preview enabling or updating a curve / 预览启用或更新曲线
    Plan {
        #[arg(long)]
        zone: String,
        #[arg(long)]
        device: String,
        #[arg(long = "point", value_parser = parse_fan_curve_point, required = true)]
        points: Vec<FanCurvePoint>,
        #[arg(long, default_value_t = 2_000)]
        poll_ms: u32,
        #[arg(long, default_value_t = 2.0)]
        hysteresis_c: f32,
        #[arg(long)]
        json: bool,
    },
    /// Apply an exact previously previewed curve / 应用已预览的曲线
    Apply {
        #[arg(long)]
        zone: String,
        #[arg(long)]
        device: String,
        #[arg(long = "point", value_parser = parse_fan_curve_point, required = true)]
        points: Vec<FanCurvePoint>,
        #[arg(long, default_value_t = 2_000)]
        poll_ms: u32,
        #[arg(long, default_value_t = 2.0)]
        hysteresis_c: f32,
        #[arg(long)]
        plan_token: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Preview restoring the previous thermal governor / 预览恢复原温控策略
    PlanDisable {
        #[arg(long)]
        json: bool,
    },
    /// Disable the curve using an exact preview token / 使用预览令牌停用曲线
    Disable {
        #[arg(long)]
        plan_token: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run the boot-persistent fan controller.
    #[command(hide = true)]
    Daemon,
    /// Force the configured pwm-fan to maximum cooling before service exit.
    #[command(hide = true)]
    FailSafe,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LedCommands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Trigger {
        led: String,
        trigger: String,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
    Rgb {
        group: String,
        #[arg(long, default_value = "solid")]
        mode: String,
        #[arg(long, default_value_t = 255)]
        red: u8,
        #[arg(long, default_value_t = 255)]
        green: u8,
        #[arg(long, default_value_t = 255)]
        blue: u8,
        #[arg(long, default_value_t = 100)]
        brightness: u8,
        #[arg(long, default_value_t = 5_000)]
        cycle_ms: u32,
        #[arg(long)]
        confirm: bool,
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn parse_fan_curve_point(value: &str) -> std::result::Result<FanCurvePoint, String> {
    let (temperature, speed) = value
        .split_once(':')
        .ok_or_else(|| "expected TEMP:SPEED, for example 55:45".to_owned())?;
    let temperature_c = temperature
        .parse::<f32>()
        .map_err(|_| "fan curve temperature must be a number".to_owned())?;
    let speed_percent = speed
        .parse::<u8>()
        .map_err(|_| "fan curve speed must be an integer from 0 to 100".to_owned())?;
    Ok(FanCurvePoint {
        temperature_c,
        speed_percent,
    })
}

pub(crate) fn default_command() -> Commands {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Commands::Tui
    } else {
        Commands::Status { json: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_cli_parses_install_and_preview_commands() {
        let cli = Cli::try_parse_from([
            "rsetup-next",
            "system",
            "install",
            "--target",
            "sda",
            "--image",
            "/tmp/radxa.img",
        ])
        .expect("parse system install");
        match cli.command {
            Some(Commands::System {
                command:
                    SystemCommands::Install {
                        target,
                        image,
                        acknowledge,
                        json,
                    },
            }) => {
                assert_eq!(target, "sda");
                assert_eq!(image, PathBuf::from("/tmp/radxa.img"));
                assert!(acknowledge.is_none());
                assert!(!json);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "rsetup-next",
            "system",
            "install",
            "--target",
            "sda",
            "--image",
            "/tmp/radxa.img",
            "--acknowledge",
            "sda",
            "--json",
        ])
        .expect("parse with acknowledgement");
        match cli.command {
            Some(Commands::System {
                command:
                    SystemCommands::Install {
                        acknowledge, json, ..
                    },
            }) => {
                assert_eq!(acknowledge.as_deref(), Some("sda"));
                assert!(json);
            }
            other => panic!("unexpected command parsed: {other:?}"),
        }

        let cli = Cli::try_parse_from(["rsetup-next", "system", "targets", "--json"])
            .expect("parse system targets");
        assert!(matches!(
            cli.command,
            Some(Commands::System {
                command: SystemCommands::Targets { json: true }
            })
        ));

        // The device name cannot be a bare flag: it is a typed string.
        assert!(
            Cli::try_parse_from([
                "rsetup-next",
                "system",
                "install",
                "--target",
                "sda",
                "--image",
                "/tmp/radxa.img",
                "--confirm",
            ])
            .is_err(),
            "there is no boolean confirmation switch"
        );
    }

    #[test]
    fn hardware_cli_parses_nvme_subcommand_and_flags() {
        let cli = Cli::try_parse_from(["rsetup-next", "hardware", "nvme"]).expect("parse nvme");
        match cli.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Nvme { json },
            }) => {
                assert!(!json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }

        let cli_json = Cli::try_parse_from(["rsetup-next", "hardware", "nvme", "--json"])
            .expect("parse nvme json");
        match cli_json.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Nvme { json },
            }) => {
                assert!(json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }
    }

    #[test]
    fn hardware_cli_parses_mmc_subcommand_and_flags() {
        let cli = Cli::try_parse_from(["rsetup-next", "hardware", "mmc"]).expect("parse mmc");
        match cli.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Mmc { json },
            }) => {
                assert!(!json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }

        let cli_json = Cli::try_parse_from(["rsetup-next", "hardware", "mmc", "--json"])
            .expect("parse mmc json");
        match cli_json.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Mmc { json },
            }) => {
                assert!(json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }
    }

    #[test]
    fn hardware_cli_parses_storage_subcommand_and_flags() {
        let cli =
            Cli::try_parse_from(["rsetup-next", "hardware", "storage"]).expect("parse storage");
        match cli.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Storage { json },
            }) => {
                assert!(!json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }

        let cli_json = Cli::try_parse_from(["rsetup-next", "hardware", "storage", "--json"])
            .expect("parse storage json");
        match cli_json.command {
            Some(Commands::Hardware {
                command: HardwareCommands::Storage { json },
            }) => {
                assert!(json);
            }
            other => panic!("unexpected command parsed: {:?}", other),
        }
    }
}
