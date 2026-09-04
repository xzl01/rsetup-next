use crate::{
    ActionRun, ActionSpec, ActionStatus, ActivityEvent, ProbeMode, RiskLevel, SourceApplyResult,
    SourceError, SourcePlan, SourceStatus, collect_snapshot,
    fan_curve::{
        FanCurveApplyResult, FanCurveManager, FanCurvePlan, FanCurveRequest, FanCurveStatus,
        FanCurveTick,
    },
    hardware::{
        GpioStatus, HardwareError, HardwareManager, LedStatus, OverlayApplyResult, OverlayPlan,
        OverlayStatus, RgbLedConfig, ThermalStatus, VideoFrame, VideoStatus,
    },
    sources::{SourceManager, source_run},
    spi_flash::{
        SpiFlashApplyResult, SpiFlashManager, SpiFlashPlan, SpiFlashRequest, SpiFlashStatus,
    },
};
use chrono::Utc;
use std::{
    collections::VecDeque,
    env, fs,
    path::Path,
    process::Command,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use uuid::Uuid;

const PRIVILEGED_HELPER: &str = "/usr/libexec/rsetup-next-helper";
const PKEXEC: &str = "/usr/bin/pkexec";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPolicy {
    DryRun,
    Live,
}

impl ExecutionPolicy {
    pub fn from_environment() -> Self {
        if cfg!(target_os = "linux") && env::var("RSETUP_EXECUTION").ok().as_deref() == Some("live")
        {
            Self::Live
        } else {
            Self::DryRun
        }
    }
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("unknown action: {0}")]
    Unknown(String),
    #[error("action is unavailable: {0}")]
    Unavailable(String),
    #[error("confirmation is required for {0}")]
    ConfirmationRequired(String),
    #[error("{0} requires root privileges")]
    RootRequired(String),
    #[error("administrator authorization failed for {0}: {1}")]
    Authorization(String, String),
    #[error("{0} requires guided input")]
    InputRequired(String),
    #[error("unable to start action: {0}")]
    Launch(String),
}

#[derive(Clone)]
pub struct Controller {
    mode: ProbeMode,
    policy: ExecutionPolicy,
    synthetic: bool,
    runs: Arc<RwLock<VecDeque<ActionRun>>>,
    activity: Arc<RwLock<VecDeque<ActivityEvent>>>,
    sources: Arc<SourceManager>,
    hardware: Arc<HardwareManager>,
    spi_flash: Arc<SpiFlashManager>,
    fan_curve: Arc<FanCurveManager>,
}

impl Controller {
    pub fn new(mode: ProbeMode, policy: ExecutionPolicy) -> Self {
        let synthetic = mode == ProbeMode::Demo || !cfg!(target_os = "linux");
        let mut activity = VecDeque::new();
        activity.push_back(ActivityEvent {
            id: Uuid::new_v4().to_string(),
            at: Utc::now(),
            kind: "system".into(),
            title: if synthetic {
                "Demo control plane ready"
            } else {
                "Local control plane ready"
            }
            .into(),
            detail: if policy == ExecutionPolicy::DryRun {
                "Inspection is active. Mutating operations will produce dry-run results.".into()
            } else {
                "Live execution is enabled for the fixed action catalog.".into()
            },
            synthetic,
        });
        Self {
            mode,
            policy,
            synthetic,
            runs: Arc::new(RwLock::new(VecDeque::new())),
            activity: Arc::new(RwLock::new(activity)),
            sources: Arc::new(SourceManager::new(synthetic)),
            hardware: Arc::new(HardwareManager::new(synthetic)),
            spi_flash: Arc::new(SpiFlashManager::new(synthetic)),
            fan_curve: Arc::new(FanCurveManager::new(synthetic)),
        }
    }

    pub fn from_environment() -> Self {
        let mode = match env::var("RSETUP_MODE").ok().as_deref() {
            Some("demo") => ProbeMode::Demo,
            Some("live") => ProbeMode::Live,
            _ => ProbeMode::Auto,
        };
        Self::new(mode, ExecutionPolicy::from_environment())
    }

    pub fn snapshot(&self) -> anyhow::Result<crate::DeviceSnapshot> {
        collect_snapshot(self.mode)
    }

    pub fn actions(&self) -> Vec<ActionSpec> {
        action_catalog(self.synthetic)
    }

