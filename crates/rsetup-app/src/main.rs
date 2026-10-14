mod i18n;
mod server;
mod tui;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use i18n::{Locale, LocaleArg};
use rsetup_core::{
    BlockDeviceBackend, Controller, ExecutionPolicy, FanCurveConfig, FanCurvePoint,
    FanCurveRequest, ProbeMode, RgbLedConfig, SourceApplyResult, SourcePlan, SourceStatus,
    SpiFlashRequest, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPlan,
    SystemInstallRequest, SystemInstallTarget, SystemInstallVerificationState, SystemMount,
};
use std::{
    fs,
    io::{BufRead, IsTerminal, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "rsetup-next",
    version,
    about = "One control plane for your SBC / 一站式 SBC 控制中心"
)]
struct Cli {
    /// Use clearly labelled synthetic board telemetry / 使用明确标注的模拟数据
    #[arg(long, global = true)]
    demo: bool,

    /// Permit the fixed action catalog to change this Linux host / 允许操作修改 Linux 主机
    #[arg(long, global = true)]
    live_execution: bool,

    /// Display language: auto, en, or zh-CN / 显示语言：auto、en 或 zh-CN
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "auto",
        value_name = "LANG"
    )]
    lang: LocaleArg,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
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
enum SystemCommands {
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
enum SourceCommands {
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
enum HardwareCommands {
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
enum OverlayCommands {
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
enum SpiFlashCommands {
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
enum VideoCommands {
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
enum ThermalCommands {
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
enum FanCurveCommands {
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
enum LedCommands {
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "rsetup_next=info".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    let locale = Locale::resolve(cli.lang);
    if cli.live_execution && !cfg!(target_os = "linux") {
        bail!(locale.text("live_linux_only"));
    }
    let mode = if cli.demo {
        ProbeMode::Demo
    } else {
        ProbeMode::Auto
    };
    let policy = if cli.live_execution {
        ExecutionPolicy::Live
    } else {
        ExecutionPolicy::from_environment()
    };
    let controller = Controller::new(mode, policy);

