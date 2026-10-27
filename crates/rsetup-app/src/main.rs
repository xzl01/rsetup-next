mod cli;
mod fan_curve;
mod i18n;
mod output;
mod server;
mod storage_format;
mod tui;

pub(crate) use storage_format::format_bytes;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use cli::{
    Cli, Commands, FanCurveCommands, HardwareCommands, LedCommands, OverlayCommands,
    SourceCommands, SpiFlashCommands, SystemCommands, ThermalCommands, VideoCommands,
    default_command,
};
use fan_curve::{fan_curve_request, run_fan_curve_daemon};
use i18n::Locale;
use output::{
    decode_base64, print_actions, print_doctor, print_json_or_debug, print_source_apply,
    print_source_plan, print_source_status, print_status,
};
use rsetup_core::{
    BlockDeviceBackend, Controller, ExecutionPolicy, FanCurveRequest, ProbeMode, RgbLedConfig,
    SpiFlashRequest, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPlan,
    SystemInstallRequest, SystemInstallTarget, SystemInstallVerificationState, SystemMount,
};
use std::{
    fs,
    io::{BufRead, IsTerminal, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use storage_format::{format_mmc_status, format_nvme_status, format_storage_status};
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

// ---------------------------------------------------------------------------
// System image installation: `system targets|plan|install`
// ---------------------------------------------------------------------------

/// Path of the root-only helper and the authorizer that starts it. The helper
/// accepts fixed verbs only. Shared so a surface test can prove the delegated
/// route without ever starting it.
pub(crate) const PRIVILEGED_HELPER: &str = rsetup_core::privileged_helper_path();
const PKEXEC: &str = rsetup_core::pkexec_path();

/// Stable codes for a system-install failure. The vocabulary lives on the error
/// itself so the helper binary, which does not share this crate root, reports the
/// same codes.
pub(crate) fn system_install_error_code(error: &SystemInstallError) -> &'static str {
    error.code()
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

    fn is_dry_run(&self) -> bool {
        true
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

pub(crate) fn effective_uid() -> Option<u32> {
    rsetup_core::effective_uid()
}

/// Whether a real write has to be delegated to the fixed-verb helper.
///
/// The demo provider writes to nothing, and root opens the device itself;
/// anything else is refused by the kernel when it opens the target, so routing
/// it through `pkexec` is what turns a `PermissionDenied` into an administrator
/// prompt. The CLI has always decided this way; the TUI and the HTTP surface now
/// share the rule instead of each writing its own copy of it.
pub(crate) fn install_needs_privilege(
    live: bool,
    synthetic: bool,
    effective_uid: Option<u32>,
) -> bool {
    rsetup_core::install_needs_privilege(live, synthetic, effective_uid)
}

/// The outcome of a privileged install apply.
///
/// A cancelled administrator prompt is a decision, not a fault in the write, so
/// it is kept apart from the core's errors instead of being flattened into an
/// I/O failure. Every surface already has one stable code for it —
/// `authorization_canceled`, the same one the other privileged operations use —
/// and this is what lets an install report it too.
pub(crate) enum InstallApplyError {
    Core(SystemInstallError),
    AuthorizationCanceled,
    /// A refusal the privileged helper classified, carried across the process
    /// boundary. The helper runs the same core, so its code is the same
    /// vocabulary; without this every distinct refusal reached the caller as one
    /// generic error, because only the prose survived the boundary.
    Helper {
        code: String,
        message: String,
    },
}

impl From<SystemInstallError> for InstallApplyError {
    fn from(error: SystemInstallError) -> Self {
        Self::Core(error)
    }
}

impl InstallApplyError {
    /// The stable code a script reads, whichever surface it came through.
    ///
    /// Borrowed rather than `'static`: a code that crossed the helper boundary is
    /// owned data, not a literal.
    pub(crate) fn code(&self) -> &str {
        match self {
            Self::Core(error) => system_install_error_code(error),
            Self::AuthorizationCanceled => "authorization_canceled",
            // The helper already classified the failure; do not re-derive it from
            // the prose.
            Self::Helper { code, .. } => code.as_str(),
        }
    }

    /// Human copy for the CLI and the TUI. The HTTP and IPC envelopes carry
    /// `code`, and the browser page replaces this text with its own translation
    /// of that code, so the wording here is the fallback for a reader without a
    /// catalogue — which is why it is written out rather than keyed.
    pub(crate) fn message(&self, locale: Locale) -> String {
        match self {
            Self::Core(error) => locale.install_error(error),
            Self::AuthorizationCanceled => if locale.is_zh() {
                "已取消管理员授权。"
            } else {
                "Administrator authorization canceled."
            }
            .into(),
            // The helper's own wording already names the device, the image and the
            // expected acknowledgement, so it is a better explanation than a
            // reconstruction from the code alone.
            Self::Helper { message, .. } => message.clone(),
        }
    }
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
                .map_err(|error| install_failure(error.into(), locale, json))?;
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
                .map_err(|error| install_failure(error.into(), locale, json))?;
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
            let execer =
                if install_needs_privilege(options.live, options.synthetic, effective_uid()) {
                    InstallExecer::Privileged
                } else {
                    InstallExecer::InProcess(&manager)
                };
            let stdin = std::io::stdin();
            if acknowledge.is_none() && !std::io::stdin().is_terminal() {
                eprintln!("{}", locale.text("install_not_a_terminal"));
            }
            let mut input = stdin.lock();
            // In JSON mode every human-readable byte goes to stderr, so stdout
            // carries exactly one JSON document that a caller can parse. The
            // preview and the typed-confirmation prompt are a conversation with a
            // person, not part of the machine-readable result.
            let mut out: Box<dyn std::io::Write> = if json {
                Box::new(std::io::stderr())
            } else {
                Box::new(std::io::stdout())
            };
            let result = run_system_install(
                &manager,
                &execer,
                &request,
                acknowledge,
                locale,
                !options.live,
                &mut input,
                &mut *out,
            )
            .map_err(|error| install_failure(error, locale, json))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                write_install_result(&result, locale, &mut *out)?;
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
) -> Result<SystemInstallApplyResult, InstallApplyError> {
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
        InstallExecer::InProcess(manager) => manager
            .apply_live(request, &plan.plan_token, &acknowledgement)
            .map_err(InstallApplyError::Core),
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
///
/// Shared by every surface that can write: the CLI, the TUI and the HTTP server
/// all preview in their own process and then hand the plan token to the helper,
/// which treats it as the advisory preview it is and issues its own token.
/// Reads the helper's error envelope: `{"error":{"code","message"}}`.
///
/// Returns `None` when stdout is not that shape, so a helper built before the
/// envelope still reports through its exit status and stderr.
pub(crate) fn helper_error_envelope(stdout: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let error = value.get("error")?;
    let code = error.get("code")?.as_str()?.to_owned();
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((code, message))
}

pub(crate) fn run_privileged_system_install(
    request: &SystemInstallRequest,
    plan_token: &str,
    acknowledgement: &str,
) -> Result<SystemInstallApplyResult, InstallApplyError> {
    if !Path::new(PRIVILEGED_HELPER).is_file() {
        return Err(SystemInstallError::RootRequired.into());
    }
    let Some(image_path) = request.image_path.as_deref() else {
        return Err(SystemInstallError::PlanRequired.into());
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
            InstallApplyError::Core(SystemInstallError::Io(format!(
                "unable to start the privileged helper: {error}"
            )))
        })?;
    if output.status.code() == Some(126) {
        // polkit's "not authorized": the administrator dismissed the prompt.
        return Err(InstallApplyError::AuthorizationCanceled);
    }
    if !output.status.success() {
        if let Some((code, message)) = helper_error_envelope(&output.stdout) {
            return Err(InstallApplyError::Helper { code, message });
        }
        // A helper built before the envelope, or one that died before printing
        // it: fall back to its exit status and stderr.
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(SystemInstallError::Io(format!(
            "the privileged helper failed: {}",
            detail.trim()
        ))
        .into());
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        InstallApplyError::Core(SystemInstallError::Io(format!(
            "invalid helper response: {error}"
        )))
    })
}

/// JSON mode prints the machine-readable envelope on stdout; the returned error
/// always carries the stable code for the human-readable path.
fn install_failure(error: InstallApplyError, locale: Locale, json: bool) -> anyhow::Error {
    let code = error.code();
    let message = error.message(locale);
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
    use std::path::PathBuf;

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
        )
        // Every test drives the in-process execer, which cannot cancel, so the
        // plain core error is what these cases are about.
        .map_err(|error| match error {
            InstallApplyError::Core(error) => error,
            InstallApplyError::AuthorizationCanceled => {
                SystemInstallError::Io("authorization canceled".into())
            }
            InstallApplyError::Helper { message, .. } => SystemInstallError::Io(message),
        });
        (result, String::from_utf8(out).expect("utf-8 output"))
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
            Err(InstallApplyError::Core(SystemInstallError::RootRequired))
        ));
    }

    /// The routing rule itself, without root, without `pkexec` and without a
    /// device. The surfaces all read this one function, so this table is the
    /// guard against one of them silently keeping an in-process write.
    /// The helper's envelope is what keeps a distinct refusal distinct. A helper
    /// that predates it, or that died before printing one, must still degrade to
    /// the old behaviour rather than being misread.
    #[test]
    fn the_helper_error_envelope_is_read_only_when_it_is_one() {
        let envelope = br#"{"error":{"code":"stale_plan","message":"the target changed"}}"#;
        assert_eq!(
            helper_error_envelope(envelope),
            Some(("stale_plan".to_owned(), "the target changed".to_owned()))
        );
        // A success payload is not an envelope.
        assert_eq!(helper_error_envelope(br#"{"bytesWritten":1}"#), None);
        // Prose from a helper that predates the envelope, or a truncated write.
        assert_eq!(
            helper_error_envelope(b"Error: something went wrong\n"),
            None
        );
        assert_eq!(
            helper_error_envelope(br#"{"error":{"message":"no code"}}"#),
            None
        );
    }

    #[test]
    fn install_privilege_routing_follows_policy_provider_and_uid() {
        let cases = [
            // Ordinary user, live policy, real device: the reported defect. It
            // must be delegated, or opening the device just fails as EACCES.
            (true, false, Some(1000), true),
            // Root already holds the privilege the helper would grant.
            (true, false, Some(0), false),
            // The demo provider writes to nothing, so there is nothing to authorize.
            (true, true, Some(1000), false),
            // A dry run never reaches a device.
            (false, false, Some(1000), false),
            // An unreadable euid is not evidence of root.
            (true, false, None, true),
        ];
        for (live, synthetic, uid, expected) in cases {
            assert_eq!(
                install_needs_privilege(live, synthetic, uid),
                expected,
                "live={live} synthetic={synthetic} uid={uid:?}"
            );
        }
    }

    /// A dismissed administrator prompt is a decision, so it must not be
    /// flattened into the code a hard I/O failure reports.
    #[test]
    fn cancelled_authorization_keeps_its_own_code() {
        let cancelled = InstallApplyError::AuthorizationCanceled;
        assert_eq!(cancelled.code(), "authorization_canceled");
        assert!(!cancelled.message(Locale::En).is_empty());
        assert!(!cancelled.message(Locale::ZhCn).is_empty());
        assert_ne!(
            cancelled.code(),
            InstallApplyError::Core(SystemInstallError::Io("x".into())).code()
        );
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
}