    pub fn activity(&self) -> Vec<ActivityEvent> {
        self.activity
            .read()
            .expect("activity lock poisoned")
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn runs(&self) -> Vec<ActionRun> {
        self.runs
            .read()
            .expect("run lock poisoned")
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn policy(&self) -> ExecutionPolicy {
        self.policy
    }

    pub fn source_status(&self) -> Result<SourceStatus, SourceError> {
        self.sources.status()
    }

    pub fn overlay_status(&self) -> Result<OverlayStatus, HardwareError> {
        self.hardware.overlay_status()
    }

    pub fn plan_overlay_change(
        &self,
        selected_ids: &[String],
    ) -> Result<OverlayPlan, HardwareError> {
        self.hardware.plan_overlays(selected_ids)
    }

    pub fn apply_overlay_change(
        &self,
        selected_ids: &[String],
        plan_token: &str,
        confirmed: bool,
    ) -> Result<OverlayApplyResult, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        if plan_token.trim().is_empty() {
            return Err(HardwareError::PlanRequired);
        }
        let result = if self.policy == ExecutionPolicy::DryRun {
            let plan = self.plan_overlay_change(selected_ids)?;
            if plan.plan_token != plan_token {
                return Err(HardwareError::StalePlan);
            }
            OverlayApplyResult {
                run: hardware_dry_run(
                    "hardware.overlays",
                    "Switch device-tree overlays",
                    &plan
                        .changes
                        .iter()
                        .map(|change| {
                            format!(
                                "{}: {}",
                                change.id,
                                if change.after_enabled {
                                    "enable"
                                } else {
                                    "disable"
                                }
                            )
                        })
                        .collect::<Vec<_>>(),
                ),
                reboot_required: plan.reboot_required,
                plan,
            }
        } else if effective_uid() != Some(0) {
            let result = run_privileged_overlay_apply(selected_ids, plan_token)?;
            self.record_run(result.run.clone());
            return Ok(result);
        } else {
            self.hardware
                .apply_overlays_live(selected_ids, plan_token)?
        };
        self.record_run(result.run.clone());
        Ok(result)
    }

    pub fn gpio_status(&self) -> Result<GpioStatus, HardwareError> {
        self.hardware.gpio_status()
    }

    pub fn gpio_status_for_profile(
        &self,
        profile_id: Option<&str>,
    ) -> Result<GpioStatus, HardwareError> {
        self.hardware.gpio_status_for_profile(profile_id)
    }

    pub fn led_status(&self) -> Result<LedStatus, HardwareError> {
        self.hardware.led_status()
    }

    pub fn spi_flash_status(&self) -> Result<SpiFlashStatus, HardwareError> {
        self.spi_flash.status()
    }

    pub fn plan_spi_flash(&self, request: &SpiFlashRequest) -> Result<SpiFlashPlan, HardwareError> {
        self.spi_flash.plan(request)
    }

    pub fn apply_spi_flash(
        &self,
        request: &SpiFlashRequest,
        plan_token: &str,
        confirmed: bool,
    ) -> Result<SpiFlashApplyResult, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        if plan_token.trim().is_empty() {
            return Err(HardwareError::PlanRequired);
        }
        let result = if self.policy == ExecutionPolicy::DryRun {
            let plan = self.plan_spi_flash(request)?;
            if plan.plan_token != plan_token {
                return Err(HardwareError::StalePlan);
            }
            let mut steps = vec![format!("back up {} before changing it", plan.target.path)];
            if let Some(image) = &plan.image {
                steps.push(format!(
                    "assemble {} using the {} layout",
                    image.title, image.layout
                ));
                steps.push(format!("write the image to {}", plan.target.path));
            } else {
                steps.push(format!("erase every block on {}", plan.target.path));
            }
            steps.push("read back and verify the SPI flash contents".into());
            SpiFlashApplyResult {
                run: hardware_dry_run(
                    &format!("hardware.spi-flash.{}", request.operation),
                    if request.operation == "install" {
                        "Install SPI boot image"
                    } else {
                        "Erase SPI boot flash"
                    },
                    &steps,
                ),
                plan,
                backup_path: None,
            }
        } else if effective_uid() != Some(0) {
            let result = run_privileged_spi_flash_apply(request, plan_token)?;
            self.record_run(result.run.clone());
            return Ok(result);
        } else {
            self.spi_flash.apply_live(request, plan_token)?
        };
        self.record_run(result.run.clone());
        Ok(result)
    }