    let install_options = InstallOptions {
        live: cli.live_execution,
        synthetic: cli.demo || !cfg!(target_os = "linux"),
    };
    match cli.command.unwrap_or_else(default_command) {
        Commands::Status { json } => print_status(&controller, locale, json)?,
        Commands::Actions { json } => print_actions(&controller, locale, json)?,
        Commands::Run {
            action,
            confirm,
            json,
        } => {
            let run = controller
                .execute(&action, confirm)
                .map_err(|error| anyhow!(locale.action_error(&error)))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&run)?);
            } else {
                println!(
                    "{}: {}",
                    locale.action_title(&run.action_id, &run.action_title),
                    locale.run_summary(&run)
                );
                if run.synthetic {
                    if let Some(spec) = controller
                        .actions()
                        .into_iter()
                        .find(|spec| spec.id == run.action_id)
                    {
                        println!("{}:", locale.text("planned_steps"));
                        for step in locale.action_steps(&spec.id, &spec.steps) {
                            println!("- {step}");
                        }
                    }
                } else if let Some(output) = run.output {
                    println!("{}:\n{output}", locale.text("raw_output"));
                }
            }
        }
        Commands::Sources { command } => match command {
            SourceCommands::Benchmark { mirror, json } => {
                let providers = mirror.map(|id| vec![id]).unwrap_or_else(|| {
                    rsetup_core::provider_catalog()
                        .into_iter()
                        .map(|provider| provider.id)
                        .collect()
                });
                let mut results = Vec::new();
                for id in providers {
                    let result = controller
                        .benchmark_source(&id)
                        .map_err(|error| anyhow!(locale.source_error(&error)))?;
                    if !json {
                        println!("{}", locale.mirror_benchmark(&result));
                    }
                    results.push(result);
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&results)?);
                }
            }
            SourceCommands::Status { json } => {
                let status = controller
                    .source_status()
                    .map_err(|error| anyhow!(locale.source_error(&error)))?;
                print_source_status(&status, locale, json)?;
            }
            SourceCommands::Plan { mirror, json } => {
                let plan = controller
                    .plan_source_change(&mirror)
                    .map_err(|error| anyhow!(locale.source_error(&error)))?;
                print_source_plan(&plan, locale, json)?;
            }
            SourceCommands::Apply {
                mirror,
                plan_token,
                confirm,
                json,
            } => {
                let result = controller
                    .apply_source_change(&mirror, &plan_token, confirm)
                    .map_err(|error| anyhow!(locale.source_error(&error)))?;
                print_source_apply(&result, locale, json)?;
            }
        },
        Commands::Hardware { command } => match command {
            HardwareCommands::Overlays { command } => match command {
                OverlayCommands::Status { json, authorize } => {
                    if authorize {
                        controller.authorize_overlay_read()?;
                    }
                    let status = controller.overlay_status()?;
                    print_json_or_debug(&status, json)?;
                }
                OverlayCommands::Plan {
                    selected_ids,
                    json,
                    authorize,
                } => {
                    if authorize {
                        controller.authorize_overlay_read()?;
                    }
                    let plan = controller.plan_overlay_change(&selected_ids)?;
                    print_json_or_debug(&plan, json)?;
                }
                OverlayCommands::Apply {
                    selected_ids,
                    plan_token,
                    confirm,
                    json,
                } => {
                    let result =
                        controller.apply_overlay_change(&selected_ids, &plan_token, confirm)?;
                    print_json_or_debug(&result, json)?;
                }
            },
            HardwareCommands::Gpio { json, authorize } => {
                if authorize {
                    controller.authorize_overlay_read()?;
                }
                let status = controller.gpio_status()?;
                print_json_or_debug(&status, json)?;
            }
            HardwareCommands::SpiFlash { command } => match command {
                SpiFlashCommands::Status { json } => {
                    let status = controller.spi_flash_status()?;
                    print_json_or_debug(&status, json)?;
                }
                SpiFlashCommands::Plan {
                    operation,
                    target,
                    image,
                    json,
                } => {
                    let request = SpiFlashRequest {
                        operation,
                        target_id: target,
                        image_id: image,
                    };
                    let plan = controller.plan_spi_flash(&request)?;
                    print_json_or_debug(&plan, json)?;
                }
                SpiFlashCommands::Apply {
                    operation,
                    target,
                    image,
                    plan_token,
                    confirm,
                    json,
                } => {
                    let request = SpiFlashRequest {
                        operation,
                        target_id: target,
                        image_id: image,
                    };
                    let result = controller.apply_spi_flash(&request, &plan_token, confirm)?;
                    print_json_or_debug(&result, json)?;
                }
            },
            HardwareCommands::Leds { command } => match command {
                LedCommands::Status { json } => {
                    let status = controller.led_status()?;
                    print_json_or_debug(&status, json)?;
                }
                LedCommands::Trigger {
                    led,
                    trigger,
                    confirm,
                    json,
                } => {
                    let run = controller.apply_led_trigger(&led, &trigger, confirm)?;
                    print_json_or_debug(&run, json)?;
                }
                LedCommands::Rgb {
                    group,
                    mode,
                    red,
                    green,
                    blue,
                    brightness,
                    cycle_ms,
                    confirm,
                    json,
                } => {
                    let config = RgbLedConfig {
                        group_id: group,
                        mode,
                        red,
                        green,
                        blue,
                        brightness,
                        cycle_ms,
                    };
                    let run = controller.apply_rgb_led(&config, confirm)?;
                    print_json_or_debug(&run, json)?;
                }
            },
            HardwareCommands::Video { command } => match command {
                VideoCommands::Status { json } => {
                    let status = controller.video_status()?;
                    print_json_or_debug(&status, json)?;
                }
                VideoCommands::Capture { device, output } => {
                    let frame = controller.capture_video_frame(&device)?;
                    let bytes = decode_base64(&frame.base64)
                        .ok_or_else(|| anyhow!("invalid frame returned by provider"))?;
                    fs::write(&output, bytes)?;
                    println!("{}", output.display());
                }
            },
            HardwareCommands::Thermal { command } => match command {
                ThermalCommands::Status { json } => {
                    let status = controller.thermal_status()?;
                    print_json_or_debug(&status, json)?;
                }
                ThermalCommands::Set {
                    policy,
                    confirm,
                    json,
                } => {
                    let run = controller.apply_thermal_policy(&policy, confirm)?;
                    print_json_or_debug(&run, json)?;
                }
                ThermalCommands::FanCurve { command } => match command {
                    FanCurveCommands::Status { json } => {
                        let status = controller.fan_curve_status()?;
                        print_json_or_debug(&status, json)?;
                    }
                    FanCurveCommands::Plan {
                        zone,
                        device,
                        points,
                        poll_ms,
                        hysteresis_c,
                        json,
                    } => {
                        let request =
                            fan_curve_request(zone, device, points, poll_ms, hysteresis_c);
                        let plan = controller.plan_fan_curve(&request)?;
                        print_json_or_debug(&plan, json)?;
                    }
                    FanCurveCommands::Apply {
                        zone,
                        device,
                        points,
                        poll_ms,
                        hysteresis_c,
                        plan_token,
                        confirm,
                        json,
                    } => {
                        let request =
                            fan_curve_request(zone, device, points, poll_ms, hysteresis_c);
                        let result = controller.apply_fan_curve(&request, &plan_token, confirm)?;
                        print_json_or_debug(&result, json)?;
                    }
                    FanCurveCommands::PlanDisable { json } => {
                        let request = FanCurveRequest {
                            enabled: false,
                            config: None,
                        };
                        let plan = controller.plan_fan_curve(&request)?;
                        print_json_or_debug(&plan, json)?;
                    }
                    FanCurveCommands::Disable {
                        plan_token,
                        confirm,
                        json,
                    } => {
                        let request = FanCurveRequest {
                            enabled: false,
                            config: None,
                        };
                        let result = controller.apply_fan_curve(&request, &plan_token, confirm)?;
                        print_json_or_debug(&result, json)?;
                    }
                    FanCurveCommands::Daemon => run_fan_curve_daemon(&controller).await?,
                    FanCurveCommands::FailSafe => {
                        let tick = controller.fan_curve_shutdown_failsafe()?;
                        tracing::warn!(
                            cooling_state = tick.cooling_state,
                            "fan curve service exit forced maximum cooling"
                        );
                    }
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
            HardwareCommands::Mmc { json } => {
                let status = controller.mmc_status()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!("{}", format_mmc_status(&status, locale));
                }
            }
            HardwareCommands::Storage { json } => {
                let status = controller.storage_status()?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!("{}", format_storage_status(&status, locale));
                }
            }
        },
        Commands::Tui => tui::run(controller, locale)?,
        Commands::Serve { listen } => server::serve(controller, listen).await?,
        Commands::Doctor { json } => print_doctor(&controller, locale, json)?,
        Commands::System { command } => run_system_command(install_options, locale, command)?,
    }
    Ok(())
}

fn parse_fan_curve_point(value: &str) -> std::result::Result<FanCurvePoint, String> {
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

fn fan_curve_request(
    zone_id: String,
    cooling_device_id: String,
    points: Vec<FanCurvePoint>,
    poll_interval_ms: u32,
    hysteresis_c: f32,
) -> FanCurveRequest {
    FanCurveRequest {
        enabled: true,
        config: Some(FanCurveConfig {
            zone_id,
            cooling_device_id,
            poll_interval_ms,
            hysteresis_c,
            points,
        }),
    }
}

async fn run_fan_curve_daemon(controller: &Controller) -> Result<()> {
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        let tick = match controller.fan_curve_tick() {
            Ok(tick) => tick,
            Err(error) => {
                force_fan_curve_failsafe(controller, "fatal controller error");
                return Err(error.into());
            }
        };
        if tick.failsafe {
            tracing::warn!(
                cooling_state = tick.cooling_state,
                "fan curve sensor fail-safe forced maximum cooling"
            );
        } else {
            tracing::debug!(
                temperature_c = tick.temperature_c,
                speed_percent = tick.speed_percent,
                cooling_state = tick.cooling_state,
                "fan curve step applied"
            );
        }
        #[cfg(unix)]
        tokio::select! {
            result = &mut interrupt => {
                result?;
                break;
            }
            _ = terminate.recv() => break,
            () = tokio::time::sleep(Duration::from_millis(u64::from(tick.poll_interval_ms))) => {}
        }
        #[cfg(not(unix))]
        tokio::select! {
            result = &mut interrupt => {
                result?;
                break;
            }
            () = tokio::time::sleep(Duration::from_millis(u64::from(tick.poll_interval_ms))) => {}
        }
    }
    force_fan_curve_failsafe(controller, "service shutdown");
    Ok(())
}

fn force_fan_curve_failsafe(controller: &Controller, reason: &str) {
    match controller.fan_curve_shutdown_failsafe() {
        Ok(tick) => tracing::warn!(
            cooling_state = tick.cooling_state,
            reason,
            "fan curve fail-safe forced maximum cooling"
        ),
        Err(error) => tracing::error!(%error, reason, "unable to force maximum cooling"),
    }
}

fn print_source_status(status: &SourceStatus, locale: Locale, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    println!("{} · {}", status.distribution_name, status.architecture);
    println!(
        "{}: {} · {}: {}",
        locale.text("system_sources"),
        status.current_system_provider.as_deref().unwrap_or("--"),
        locale.text("radxa_sources"),
        status.current_radxa_provider.as_deref().unwrap_or("--")
    );
    println!("{}:", locale.text("managed_source_files"));
    for file in &status.files {
        println!(
            "- {} · {} · {}",
            file.path, file.format, file.managed_entries
        );
    }
    println!("{}:", locale.text("mirror_providers"));
    for provider in &status.providers {
        println!(
            "- {:<10} {} · {}",
            provider.id, provider.name, provider.location
        );
    }
    Ok(())
}

fn print_source_plan(plan: &SourcePlan, locale: Locale, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(plan)?);
        return Ok(());
    }
    println!(
        "{}: {} ({})",
        locale.text("selected_mirror"),
        plan.provider.name,
        plan.provider.id
    );
    println!("{}: {}", locale.text("source_plan_token"), plan.plan_token);
    if plan.changes.is_empty() {
        println!("{}", locale.text("no_source_changes"));
    }
    for change in &plan.changes {
        println!(
            "\n{} · {} {}",
            change.path,
            change.replacements,
            locale.text("replacements")
        );
        for (before, after) in change.before.iter().zip(&change.after) {
            println!("- {before}\n+ {after}");
        }
    }
    for warning in &plan.warnings {
        println!("! {}", locale.source_warning(warning));
    }
    Ok(())
}

fn print_source_apply(result: &SourceApplyResult, locale: Locale, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    println!(
        "{}: {}",
        locale.action_title(&result.run.action_id, &result.run.action_title),
        locale.run_summary(&result.run)
    );
    if !result.backups.is_empty() {
        println!("{}:", locale.text("backup_files"));
        for backup in &result.backups {
            println!("- {backup}");
        }
    }
    if let Some(output) = &result.run.output {
        println!("{}:\n{output}", locale.text("raw_output"));
    }
    Ok(())
}

fn default_command() -> Commands {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Commands::Tui
    } else {
        Commands::Status { json: false }
    }
}

