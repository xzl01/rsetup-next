mod i18n;
mod server;
mod tui;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use i18n::{Locale, LocaleArg};
use rsetup_core::{
    Controller, ExecutionPolicy, FanCurveConfig, FanCurvePoint, FanCurveRequest, ProbeMode,
    RgbLedConfig, SourceApplyResult, SourcePlan, SourceStatus, SpiFlashRequest,
};
use std::{fs, io::IsTerminal, net::SocketAddr, path::PathBuf, time::Duration};
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
    let memory = percent(
        snapshot.metrics.memory_used_bytes,
        snapshot.metrics.memory_total_bytes,
    );
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
    if locale.is_zh() {
        println!(
            "处理器 {:>5.1}%   内存 {:>5.1}%   温度 {temperature}",
            snapshot.metrics.cpu_percent, memory
        );
    } else {
        println!(
            "CPU {:>5.1}%   MEM {:>5.1}%   TEMP {temperature}",
            snapshot.metrics.cpu_percent, memory
        );
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
            out.push(format!(
                "  当前温度:         {:.1} °C",
                dev.smart.temperature_c
            ));
            out.push(format!(
                "  备用空间/阈值:    {}% / {}%",
                dev.smart.available_spare_percent, dev.smart.spare_threshold_percent
            ));
            out.push(format!(
                "  已使用寿命:       {}%",
                dev.smart.percentage_used
            ));
            out.push(format!(
                "  数据读写量:       读取 {} / 写入 {}",
                format_bytes(dev.smart.data_read_bytes),
                format_bytes(dev.smart.data_written_bytes)
            ));
            out.push(format!(
                "  通电时间/不安全关机: {} 小时 / {} 次",
                dev.smart.power_on_hours, dev.smart.unsafe_shutdowns
            ));
            out.push(format!(
                "  错误计数:         介质错误 {} / 错误日志项 {}",
                dev.smart.media_errors, dev.smart.num_err_log_entries
            ));
            let warning_str = if dev.smart.warning_flags.is_empty() {
                "无".to_string()
            } else {
                dev.smart.warning_flags.join(", ")
            };
            out.push(format!("  告警状态:         {}", warning_str));
        } else {
            out.push(format!("  Model:            {}", dev.model));
            out.push(format!("  Serial Number:    {}", dev.serial));
            out.push(format!("  Firmware:         {}", dev.firmware));
            out.push(format!(
                "  Total Capacity:   {} ({} bytes)",
                size_str, dev.total_bytes
            ));
            out.push(format!(
                "  Temperature:      {:.1} °C",
                dev.smart.temperature_c
            ));
            out.push(format!(
                "  Available Spare:  {}% (threshold: {}%)",
                dev.smart.available_spare_percent, dev.smart.spare_threshold_percent
            ));
            out.push(format!(
                "  Percentage Used:  {}%",
                dev.smart.percentage_used
            ));
            out.push(format!(
                "  Data Read/Write:  Read {} / Written {}",
                format_bytes(dev.smart.data_read_bytes),
                format_bytes(dev.smart.data_written_bytes)
            ));
            out.push(format!(
                "  Power-on/Shutdown: {} hrs / {} unsafe shutdowns",
                dev.smart.power_on_hours, dev.smart.unsafe_shutdowns
            ));
            out.push(format!(
                "  Error Counts:     Media errors: {} / Error entries: {}",
                dev.smart.media_errors, dev.smart.num_err_log_entries
            ));
            let warning_str = if dev.smart.warning_flags.is_empty() {
                "None".to_string()
            } else {
                dev.smart.warning_flags.join(", ")
            };
            out.push(format!("  Critical Warning: {}", warning_str));
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
            let pre_eol_str = match dev.health.pre_eol_info {
                0 => "未定义".to_string(),
                1 => "正常".to_string(),
                2 => "预警 (80% 寿命)".to_string(),
                3 => "紧急 (建议更换)".to_string(),
                other => format!("未知 ({other})"),
            };
            let life_str = |value: Option<u8>| -> String {
                match value {
                    Some(p) => format!("{p}%"),
                    None => "不支持".to_string(),
                }
            };
            let life_a_str = life_str(dev.health.life_time_est_a_percent);
            let life_b_str = life_str(dev.health.life_time_est_b_percent);
            out.push(format!("  预 EOL 状态:       {pre_eol_str}"));
            out.push(format!("  寿命估计 (A/B):   {life_a_str} / {life_b_str}"));
            let warning_str = if dev.health.warning_flags.is_empty() {
                "无".to_string()
            } else {
                dev.health.warning_flags.join(", ")
            };
            out.push(format!("  告警标志:         {warning_str}"));
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
            let pre_eol_str = match dev.health.pre_eol_info {
                0 => "Undefined".to_string(),
                1 => "Normal".to_string(),
                2 => "Warning (80% endurance)".to_string(),
                3 => "Urgent (replace soon)".to_string(),
                other => format!("Unknown ({other})"),
            };
            let life_str = |value: Option<u8>| -> String {
                match value {
                    Some(p) => format!("{p}%"),
                    None => "N/A".to_string(),
                }
            };
            let life_a_str = life_str(dev.health.life_time_est_a_percent);
            let life_b_str = life_str(dev.health.life_time_est_b_percent);
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

    out.join("\n")
}

fn format_storage_status(status: &rsetup_core::StorageStatus, locale: Locale) -> String {
    let nvme = format_nvme_status(&status.nvme, locale);
    let mmc = format_mmc_status(&status.mmc, locale);
    format!("{nvme}\n\n{mmc}")
}

fn percent(value: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        value as f32 / total as f32 * 100.0
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use rsetup_core::{
        MmcDevice, MmcHealth, MmcStatus, NvmeDevice, NvmeSmartLog, NvmeStatus, StorageStatus,
    };

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
                smart: NvmeSmartLog {
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
                },
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
                pre_eol_info: 1,
                life_time_est_a_percent: None,
                life_time_est_b_percent: None,
                warning_flags: Vec::new(),
            },
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
                smart: NvmeSmartLog {
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
                },
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