    pub fn apply_led_trigger(
        &self,
        led_id: &str,
        trigger: &str,
        confirmed: bool,
    ) -> Result<ActionRun, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        let status = self.led_status()?;
        let led = status
            .leds
            .iter()
            .find(|led| led.id == led_id)
            .ok_or_else(|| HardwareError::InvalidInput(format!("unknown LED {led_id}")))?;
        if !led.available_triggers.iter().any(|item| item == trigger) {
            return Err(HardwareError::InvalidInput(format!(
                "trigger {trigger} is not available for {led_id}"
            )));
        }
        let run = if self.policy == ExecutionPolicy::DryRun {
            hardware_dry_run(
                "hardware.led-trigger",
                "Set LED trigger",
                &[
                    format!("set {led_id} trigger to {trigger}"),
                    "persist the LED trigger for boot".into(),
                ],
            )
        } else if effective_uid() != Some(0) {
            let run = run_privileged_led_trigger(led_id, trigger)?;
            self.record_run(run.clone());
            return Ok(run);
        } else {
            self.hardware.apply_led_trigger_live(led_id, trigger)?
        };
        self.record_run(run.clone());
        Ok(run)
    }

    pub fn apply_rgb_led(
        &self,
        config: &RgbLedConfig,
        confirmed: bool,
    ) -> Result<ActionRun, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        self.hardware.validate_rgb_led_config(config)?;
        let run = if self.policy == ExecutionPolicy::DryRun {
            hardware_dry_run(
                "hardware.rgb-led",
                "Set RGB LED pattern",
                &[
                    format!("set {} to {} mode", config.group_id, config.mode),
                    format!(
                        "use color #{:02x}{:02x}{:02x}, {}% brightness, {}ms cycle",
                        config.red, config.green, config.blue, config.brightness, config.cycle_ms
                    ),
                    "persist the RGB LED pattern for boot".into(),
                ],
            )
        } else if effective_uid() != Some(0) {
            let run = run_privileged_rgb_led(config)?;
            self.record_run(run.clone());
            return Ok(run);
        } else {
            self.hardware.apply_rgb_led_live(config)?
        };
        self.record_run(run.clone());
        Ok(run)
    }

    pub fn restore_led_state(&self) -> Result<ActionRun, HardwareError> {
        if self.policy != ExecutionPolicy::Live || effective_uid() != Some(0) {
            return Err(HardwareError::RootRequired);
        }
        self.hardware.restore_led_state_live()
    }

    pub fn video_status(&self) -> Result<VideoStatus, HardwareError> {
        self.hardware.video_status()
    }

    pub fn capture_video_frame(&self, device_id: &str) -> Result<VideoFrame, HardwareError> {
        self.hardware.capture_video_frame(device_id)
    }

    pub fn thermal_status(&self) -> Result<ThermalStatus, HardwareError> {
        self.hardware.thermal_status()
    }

    pub fn apply_thermal_policy(
        &self,
        policy: &str,
        confirmed: bool,
    ) -> Result<ActionRun, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        if self.fan_curve_status()?.config.is_some() {
            return Err(HardwareError::Conflict(
                "disable the active fan curve before changing the thermal governor".into(),
            ));
        }
        let run = if self.policy == ExecutionPolicy::DryRun {
            let status = self.thermal_status()?;
            if !status.available_policies.iter().any(|item| item == policy) {
                return Err(HardwareError::InvalidInput(format!(
                    "policy {policy} is not available"
                )));
            }
            hardware_dry_run(
                "hardware.thermal-policy",
                "Set fan and thermal policy",
                &[
                    format!("set every supported thermal zone to {policy}"),
                    format!("persist {policy} for boot"),
                ],
            )
        } else if effective_uid() != Some(0) {
            let run = run_privileged_thermal_apply(policy)?;
            self.record_run(run.clone());
            return Ok(run);
        } else {
            self.hardware.apply_thermal_policy_live(policy)?
        };
        self.record_run(run.clone());
        Ok(run)
    }

    pub fn restore_thermal_policy(&self) -> Result<ActionRun, HardwareError> {
        if self.policy != ExecutionPolicy::Live || effective_uid() != Some(0) {
            return Err(HardwareError::RootRequired);
        }
        self.hardware.restore_thermal_policy_live()
    }

    pub fn fan_curve_status(&self) -> Result<FanCurveStatus, HardwareError> {
        self.fan_curve.status()
    }

    pub fn plan_fan_curve(&self, request: &FanCurveRequest) -> Result<FanCurvePlan, HardwareError> {
        self.fan_curve.plan(request)
    }

    pub fn apply_fan_curve(
        &self,
        request: &FanCurveRequest,
        plan_token: &str,
        confirmed: bool,
    ) -> Result<FanCurveApplyResult, HardwareError> {
        if !confirmed {
            return Err(HardwareError::ConfirmationRequired);
        }
        if plan_token.trim().is_empty() {
            return Err(HardwareError::PlanRequired);
        }
        let result = if self.policy == ExecutionPolicy::DryRun {
            let plan = self.plan_fan_curve(request)?;
            if plan.plan_token != plan_token {
                return Err(HardwareError::StalePlan);
            }
            let steps = if request.enabled {
                let config = request.config.as_ref().expect("validated fan curve");
                vec![
                    format!("set {} to the user_space governor", config.zone_id),
                    format!(
                        "control {} with {} curve points",
                        config.cooling_device_id,
                        config.points.len()
                    ),
                    "persist the curve and start its fail-safe control service".into(),
                ]
            } else {
                vec![
                    "stop and disable the fan curve service".into(),
                    "restore the previous thermal governor".into(),
                ]
            };
            FanCurveApplyResult {
                run: hardware_dry_run(
                    if request.enabled {
                        "hardware.fan-curve.apply"
                    } else {
                        "hardware.fan-curve.disable"
                    },
                    if request.enabled {
                        "Apply fan curve"
                    } else {
                        "Disable fan curve"
                    },
                    &steps,
                ),
                plan,
            }
        } else if effective_uid() != Some(0) {
            let result = run_privileged_fan_curve_apply(request, plan_token)?;
            self.record_run(result.run.clone());
            return Ok(result);
        } else {
            self.fan_curve.apply_live(request, plan_token)?
        };
        self.record_run(result.run.clone());
        Ok(result)
    }

    pub fn fan_curve_tick(&self) -> Result<FanCurveTick, HardwareError> {
        if self.policy != ExecutionPolicy::Live || effective_uid() != Some(0) {
            return Err(HardwareError::RootRequired);
        }
        self.fan_curve.tick()
    }

    pub fn fan_curve_shutdown_failsafe(&self) -> Result<FanCurveTick, HardwareError> {
        if self.policy != ExecutionPolicy::Live || effective_uid() != Some(0) {
            return Err(HardwareError::RootRequired);
        }
        self.fan_curve.shutdown_failsafe()
    }

    pub fn plan_source_change(&self, provider_id: &str) -> Result<SourcePlan, SourceError> {
        let mut plan = self.sources.plan(provider_id)?;
        if self.policy == ExecutionPolicy::DryRun
            && !plan.warnings.iter().any(|warning| warning == "dry_run")
        {
            plan.warnings.push("dry_run".into());
        }
        Ok(plan)
    }

    pub fn apply_source_change(
        &self,
        provider_id: &str,
        plan_token: &str,
        confirmed: bool,
    ) -> Result<SourceApplyResult, SourceError> {
        if !confirmed {
            return Err(SourceError::ConfirmationRequired);
        }
        if plan_token.trim().is_empty() {
            return Err(SourceError::PlanRequired);
        }
        let started_at = Utc::now();
        let result = if self.policy == ExecutionPolicy::DryRun {
            let plan = self.plan_source_change(provider_id)?;
            if plan.plan_token != plan_token {
                return Err(SourceError::StalePlan);
            }
            let output = Some(source_plan_output(&plan));
            SourceApplyResult {
                run: source_run(
                    ActionStatus::Planned,
                    true,
                    "Dry run completed; no APT source file was changed.".into(),
                    output,
                    started_at,
                ),
                plan,
                backups: Vec::new(),
                rolled_back: false,
            }
        } else {
            if effective_uid() != Some(0) {
                let result = run_privileged_source_apply(provider_id, plan_token)?;
                self.record_run(result.run.clone());
                return Ok(result);
            }
            let outcome = self.sources.apply_live(provider_id, plan_token)?;
            SourceApplyResult {
                run: source_run(
                    outcome.status,
                    false,
                    outcome.summary,
                    outcome.output,
                    started_at,
                ),
                plan: outcome.plan,
                backups: outcome.backups,
                rolled_back: outcome.rolled_back,
            }
        };
        self.record_run(result.run.clone());
        Ok(result)
    }

    pub fn execute(&self, action_id: &str, confirmed: bool) -> Result<ActionRun, ActionError> {
        let action = self
            .actions()
            .into_iter()
            .find(|candidate| candidate.id == action_id)
            .ok_or_else(|| ActionError::Unknown(action_id.into()))?;
        if !action.available {
            return Err(ActionError::Unavailable(action.title));
        }
        if action.id == "system.change-sources" {
            return Err(ActionError::InputRequired(action.title));
        }
        if action.risk >= RiskLevel::Guarded && !confirmed {
            return Err(ActionError::ConfirmationRequired(action.title));
        }
        if self.policy == ExecutionPolicy::Live
            && action.requires_root
            && effective_uid() != Some(0)
        {
            let run = run_privileged_action(&action.id, &action.title)?;
            self.record_run(run.clone());
            return Ok(run);
        }

        let started_at = Utc::now();
        let synthetic = self.policy == ExecutionPolicy::DryRun;
        let (status, summary, output) = if synthetic {
            (
                ActionStatus::Succeeded,
                "Dry run completed; no system state was changed.".into(),
                Some(format!("planned steps:\n- {}", action.steps.join("\n- "))),
            )
        } else if let Some(command) = &action.command {
            let (program, args) = command.split_first().expect("action command is not empty");
            match Command::new(program).args(args).output() {
                Ok(result) if result.status.success() => (
                    ActionStatus::Succeeded,
                    "Operation completed successfully.".into(),
                    bounded_output(&result.stdout, &result.stderr),
                ),
                Ok(result) => (
                    ActionStatus::Failed,
                    format!("Operation exited with {}.", result.status),
                    bounded_output(&result.stdout, &result.stderr),
                ),
                Err(error) => return Err(ActionError::Launch(error.to_string())),
            }
        } else {
            execute_builtin_action(&action.id)?
        };
        let run = ActionRun {
            id: Uuid::new_v4().to_string(),
            action_id: action.id.clone(),
            action_title: action.title.clone(),
            status,
            synthetic,
            summary: summary.clone(),
            output,
            started_at,
            finished_at: Some(Utc::now()),
        };
        self.record_run(run.clone());
        Ok(run)
    }

    fn record_run(&self, run: ActionRun) {
        {
            let mut runs = self.runs.write().expect("run lock poisoned");
            runs.push_back(run.clone());
            while runs.len() > 40 {
                runs.pop_front();
            }
        }
        let mut activity = self.activity.write().expect("activity lock poisoned");
        activity.push_back(ActivityEvent {
            id: Uuid::new_v4().to_string(),
            at: Utc::now(),
            kind: "action".into(),
            title: run.action_title,
            detail: run.summary,
            synthetic: run.synthetic,
        });
        while activity.len() > 80 {
            activity.pop_front();
        }
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::from_environment()
    }
}