fn print_status(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let snapshot = controller.snapshot()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let memory = snapshot
        .metrics
        .memory_percent()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| locale.text("not_available").into());
    println!(
        "{} · {}",
        snapshot.identity.product, snapshot.identity.hostname
    );
    println!(
        "{} / {} / {}",
        snapshot.identity.operating_system,
        snapshot.identity.kernel,
        snapshot.identity.architecture
    );
    let temperature = snapshot
        .metrics
        .temperature_c
        .map(|value| format!("{value:.1}°C"))
        .unwrap_or_else(|| locale.text("not_available").into());
    let cpu = snapshot
        .metrics
        .cpu_percent
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| locale.text("not_available").into());
    if locale.is_zh() {
        println!("处理器 {cpu:>6}   内存 {memory:>6}   温度 {temperature}");
    } else {
        println!("CPU {cpu:>6}   MEM {memory:>6}   TEMP {temperature}");
    }
    println!(
        "{} {} · {} {} · {} {}{}",
        snapshot.interfaces.len(),
        locale.text("network_interfaces"),
        snapshot
            .capabilities
            .iter()
            .filter(|cap| cap.available)
            .count(),
        locale.text("capability_signals"),
        snapshot.alerts.len(),
        locale.text("alerts"),
        if snapshot.synthetic {
            format!(" · {}", locale.text("synthetic_data"))
        } else {
            String::new()
        }
    );
    Ok(())
}

fn print_actions(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let actions = controller.actions();
    if json {
        println!("{}", serde_json::to_string_pretty(&actions)?);
        return Ok(());
    }
    for action in actions {
        let unavailable = if action.available {
            String::new()
        } else {
            format!(
                " [{}: {}]",
                locale.text("unavailable"),
                locale.action_unavailable_reason(
                    action.unavailable_reason.as_deref().unwrap_or("--")
                )
            )
        };
        println!(
            "{:<36} {:<9} {}{}{}",
            action.id,
            locale.risk(action.risk),
            locale.action_title(&action.id, &action.title),
            if action.requires_root {
                format!(" [{}]", locale.text("root"))
            } else {
                String::new()
            },
            unavailable,
        );
    }
    Ok(())
}

fn print_doctor(controller: &Controller, locale: Locale, json: bool) -> Result<()> {
    let snapshot = controller.snapshot()?;
    let checks: Vec<(&str, bool, &str)> = vec![
        (
            "probe",
            true,
            if snapshot.synthetic {
                "synthetic demo provider"
            } else {
                "live Linux provider"
            },
        ),
        (
            "execution",
            true,
            match controller.policy() {
                ExecutionPolicy::DryRun => "dry-run guard enabled",
                ExecutionPolicy::Live => "LIVE changes enabled",
            },
        ),
        (
            "native-actions",
            true,
            "built into the rsetup-next control plane",
        ),
        (
            "device-tree",
            std::path::Path::new("/proc/device-tree/model").exists(),
            "SBC model probe",
        ),
    ];
    if json {
        let value: Vec<_> = checks.iter().map(|(id, ready, detail)| serde_json::json!({"id": id, "ready": ready, "detail": detail})).collect();
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        for (id, ready, detail) in checks {
            let label = match id {
                "probe" => locale.text("probe"),
                "execution" => locale.text("execution"),
                "native-actions" => locale.text("native_actions"),
                "device-tree" => locale.text("device_tree"),
                _ => id,
            };
            let detail = match detail {
                "synthetic demo provider" => locale.text("synthetic_provider"),
                "live Linux provider" => locale.text("live_provider"),
                "dry-run guard enabled" => locale.text("dry_run_enabled"),
                "LIVE changes enabled" => locale.text("live_changes_enabled"),
                "built into the rsetup-next control plane" => locale.text("native_actions_ready"),
                "SBC model probe" => locale.text("board_model_probe"),
                _ => detail,
            };
            println!(
                "{} {:<14} {}",
                if ready {
                    locale.text("ready")
                } else {
                    locale.text("unavailable")
                },
                label,
                detail
            );
        }
    }
    Ok(())
}

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

