mod i18n;
mod server;
mod tui;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use i18n::{Locale, LocaleArg};
use rsetup_core::{
    Controller, ExecutionPolicy, ProbeMode, SourceApplyResult, SourcePlan, SourceStatus,
};
use std::{fs, io::IsTerminal, net::SocketAddr, path::PathBuf};
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
    /// Print the current board snapshot / 显示当前开发板状态
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
    /// Inspect and manage board hardware / 查看和管理开发板硬件
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
}

#[derive(Debug, Subcommand)]
enum OverlayCommands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Plan {
        #[arg(long = "enable")]
        selected_ids: Vec<String>,
        #[arg(long)]
        json: bool,
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
                OverlayCommands::Status { json } => {
                    let status = controller.overlay_status()?;
                    print_json_or_debug(&status, json)?;
                }
                OverlayCommands::Plan { selected_ids, json } => {
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
            HardwareCommands::Gpio { json } => {
                let status = controller.gpio_status()?;
                print_json_or_debug(&status, json)?;
            }
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
            },
        },
        Commands::Tui => tui::run(controller, locale)?,
        Commands::Serve { listen } => server::serve(controller, listen).await?,
        Commands::Doctor { json } => print_doctor(&controller, locale, json)?,
    }
    Ok(())
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
            "board model probe",
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
                "board model probe" => locale.text("board_model_probe"),
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
    for chunk in value.as_bytes().chunks_exact(4) {
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