fn action_catalog(synthetic: bool) -> Vec<ActionSpec> {
    let mut actions = vec![
        action(
            "system.inspect",
            "Run system inspection",
            "Refresh board identity, health signals, services, and detected hardware.",
            "Observe",
            RiskLevel::Safe,
            false,
            2,
            &[
                "Read operating-system and device-tree identity",
                "Probe storage, network, thermal, and services",
                "Recalculate alerts",
            ],
            None,
        ),
        action(
            "system.update",
            "Update operating system",
            "Refresh package indexes and upgrade installed packages.",
            "Maintain",
            RiskLevel::High,
            true,
            600,
            &[
                "Refresh package metadata",
                "Upgrade installed packages",
                "Report held packages",
            ],
            None,
        ),
        action(
            "system.change-sources",
            "Change package mirrors",
            "Select a trusted Debian, Ubuntu, and Radxa mirror with a preview, backup, and automatic rollback if package refresh fails.",
            "Maintain",
            RiskLevel::Guarded,
            true,
            45,
            &[
                "Detect managed APT source entries",
                "Preview only recognized Debian, Ubuntu, and Radxa URL changes",
                "Back up and atomically replace affected files",
                "Refresh package metadata or restore the previous files on failure",
            ],
            None,
        ),
        action(
            "service.ssh-install",
            "Install remote shell",
            "Install the OpenSSH server package without changing its current enablement state.",
            "Connect",
            RiskLevel::Guarded,
            true,
            90,
            &[
                "Install the OpenSSH server package",
                "Refresh the detected SSH service state",
            ],
            None,
        ),
        action(
            "service.ssh-enable",
            "Enable remote shell",
            "Enable and start SSH. Confirm that remote-login accounts use strong credentials first.",
            "Connect",
            RiskLevel::Guarded,
            true,
            8,
            &[
                "Check SSH service and account security",
                "Enable service at boot",
                "Start the service",
            ],
            Some(&["systemctl", "enable", "--now", "ssh.service"]),
        ),
        action(
            "service.ssh-disable",
            "Disable remote shell",
            "Stop SSH and prevent it from starting automatically.",
            "Connect",
            RiskLevel::High,
            true,
            8,
            &[
                "Stop the SSH service",
                "Disable automatic startup",
                "Verify the service state",
            ],
            Some(&["systemctl", "disable", "--now", "ssh.service"]),
        ),
        action(
            "service.ssh-regenerate-host-keys",
            "Regenerate SSH host keys",
            "Replace this device's SSH server identity and restart the service.",
            "Connect",
            RiskLevel::High,
            true,
            30,
            &[
                "Remove existing SSH host key files",
                "Generate a new host key set",
                "Refresh the detected SSH service state",
            ],
            None,
        ),
        action(
            "service.ssh-remove",
            "Remove remote shell",
            "Remove the OpenSSH server package from this device.",
            "Connect",
            RiskLevel::High,
            true,
            90,
            &[
                "Remove the OpenSSH server package",
                "Refresh the detected SSH service state",
            ],
            None,
        ),
        action(
            "network.restart",
            "Restart network manager",
            "Restart NetworkManager and re-evaluate local interfaces.",
            "Connect",
            RiskLevel::High,
            true,
            20,
            &[
                "Record active interfaces",
                "Restart NetworkManager",
                "Wait for interface recovery",
            ],
            Some(&["systemctl", "restart", "NetworkManager.service"]),
        ),
        action(
            "service.docker-install",
            "Install container runtime",
            "Install the distribution Docker package without enabling the service.",
            "Services",
            RiskLevel::Guarded,
            true,
            180,
            &[
                "Install the Docker package",
                "Refresh the detected Docker service state",
            ],
            None,
        ),
        action(
            "service.docker-enable",
            "Enable container runtime",
            "Enable and start the Docker service.",
            "Services",
            RiskLevel::Guarded,
            true,
            15,
            &[
                "Enable the Docker service at boot",
                "Start the Docker service",
                "Verify the service state",
            ],
            Some(&["systemctl", "enable", "--now", "docker.service"]),
        ),
        action(
            "service.docker-disable",
            "Disable container runtime",
            "Stop Docker and prevent it from starting automatically.",
            "Services",
            RiskLevel::High,
            true,
            20,
            &[
                "Stop running containers through the Docker service",
                "Disable automatic startup",
                "Verify the service state",
            ],
            Some(&["systemctl", "disable", "--now", "docker.service"]),
        ),
        action(
            "service.docker-remove",
            "Remove container runtime",
            "Remove the distribution Docker package while retaining container data.",
            "Services",
            RiskLevel::High,
            true,
            120,
            &[
                "Remove the Docker package",
                "Keep existing images and container data under /var/lib/docker",
            ],
            None,
        ),
        action(
            "storage.expand-root",
            "Expand root filesystem",
            "Grow the supported root filesystem to occupy available storage.",
            "Storage",
            RiskLevel::High,
            true,
            120,
            &[
                "Resolve root block device",
                "Validate ext4 or btrfs",
                "Expand filesystem",
                "Verify resulting capacity",
            ],
            None,
        ),
        action(
            "power.enable-sleep",
            "Enable sleep and hibernate",
            "Restore systemd sleep and hibernate targets.",
            "Power",
            RiskLevel::Guarded,
            true,
            5,
            &[
                "Unmask sleep targets",
                "Reload systemd",
                "Verify target state",
            ],
            None,
        ),
        action(
            "power.disable-sleep",
            "Disable sleep and hibernate",
            "Keep the SBC available by masking system sleep and hibernate targets.",
            "Power",
            RiskLevel::Guarded,
            true,
            5,
            &[
                "Mask sleep targets",
                "Reload systemd",
                "Verify target state",
            ],
            None,
        ),
        action(
            "system.reboot",
            "Reboot device",
            "Stop services and reboot the local board immediately.",
            "Power",
            RiskLevel::Critical,
            true,
            60,
            &[
                "Flush pending writes",
                "Stop services",
                "Request system reboot",
            ],
            Some(&["systemctl", "reboot"]),
        ),
    ];
    if !synthetic {
        apply_live_availability(&mut actions);
    }
    actions
}