fn format_nvme_status(status: &rsetup_core::NvmeStatus, locale: Locale) -> String {
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

fn format_mmc_status(status: &rsetup_core::MmcStatus, locale: Locale) -> String {
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

fn format_storage_status(status: &rsetup_core::StorageStatus, locale: Locale) -> String {
    let nvme = format_nvme_status(&status.nvme, locale);
    let mmc = format_mmc_status(&status.mmc, locale);
    format!("{nvme}\n\n{mmc}")
}

fn print_json_or_debug<T>(value: &T, json: bool) -> Result<()>
where
    T: serde::Serialize + std::fmt::Debug,
{
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{value:#?}");
    }
    Ok(())
}

fn decode_base64(value: &str) -> Option<Vec<u8>> {
    fn decode(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if value.len() % 4 != 0 {
        return None;
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let chunk_count = value.len() / 4;
    for (index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        if (chunk.contains(&b'=') && index + 1 != chunk_count)
            || (chunk[2] == b'=' && chunk[3] != b'=')
        {
            return None;
        }
        let a = u32::from(decode(chunk[0])?);
        let b = u32::from(decode(chunk[1])?);
        let c = if chunk[2] == b'=' {
            0
        } else {
            u32::from(decode(chunk[2])?)
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            u32::from(decode(chunk[3])?)
        };
        if (chunk[2] == b'=' && b & 15 != 0) || (chunk[3] == b'=' && chunk[2] != b'=' && c & 3 != 0)
        {
            return None;
        }
        let bits = (a << 18) | (b << 12) | (c << 6) | d;
        output.push((bits >> 16) as u8);
        if chunk[2] != b'=' {
            output.push((bits >> 8) as u8);
        }
        if chunk[3] != b'=' {
            output.push(bits as u8);
        }
    }
    Some(output)
}

// ---------------------------------------------------------------------------
// System image installation: `system targets|plan|install`
// ---------------------------------------------------------------------------

/// Path of the root-only helper and the authorizer that starts it. The helper
/// accepts fixed verbs only.
const PRIVILEGED_HELPER: &str = "/usr/libexec/rsetup-next-helper";
const PKEXEC: &str = "pkexec";

/// Stable codes for a system-install failure. This is the vocabulary every
/// surface reports (CLI, HTTP envelope, Tauri command error), so a script reads
/// one code whichever door it came through.
pub(crate) fn system_install_error_code(error: &SystemInstallError) -> &'static str {
    match error {
        SystemInstallError::Unsupported(_) => "install_unsupported",
        SystemInstallError::InvalidInput(_) => "invalid_install_request",
        SystemInstallError::ConfirmationRequired => "confirmation_required",
        SystemInstallError::AcknowledgementMismatch { .. } => "acknowledgement_mismatch",
        SystemInstallError::PlanRequired => "plan_required",
        SystemInstallError::StalePlan => "stale_plan",
        SystemInstallError::RootRequired => "root_required",
        SystemInstallError::Conflict(_) => "install_conflict",
        SystemInstallError::RunningSystem(_) => "writes_running_system",
        SystemInstallError::Io(_) => "internal_error",
    }
}

/// The two CLI switches the install path needs, captured before the command
/// value is consumed.
#[derive(Clone, Copy)]
struct InstallOptions {
    /// `--live-execution`: a real write is permitted.
    live: bool,
    /// Live inspection on Linux, the labelled demo provider elsewhere — the same
    /// resolution the rest of the CLI uses for its probe mode.
    synthetic: bool,
}

fn system_install_backend(options: InstallOptions) -> Arc<dyn BlockDeviceBackend> {
    if options.synthetic {
        Arc::new(SyntheticBackend::default())
    } else {
        Arc::new(SystemBlockBackend::new(None))
    }
}

/// Without `--live-execution` the install runs against a dry-run writer: the
/// real device identity is shown and the acknowledgement is judged by the core,
/// but no byte reaches a device.
fn system_install_manager(options: InstallOptions) -> SystemInstallManager {
    let backend = system_install_backend(options);
    let backend: Arc<dyn BlockDeviceBackend> = if options.live {
        backend
    } else {
        Arc::new(DryRunBackend::new(backend))
    };
    SystemInstallManager::with_backend(backend, Arc::new(SystemImageOpener), options.synthetic)
}

/// Reports the real target identity but never writes and never asks the kernel
/// to re-read a partition table.
struct DryRunBackend {
    inner: Arc<dyn BlockDeviceBackend>,
    written: Mutex<u64>,
}

impl DryRunBackend {
    fn new(inner: Arc<dyn BlockDeviceBackend>) -> Self {
        Self {
            inner,
            written: Mutex::new(0),
        }
    }
}

impl BlockDeviceBackend for DryRunBackend {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        self.inner.targets()
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        self.inner.mounts()
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        *self.written.lock().expect("dry-run counter") += chunk.len() as u64;
        Ok(())
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> Result<Vec<rsetup_core::SystemInstallPartition>, SystemInstallError> {
        Err(SystemInstallError::Unsupported(
            "dry run: the partition table was not re-read".into(),
        ))
    }
}

fn system_install_request(target: &str, image: &Path) -> SystemInstallRequest {
    SystemInstallRequest {
        target_id: target.to_owned(),
        image_path: Some(image.display().to_string()),
        image_id: None,
    }
}

/// Where the write happens: in this process, or through the root-only helper.
enum InstallExecer<'a> {
    InProcess(&'a SystemInstallManager),
    Privileged,
}

fn effective_uid() -> Option<u32> {
    // SAFETY: geteuid has no preconditions.
    Some(unsafe { libc::geteuid() })
}

fn run_system_command(
    options: InstallOptions,
    locale: Locale,
    command: SystemCommands,
) -> Result<()> {
    match command {
        SystemCommands::Targets { json } => {
            let manager = system_install_manager(options);
            let targets = manager
                .targets()
                .map_err(|error| install_failure(error, locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&targets)?);
            } else {
                write_install_targets(&targets, locale, &mut std::io::stdout())?;
            }
            Ok(())
        }
        SystemCommands::Plan {
            target,
            image,
            json,
        } => {
            let manager = system_install_manager(options);
            let request = system_install_request(&target, &image);
            let plan = manager
                .plan(&request)
                .map_err(|error| install_failure(error, locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                write_install_plan(&plan, locale, &mut std::io::stdout())?;
            }
            Ok(())
        }
        SystemCommands::Install {
            target,
            image,
            acknowledge,
            json,
        } => {
            let manager = system_install_manager(options);
            let request = system_install_request(&target, &image);
            // A live write that needs root goes through the fixed-verb helper,
            // exactly like the other privileged operations.
            let execer = if options.live && !options.synthetic && effective_uid() != Some(0) {
                InstallExecer::Privileged
            } else {
                InstallExecer::InProcess(&manager)
            };
            let stdin = std::io::stdin();
            if acknowledge.is_none() && !std::io::stdin().is_terminal() {
                eprintln!("{}", locale.text("install_not_a_terminal"));
            }
            let mut input = stdin.lock();
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let result = run_system_install(
                &manager,
                &execer,
                &request,
                acknowledge,
                locale,
                !options.live,
                &mut input,
                &mut out,
            )
            .map_err(|error| install_failure(error, locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                write_install_result(&result, locale, &mut std::io::stdout())?;
            }
            Ok(())
        }
    }
}

/// Plans, shows every irreversible consequence, takes the typed device name and
/// applies it. The typed value is judged once, inside the core, so this CLI can
/// never drift from the other surfaces.
#[allow(clippy::too_many_arguments)]
fn run_system_install(
    manager: &SystemInstallManager,
    execer: &InstallExecer<'_>,
    request: &SystemInstallRequest,
    acknowledgement: Option<String>,
    locale: Locale,
    dry_run: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<SystemInstallApplyResult, SystemInstallError> {
    let plan = manager.plan(request)?;
    write_install_plan(&plan, locale, out).map_err(io_failure)?;
    if dry_run {
        writeln!(out, "{}", locale.text("install_dry_run_notice")).map_err(io_failure)?;
    }
    let acknowledgement = match acknowledgement {
        Some(value) => value,
        None => prompt_install_acknowledgement(&plan, locale, input, out).map_err(io_failure)?,
    };
    match execer {
        InstallExecer::InProcess(manager) => {
            manager.apply_live(request, &plan.plan_token, &acknowledgement)
        }
        InstallExecer::Privileged => {
            run_privileged_system_install(request, &plan.plan_token, &acknowledgement)
        }
    }
}

fn io_failure(error: std::io::Error) -> SystemInstallError {
    SystemInstallError::Io(format!("unable to read the typed confirmation: {error}"))
}

/// Asks for the device name and returns exactly what the user typed; the core
/// decides whether it is acceptable.
fn prompt_install_acknowledgement(
    plan: &SystemInstallPlan,
    locale: Locale,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> std::io::Result<String> {
    writeln!(
        out,
        "{}: {}",
        locale.text("install_confirm_prompt"),
        plan.target.id
    )?;
    writeln!(out, "{}", locale.text("install_confirm_hint"))?;
    write!(out, "> ")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// The root-only helper's fixed verb: no shell, no arbitrary arguments, and the
/// helper re-validates the image and re-judges the typed name with the core.
fn run_privileged_system_install(
    request: &SystemInstallRequest,
    plan_token: &str,
    acknowledgement: &str,
) -> Result<SystemInstallApplyResult, SystemInstallError> {
    if !Path::new(PRIVILEGED_HELPER).is_file() {
        return Err(SystemInstallError::RootRequired);
    }
    let Some(image_path) = request.image_path.as_deref() else {
        return Err(SystemInstallError::PlanRequired);
    };
    let output = std::process::Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "system-install-apply",
            &request.target_id,
            image_path,
            plan_token,
            acknowledgement,
            "--confirmed",
        ])
        .output()
        .map_err(|error| {
            SystemInstallError::Io(format!("unable to start the privileged helper: {error}"))
        })?;
    if output.status.code() == Some(126) {
        return Err(SystemInstallError::Io(
            "administrator authorization was canceled".into(),
        ));
    }
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(SystemInstallError::Io(format!(
            "the privileged helper failed: {}",
            detail.trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| SystemInstallError::Io(format!("invalid helper response: {error}")))
}

/// JSON mode prints the machine-readable envelope on stdout; the returned error
/// always carries the stable code for the human-readable path.
fn install_failure(error: SystemInstallError, locale: Locale, json: bool) -> anyhow::Error {
    let code = system_install_error_code(&error);
    let message = locale.install_error(&error);
    if json {
        println!(
            "{}",
            serde_json::json!({"error": {"code": code, "message": message}})
        );
    }
    anyhow!("{code}: {message}")
}

fn write_install_targets(
    targets: &[SystemInstallTarget],
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    for target in targets {
        let mut notes = Vec::new();
        if target.removable {
            notes.push(locale.text("install_removable").to_string());
        }
        if !target.mount_points.is_empty() {
            notes.push(format!(
                "{} {}",
                locale.text("install_mounted"),
                target.mount_points.join(", ")
            ));
        }
        if target.holds_running_system {
            notes.push(locale.text("install_running_system").to_string());
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!(" · {}", notes.join(" · "))
        };
        writeln!(
            out,
            "{} — {} {} · {} · {} {}{}",
            target.path,
            locale.install_device_kind(target.kind),
            target.model,
            format_bytes(target.size_bytes),
            target.partitions.len(),
            locale.text("install_destroyed_partitions"),
            suffix
        )?;
    }
    Ok(())
}

/// The full consequence list a user must see before typing the device name.
fn write_install_plan(
    plan: &SystemInstallPlan,
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", locale.text("install_plan_title"))?;
    writeln!(
        out,
        "{}: {} — {} ({})",
        locale.text("install_target"),
        plan.target.path,
        locale.install_device_kind(plan.target.kind),
        plan.target.model
    )?;
    writeln!(
        out,
        "{}: {}",
        locale.text("install_capacity"),
        format_bytes(plan.target.size_bytes)
    )?;
    if plan.target.removable {
        writeln!(out, "{}", locale.text("install_removable"))?;
    }
    if !plan.target.mount_points.is_empty() {
        writeln!(
            out,
            "{}: {}",
            locale.text("install_mounted"),
            plan.target.mount_points.join(", ")
        )?;
    }
    if plan.target.holds_running_system {
        writeln!(out, "{}", locale.text("install_running_system"))?;
    }
    if plan.destroys.is_empty() {
        writeln!(out, "{}", locale.text("install_no_partitions"))?;
    } else {
        writeln!(
            out,
            "{} ({}):",
            locale.text("install_destroyed_partitions"),
            plan.destroys.len()
        )?;
        for partition in &plan.destroys {
            let mount = partition
                .mount_point
                .as_deref()
                .map(|point| format!(" · {} {point}", locale.text("install_partition_mounted_at")))
                .unwrap_or_default();
            writeln!(
                out,
                "  - {} · {}{mount}",
                partition.name,
                format_bytes(partition.size_bytes)
            )?;
        }
    }
    writeln!(out, "{}: {}", locale.text("install_image"), plan.image.path)?;
    writeln!(
        out,
        "  {}: {:?} · {}: {} · {}: {}",
        locale.text("install_image_format"),
        plan.image.format,
        locale.text("install_image_size"),
        format_bytes(plan.image.size_bytes),
        locale.text("install_image_uncompressed"),
        format_bytes(plan.image.uncompressed_bytes)
    )?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_image_sha256"),
        plan.image.sha256_prefix
    )?;
    let table = plan.image.partition_table.as_deref().unwrap_or("");
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_partition_table"),
        if table.is_empty() {
            locale.text("install_partition_table_unknown").to_string()
        } else {
            table.to_uppercase()
        }
    )?;
    if plan.requires_root {
        writeln!(out, "{}", locale.text("install_requires_root"))?;
    }
    for warning in &plan.warnings {
        writeln!(out, "! {}", locale.install_warning(warning))?;
    }
    Ok(())
}

fn write_install_result(
    result: &SystemInstallApplyResult,
    locale: Locale,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    writeln!(out, "{}", locale.text("install_written"))?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_target"),
        result.plan.target.path
    )?;
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_bytes_written"),
        format_bytes(result.bytes_written)
    )?;
    // "Unsupported" covers two different things: a host that cannot re-read a
    // partition table, and a compressed image whose layout was never parsed. The
    // core says which one it was, so its own wording is what gets printed rather
    // than a general label that would be wrong for one of them.
    let verification = match result.verification.state {
        SystemInstallVerificationState::Matched => {
            locale.text("install_verification_matched").to_string()
        }
        SystemInstallVerificationState::Mismatched => {
            let text = locale.text("install_verification_mismatched").to_string();
            match &result.verification.detail {
                Some(detail) => format!("{text} ({detail})"),
                None => text,
            }
        }
        SystemInstallVerificationState::Unsupported => {
            // Two different situations share this state: a host that cannot re-read
            // a partition table, and a compressed image whose layout was never
            // parsed (the table it did re-read had nothing to compare against). The
            // second one carries the table it read, and saying "this host cannot
            // re-read partition tables" there would be wrong, so it gets its own
            // wording with the core's reason.
            if result.verification.observed.is_some() {
                let text = locale
                    .text("install_verification_compressed_not_compared")
                    .to_string();
                match &result.verification.detail {
                    Some(detail) => format!("{text} ({detail})"),
                    None => text,
                }
            } else {
                match &result.verification.detail {
                    Some(detail) => detail.clone(),
                    None => locale.text("install_verification_unsupported").to_string(),
                }
            }
        }
    };
    writeln!(
        out,
        "  {}: {}",
        locale.text("install_verification"),
        verification
    )?;
    if result.run.synthetic {
        writeln!(out, "  {}", locale.text("install_synthetic_notice"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rsetup_core::{
        MmcDevice, MmcHealth, MmcStatus, NvmeDevice, NvmeSmartLog, NvmeStatus, StorageStatus,
    };

    /// Scratch directory under `target/`, removed when the test ends so image
    /// sized fixtures never accumulate.
    struct InstallScratch(PathBuf);

    impl std::ops::Deref for InstallScratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for InstallScratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn install_scratch(name: &str) -> InstallScratch {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/system-install-cli-tests")
            .join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(&root).expect("create scratch directory");
        InstallScratch(root)
    }

    /// A minimal image with a valid MBR: two sectors of table, one partition.
    fn mbr_image() -> Vec<u8> {
        let mut image = vec![0u8; 2 * 1024 * 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x83;
        image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
        image
    }

    fn demo_install_manager(live: bool) -> SystemInstallManager {
        system_install_manager(InstallOptions {
            live,
            synthetic: true,
        })
    }

    fn install_request_for(target: &str, image: &Path) -> SystemInstallRequest {
        system_install_request(target, image)
    }

    /// Runs the CLI install flow with an injected terminal.
    fn run_install(
        manager: &SystemInstallManager,
        target: &str,
        image: &Path,
        acknowledgement: Option<&str>,
        typed: &str,
        dry_run: bool,
    ) -> (Result<SystemInstallApplyResult, SystemInstallError>, String) {
        let request = install_request_for(target, image);
        let execer = InstallExecer::InProcess(manager);
        let mut input = std::io::Cursor::new(typed.as_bytes().to_vec());
        let mut out = Vec::new();
        let result = run_system_install(
            manager,
            &execer,
            &request,
            acknowledgement.map(str::to_owned),
            Locale::En,
            dry_run,
            &mut input,
            &mut out,
        );
        (result, String::from_utf8(out).expect("utf-8 output"))
    }

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
    fn install_shows_consequences_before_prompting() {
        let scratch = install_scratch("plan-first");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        let (result, output) = run_install(&manager, "sda", &image, None, "sda\n", true);
        result.expect("the demo install succeeds");
        let plan_at = output
            .find("System image write preview")
            .expect("plan shown");
        let prompt_at = output
            .find("Type the target device name")
            .expect("prompt shown");
        assert!(plan_at < prompt_at, "the consequences come first: {output}");
        assert!(output.contains("/dev/sda"), "target identity: {output}");
        assert!(output.contains("RADXA UFS 128GB"), "target model: {output}");
        assert!(output.contains("Existing partitions destroyed"), "{output}");
        assert!(
            output.contains("sda1") || output.contains("sda2"),
            "{output}"
        );
        assert!(output.contains("SHA-256"), "image identity: {output}");
        assert!(
            output.contains("Writing replaces every partition"),
            "{output}"
        );
        assert!(output.contains("cannot be rolled back"), "{output}");
        assert!(output.contains("Dry run: nothing is written"), "{output}");
        assert!(
            output.chars().filter(|c| *c == '\n').count() > 12,
            "the checklist is printed in full"
        );
    }

    #[test]
    fn install_requires_the_typed_device_name() {
        let scratch = install_scratch("typed-name");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);

        // Empty input is not a confirmation.
        let (result, output) = run_install(&manager, "sda", &image, None, "\n", true);
        assert!(
            matches!(result, Err(SystemInstallError::ConfirmationRequired)),
            "{result:?}"
        );
        assert!(output.contains("Type the target device name"), "{output}");

        // A boolean-looking value cannot pass; the name must match exactly.
        for typed in ["true", "yes", "1", "sda1", "/dev/sda", "SDA"] {
            let (result, _) = run_install(&manager, "sda", &image, None, typed, true);
            match result {
                Err(SystemInstallError::AcknowledgementMismatch {
                    expected, provided, ..
                }) => {
                    assert_eq!(expected, "sda");
                    assert_eq!(provided, typed);
                }
                other => panic!("{typed} must be rejected: {other:?}"),
            }
        }

        // The exact device name is accepted, and the write happens through the
        // same core call the other surfaces will use.
        let (result, _) = run_install(&manager, "sda", &image, None, "sda\n", true);
        let result = result.expect("the typed name is accepted");
        assert_eq!(result.plan.target.id, "sda");
        assert_eq!(result.bytes_written, mbr_image().len() as u64);
        assert!(result.run.synthetic, "the demo provider is labelled");

        // --acknowledge carries the same string, and is still judged by the core.
        let (result, _) = run_install(&manager, "sda", &image, Some("sda1"), "", true);
        assert!(matches!(
            result,
            Err(SystemInstallError::AcknowledgementMismatch { .. })
        ));
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn install_refuses_the_device_that_hosts_the_running_system() {
        let scratch = install_scratch("running-system");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        // The demo SD card carries `/` and `/boot`, and no typed name can change
        // that.
        let (result, output) = run_install(&manager, "mmcblk1", &image, Some("mmcblk1"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::RunningSystem(ref path)) if path == "/dev/mmcblk1"),
            "{result:?}"
        );
        assert!(
            !output.contains("Type the target device name"),
            "the refusal happens before any confirmation: {output}"
        );
    }

    #[test]
    fn install_rejects_an_untrusted_image_path() {
        let scratch = install_scratch("image-path");
        let real = scratch.join("real.img");
        fs::write(&real, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);

        // Relative paths never reach the privileged side.
        let (result, _) = run_install(
            &manager,
            "sda",
            Path::new("system.img"),
            Some("sda"),
            "",
            true,
        );
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(ref detail)) if detail.contains("absolute")),
            "{result:?}"
        );

        // A symlink is not a regular file.
        let link = scratch.join("link.img");
        std::os::unix::fs::symlink(&real, &link).expect("create symlink");
        let (result, _) = run_install(&manager, "sda", &link, Some("sda"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(ref detail)) if detail.contains("regular file")),
            "{result:?}"
        );

        // A missing file is reported before any write.
        let (result, _) = run_install(
            &manager,
            "sda",
            &scratch.join("absent.img"),
            Some("sda"),
            "",
            true,
        );
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(_))),
            "{result:?}"
        );

        // An image that is not a disk image is refused.
        let empty = scratch.join("empty.img");
        fs::write(&empty, b"not an image").expect("write file");
        let (result, _) = run_install(&manager, "sda", &empty, Some("sda"), "", true);
        assert!(
            matches!(result, Err(SystemInstallError::InvalidInput(_))),
            "{result:?}"
        );
    }

    #[test]
    fn the_cli_reports_the_core_verification_reason() {
        let scratch = install_scratch("verify-line");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");
        let mut rendered = Vec::new();
        write_install_result(&result, Locale::En, &mut rendered).expect("render the result");
        let output = String::from_utf8(rendered).expect("utf-8");
        // The demo writer refuses to re-read the table, and the CLI must print that
        // reason rather than a generic label or a mismatch.
        assert_eq!(
            result.verification.state,
            SystemInstallVerificationState::Unsupported
        );
        // The value printed is the core's own reason for this run, not the
        // general "unsupported" label, which would be wrong here (the host does
        // re-read tables; this provider does not).
        assert!(
            output.contains("does not re-read partition tables"),
            "{output}"
        );
        assert!(
            !output.contains(Locale::En.text("install_verification_unsupported")),
            "a generic label must not replace the real reason: {output}"
        );
        assert!(!output.contains("DOES NOT match"), "{output}");
    }

    /// The CLI text line separates the same two situations, and keeps preferring
    /// the core's reason: the JSON already carried `detail`, so the text now agrees
    /// with it instead of printing one label for both.
    #[test]
    fn the_cli_wording_separates_the_compressed_case_from_the_platform_one() {
        let scratch = install_scratch("cli-verification-wording");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");

        for locale in [Locale::En, Locale::ZhCn] {
            for observed in [true, false] {
                let mut shaped = result.clone();
                shaped.verification = rsetup_core::SystemInstallVerification {
                    state: SystemInstallVerificationState::Unsupported,
                    expected: Vec::new(),
                    observed: observed.then(|| {
                        vec![rsetup_core::SystemInstallPartition {
                            name: "sda1".into(),
                            number: 1,
                            start_bytes: 1024 * 1024,
                            size_bytes: 4 * 1024 * 1024,
                            mount_point: None,
                        }]
                    }),
                    detail: Some(if observed {
                        "the image is compressed and was not parsed before writing, so the re-read partition table was not compared".into()
                    } else {
                        "this host cannot re-read partition tables".into()
                    }),
                };
                let mut out = Vec::new();
                write_install_result(&shaped, locale, &mut out).expect("render the result");
                let text = String::from_utf8(out).expect("utf-8");
                if observed {
                    assert!(
                        text.contains(locale.text("install_verification_compressed_not_compared")),
                        "{locale:?} must word the compressed case: {text}"
                    );
                    assert!(
                        text.contains("the image is compressed and was not parsed"),
                        "{locale:?} must carry the core's reason: {text}"
                    );
                    assert!(
                        !text.contains(Locale::ZhCn.text("install_verification_unsupported")),
                        "{locale:?}: the platform label is wrong here: {text}"
                    );
                } else {
                    assert!(
                        text.contains("cannot re-read partition tables"),
                        "{locale:?} keeps the general reason: {text}"
                    );
                    assert!(
                        !text.contains(locale.text("install_verification_compressed_not_compared")),
                        "{locale:?} must not use the compressed wording: {text}"
                    );
                }
                // The line is informational: no code prefix, no failure wording.
                assert!(text.contains(locale.text("install_verification")), "{text}");
                assert!(!text.contains("install_unsupported"), "{text}");
                assert!(!text.contains("DOES NOT match"), "{text}");

                // The JSON was already correct and is untouched: the same run
                // serializes with the state and the detail the text now agrees with.
                let json = serde_json::to_value(&shaped).expect("serialize the result");
                assert_eq!(
                    json["verification"]["state"],
                    serde_json::json!("unsupported"),
                    "{json}"
                );
                assert!(
                    json["verification"]["detail"]
                        .as_str()
                        .is_some_and(|detail| detail.contains(if observed {
                            "compressed"
                        } else {
                            "cannot re-read"
                        })),
                    "{json}"
                );
            }
        }
    }

    /// The board's misleading line, at the CLI: a re-read that failed must not be
    /// printed as a mismatch, while a genuine mismatch keeps its wording.
    #[test]
    fn a_failed_reread_is_not_printed_as_a_mismatch() {
        let scratch = install_scratch("cli-reread-failure");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(true);
        let (result, _) = run_install(&manager, "sda", &image, Some("sda"), "", false);
        let result = result.expect("the write runs");

        for locale in [Locale::En, Locale::ZhCn] {
            // What the board saw: the re-read failed, the write had completed.
            let mut failed = result.clone();
            failed.verification = rsetup_core::SystemInstallVerification {
                state: SystemInstallVerificationState::Unsupported,
                expected: Vec::new(),
                observed: None,
                detail: Some(
                    "the kernel could not re-read the partition table of /dev/sda after the image was written, so the write could not be verified: unable to write the system image: the kernel did not re-read /dev/sda: Invalid argument (os error 22)".into(),
                ),
            };
            let mut out = Vec::new();
            write_install_result(&failed, locale, &mut out).expect("render the result");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(
                text.contains("could not re-read the partition table of /dev/sda"),
                "{locale:?}: {text}"
            );
            assert!(
                text.contains("after the image was written"),
                "{locale:?} must say the write completed: {text}"
            );
            assert!(
                text.contains("Invalid argument (os error 22)"),
                "{locale:?} must carry the kernel's reason: {text}"
            );
            assert!(
                !text.contains(locale.text("install_verification_mismatched")),
                "{locale:?}: the board's misleading line must be gone: {text}"
            );
            assert!(!text.contains("DOES NOT match"), "{locale:?}: {text}");
            assert!(
                !text.contains("differs from the image"),
                "{locale:?}: {text}"
            );

            // The genuine mismatch — the table was read and differs — is unchanged.
            let mut mismatched = result.clone();
            mismatched.verification = rsetup_core::SystemInstallVerification {
                state: SystemInstallVerificationState::Mismatched,
                expected: vec![rsetup_core::SystemInstallPartition {
                    name: "sda1".into(),
                    number: 1,
                    start_bytes: 1024 * 1024,
                    size_bytes: 4 * 1024 * 1024,
                    mount_point: None,
                }],
                observed: Some(vec![rsetup_core::SystemInstallPartition {
                    name: "p1".into(),
                    number: 1,
                    start_bytes: 4096,
                    size_bytes: 8 * 1024 * 1024,
                    mount_point: None,
                }]),
                detail: Some("the re-read partition table differs from the image".into()),
            };
            let mut out = Vec::new();
            write_install_result(&mismatched, locale, &mut out).expect("render the result");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(
                text.contains(locale.text("install_verification_mismatched")),
                "{locale:?} keeps the mismatch wording: {text}"
            );
            assert!(
                text.contains("differs from the image"),
                "{locale:?}: {text}"
            );
            assert!(
                !text.contains("could not re-read"),
                "{locale:?}: the two cases stay distinguishable: {text}"
            );
        }
    }

    /// The backup-GPT notice reaches the operator in the plan the CLI prints, and
    /// only when the plan carries it. The token is produced by the core (proved
    /// there); this checks the surface that shows it.
    #[test]
    fn the_backup_gpt_notice_is_printed_with_the_plan() {
        const NOTICE: &str = "the_backup_gpt_table_stays_at_the_end_of_the_image";
        let scratch = install_scratch("gpt-notice");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        let mut plan = manager
            .plan(&install_request_for("sda", &image))
            .expect("plan");
        assert!(
            !plan.warnings.contains(&NOTICE.to_owned()),
            "an MBR image says nothing about a backup GPT: {:?}",
            plan.warnings
        );

        for locale in [Locale::En, Locale::ZhCn] {
            let notice = locale.install_warning(NOTICE);
            assert!(
                notice.contains("sgdisk -e"),
                "the advice names the optional fix: {notice}"
            );
            let mut out = Vec::new();
            write_install_plan(&plan, locale, &mut out).expect("render the plan");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(
                !text.contains(&notice),
                "{locale:?} must not invent a GPT notice: {text}"
            );
        }

        plan.warnings.push(NOTICE.to_owned());
        for locale in [Locale::En, Locale::ZhCn] {
            let mut out = Vec::new();
            write_install_plan(&plan, locale, &mut out).expect("render the plan");
            let text = String::from_utf8(out).expect("utf-8");
            let notice = locale.install_warning(NOTICE);
            assert!(
                text.contains(&format!("! {notice}")),
                "{locale:?} must show the notice where the plan is read: {text}"
            );
            for fragment in match locale {
                Locale::En => vec!["primary GPT", "backup GPT", "end of the device"],
                Locale::ZhCn => vec!["主 GPT", "备份 GPT", "设备末尾"],
            } {
                assert!(
                    text.contains(fragment),
                    "{locale:?}: missing {fragment}: {notice}"
                );
            }
        }
    }

    #[test]
    fn install_error_codes_are_stable_across_surfaces() {
        let cases: Vec<(SystemInstallError, &str)> = vec![
            (
                SystemInstallError::Unsupported("x".into()),
                "install_unsupported",
            ),
            (
                SystemInstallError::InvalidInput("x".into()),
                "invalid_install_request",
            ),
            (
                SystemInstallError::ConfirmationRequired,
                "confirmation_required",
            ),
            (
                SystemInstallError::AcknowledgementMismatch {
                    expected: "sda".into(),
                    target: "/dev/sda".into(),
                    provided: "true".into(),
                },
                "acknowledgement_mismatch",
            ),
            (SystemInstallError::PlanRequired, "plan_required"),
            (SystemInstallError::StalePlan, "stale_plan"),
            (SystemInstallError::RootRequired, "root_required"),
            (SystemInstallError::Conflict("x".into()), "install_conflict"),
            (
                SystemInstallError::RunningSystem("/dev/mmcblk1".into()),
                "writes_running_system",
            ),
            (SystemInstallError::Io("x".into()), "internal_error"),
        ];
        for (error, code) in cases {
            assert_eq!(system_install_error_code(&error), code);
            assert!(!Locale::En.install_error(&error).is_empty());
            assert!(!Locale::ZhCn.install_error(&error).is_empty());
        }
    }

    #[test]
    fn privileged_install_without_the_helper_requires_root() {
        if Path::new(PRIVILEGED_HELPER).is_file() {
            // A host that ships the helper cannot assert its absence; the real
            // path is exercised on the board.
            return;
        }
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some("/tmp/radxa.img".into()),
            image_id: None,
        };
        assert!(matches!(
            run_privileged_system_install(&request, "install-plan-0", "sda"),
            Err(SystemInstallError::RootRequired)
        ));
    }

    #[test]
    fn install_plan_preview_is_localized() {
        let scratch = install_scratch("preview");
        let image = scratch.join("system.img");
        fs::write(&image, mbr_image()).expect("write image");
        let manager = demo_install_manager(false);
        let plan = manager
            .plan(&install_request_for("sda", &image))
            .expect("plan");
        for locale in [Locale::En, Locale::ZhCn] {
            let mut out = Vec::new();
            write_install_plan(&plan, locale, &mut out).expect("render plan");
            let text = String::from_utf8(out).expect("utf-8");
            assert!(text.contains("/dev/sda"), "{locale:?}: {text}");
            assert!(text.contains(&plan.image.sha256_prefix), "{locale:?}");
            assert!(text.contains("UFS"), "{locale:?}: {text}");
            assert!(
                text.contains(&format_bytes(plan.target.size_bytes)),
                "{locale:?}: {text}"
            );
            let mut targets_out = Vec::new();
            let targets = manager.targets().expect("demo targets");
            write_install_targets(&targets, locale, &mut targets_out).expect("render targets");
            let targets_text = String::from_utf8(targets_out).expect("utf-8");
            assert!(targets_text.contains("/dev/sda"), "{targets_text}");
            assert!(targets_text.contains("/dev/mmcblk1"), "{targets_text}");
        }
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

#[cfg(test)]
mod decoding_tests {
    use super::decode_base64;

    #[test]
    fn base64_requires_canonical_padding() {
        for invalid in ["AB=C", "Zg==AAAA", "Zh==", "Zm9=", "====", "A===", "abc"] {
            assert!(decode_base64(invalid).is_none(), "{invalid}");
        }
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zm8=").unwrap(), b"fo");
        assert_eq!(decode_base64("Zm9v").unwrap(), b"foo");
    }
}
