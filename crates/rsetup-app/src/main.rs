mod i18n;
mod server;
mod tui;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};
use i18n::{Locale, LocaleArg};
use rsetup_core::{
    Controller, ExecutionPolicy, ProbeMode, SourceApplyResult, SourcePlan, SourceStatus,
};
use std::{io::IsTerminal, net::SocketAddr};
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