#[expect(
    clippy::too_many_arguments,
    reason = "action catalog rows stay auditable when every safety field is explicit"
)]
fn action(
    id: &str,
    title: &str,
    description: &str,
    category: &str,
    risk: RiskLevel,
    requires_root: bool,
    estimated_seconds: u32,
    steps: &[&str],
    command: Option<&[&str]>,
) -> ActionSpec {
    ActionSpec {
        id: id.into(),
        title: title.into(),
        description: description.into(),
        category: category.into(),
        risk,
        requires_root,
        available: true,
        unavailable_reason: None,
        estimated_seconds,
        steps: steps.iter().map(|step| (*step).into()).collect(),
        command: command.map(|parts| parts.iter().map(|part| (*part).into()).collect()),
    }
}

fn apply_live_availability(actions: &mut [ActionSpec]) {
    for action in actions {
        let unavailable = match action.id.as_str() {
            "system.update" => missing_commands(&["apt-get", "apt-mark"]),
            "system.change-sources" => missing_commands(&["apt-get"]),
            "service.ssh-install" => {
                if package_installed("openssh-server") {
                    Some("OpenSSH server is already installed.".into())
                } else {
                    missing_commands(&["apt-get"])
                }
            }
            "service.ssh-enable" => service_enable_unavailable(
                "openssh-server",
                "ssh.service",
                "SSH is already enabled and running.",
            ),
            "service.ssh-disable" => service_disable_unavailable(
                "openssh-server",
                "ssh.service",
                "SSH is already disabled and stopped.",
            ),
            "service.ssh-regenerate-host-keys" => {
                package_action_unavailable("openssh-server", "ssh.service")
                    .or_else(|| missing_commands(&["dpkg-reconfigure"]))
            }
            "service.ssh-remove" => package_only_unavailable("openssh-server")
                .or_else(|| missing_commands(&["apt-get"])),
            "network.restart" => service_action_unavailable("NetworkManager.service"),
            "service.docker-install" => {
                if package_installed("docker.io") {
                    Some("Docker is already installed.".into())
                } else {
                    missing_commands(&["apt-get"])
                }
            }
            "service.docker-enable" => service_enable_unavailable(
                "docker.io",
                "docker.service",
                "Docker is already enabled and running.",
            ),
            "service.docker-disable" => service_disable_unavailable(
                "docker.io",
                "docker.service",
                "Docker is already disabled and stopped.",
            ),
            "service.docker-remove" => {
                package_only_unavailable("docker.io").or_else(|| missing_commands(&["apt-get"]))
            }
            "storage.expand-root" => missing_commands(&["findmnt", "blkid"]).or_else(|| {
                if command_exists("resize2fs") || command_exists("btrfs") {
                    None
                } else {
                    Some("Neither resize2fs nor btrfs is installed.".into())
                }
            }),
            "power.enable-sleep" => missing_commands(&["systemctl"]).or_else(|| {
                (!sleep_targets().iter().any(|unit| unit_is_masked(unit)))
                    .then(|| "Sleep and hibernate targets are already enabled.".into())
            }),
            "power.disable-sleep" => missing_commands(&["systemctl"]).or_else(|| {
                sleep_targets()
                    .iter()
                    .all(|unit| unit_is_masked(unit))
                    .then(|| "Sleep and hibernate targets are already disabled.".into())
            }),
            "system.reboot" => missing_commands(&["systemctl"]),
            _ => None,
        };
        if let Some(reason) = unavailable {
            action.available = false;
            action.unavailable_reason = Some(reason);
        }
    }
}

fn package_action_unavailable(package: &str, unit: &str) -> Option<String> {
    if let Some(reason) = package_only_unavailable(package) {
        return Some(reason);
    }
    service_action_unavailable(unit)
}

fn service_enable_unavailable(package: &str, unit: &str, current: &str) -> Option<String> {
    package_action_unavailable(package, unit)
        .or_else(|| (unit_is_enabled(unit) && service_is_active(unit)).then(|| current.into()))
}

fn service_disable_unavailable(package: &str, unit: &str, current: &str) -> Option<String> {
    package_action_unavailable(package, unit)
        .or_else(|| (!unit_is_enabled(unit) && !service_is_active(unit)).then(|| current.into()))
}

fn package_only_unavailable(package: &str) -> Option<String> {
    (!package_installed(package)).then(|| format!("Package {package} is not installed."))
}

fn service_action_unavailable(unit: &str) -> Option<String> {
    missing_commands(&["systemctl"]).or_else(|| {
        if systemd_unit_exists(unit) {
            None
        } else {
            Some(format!("Systemd unit {unit} is not installed."))
        }
    })
}

fn missing_commands(programs: &[&str]) -> Option<String> {
    let missing = programs
        .iter()
        .copied()
        .filter(|program| !command_exists(program))
        .collect::<Vec<_>>();
    (!missing.is_empty()).then(|| format!("Missing required command(s): {}.", missing.join(", ")))
}

fn command_exists(program: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

fn package_installed(package: &str) -> bool {
    Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Abbrev}", package])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).starts_with("ii ")
        })
}

fn systemd_unit_exists(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["list-unit-files", unit, "--no-legend"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().next() == Some(unit))
        })
}

