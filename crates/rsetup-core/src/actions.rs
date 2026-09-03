use crate::{
    ActionRun, ActionSpec, ActionStatus, ActivityEvent, ProbeMode, RiskLevel, SourceApplyResult,
    SourceError, SourcePlan, SourceStatus, collect_snapshot,
    sources::{SourceManager, source_run},
};
use chrono::Utc;
use std::{
    collections::VecDeque,
    env,
    process::Command,
    sync::{Arc, RwLock},
};
use thiserror::Error;
use uuid::Uuid;

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
    #[error("{0} requires guided input")]
    InputRequired(String),
    #[error("unable to start action: {0}")]
    Launch(String),
}

#[derive(Clone)]
pub struct Controller {
    mode: ProbeMode,
    policy: ExecutionPolicy,
    actions: Arc<Vec<ActionSpec>>,
    runs: Arc<RwLock<VecDeque<ActionRun>>>,
    activity: Arc<RwLock<VecDeque<ActivityEvent>>>,
    sources: Arc<SourceManager>,
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
            actions: Arc::new(action_catalog()),
            runs: Arc::new(RwLock::new(VecDeque::new())),
            activity: Arc::new(RwLock::new(activity)),
            sources: Arc::new(SourceManager::new(synthetic)),
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
        self.actions.as_ref().clone()
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
                return Err(SourceError::RootRequired);
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
            .actions
            .iter()
            .find(|candidate| candidate.id == action_id)
            .cloned()
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
            return Err(ActionError::RootRequired(action.title));
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

fn action_catalog() -> Vec<ActionSpec> {
    vec![
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
            "service.ssh-enable",
            "Enable remote shell",
            "Enable and start the system SSH service.",
            "Connect",
            RiskLevel::Guarded,
            true,
            8,
            &[
                "Check SSH service",
                "Enable service at boot",
                "Start the service",
            ],
            Some(&["systemctl", "enable", "--now", "ssh.service"]),
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
    ]
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

fn effective_uid() -> Option<u32> {
    let output = Command::new("id").arg("-u").output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
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
        "storage.expand-root" => execute_root_expansion(),
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
    fn catalog_has_no_legacy_rsetup_executor() {
        for action in action_catalog() {
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
}
