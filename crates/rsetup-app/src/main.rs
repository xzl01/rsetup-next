mod cli;
mod fan_curve;
mod i18n;
mod output;
mod server;
mod storage_format;
mod system_install;
mod tui;

pub(crate) use storage_format::format_bytes;
#[cfg(test)]
pub(crate) use system_install::PRIVILEGED_HELPER;
pub(crate) use system_install::{
    InstallApplyError, effective_uid, install_needs_privilege, run_privileged_system_install,
    system_install_error_code,
};

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use cli::{
    Cli, Commands, FanCurveCommands, HardwareCommands, LedCommands, OverlayCommands,
    SourceCommands, SpiFlashCommands, ThermalCommands, VideoCommands, default_command,
};
use fan_curve::{fan_curve_request, run_fan_curve_daemon};
use i18n::Locale;
use output::{
    decode_base64, print_actions, print_doctor, print_json_or_debug, print_source_apply,
    print_source_plan, print_source_status, print_status,
};
use rsetup_core::{
    Controller, ExecutionPolicy, FanCurveRequest, ProbeMode, RgbLedConfig, SpiFlashRequest,
};
use std::fs;
use storage_format::{format_mmc_status, format_nvme_status, format_storage_status};
use system_install::{InstallOptions, run_system_command};
use tracing_subscriber::EnvFilter;

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