fn unit_is_enabled(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-enabled", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

fn service_is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

fn unit_is_masked(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-enabled", unit])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "masked")
}

fn sleep_targets() -> [&'static str; 5] {
    [
        "sleep.target",
        "suspend.target",
        "hibernate.target",
        "hybrid-sleep.target",
        "suspend-then-hibernate.target",
    ]
}

fn effective_uid() -> Option<u32> {
    let output = Command::new("id").arg("-u").output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn run_privileged_action(action_id: &str, title: &str) -> Result<ActionRun, ActionError> {
    privileged_helper_ready().map_err(|_| ActionError::RootRequired(title.into()))?;
    let output = Command::new(PKEXEC)
        .args([PRIVILEGED_HELPER, "action", action_id, "--confirmed"])
        .output()
        .map_err(|error| ActionError::Authorization(title.into(), error.to_string()))?;
    if !output.status.success() {
        return Err(ActionError::Authorization(
            title.into(),
            helper_error(&output.stderr, output.status),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| ActionError::Launch(format!("invalid helper response: {error}")))
}

fn run_privileged_source_apply(
    provider_id: &str,
    plan_token: &str,
) -> Result<SourceApplyResult, SourceError> {
    privileged_helper_ready().map_err(|_| SourceError::RootRequired)?;
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "sources-apply",
            provider_id,
            plan_token,
            "--confirmed",
        ])
        .output()
        .map_err(|error| SourceError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(SourceError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| SourceError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_overlay_apply(
    selected_ids: &[String],
    plan_token: &str,
) -> Result<OverlayApplyResult, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let selected = selected_ids.join(",");
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "overlays-apply",
            &selected,
            plan_token,
            "--confirmed",
        ])
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_spi_flash_apply(
    request: &SpiFlashRequest,
    plan_token: &str,
) -> Result<SpiFlashApplyResult, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let image_id = request.image_id.as_deref().unwrap_or("-");
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "spi-flash-apply",
            &request.operation,
            &request.target_id,
            image_id,
            plan_token,
            "--confirmed",
        ])
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_thermal_apply(policy: &str) -> Result<ActionRun, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let output = Command::new(PKEXEC)
        .args([PRIVILEGED_HELPER, "thermal-apply", policy, "--confirmed"])
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_fan_curve_apply(
    request: &FanCurveRequest,
    plan_token: &str,
) -> Result<FanCurveApplyResult, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let request_json = serde_json::to_string(request)
        .map_err(|error| HardwareError::Io(format!("invalid fan curve request: {error}")))?;
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "fan-curve-apply",
            &request_json,
            plan_token,
            "--confirmed",
        ])
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_led_trigger(led_id: &str, trigger: &str) -> Result<ActionRun, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "led-trigger",
            led_id,
            trigger,
            "--confirmed",
        ])
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn run_privileged_rgb_led(config: &RgbLedConfig) -> Result<ActionRun, HardwareError> {
    privileged_helper_ready().map_err(|_| HardwareError::RootRequired)?;
    let arguments = [
        PRIVILEGED_HELPER.to_owned(),
        "led-rgb".into(),
        config.group_id.clone(),
        config.mode.clone(),
        config.red.to_string(),
        config.green.to_string(),
        config.blue.to_string(),
        config.brightness.to_string(),
        config.cycle_ms.to_string(),
        "--confirmed".into(),
    ];
    let output = Command::new(PKEXEC)
        .args(arguments)
        .output()
        .map_err(|error| HardwareError::Authorization(error.to_string()))?;
    if !output.status.success() {
        return Err(HardwareError::Authorization(helper_error(
            &output.stderr,
            output.status,
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| HardwareError::Io(format!("invalid helper response: {error}")))
}

fn hardware_dry_run(action_id: &str, title: &str, steps: &[String]) -> ActionRun {
    ActionRun {
        id: Uuid::new_v4().to_string(),
        action_id: action_id.into(),
        action_title: title.into(),
        status: ActionStatus::Planned,
        synthetic: true,
        summary: "Dry run completed; no hardware configuration was changed.".into(),
        output: Some(format!("planned steps:\n- {}", steps.join("\n- "))),
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
    }
}

fn privileged_helper_ready() -> Result<(), ()> {
    if cfg!(target_os = "linux")
        && Path::new(PKEXEC).is_file()
        && Path::new(PRIVILEGED_HELPER).is_file()
    {
        Ok(())
    } else {
        Err(())
    }
}

fn helper_error(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_owned();
    if detail.is_empty() {
        format!("privileged helper exited with {status}")
    } else {
        detail
    }
}

#[derive(Debug)]
struct CommandStep {
    label: &'static str,
    program: &'static str,
    args: Vec<String>,
}

impl CommandStep {
    fn new(label: &'static str, program: &'static str, args: &[&str]) -> Self {
        Self {
            label,
            program,
            args: args.iter().map(|value| (*value).into()).collect(),
        }
    }
}

type BuiltinActionResult = (ActionStatus, String, Option<String>);

fn execute_builtin_action(action_id: &str) -> Result<BuiltinActionResult, ActionError> {
    match action_id {
        "system.inspect" => Ok((
            ActionStatus::Succeeded,
            "Inspection completed.".into(),
            None,
        )),
        "system.update" => execute_command_steps(vec![
            CommandStep::new("Refresh package metadata", "apt-get", &["update"]),
            CommandStep::new(
                "Upgrade installed packages",
                "apt-get",
                &["--assume-yes", "dist-upgrade", "--allow-downgrades"],
            ),
            CommandStep::new("Report held packages", "apt-mark", &["showhold"]),
        ]),
        "service.ssh-install" => {
            execute_package_action("Install OpenSSH server", "install", "openssh-server")
        }
        "service.ssh-regenerate-host-keys" => execute_ssh_host_key_regeneration(),
        "service.ssh-remove" => {
            execute_package_action("Remove OpenSSH server", "remove", "openssh-server")
        }
        "service.docker-install" => {
            execute_package_action("Install Docker", "install", "docker.io")
        }
        "service.docker-remove" => execute_package_action("Remove Docker", "remove", "docker.io"),
        "storage.expand-root" => execute_root_expansion(),
        "power.enable-sleep" => execute_command_steps(vec![
            CommandStep::new(
                "Unmask sleep targets",
                "systemctl",
                &[
                    "unmask",
                    "sleep.target",
                    "suspend.target",
                    "hibernate.target",
                    "hybrid-sleep.target",
                    "suspend-then-hibernate.target",
                ],
            ),
            CommandStep::new("Reload systemd", "systemctl", &["daemon-reload"]),
            CommandStep::new(
                "Verify target state",
                "systemctl",
                &[
                    "show",
                    "--property=LoadState",
                    "--property=UnitFileState",
                    "sleep.target",
                    "suspend.target",
                    "hibernate.target",
                    "hybrid-sleep.target",
                    "suspend-then-hibernate.target",
                ],
            ),
        ]),
        "power.disable-sleep" => execute_command_steps(vec![
            CommandStep::new(
                "Mask sleep targets",
                "systemctl",
                &[
                    "mask",
                    "sleep.target",
                    "suspend.target",
                    "hibernate.target",
                    "hybrid-sleep.target",
                    "suspend-then-hibernate.target",
                ],
            ),
            CommandStep::new("Reload systemd", "systemctl", &["daemon-reload"]),
            CommandStep::new(
                "Verify target state",
                "systemctl",
                &[
                    "show",
                    "--property=LoadState",
                    "--property=UnitFileState",
                    "sleep.target",
                    "suspend.target",
                    "hibernate.target",
                    "hybrid-sleep.target",
                    "suspend-then-hibernate.target",
                ],
            ),
        ]),
        _ => Err(ActionError::Launch(format!(
            "no built-in executor is registered for {action_id}"
        ))),
    }
}

fn execute_package_action(
    label: &'static str,
    operation: &'static str,
    package: &'static str,
) -> Result<BuiltinActionResult, ActionError> {
    execute_command_steps(vec![CommandStep::new(
        label,
        "apt-get",
        &["--assume-yes", operation, package],
    )])
}

fn execute_ssh_host_key_regeneration() -> Result<BuiltinActionResult, ActionError> {
    let mut transcript = String::from("== Remove existing SSH host keys ==\n");
    let entries = fs::read_dir("/etc/ssh")
        .map_err(|error| ActionError::Launch(format!("unable to read /etc/ssh: {error}")))?;
    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.map_err(|error| ActionError::Launch(error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("ssh_host_") {
            fs::remove_file(entry.path()).map_err(|error| {
                ActionError::Launch(format!(
                    "unable to remove {}: {error}",
                    entry.path().display()
                ))
            })?;
            transcript.push_str(&format!("removed /etc/ssh/{name}\n"));
            removed += 1;
        }
    }
    if removed == 0 {
        transcript.push_str("no existing host key files were found\n");
    }
    let regenerate = CommandStep::new(
        "Generate SSH host keys",
        "dpkg-reconfigure",
        &["-f", "noninteractive", "openssh-server"],
    );
    let output = run_step(&regenerate, &mut transcript)?;
    if !output.status.success() {
        return Ok(failed_step(&regenerate, &output, transcript));
    }
    Ok((
        ActionStatus::Succeeded,
        "SSH host keys were regenerated successfully.".into(),
        bounded_text(&transcript),
    ))
}

fn execute_root_expansion() -> Result<BuiltinActionResult, ActionError> {
    let mut transcript = String::new();
    let find_root = CommandStep::new(
        "Resolve root block device",
        "findmnt",
        &["--nofsroot", "--noheadings", "--output", "SOURCE", "/"],
    );
    let root_output = run_step(&find_root, &mut transcript)?;
    if !root_output.status.success() {
        return Ok(failed_step(&find_root, &root_output, transcript));
    }
    let root_device = String::from_utf8_lossy(&root_output.stdout)
        .trim()
        .to_owned();
    if !root_device.starts_with("/dev/") {
        return Ok((
            ActionStatus::Failed,
            "Root filesystem is not backed by a supported block device.".into(),
            bounded_text(&transcript),
        ));
    }

    let filesystem_step = CommandStep {
        label: "Detect root filesystem",
        program: "blkid",
        args: vec![
            "-s".into(),
            "TYPE".into(),
            "-o".into(),
            "value".into(),
            root_device.clone(),
        ],
    };
    let filesystem_output = run_step(&filesystem_step, &mut transcript)?;
    if !filesystem_output.status.success() {
        return Ok(failed_step(
            &filesystem_step,
            &filesystem_output,
            transcript,
        ));
    }
    let filesystem = String::from_utf8_lossy(&filesystem_output.stdout)
        .trim()
        .to_ascii_lowercase();
    let expand_step = match filesystem.as_str() {
        "ext4" => CommandStep {
            label: "Expand ext4 filesystem",
            program: "resize2fs",
            args: vec![root_device],
        },
        "btrfs" => CommandStep::new(
            "Expand btrfs filesystem",
            "btrfs",
            &["filesystem", "resize", "max", "/"],
        ),
        _ => {
            transcript.push_str(&format!("Unsupported filesystem: {filesystem}\n"));
            return Ok((
                ActionStatus::Failed,
                format!("Unsupported root filesystem: {filesystem}."),
                bounded_text(&transcript),
            ));
        }
    };
    let expand_output = run_step(&expand_step, &mut transcript)?;
    if !expand_output.status.success() {
        return Ok(failed_step(&expand_step, &expand_output, transcript));
    }

    Ok((
        ActionStatus::Succeeded,
        "Operation completed successfully.".into(),
        bounded_text(&transcript),
    ))
}

fn execute_command_steps(steps: Vec<CommandStep>) -> Result<BuiltinActionResult, ActionError> {
    let mut transcript = String::new();
    for step in &steps {
        let output = run_step(step, &mut transcript)?;
        if !output.status.success() {
            return Ok(failed_step(step, &output, transcript));
        }
    }
    Ok((
        ActionStatus::Succeeded,
        "Operation completed successfully.".into(),
        bounded_text(&transcript),
    ))
}

fn run_step(
    step: &CommandStep,
    transcript: &mut String,
) -> Result<std::process::Output, ActionError> {
    transcript.push_str(&format!(
        "== {} ==\n$ {} {}\n",
        step.label,
        step.program,
        step.args.join(" ")
    ));
    let output = Command::new(step.program)
        .args(&step.args)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
        .map_err(|error| ActionError::Launch(format!("{}: {error}", step.program)))?;
    transcript.push_str(&String::from_utf8_lossy(&output.stdout));
    transcript.push_str(&String::from_utf8_lossy(&output.stderr));
    if !transcript.ends_with('\n') {
        transcript.push('\n');
    }
    Ok(output)
}

fn failed_step(
    step: &CommandStep,
    output: &std::process::Output,
    transcript: String,
) -> BuiltinActionResult {
    (
        ActionStatus::Failed,
        format!("{} failed with {}.", step.label, output.status),
        bounded_text(&transcript),
    )
}

fn bounded_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(8_000).collect())
    }
}

fn bounded_output(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let joined = format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(8_000).collect())
    }
}

fn source_plan_output(plan: &SourcePlan) -> String {
    if plan.changes.is_empty() {
        return "No managed APT source entry would change.".into();
    }
    plan.changes
        .iter()
        .flat_map(|change| {
            let mut lines = vec![format!(
                "{} ({} replacement(s))",
                change.path, change.replacements
            )];
            lines.extend(
                change
                    .before
                    .iter()
                    .zip(&change.after)
                    .flat_map(|(before, after)| [format!("- {before}"), format!("+ {after}")]),
            );
            lines
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FanCurveConfig, FanCurvePoint};

    #[test]
    fn guarded_action_requires_confirmation() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let result = controller.execute("service.ssh-enable", false);
        assert!(matches!(result, Err(ActionError::ConfirmationRequired(_))));
    }

    #[test]
    fn dry_run_never_executes_live_command() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let run = controller
            .execute("system.reboot", true)
            .expect("dry run should succeed");
        assert_eq!(run.status, ActionStatus::Succeeded);
        assert!(run.synthetic);
        assert_eq!(controller.runs().len(), 1);
    }

    #[test]
    fn source_action_requires_guided_input() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let result = controller.execute("system.change-sources", true);
        assert!(matches!(result, Err(ActionError::InputRequired(_))));
    }

    #[test]
    fn source_apply_is_a_plan_in_dry_run_mode() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let plan = controller.plan_source_change("ustc").unwrap();
        let result = controller
            .apply_source_change("ustc", &plan.plan_token, true)
            .unwrap();
        assert_eq!(result.run.status, ActionStatus::Planned);
        assert!(result.run.synthetic);
        assert!(!result.plan.changes.is_empty());
    }

    #[test]
    fn source_apply_requires_explicit_confirmation() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let plan = controller.plan_source_change("cqu").unwrap();
        let result = controller.apply_source_change("cqu", &plan.plan_token, false);
        assert!(matches!(result, Err(SourceError::ConfirmationRequired)));
    }

    #[test]
    fn source_apply_rejects_missing_or_stale_plan() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        assert!(matches!(
            controller.apply_source_change("cqu", "", true),
            Err(SourceError::PlanRequired)
        ));
        assert!(matches!(
            controller.apply_source_change("cqu", "plan-v1-stale", true),
            Err(SourceError::StalePlan)
        ));
    }

    #[test]
    fn spi_flash_apply_requires_bound_plan_and_confirmation() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let request = SpiFlashRequest {
            operation: "install".into(),
            target_id: "mtd0".into(),
            image_id: Some("rock-5b-rk3588:rockchip-rk35".into()),
        };
        let plan = controller.plan_spi_flash(&request).unwrap();
        assert!(matches!(
            controller.apply_spi_flash(&request, &plan.plan_token, false),
            Err(HardwareError::ConfirmationRequired)
        ));
        assert!(matches!(
            controller.apply_spi_flash(&request, "stale", true),
            Err(HardwareError::StalePlan)
        ));
        let result = controller
            .apply_spi_flash(&request, &plan.plan_token, true)
            .unwrap();
        assert_eq!(result.run.status, ActionStatus::Planned);
        assert!(result.run.synthetic);
        assert!(result.backup_path.is_none());
    }

    #[test]
    fn fan_curve_apply_requires_exact_plan_and_confirmation() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let request = FanCurveRequest {
            enabled: true,
            config: Some(FanCurveConfig {
                zone_id: "thermal_zone0".into(),
                cooling_device_id: "cooling_device0".into(),
                poll_interval_ms: 2_000,
                hysteresis_c: 2.0,
                points: vec![
                    FanCurvePoint {
                        temperature_c: 40.0,
                        speed_percent: 20,
                    },
                    FanCurvePoint {
                        temperature_c: 70.0,
                        speed_percent: 75,
                    },
                    FanCurvePoint {
                        temperature_c: 82.0,
                        speed_percent: 100,
                    },
                ],
            }),
        };
        let plan = controller.plan_fan_curve(&request).unwrap();
        assert!(matches!(
            controller.apply_fan_curve(&request, &plan.plan_token, false),
            Err(HardwareError::ConfirmationRequired)
        ));

        let mut changed = request.clone();
        changed.config.as_mut().unwrap().points[1].speed_percent = 76;
        assert!(matches!(
            controller.apply_fan_curve(&changed, &plan.plan_token, true),
            Err(HardwareError::StalePlan)
        ));

        let result = controller
            .apply_fan_curve(&request, &plan.plan_token, true)
            .unwrap();
        assert_eq!(result.run.status, ActionStatus::Planned);
        assert!(result.run.synthetic);
        assert_eq!(result.plan.resolved_points.len(), 3);
    }

    #[test]
    fn catalog_has_no_legacy_rsetup_executor() {
        for action in action_catalog(true) {
            assert!(!action.description.to_ascii_lowercase().contains("legacy"));
            assert_ne!(
                action
                    .command
                    .as_ref()
                    .and_then(|command| command.first())
                    .map(String::as_str),
                Some("rsetup")
            );
        }
    }

    #[test]
    fn demo_catalog_contains_migrated_service_lifecycle_actions() {
        let actions = action_catalog(true);
        let identifiers = actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        for expected in [
            "service.ssh-install",
            "service.ssh-enable",
            "service.ssh-disable",
            "service.ssh-regenerate-host-keys",
            "service.ssh-remove",
            "service.docker-install",
            "service.docker-enable",
            "service.docker-disable",
            "service.docker-remove",
            "power.enable-sleep",
            "power.disable-sleep",
        ] {
            assert!(
                identifiers.contains(expected),
                "missing migrated action {expected}"
            );
        }
        assert!(actions.iter().all(|action| action.available));
    }
}
