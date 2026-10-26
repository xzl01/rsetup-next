use crate::i18n::Locale;
use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use rsetup_core::{
    ActionRun, ActionSpec, BlockDeviceBackend, Controller, DeviceSnapshot, ExecutionPolicy,
    RiskLevel, SourcePlan, SourceStatus, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPartition,
    SystemInstallPlan, SystemInstallRequest, SystemInstallTarget, SystemInstallVerificationState,
    SystemMount,
};
use std::{io, time::Duration};

#[cfg(test)]
pub(in crate::tui) mod fixtures;
mod storage;
mod textfit;
mod theme;

use crate::tui::storage::mmc::mmc_device_lines;
use crate::tui::storage::nvme::nvme_device_lines;
use crate::tui::storage::{render_storage_summary, storage_error_lines};
use crate::tui::textfit::wrapped_rows_capped;
use crate::tui::theme::{AMBER, BONE, CORAL, INK, MUTED, SIGNAL, instrument};

pub fn run(controller: Controller, locale: Locale) -> Result<()> {
    // Ratatui installs a panic hook; the guard also covers initialization errors.
    struct RestoreTerminal;
    impl Drop for RestoreTerminal {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = RestoreTerminal;
    let mut terminal = ratatui::try_init()?;
    terminal.clear()?;
    run_loop(&mut terminal, controller, locale)
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    controller: Controller,
    locale: Locale,
) -> Result<()> {
    let mut state = App::new(controller, locale)?;
    loop {
        state.poll_benchmark();
        terminal.draw(|frame| render(frame, &mut state))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if state.install.open {
                match key.code {
                    KeyCode::Esc => state.install.close(),
                    other => state.install_key(other),
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('i') if !state.source_picker => state.install.open_panel(),
                KeyCode::Esc if state.source_picker => state.close_source_picker(),
                KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down => state.next(),
                KeyCode::Char('k') | KeyCode::Up => state.previous(),
                KeyCode::Char('r') => state.refresh()?,
                KeyCode::Char('b') if state.source_picker => state.start_benchmark(),
                KeyCode::Enter => state.request_run(),
                KeyCode::Char('y') if state.confirm_pending => state.execute_selected()?,
                KeyCode::Char('n') if state.confirm_pending => state.confirm_pending = false,
                _ => {}
            }
        }
    }
    Ok(())
}

struct App {
    controller: Controller,
    locale: Locale,
    snapshot: DeviceSnapshot,
    actions: Vec<ActionSpec>,
    selected: usize,
    confirm_pending: bool,
    last_run: Option<ActionRun>,
    source_status: SourceStatus,
    source_plan: Option<SourcePlan>,
    source_picker: bool,
    source_selected: usize,
    notice: Option<String>,
    pub(crate) nvme_status: rsetup_core::NvmeStatus,
    pub(crate) mmc_status: rsetup_core::MmcStatus,
    pub nvme_error: Option<rsetup_core::HardwareError>,
    pub mmc_error: Option<rsetup_core::HardwareError>,
    benchmark_rx: Option<
        std::sync::mpsc::Receiver<Result<rsetup_core::MirrorBenchmark, rsetup_core::SourceError>>,
    >,
    benchmarks: std::collections::BTreeMap<String, rsetup_core::MirrorBenchmark>,
    install: InstallPanel,
}

impl App {
    fn new(controller: Controller, locale: Locale) -> Result<Self> {
        let snapshot = controller.snapshot()?;
        let actions = controller.actions();
        let source_status = controller
            .source_status()
            .map_err(|error| anyhow!(locale.source_error(&error)))?;
        let source_selected = source_status
            .providers
            .iter()
            .position(|provider| {
                Some(provider.id.as_str()) == source_status.current_system_provider.as_deref()
            })
            .unwrap_or(0);
        let live = controller.policy() == ExecutionPolicy::Live;
        let synthetic = snapshot.synthetic;
        let live_write = live && !synthetic;
        let install = InstallPanel::from_manager(
            system_install_manager(synthetic, live_write),
            live_write,
            crate::install_needs_privilege(live, synthetic, crate::effective_uid()),
        );
        let mut app = Self {
            controller,
            locale,
            snapshot,
            actions,
            selected: 0,
            confirm_pending: false,
            last_run: None,
            source_status,
            source_plan: None,
            source_picker: false,
            source_selected,
            notice: None,
            nvme_status: rsetup_core::NvmeStatus {
                initialized: false,
                devices: vec![],
                message: None,
            },
            mmc_status: rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: None,
            },
            nvme_error: None,
            mmc_error: None,
            benchmark_rx: None,
            benchmarks: Default::default(),
            install,
        };
        app.refresh_storage();
        Ok(app)
    }

    fn next(&mut self) {
        if self.source_picker {
            self.source_selected = (self.source_selected + 1)
                .min(self.source_status.providers.len().saturating_sub(1));
            self.confirm_pending = false;
            self.update_source_plan();
            return;
        }
        self.selected = (self.selected + 1).min(self.actions.len().saturating_sub(1));
        self.confirm_pending = false;
    }

    fn start_benchmark(&mut self) {
        if self.benchmark_rx.is_some() {
            return;
        }
        let Some(provider) = self.source_status.providers.get(self.source_selected) else {
            return;
        };
        let id = provider.id.clone();
        let controller = self.controller.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.benchmark_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(controller.benchmark_source(&id));
        });
    }

    fn poll_benchmark(&mut self) {
        let Some(rx) = &self.benchmark_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.benchmark_rx = None;
                match result {
                    Ok(result) if result.source_revision == self.source_status.source_revision => {
                        self.benchmarks.insert(result.provider_id.clone(), result);
                    }
                    Ok(_) => {}
                    Err(error) => self.notice = Some(self.locale.source_error(&error)),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.benchmark_rx = None;
            }
        }
    }

    fn previous(&mut self) {
        if self.source_picker {
            self.source_selected = self.source_selected.saturating_sub(1);
            self.confirm_pending = false;
            self.update_source_plan();
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.confirm_pending = false;
    }

    /// Keys while the install panel owns the screen. The destructive key is
    /// routed to the core: this panel never decides by itself whether the typed
    /// name is acceptable.
    fn install_key(&mut self, code: KeyCode) {
        let listing = self.install.focus == InstallFocus::Targets;
        match code {
            KeyCode::Tab => self.install.focus_next(),
            // Only the device list owns these keys; while a text field has the
            // focus they are ordinary characters (paths contain j, k and r).
            KeyCode::Char('j') | KeyCode::Down if listing => self.install.next_target(),
            KeyCode::Char('k') | KeyCode::Up if listing => self.install.previous_target(),
            KeyCode::Char('r') if listing => self.install.refresh_targets(),
            KeyCode::Backspace => self.install.backspace(),
            KeyCode::Enter => self.install.confirm(&self.locale),
            KeyCode::Char(character) => self.install.push(character),
            _ => {}
        }
    }

    fn refresh_storage(&mut self) {
        match self.controller.nvme_status() {
            Ok(status) => {
                self.nvme_status = status;
                self.nvme_error = None;
            }
            Err(error) => {
                self.nvme_status = rsetup_core::NvmeStatus {
                    initialized: false,
                    devices: vec![],
                    message: None,
                };
                self.nvme_error = Some(error);
            }
        }
        match self.controller.mmc_status() {
            Ok(status) => {
                self.mmc_status = status;
                self.mmc_error = None;
            }
            Err(error) => {
                self.mmc_status = rsetup_core::MmcStatus {
                    initialized: false,
                    devices: vec![],
                    message: None,
                };
                self.mmc_error = Some(error);
            }
        }
    }

    fn refresh(&mut self) -> Result<()> {
        self.snapshot = self.controller.snapshot()?;
        self.actions = self.controller.actions();
        self.selected = self.selected.min(self.actions.len().saturating_sub(1));
        self.source_status = self
            .controller
            .source_status()
            .map_err(|error| anyhow!(self.locale.source_error(&error)))?;
        self.refresh_storage();
        if self.source_picker {
            self.update_source_plan();
        }
        Ok(())
    }

    fn request_run(&mut self) {
        let Some(action) = self.actions.get(self.selected) else {
            return;
        };
        if !action.available {
            self.notice = Some(action.unavailable_reason.as_deref().map_or_else(
                || self.locale.text("not_available").into(),
                |reason| self.locale.action_unavailable_reason(reason),
            ));
            self.confirm_pending = false;
            return;
        }
        if action.id == "system.change-sources" {
            self.source_picker = true;
            self.notice = None;
            self.update_source_plan();
            return;
        }
        if action.risk == RiskLevel::Safe {
            let _ = self.execute_selected();
        } else {
            self.confirm_pending = true;
        }
    }

    fn execute_selected(&mut self) -> Result<()> {
        if self.source_picker {
            let Some(provider) = self.source_status.providers.get(self.source_selected) else {
                return Ok(());
            };
            let provider_id = provider.id.clone();
            let Some(plan) = self.source_plan.as_ref() else {
                self.update_source_plan();
                return Ok(());
            };
            let plan_token = plan.plan_token.clone();
            let result = self
                .controller
                .apply_source_change(&provider_id, &plan_token, true)
                .map_err(|error| anyhow!(self.locale.source_error(&error)))?;
            self.last_run = Some(result.run);
            self.notice = Some(if result.rolled_back {
                self.locale.text("source_rolled_back").into()
            } else if result.backups.is_empty() {
                self.locale.text("source_plan_ready").into()
            } else {
                format!(
                    "{}: {}",
                    self.locale.text("backup_files"),
                    result.backups.len()
                )
            });
            self.confirm_pending = false;
            self.source_picker = false;
            self.refresh()?;
            return Ok(());
        }
        if let Some(action) = self.actions.get(self.selected) {
            self.last_run = Some(
                self.controller
                    .execute(&action.id, true)
                    .map_err(|error| anyhow!(self.locale.action_error(&error)))?,
            );
            self.confirm_pending = false;
            self.refresh()?;
        }
        Ok(())
    }

    fn update_source_plan(&mut self) {
        let Some(provider) = self.source_status.providers.get(self.source_selected) else {
            self.source_plan = None;
            return;
        };
        match self.controller.plan_source_change(&provider.id) {
            Ok(plan) => {
                self.source_plan = Some(plan);
                self.notice = None;
            }
            Err(error) => {
                self.source_plan = None;
                self.notice = Some(self.locale.source_error(&error));
            }
        }
    }

    fn close_source_picker(&mut self) {
        self.source_picker = false;
        self.confirm_pending = false;
        self.source_plan = None;
    }
}

// ---------------------------------------------------------------------------
// System image install panel
// ---------------------------------------------------------------------------

/// Which part of the panel owns typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallFocus {
    Targets,
    Image,
    Confirm,
}

/// What the panel reports under the field: a core error, or a state that has no
/// core error of its own.
#[derive(Debug)]
enum InstallStatus {
    Error(SystemInstallError),
    Info(&'static str),
    Written,
    /// The target is real but the write was withheld: `--live-execution` was not
    /// given, so the bytes were only counted.
    Planned,
    /// The administrator dismissed the authorization prompt. Kept apart from
    /// `Error` because nothing failed: the write was declined.
    Canceled,
    /// A refusal the privileged helper classified and reported over its error
    /// envelope, carrying the helper's own stable code and explanation.
    Helper {
        code: String,
        message: String,
    },
}

/// The install panel. It owns a core manager, so every gate, every image check
/// and the typed confirmation itself are the core's decisions; the panel only
/// presents them and routes keystrokes.
struct InstallPanel {
    manager: SystemInstallManager,
    open: bool,
    targets: Vec<SystemInstallTarget>,
    selected: usize,
    image_path: String,
    focus: InstallFocus,
    plan: Option<SystemInstallPlan>,
    typed: String,
    status: Option<InstallStatus>,
    result: Option<SystemInstallApplyResult>,
    /// Whether Enter on a matching name really writes (policy live and a real
    /// device backend). The hint is only a label; the core still decides.
    live_write: bool,
    /// Whether that write has to go through the fixed-verb helper instead of
    /// this process. Fixed for the session: neither the policy, the provider nor
    /// the uid can change while the panel is open.
    privileged: bool,
}

impl InstallPanel {
    fn from_manager(manager: SystemInstallManager, live_write: bool, privileged: bool) -> Self {
        let mut panel = Self {
            manager,
            live_write,
            privileged,
            open: false,
            targets: Vec::new(),
            selected: 0,
            image_path: String::new(),
            focus: InstallFocus::Targets,
            plan: None,
            typed: String::new(),
            status: None,
            result: None,
        };
        panel.refresh_targets();
        panel
    }

    fn open_panel(&mut self) {
        self.open = true;
        self.refresh_targets();
    }

    fn close(&mut self) {
        self.open = false;
        // Leaving the panel drops the plan and the typed name, so a stale token
        // can never be applied from a screen the user has left.
        self.plan = None;
        self.typed.clear();
        self.result = None;
        self.status = None;
    }

    fn refresh_targets(&mut self) {
        self.plan = None;
        self.result = None;
        self.status = None;
        self.selected = 0;
        match self.manager.targets() {
            Ok(targets) => self.targets = targets,
            Err(error) => {
                self.targets.clear();
                self.status = Some(InstallStatus::Error(error));
            }
        }
    }

    fn selected_target(&self) -> Option<&SystemInstallTarget> {
        self.targets.get(self.selected)
    }

    fn next_target(&mut self) {
        if self.focus != InstallFocus::Targets {
            return;
        }
        self.selected = (self.selected + 1).min(self.targets.len().saturating_sub(1));
        self.load_plan();
    }

    fn previous_target(&mut self) {
        if self.focus != InstallFocus::Targets {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.load_plan();
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            InstallFocus::Targets => InstallFocus::Image,
            InstallFocus::Image => InstallFocus::Confirm,
            InstallFocus::Confirm => InstallFocus::Targets,
        };
    }

    fn push(&mut self, character: char) {
        match self.focus {
            InstallFocus::Targets => {}
            InstallFocus::Image => self.image_path.push(character),
            // The typed name is passed to the core verbatim, trimmed the same
            // way the CLI trims it.
            InstallFocus::Confirm => self.typed.push(character),
        }
    }

    fn backspace(&mut self) {
        match self.focus {
            InstallFocus::Targets => {}
            InstallFocus::Image => {
                self.image_path.pop();
            }
            InstallFocus::Confirm => {
                self.typed.pop();
            }
        }
    }

    /// Enter: load the plan from the selected target and image, or ask the core
    /// to write.
    fn confirm(&mut self, locale: &Locale) {
        match self.focus {
            InstallFocus::Targets | InstallFocus::Image => self.load_plan(),
            InstallFocus::Confirm => self.attempt(locale),
        }
    }

    fn load_plan(&mut self) {
        self.result = None;
        let Some(target) = self.selected_target() else {
            self.plan = None;
            self.status = Some(InstallStatus::Info("install_no_targets"));
            return;
        };
        let request = SystemInstallRequest {
            target_id: target.id.clone(),
            image_path: Some(self.image_path.trim().to_owned()),
            image_id: None,
        };
        match self.manager.plan(&request) {
            Ok(plan) => {
                self.plan = Some(plan);
                self.typed.clear();
                self.status = None;
            }
            Err(error) => {
                // A refused plan (running system, mounted target, bad image) is
                // reported exactly as the core phrased it.
                self.plan = None;
                self.status = Some(InstallStatus::Error(error));
            }
        }
    }

    /// The destructive action. The typed name goes to the core untouched: an
    /// empty value is `confirmation_required`, a wrong one is
    /// `acknowledgement_mismatch`, and only the exact target name writes.
    fn attempt(&mut self, _locale: &Locale) {
        let Some(plan) = self.plan.as_ref() else {
            self.status = Some(InstallStatus::Info("install_not_planned"));
            return;
        };
        let request = plan.request.clone();
        let plan_token = plan.plan_token.clone();
        // Same routing as the CLI: an ordinary user's live write is handed to the
        // helper, which asks the administrator, rather than being attempted here
        // and failing on the device node. The typed name still travels with it
        // and is judged by the core on the other side.
        let applied = if self.privileged {
            crate::run_privileged_system_install(&request, &plan_token, &self.typed)
        } else {
            self.manager
                .apply_live(&request, &plan_token, &self.typed)
                .map_err(crate::InstallApplyError::Core)
        };
        match applied {
            Ok(result) => {
                // A dry run reports a real target but writes nothing, so the panel
                // must not announce a write the result itself denies.
                self.status = Some(if result.dry_run {
                    InstallStatus::Planned
                } else {
                    InstallStatus::Written
                });
                self.result = Some(result);
            }
            Err(error) => {
                self.result = None;
                self.status = Some(match error {
                    crate::InstallApplyError::Core(error) => InstallStatus::Error(error),
                    crate::InstallApplyError::AuthorizationCanceled => InstallStatus::Canceled,
                    // The helper classified it; keep its code and wording rather
                    // than collapsing them into a generic failure.
                    crate::InstallApplyError::Helper { code, message } => {
                        InstallStatus::Helper { code, message }
                    }
                });
            }
        }
    }

    /// Affordance only: it mirrors the core's rule using the plan's own expected
    /// device name so the panel can show the destructive key as disabled. The
    /// authoritative judgement is still the core's, which is what `attempt`
    /// calls.
    fn acknowledgement_ready(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            let typed = self.typed.trim();
            !typed.is_empty() && typed == plan.target.id
        })
    }
}

/// A write that only counts bytes: the panel can show the real device identity
/// and still let the core judge the confirmation while nothing reaches a device.
/// The CLI keeps an equivalent private wrapper in `main.rs`; hoisting one shared
/// implementation is a follow-up because `main.rs` is out of scope here.
struct DryRunWriter {
    inner: std::sync::Arc<dyn BlockDeviceBackend>,
}

impl DryRunWriter {
    fn new(inner: std::sync::Arc<dyn BlockDeviceBackend>) -> Self {
        Self { inner }
    }
}

impl BlockDeviceBackend for DryRunWriter {
    fn targets(&self) -> std::result::Result<Vec<SystemInstallTarget>, SystemInstallError> {
        self.inner.targets()
    }

    fn mounts(&self) -> std::result::Result<Vec<SystemMount>, SystemInstallError> {
        self.inner.mounts()
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        _chunk: &[u8],
    ) -> std::result::Result<(), SystemInstallError> {
        Ok(())
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> std::result::Result<Vec<SystemInstallPartition>, SystemInstallError> {
        Err(SystemInstallError::Unsupported(
            "dry run: the partition table was not re-read".into(),
        ))
    }
}

/// Live inspection on Linux, the labelled demo provider elsewhere; a real write
/// additionally needs the controller's live policy, exactly like the CLI needs
/// `--live-execution`.
fn system_install_manager(synthetic: bool, live: bool) -> SystemInstallManager {
    let backend: std::sync::Arc<dyn BlockDeviceBackend> = if synthetic {
        std::sync::Arc::new(SyntheticBackend::default())
    } else {
        std::sync::Arc::new(SystemBlockBackend::new(None))
    };
    let backend: std::sync::Arc<dyn BlockDeviceBackend> = if live {
        backend
    } else {
        std::sync::Arc::new(DryRunWriter::new(backend))
    };
    SystemInstallManager::with_backend(backend, std::sync::Arc::new(SystemImageOpener), synthetic)
}

fn install_device_kind_label(locale: Locale, kind: rsetup_core::BlockDeviceKind) -> &'static str {
    locale.install_device_kind(kind)
}

fn install_target_notes(target: &SystemInstallTarget, locale: Locale) -> String {
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
    if notes.is_empty() {
        String::new()
    } else {
        format!(" · {}", notes.join(" · "))
    }
}

/// The consequence list, rendered from a core plan.
fn install_plan_lines<'a>(plan: &SystemInstallPlan, locale: Locale) -> Vec<Line<'a>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", locale.text("install_target")),
                Style::default().fg(MUTED),
            ),
            Span::styled(plan.target.path.clone(), Style::default().fg(BONE)),
            Span::styled(
                format!(
                    " — {} ({})",
                    install_device_kind_label(locale, plan.target.kind),
                    plan.target.model
                ),
                Style::default().fg(MUTED),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{} ", locale.text("install_capacity")),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                crate::format_bytes(plan.target.size_bytes),
                Style::default().fg(BONE),
            ),
            Span::styled(
                install_target_notes(&plan.target, locale),
                Style::default().fg(AMBER),
            ),
        ]),
    ];
    if plan.destroys.is_empty() {
        lines.push(Line::from(Span::styled(
            locale.text("install_no_partitions"),
            Style::default().fg(MUTED),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "{} ({})",
                locale.text("install_destroyed_partitions"),
                plan.destroys.len()
            ),
            Style::default().fg(CORAL),
        )));
        for partition in &plan.destroys {
            let mount = partition
                .mount_point
                .as_deref()
                .map(|point| format!(" · {} {point}", locale.text("install_partition_mounted_at")))
                .unwrap_or_default();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  - {} ", partition.name),
                    Style::default().fg(BONE),
                ),
                Span::styled(
                    format!("{}{mount}", crate::format_bytes(partition.size_bytes)),
                    Style::default().fg(MUTED),
                ),
            ]));
        }
    }
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", locale.text("install_image")),
            Style::default().fg(MUTED),
        ),
        Span::styled(plan.image.path.clone(), Style::default().fg(BONE)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {:?} · {}: {} · {}: {}",
            locale.text("install_image_format"),
            plan.image.format,
            locale.text("install_image_size"),
            crate::format_bytes(plan.image.size_bytes),
            locale.text("install_image_uncompressed"),
            crate::format_bytes(plan.image.uncompressed_bytes)
        ),
        Style::default().fg(MUTED),
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {}",
            locale.text("install_image_sha256"),
            plan.image.sha256_prefix
        ),
        Style::default().fg(MUTED),
    )));
    let table = plan.image.partition_table.as_deref().unwrap_or("");
    lines.push(Line::from(Span::styled(
        format!(
            "  {}: {}",
            locale.text("install_partition_table"),
            if table.is_empty() {
                locale.text("install_partition_table_unknown").to_string()
            } else {
                table.to_uppercase()
            }
        ),
        Style::default().fg(MUTED),
    )));
    if plan.requires_root {
        lines.push(Line::from(Span::styled(
            locale.text("install_requires_root"),
            Style::default().fg(AMBER),
        )));
    }
    for warning in &plan.warnings {
        lines.push(Line::from(Span::styled(
            format!("! {}", locale.install_warning(warning)),
            Style::default().fg(CORAL),
        )));
    }
    lines
}

/// The wording for a finished write's verification line.
///
/// The core reports `Unsupported` for two different situations: a host that cannot
/// re-read a partition table at all, and a compressed image whose layout was never
/// parsed so the table it did re-read had nothing to be compared against. The
/// second one carries the table it read (`observed`), which is what tells them
/// apart — and saying "this host cannot re-read partition tables" there would be
/// simply wrong. The core's own `detail` is preferred whenever it gives one.
fn verification_text(result: &SystemInstallApplyResult, locale: Locale) -> String {
    let with_reason = |key: &str| match &result.verification.detail {
        Some(detail) => format!("{} ({detail})", locale.text(key)),
        None => locale.text(key).to_string(),
    };
    match result.verification.state {
        SystemInstallVerificationState::Matched => {
            locale.text("install_verification_matched").to_string()
        }
        SystemInstallVerificationState::Mismatched => {
            locale.text("install_verification_mismatched").to_string()
        }
        SystemInstallVerificationState::Unsupported => {
            if result.verification.observed.is_some() {
                with_reason("install_verification_compressed_not_compared")
            } else {
                with_reason("install_verification_unsupported")
            }
        }
    }
}

fn render_install_panel(frame: &mut Frame, app: &App, area: Rect) {
    let locale = app.locale;
    let panel = &app.install;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Step 1: the devices an image could be written to.
    lines.push(Line::from(Span::styled(
        locale.text("install_step_targets"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    if panel.targets.is_empty() {
        lines.push(Line::from(Span::styled(
            locale.text("install_no_targets"),
            Style::default().fg(AMBER),
        )));
    }
    for (index, target) in panel.targets.iter().enumerate() {
        let selected = index == panel.selected;
        let marker = if selected { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(if selected { SIGNAL } else { MUTED }),
            ),
            Span::styled(
                format!("{} ", target.path),
                Style::default()
                    .fg(if selected { BONE } else { MUTED })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!(
                    "{} {} · {}",
                    install_device_kind_label(locale, target.kind),
                    target.model,
                    crate::format_bytes(target.size_bytes)
                ),
                Style::default().fg(MUTED),
            ),
            Span::styled(
                install_target_notes(target, locale),
                Style::default().fg(AMBER),
            ),
        ]));
    }

    // Step 2: the image, and the hint to preview it.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("install_step_image"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled(
            format!("{}: ", locale.text("install_field_image")),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            if panel.image_path.is_empty() {
                locale.text("install_plan_hint").to_string()
            } else {
                panel.image_path.clone()
            },
            Style::default().fg(if panel.image_path.is_empty() {
                MUTED
            } else {
                BONE
            }),
        ),
        Span::styled(
            if matches!(panel.focus, InstallFocus::Image) {
                " ▌"
            } else {
                ""
            },
            Style::default().fg(SIGNAL),
        ),
    ]));

    // Step 3: every irreversible consequence of the loaded plan.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        locale.text("install_step_confirm"),
        Style::default().fg(SIGNAL).add_modifier(Modifier::BOLD),
    )));
    if let Some(plan) = &panel.plan {
        lines.extend(install_plan_lines(plan, locale));
    } else {
        lines.push(Line::from(Span::styled(
            locale.text("install_plan_hint"),
            Style::default().fg(MUTED),
        )));
    }

    // The typed confirmation, and whether the destructive key is enabled.
    lines.push(Line::from(""));
    let ready = panel.acknowledgement_ready();
    lines.push(Line::from(vec![
        Span::styled(
            format!("{}: ", locale.text("install_field_confirm")),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            if panel.typed.is_empty() {
                "_____".to_string()
            } else {
                panel.typed.clone()
            },
            Style::default().fg(if ready { SIGNAL } else { BONE }),
        ),
        Span::styled(
            if matches!(panel.focus, InstallFocus::Confirm) {
                " ▌"
            } else {
                ""
            },
            Style::default().fg(SIGNAL),
        ),
    ]));
    if panel.plan.is_some() {
        let hint = if ready {
            if !panel.live_write {
                locale.text("install_dry_run_ready")
            } else {
                locale.text("install_ready")
            }
        } else {
            locale.text("install_disabled")
        };
        lines.push(Line::from(Span::styled(
            hint,
            Style::default().fg(if ready { SIGNAL } else { CORAL }),
        )));
    }

    // Status: the core's own error, or the outcome of a write.
    if let Some(status) = &panel.status {
        match status {
            InstallStatus::Error(error) => {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{}: {}",
                        crate::system_install_error_code(error),
                        locale.install_error(error)
                    ),
                    Style::default().fg(CORAL),
                )));
            }
            InstallStatus::Helper { code, message } => {
                lines.push(Line::from(Span::styled(
                    format!("{code}: {message}"),
                    Style::default().fg(CORAL),
                )));
            }
            InstallStatus::Canceled => {
                // Amber like the informational lines, not coral: the write was
                // declined, so there is no error code to name.
                lines.push(Line::from(Span::styled(
                    if locale.is_zh() {
                        "已取消管理员授权，未写入任何数据。"
                    } else {
                        "Administrator authorization canceled. Nothing was written."
                    },
                    Style::default().fg(AMBER),
                )));
            }
            InstallStatus::Info(key) => {
                lines.push(Line::from(Span::styled(
                    locale.text(key),
                    Style::default().fg(AMBER),
                )));
            }
            InstallStatus::Planned => {
                if let Some(result) = &panel.result {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}: {} — {}",
                            locale.text("install_planned"),
                            locale.text("install_target"),
                            crate::format_bytes(result.bytes_written),
                        ),
                        Style::default().fg(AMBER),
                    )));
                }
            }
            InstallStatus::Written => {
                if let Some(result) = &panel.result {
                    let verification = verification_text(result, locale);
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{}: {} — {}: {}",
                            locale.text("install_written"),
                            locale.text("install_target"),
                            crate::format_bytes(result.bytes_written),
                            verification
                        ),
                        Style::default().fg(SIGNAL),
                    )));
                    if result.run.synthetic {
                        lines.push(Line::from(Span::styled(
                            locale.text("install_synthetic_notice"),
                            Style::default().fg(AMBER),
                        )));
                    }
                }
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(instrument(locale.text("install_panel_title")))
            .wrap(Wrap { trim: false })
            .scroll((0, 0)),
        area,
    );
}

fn render_install_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = if app.snapshot.synthetic {
        app.locale.text("synthetic_blocked")
    } else {
        app.locale.text("device_stable")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" TAB ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" J/K ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" ENTER ", Style::default().fg(INK).bg(BONE)),
            Span::styled(" ESC ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!("  {}  ", app.locale.text("install_panel_footer"))),
            Span::styled(
                status.to_string(),
                Style::default().fg(if app.snapshot.synthetic {
                    AMBER
                } else {
                    SIGNAL
                }),
            ),
        ]))
        .style(Style::default().fg(MUTED))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(MUTED)),
        ),
        area,
    );
}

fn render(frame: &mut Frame, app: &mut App) {
    let canvas = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(INK)), canvas);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(18),
            Constraint::Length(3),
        ])
        .split(canvas);
    render_header(frame, app, rows[0]);
    if app.install.open {
        render_install_panel(frame, app, rows[1]);
        render_install_footer(frame, app, rows[2]);
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(61), Constraint::Percentage(39)])
        .split(rows[1]);
    render_mission(frame, app, columns[0]);
    render_actions(frame, app, columns[1]);
    render_footer(frame, app, rows[2]);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let mode = if app.snapshot.synthetic {
        app.locale.text("demo_dry")
    } else {
        app.locale.text("local_live")
    };
    let line = Line::from(vec![
        Span::styled(
            " RSETUP ",
            Style::default()
                .fg(INK)
                .bg(SIGNAL)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", app.locale.text("mission_control")),
            Style::default().fg(BONE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "   {} · {}   ",
                app.snapshot.identity.product, app.snapshot.identity.hostname
            ),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            mode,
            Style::default().fg(if app.snapshot.synthetic {
                AMBER
            } else {
                SIGNAL
            }),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(MUTED)),
        ),
        area,
    );
}

fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
    // Unified storage card: NVMe devices take 3 content rows each, MMC/SD
    // devices 2, with one blank row between devices. Long lines wrap inside
    // the card's inner width (the device line alone wraps to two rows on a
    // 100-column terminal), so measure the wrapped row count at the card's
    // width to size the card for what will actually render.
    let has_nvme_devices = app.nvme_status.initialized && !app.nvme_status.devices.is_empty();
    let has_mmc_devices = app.mmc_status.initialized && !app.mmc_status.devices.is_empty();
    let nvme_count = if has_nvme_devices {
        app.nvme_status.devices.len()
    } else {
        0
    };
    let mmc_count = if has_mmc_devices {
        app.mmc_status.devices.len()
    } else {
        0
    };
    let inner_width = (area.width.saturating_sub(2)) as usize;
    // On short viewports keep the two cards above and the service list below
    // intact rather than growing the storage card past the available room.  It
    // is also the row cap the measurements are taken at: a card never renders
    // taller than this, so the scratch buffer never grows with the content.
    let headroom = area.height.saturating_sub(6 + 6 + 4) as usize;
    let nvme_rows = if has_nvme_devices {
        app.nvme_status
            .devices
            .iter()
            .map(|dev| {
                let lines = nvme_device_lines(app, dev, inner_width, headroom).unwrap_or_default();
                wrapped_rows_capped(&lines, inner_width, headroom)
            })
            .sum()
    } else {
        0
    };
    let mmc_rows = if has_mmc_devices {
        app.mmc_status
            .devices
            .iter()
            .map(|dev| {
                let lines = mmc_device_lines(app, dev, inner_width, headroom).unwrap_or_default();
                wrapped_rows_capped(&lines, inner_width, headroom)
            })
            .sum()
    } else {
        0
    };
    let error_lines = storage_error_lines(app);
    let error_rows = wrapped_rows_capped(&error_lines, inner_width, headroom);
    let device_count = nvme_count + mmc_count;
    let content_rows = if device_count == 0 && error_rows == 0 {
        0
    } else {
        error_rows
            + nvme_rows
            + mmc_rows
            + (device_count.saturating_sub(1))
            + if error_rows > 0 && device_count > 0 {
                1
            } else {
                0
            }
    };
    let desired_height = if content_rows == 0 {
        3
    } else {
        2 + content_rows
    };
    let height = desired_height.min(headroom).max(3);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(height as u16),
            Constraint::Min(4),
        ])
        .split(area);
    let cpu = app
        .snapshot
        .metrics
        .cpu_percent
        .map(|v| v.clamp(0.0, 100.0) as u16)
        .unwrap_or(0);
    let memory = app.snapshot.metrics.memory_percent();
    let gauges = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    frame.render_widget(
        Gauge::default()
            .block(instrument(app.locale.text("cpu_load")))
            .gauge_style(Style::default().fg(SIGNAL).bg(Color::Rgb(42, 46, 38)))
            .percent(cpu)
            .label(
                app.snapshot
                    .metrics
                    .cpu_percent
                    .map(|v| format!("{v:.1}%"))
                    .unwrap_or_else(|| app.locale.text("not_available").into()),
            ),
        gauges[0],
    );
    frame.render_widget(
        Gauge::default()
            .block(instrument(app.locale.text("memory_bus")))
            .gauge_style(
                Style::default()
                    .fg(Color::Rgb(103, 214, 255))
                    .bg(Color::Rgb(42, 46, 38)),
            )
            .percent(memory.map(|v| v.clamp(0.0, 100.0) as u16).unwrap_or(0))
            .label(
                memory
                    .map(|v| format!("{v:.1}%"))
                    .unwrap_or_else(|| app.locale.text("not_available").into()),
            ),
        gauges[1],
    );

    let temp = app
        .snapshot
        .metrics
        .temperature_c
        .map(|v| format!("{v:.1} °C"))
        .unwrap_or_else(|| app.locale.text("no_sensor").into());
    let load = app
        .snapshot
        .metrics
        .load_average
        .map(|v| format!("{:.2} {:.2} {:.2}", v[0], v[1], v[2]))
        .unwrap_or_else(|| app.locale.text("not_available").into());
    let uptime = app
        .snapshot
        .metrics
        .uptime_seconds
        .map(|v| duration(v, app.locale))
        .unwrap_or_else(|| app.locale.text("not_available").into());
    let identity = format!(
        "{}\n{} / {}\n{} {}  ·  {} {}  ·  {} {}",
        app.snapshot.identity.product,
        app.snapshot.identity.soc,
        app.snapshot.identity.architecture,
        app.locale.text("uptime"),
        uptime,
        app.locale.text("load_average"),
        load,
        app.locale.text("thermal"),
        temp
    );
    frame.render_widget(
        Paragraph::new(identity)
            .style(Style::default().fg(BONE))
            .block(instrument(app.locale.text("device_core")))
            .wrap(Wrap { trim: true }),
        rows[1],
    );

    render_storage_summary(frame, app, rows[2]);

    let services = app
        .snapshot
        .services
        .iter()
        .map(|service| {
            format!(
                "{}  ·  {}  ·  {}",
                app.locale.service_state(service.state),
                app.locale.service_label(&service.id, &service.label),
                app.locale.service_detail(&service.detail)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(services)
            .style(Style::default().fg(MUTED))
            .block(instrument(app.locale.text("service_signals")))
            .wrap(Wrap { trim: true }),
        rows[3],
    );
}

fn render_actions(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(8)])
        .split(area);
    if app.source_picker {
        render_source_picker(frame, app, rows);
        return;
    }
    let items: Vec<ListItem> = app
        .actions
        .iter()
        .map(|action| {
            let color = if action.available {
                match action.risk {
                    RiskLevel::Safe => SIGNAL,
                    RiskLevel::Guarded => AMBER,
                    RiskLevel::High | RiskLevel::Critical => CORAL,
                }
            } else {
                MUTED
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", app.locale.risk(action.risk)),
                    Style::default().fg(color),
                ),
                Span::styled(
                    app.locale.action_title(&action.id, &action.title),
                    Style::default().fg(BONE),
                ),
            ]))
        })
        .collect();
    let mut list_state = ListState::default().with_selected(Some(app.selected));
    let list = List::new(items)
        .block(instrument(app.locale.text("guided_operations")))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(48, 54, 42))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸");
    frame.render_stateful_widget(list, rows[0], &mut list_state);

    let detail = if app.confirm_pending {
        format!(
            "{}\n{}",
            app.locale.text("confirm_change"),
            app.locale.text("confirm_help")
        )
    } else if let Some(run) = &app.last_run {
        format!(
            "{}\n{}\n{}{}",
            app.locale.text("last_result"),
            app.locale.action_title(&run.action_id, &run.action_title),
            app.locale.run_summary(run),
            app.notice
                .as_ref()
                .map(|notice| format!("\n{notice}"))
                .unwrap_or_default()
        )
    } else if let Some(action) = app.actions.get(app.selected) {
        if action.available {
            format!(
                "{}\n{}\n{} {} · ~{}s",
                action.id,
                app.locale
                    .action_description(&action.id, &action.description),
                action.steps.len(),
                app.locale.text("steps_short"),
                action.estimated_seconds
            )
        } else {
            format!(
                "{}\n{}\n{}: {}",
                action.id,
                app.locale
                    .action_description(&action.id, &action.description),
                app.locale.text("unavailable"),
                app.locale.action_unavailable_reason(
                    action.unavailable_reason.as_deref().unwrap_or("--")
                )
            )
        }
    } else {
        app.locale.text("no_operation").into()
    };
    frame.render_widget(
        Paragraph::new(detail)
            .style(Style::default().fg(if app.confirm_pending { AMBER } else { MUTED }))
            .block(instrument(app.locale.text("task_brief")))
            .wrap(Wrap { trim: true }),
        rows[1],
    );
}

fn render_source_picker(frame: &mut Frame, app: &mut App, rows: std::rc::Rc<[Rect]>) {
    let items = app
        .source_status
        .providers
        .iter()
        .map(|provider| {
            let system = if provider.system_endpoint.is_some() {
                "SYS"
            } else {
                "---"
            };
            let radxa = if provider.radxa_endpoint.is_some() {
                "RADXA"
            } else {
                "-----"
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<5} {:<5} ", system, radxa),
                    Style::default().fg(SIGNAL),
                ),
                Span::styled(&provider.name, Style::default().fg(BONE)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default().with_selected(Some(app.source_selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(instrument(app.locale.text("source_picker")))
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(48, 54, 42))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸"),
        rows[0],
        &mut list_state,
    );

    let mut detail = if app.confirm_pending {
        format!(
            "{}\n{}",
            app.locale.text("confirm_change"),
            app.locale.text("confirm_help")
        )
    } else if let Some(plan) = &app.source_plan {
        let replacements = plan
            .changes
            .iter()
            .map(|change| change.replacements)
            .sum::<usize>();
        let mut lines = vec![format!(
            "{} · {} {} · {} {}",
            plan.provider.name,
            plan.changes.len(),
            app.locale.text("source_files_short"),
            replacements,
            app.locale.text("replacements")
        )];
        lines.extend(plan.changes.iter().map(|change| change.path.clone()));
        lines.extend(
            plan.warnings
                .iter()
                .map(|warning| app.locale.source_warning(warning)),
        );
        lines.join("\n")
    } else {
        app.notice
            .clone()
            .unwrap_or_else(|| app.locale.text("no_operation").into())
    };
    frame.render_widget(
        Paragraph::new({
            if !app.confirm_pending {
                detail.push_str(&benchmark_detail(app));
            }
            detail
        })
        .style(Style::default().fg(if app.confirm_pending { AMBER } else { MUTED }))
        .block(instrument(app.locale.text("source_plan")))
        .wrap(Wrap { trim: true }),
        rows[1],
    );
}

fn benchmark_detail(app: &App) -> String {
    let hint = if app.benchmark_rx.is_some() {
        if app.locale.is_zh() {
            "正在测速…"
        } else {
            "Testing mirror…"
        }
    } else if app.locale.is_zh() {
        "[b] 测试选中镜像 · 索引采样，非带宽上限"
    } else {
        "[b] Test selected mirror · Index sample, not peak bandwidth"
    };
    let mut text = format!("\n\n{hint}");
    if let Some(result) = app
        .source_status
        .providers
        .get(app.source_selected)
        .and_then(|provider| app.benchmarks.get(&provider.id))
        .filter(|result| result.source_revision == app.source_status.source_revision)
    {
        text.push_str(&format!("\n{}", app.locale.mirror_benchmark(result)));
    }
    if let Some(notice) = &app.notice {
        text.push_str(&format!("\n{notice}"));
    }
    text
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let status = if app.snapshot.synthetic {
        app.locale.text("synthetic_blocked")
    } else {
        app.locale.text("device_stable")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ↑/↓ ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!(" {}  ", app.locale.text("select"))),
            Span::styled(" ENTER ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!(" {}  ", app.locale.text("run"))),
            Span::styled(" R ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!(" {}  ", app.locale.text("refresh"))),
            Span::styled(" I ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!(" {}  ", app.locale.text("install_open_hint"))),
            Span::styled(" Q ", Style::default().fg(INK).bg(BONE)),
            Span::raw(format!(" {}", app.locale.text("exit"))),
            Span::styled(
                format!("    {status}"),
                Style::default().fg(if app.snapshot.synthetic {
                    AMBER
                } else {
                    SIGNAL
                }),
            ),
        ]))
        .style(Style::default().fg(MUTED))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(MUTED)),
        ),
        area,
    );
}

fn duration(seconds: u64, locale: Locale) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    if locale.is_zh() {
        format!("{days}天 {hours}小时")
    } else {
        format!("{days}d {hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::fixtures::{eol_text, flatten_screen, render_mission_rows, rock3a_warning_app};
    use ratatui::Terminal;
    use rsetup_core::{ExecutionPolicy, ProbeMode};

    /// Scratch directory under `target/`, removed when the test ends.
    struct InstallScratch(std::path::PathBuf);

    impl std::ops::Deref for InstallScratch {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for InstallScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn install_scratch(name: &str) -> InstallScratch {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/system-install-tui-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create scratch directory");
        InstallScratch(root)
    }

    /// A minimal image with a valid MBR.
    fn install_image_bytes() -> Vec<u8> {
        let mut image = vec![0u8; 2 * 1024 * 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x83;
        image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
        image
    }

    /// Counts every byte handed to a device, so a test can prove that nothing
    /// was written when the confirmation was refused. It wraps the demo device
    /// list, so no real block device is involved.
    struct RecordingBackend {
        inner: SyntheticBackend,
        writes: std::sync::Arc<std::sync::Mutex<u64>>,
    }

    impl BlockDeviceBackend for RecordingBackend {
        fn targets(&self) -> std::result::Result<Vec<SystemInstallTarget>, SystemInstallError> {
            self.inner.targets()
        }

        fn mounts(&self) -> std::result::Result<Vec<SystemMount>, SystemInstallError> {
            self.inner.mounts()
        }

        fn write_chunk(
            &self,
            _target: &SystemInstallTarget,
            _offset: u64,
            chunk: &[u8],
        ) -> std::result::Result<(), SystemInstallError> {
            *self.writes.lock().expect("write counter") += chunk.len() as u64;
            Ok(())
        }

        fn reread_partitions(
            &self,
            _target: &SystemInstallTarget,
        ) -> std::result::Result<Vec<SystemInstallPartition>, SystemInstallError> {
            Err(SystemInstallError::Unsupported(
                "the recording backend does not re-read partition tables".into(),
            ))
        }
    }

    fn recording_panel() -> (InstallPanel, std::sync::Arc<std::sync::Mutex<u64>>) {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(0));
        let backend = std::sync::Arc::new(RecordingBackend {
            inner: SyntheticBackend::default(),
            writes: writes.clone(),
        });
        let manager = SystemInstallManager::with_backend(
            backend,
            std::sync::Arc::new(SystemImageOpener),
            true,
        );
        // Never privileged: this panel exists to observe an in-process write.
        (InstallPanel::from_manager(manager, false, false), writes)
    }

    fn demo_app(locale: Locale) -> App {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        App::new(controller, locale).expect("init app")
    }

    fn render_app(app: &mut App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn select_target(panel: &mut InstallPanel, id: &str) {
        panel.selected = panel
            .targets
            .iter()
            .position(|target| target.id == id)
            .unwrap_or_else(|| panic!("{id} is not among the demo targets"));
    }

    fn type_into(panel: &mut InstallPanel, text: &str) {
        for character in text.chars() {
            panel.push(character);
        }
    }

    fn written(writes: &std::sync::Arc<std::sync::Mutex<u64>>) -> u64 {
        *writes.lock().expect("write counter")
    }

    #[test]
    fn install_panel_shows_every_consequence_and_never_writes_before_the_name_matches() {
        let scratch = install_scratch("panel");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        select_target(&mut app.install, "sda");

        // Step 2: type the image path, then load the plan with Enter.
        app.install.focus_next();
        assert_eq!(app.install.focus, InstallFocus::Image);
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        let plan = app.install.plan.as_ref().expect("plan loaded");
        let sha = plan.image.sha256_prefix.clone();
        let capacity = crate::format_bytes(plan.target.size_bytes);

        let text = render_app(&mut app, 140, 60);
        // Target identity, the destroyed partitions, the image identity and both
        // irreversible warnings are on screen before anything can be written.
        assert!(text.contains("/dev/sda"), "{text}");
        assert!(text.contains("RADXA UFS 128GB"), "{text}");
        assert!(text.contains("UFS"), "{text}");
        assert!(text.contains(&capacity), "{text}");
        assert!(text.contains("Existing partitions destroyed (3)"), "{text}");
        assert!(text.contains("sda1"), "{text}");
        assert!(text.contains("SHA-256 (first 1 MiB)"), "{text}");
        assert!(text.contains(&sha), "{text}");
        assert!(text.contains("MBR"), "{text}");
        assert!(
            text.contains("Writing replaces every partition on the target."),
            "{text}"
        );
        assert!(text.contains("This write cannot be rolled back."), "{text}");
        assert!(
            text.contains("Administrator authorization required"),
            "{text}"
        );
        // The destructive key is shown as disabled, and nothing was written.
        assert!(
            text.contains("Disabled: the typed device name does not match"),
            "{text}"
        );
        assert_eq!(
            written(&writes),
            0,
            "nothing may be written before confirming"
        );

        // A boolean-looking value is refused by the core, and still nothing is
        // written.
        app.install.focus_next();
        assert_eq!(app.install.focus, InstallFocus::Confirm);
        type_into(&mut app.install, "true");
        app.install.confirm(&app.locale);
        match &app.install.status {
            Some(InstallStatus::Error(SystemInstallError::AcknowledgementMismatch {
                expected,
                provided,
                ..
            })) => {
                assert_eq!(expected, "sda");
                assert_eq!(provided, "true");
            }
            other => panic!("a boolean must not confirm: {other:?}"),
        }
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("acknowledgement_mismatch"), "{text}");
        assert_eq!(written(&writes), 0);

        // An empty value is not a confirmation either.
        for _ in 0.."true".len() {
            app.install.backspace();
        }
        app.install.confirm(&app.locale);
        assert!(matches!(
            app.install.status,
            Some(InstallStatus::Error(
                SystemInstallError::ConfirmationRequired
            ))
        ));
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("confirmation_required"), "{text}");
        assert_eq!(written(&writes), 0);

        // The exact device name enables the write.
        type_into(&mut app.install, "sda");
        let text = render_app(&mut app, 140, 60);
        assert!(
            !text.contains("Disabled: the typed device name does not match"),
            "{text}"
        );
        assert!(
            text.contains("Dry run: Enter only simulates the write"),
            "{text}"
        );
        app.install.confirm(&app.locale);
        let result = app.install.result.as_ref().expect("the write ran");
        assert_eq!(result.plan.target.id, "sda");
        assert_eq!(result.bytes_written, install_image_bytes().len() as u64);
        assert_eq!(written(&writes), result.bytes_written);
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("Write complete"), "{text}");
        assert!(
            text.contains("Synthetic target: no real device was written"),
            "{text}"
        );
    }

    #[test]
    fn install_key_routes_typing_listing_and_confirmation() {
        let (panel, writes) = recording_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        app.install.open_panel();

        // TAB moves the focus; characters land in the focused field, including
        // the J/K/R keys that drive the list.
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Image);
        for character in "/tmp/rock/r.img".chars() {
            app.install_key(KeyCode::Char(character));
        }
        assert_eq!(app.install.image_path, "/tmp/rock/r.img");
        app.install_key(KeyCode::Backspace);
        assert_eq!(app.install.image_path, "/tmp/rock/r.im");

        // The missing image is reported by the core when the plan is loaded.
        app.install_key(KeyCode::Enter);
        assert!(
            matches!(
                app.install.status,
                Some(InstallStatus::Error(SystemInstallError::InvalidInput(_)))
            ),
            "{:?}",
            app.install.status
        );

        // Back on the list, J/K move the selection and R reloads the devices.
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Confirm);
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Targets);
        let before = app.install.selected;
        app.install_key(KeyCode::Char('j'));
        assert_ne!(app.install.selected, before);
        app.install_key(KeyCode::Char('k'));
        assert_eq!(app.install.selected, before);
        app.install_key(KeyCode::Char('r'));
        assert!(app.install.plan.is_none());
        assert!(!app.install.targets.is_empty());
        assert_eq!(written(&writes), 0);
    }

    /// A finished write whose verification is one of the two shapes core reports as
    /// "unsupported": a compressed image (the table was re-read, there was nothing
    /// to compare it against) and a host that cannot re-read a table at all.
    fn finished_install(locale: Locale, observed: bool) -> App {
        finished_install_with(
            locale,
            rsetup_core::SystemInstallVerification {
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
            },
        )
    }

    /// A finished write whose verification is supplied by the test.
    fn finished_install_with(
        locale: Locale,
        verification: rsetup_core::SystemInstallVerification,
    ) -> App {
        // A unique scratch name per call: these tests run in parallel in one
        // process, and a shared directory would be removed by whichever finishes
        // first.
        static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The counter alone makes the name unique; the shape no longer needs to be
        // part of it.
        let scratch = install_scratch(&format!("verification-{call}"));
        let image = scratch.join("system.img");
        std::fs::write(&image, install_image_bytes()).expect("write the fixture image");
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(locale);
        app.install = panel;
        select_target(&mut app.install, "sda");
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        app.install.focus_next();
        type_into(&mut app.install, "sda");
        app.install.confirm(&app.locale);
        let mut result = app.install.result.clone().unwrap_or_else(|| {
            panic!(
                "the demo write produced a result: status={:?} plan={:?}",
                app.install.status,
                app.install
                    .plan
                    .as_ref()
                    .map(|plan| plan.plan_token.clone())
            )
        });
        // Keep the run itself: the write is done, only the verification differs.
        result.verification = verification;
        app.install.result = Some(result);
        app
    }

    /// The board's case, rendered: the kernel refused the re-read (EINVAL) after a
    /// write that did complete. The panel must not print the mismatch wording, and
    /// the honest reason has to reach the operator.
    #[test]
    fn a_failed_reread_is_rendered_as_unverified_not_as_a_mismatch() {
        /// Compare on letters and digits only: the panel folds long lines and the
        /// folds carry border cells between the halves.
        fn letters(text: &str) -> String {
            text.chars()
                .filter(|character| character.is_alphanumeric())
                .collect()
        }
        for locale in [Locale::En, Locale::ZhCn] {
            let failed = render_app(
                &mut finished_install_with(
                    locale,
                    rsetup_core::SystemInstallVerification {
                        state: SystemInstallVerificationState::Unsupported,
                        expected: Vec::new(),
                        observed: None,
                        detail: Some(
                            "the kernel could not re-read the partition table of /dev/sda after the image was written, so the write could not be verified: unable to write the system image: the kernel did not re-read /dev/sda: Invalid argument (os error 22)".into(),
                        ),
                    },
                ),
                150,
                60,
            );
            let failed_letters = letters(&failed);
            for expected in [
                "could not re-read the partition table of /dev/sda",
                "after the image was written",
                "could not be verified",
                "did not re-read /dev/sda",
                "Invalid argument (os error 22)",
            ] {
                assert!(
                    failed_letters.contains(&letters(expected)),
                    "{locale:?} must render {expected:?}: {failed}"
                );
            }
            for forbidden in [
                locale.text("install_verification_mismatched"),
                "differs from the image",
                locale.text("install_verification_compressed_not_compared"),
            ] {
                assert!(
                    !failed_letters.contains(&letters(forbidden)),
                    "{locale:?} must not render {forbidden:?}: {failed}"
                );
            }
            // And it stays a success: the image was written.
            assert!(
                failed_letters.contains(&letters(locale.text("install_written"))),
                "{failed}"
            );
        }
    }

    /// Both `Unsupported` shapes, rendered: the compressed image says why it could
    /// not be compared, the platform case keeps the general wording, and neither is
    /// rendered as a failure.
    #[test]
    fn a_compressed_image_explains_that_the_table_was_not_compared() {
        // Wide CJK characters leave a filler cell between glyphs and long lines
        // fold, so both the rendered text and the expected fragments are compared
        // with their whitespace removed.
        fn squash(text: &str) -> String {
            text.split_whitespace().collect::<Vec<_>>().join("")
        }
        for locale in [Locale::En, Locale::ZhCn] {
            let compressed = squash(&render_app(&mut finished_install(locale, true), 150, 60));
            let compressed_key = match locale {
                Locale::ZhCn => "压缩镜像的布局未在写入前解析",
                Locale::En => "its layout was not parsed before writing",
            };
            assert!(
                compressed.contains(&squash(compressed_key)),
                "{locale:?} must say the layout was not compared: {compressed}"
            );
            assert!(
                compressed.contains(&squash("the image is compressed and was not parsed")),
                "{locale:?} must show the core's own reason: {compressed}"
            );
            assert!(
                !compressed.contains(&squash("当前主机无法重读分区表")),
                "the platform wording is wrong for a compressed image: {compressed}"
            );
            // The write is still presented as a success.
            assert!(
                compressed.contains(&squash(locale.text("install_written"))),
                "{compressed}"
            );
            assert!(
                !compressed.contains("install_unsupported"),
                "no error code is shown for a successful write: {compressed}"
            );

            let platform = squash(&render_app(&mut finished_install(locale, false), 150, 60));
            let platform_key = match locale {
                Locale::ZhCn => "当前主机无法重读分区表",
                Locale::En => "cannot re-read partition tables",
            };
            assert!(
                platform.contains(&squash(platform_key)),
                "{locale:?} keeps the general reason for a host that cannot re-read: {platform}"
            );
            assert!(
                !platform.contains(&squash(compressed_key)),
                "{locale:?} must not use the compressed wording here: {platform}"
            );
        }
    }

    /// The success semantics are checked on the model too: the run is a success and
    /// the panel reports a finished write, not an error.
    #[test]
    fn an_uncomparable_verification_is_not_a_failure() {
        for observed in [true, false] {
            let app = finished_install(Locale::En, observed);
            let result = app.install.result.as_ref().expect("result");
            assert_eq!(result.run.status, rsetup_core::ActionStatus::Succeeded);
            assert!(matches!(app.install.status, Some(InstallStatus::Written)));
            assert_eq!(
                result.verification.state,
                SystemInstallVerificationState::Unsupported
            );
        }
    }

    #[test]
    fn install_panel_requires_a_plan_before_writing() {
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        panel.focus = InstallFocus::Confirm;
        type_into(&mut panel, "sda");
        panel.confirm(&Locale::En);
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Info("install_not_planned"))
            ),
            "{:?}",
            panel.status
        );
        assert_eq!(written(&writes), 0);
    }

    /// The reported defect, at this surface: when the panel is told the write has
    /// to be delegated, Enter must not touch the in-process backend at all.
    ///
    /// The helper is absent on a development host, so delegation is observable as
    /// `root_required` before `pkexec` is reached and the write counter stays at
    /// zero. A board that ships the helper, or a run as root, skips instead of
    /// starting a real authorization prompt.
    #[test]
    fn a_privileged_panel_delegates_instead_of_writing_in_process() {
        if std::path::Path::new(crate::PRIVILEGED_HELPER).is_file()
            || crate::effective_uid() == Some(0)
        {
            return;
        }
        let scratch = install_scratch("panel-privileged");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.privileged = true;
        panel.open_panel();
        panel.selected = panel
            .targets
            .iter()
            .position(|target| target.id == "sda")
            .expect("the demo target list carries sda");
        panel.focus_next();
        type_into(&mut panel, &image.display().to_string());
        panel.confirm(&Locale::En);
        assert!(panel.plan.is_some(), "{:?}", panel.status);

        panel.focus_next();
        type_into(&mut panel, "sda");
        panel.confirm(&Locale::En);

        assert_eq!(
            written(&writes),
            0,
            "a delegated write must not reach the in-process backend"
        );
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Error(SystemInstallError::RootRequired))
            ),
            "{:?}",
            panel.status
        );
    }

    #[test]
    fn install_panel_is_localized_and_marks_the_boot_device() {
        let scratch = install_scratch("panel-zh");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::ZhCn);
        app.install = panel;
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);

        let text = render_app(&mut app, 140, 60);
        let squashed = text.split_whitespace().collect::<Vec<_>>().join("");
        assert!(squashed.contains("系统镜像安装"), "{squashed}");
        assert!(squashed.contains("1选择目标设备"), "{squashed}");
        assert!(squashed.contains("将被清除的现有分区"), "{squashed}");
        assert!(
            squashed.contains("写入会替换目标设备上的全部分区。"),
            "{squashed}"
        );
        assert!(squashed.contains("此写入无法回滚。"), "{squashed}");
        assert!(
            squashed.contains("已禁用：键入的设备名尚未匹配目标"),
            "{squashed}"
        );
        assert!(squashed.contains("承载正在运行的系统"), "{squashed}");
        assert!(squashed.contains("可移动介质"), "{squashed}");
        assert!(squashed.contains("SHA-256（前1MiB）"), "{squashed}");
    }

    #[test]
    fn install_panel_refuses_the_device_that_hosts_the_running_system() {
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        select_target(&mut panel, "mmcblk1");
        panel.load_plan();
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Error(SystemInstallError::RunningSystem(_)))
            ),
            "{:?}",
            panel.status
        );
        assert!(
            panel.plan.is_none(),
            "a refused device has no plan to apply"
        );
        assert_eq!(written(&writes), 0);

        // There is no plan, so even the exact typed name cannot write anything.
        panel.focus = InstallFocus::Confirm;
        type_into(&mut panel, "mmcblk1");
        panel.confirm(&Locale::En);
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Info("install_not_planned"))
            ),
            "{:?}",
            panel.status
        );
        assert_eq!(written(&writes), 0);

        // The list still shows why that device is refused.
        let mut app = demo_app(Locale::En);
        app.install = panel;
        app.install.open = true;
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("/dev/mmcblk1"), "{text}");
        assert!(text.contains("Hosts the running system"), "{text}");
    }

    #[test]
    fn install_panel_edits_only_the_focused_field() {
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        panel.focus_next();
        type_into(&mut panel, "/tmp");
        assert_eq!(panel.image_path, "/tmp");
        panel.backspace();
        assert_eq!(panel.image_path, "/tm");

        panel.focus_next();
        type_into(&mut panel, "sda");
        assert_eq!(panel.typed, "sda");
        panel.backspace();
        assert_eq!(panel.typed, "sd");

        // J/K only move the device selection while the list owns the focus.
        let before = panel.selected;
        panel.next_target();
        assert_eq!(panel.selected, before);
        panel.focus_next();
        assert_eq!(panel.focus, InstallFocus::Targets);
        panel.next_target();
        assert_ne!(panel.selected, before);

        // Leaving the panel drops the plan and the typed name.
        panel.close();
        assert!(panel.plan.is_none());
        assert!(panel.typed.is_empty());
        assert!(!panel.open);
    }

    #[test]
    fn install_panel_typing_is_trimmed_like_the_cli() {
        let scratch = install_scratch("panel-trim");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        select_target(&mut app.install, "sda");
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        app.install.focus_next();
        // Spaces around the name are trimmed, exactly as the CLI trims stdin.
        type_into(&mut app.install, "  sda  ");
        app.install.confirm(&app.locale);
        assert!(app.install.result.is_some(), "{:?}", app.install.status);
        assert!(written(&writes) > 0);
    }

    #[test]
    fn mission_gauges_distinguish_unknown_from_zero_in_both_locales() {
        for locale in [Locale::En, Locale::ZhCn] {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, locale).unwrap();
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = Terminal::new(backend).unwrap();
            app.snapshot.metrics.cpu_percent = None;
            app.snapshot.metrics.memory_used_bytes = None;
            app.snapshot.metrics.memory_total_bytes = None;
            app.snapshot.metrics.load_average = None;
            app.snapshot.metrics.uptime_seconds = None;
            terminal
                .draw(|frame| render_mission(frame, &app, frame.area()))
                .unwrap();
            let gauge_text = terminal.backend().buffer().content()[..120 * 6]
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            // Buffer cells reserve an extra column for wide CJK characters.
            let gauge_text = gauge_text.split_whitespace().collect::<Vec<_>>().join("");
            assert!(gauge_text.matches(locale.text("not_available")).count() >= 2);
            assert!(!gauge_text.contains("0.0%"));
            app.snapshot.metrics.cpu_percent = Some(0.0);
            app.snapshot.metrics.memory_used_bytes = Some(0);
            app.snapshot.metrics.memory_total_bytes = Some(100);
            terminal
                .draw(|frame| render_mission(frame, &app, frame.area()))
                .unwrap();
            let gauge_text = terminal.backend().buffer().content()[..120 * 6]
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert_eq!(gauge_text.matches("0.0%").count(), 2);
        }
    }

    #[test]
    fn test_tui_app_loads_and_refreshes_nvme_status() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::En).expect("init app");
        assert!(app.nvme_status.initialized);
        assert_eq!(app.nvme_status.devices.len(), 1);
        app.refresh().expect("refresh app");
        assert!(app.nvme_status.initialized);
    }

    #[test]
    fn test_tui_app_loads_and_refreshes_mmc_status() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::En).expect("init app");
        // Demo mode returns 2 MMC devices (eMMC + SD).
        assert!(app.mmc_status.initialized);
        assert_eq!(app.mmc_status.devices.len(), 2);
        app.refresh().expect("refresh app");
        assert!(app.mmc_status.initialized);
        assert_eq!(app.mmc_status.devices.len(), 2);
    }

    #[test]
    fn test_render_mission_gives_the_mmc_card_the_rows_it_needs() {
        // 33 columns of card (31 inner) on a viewport tall enough to spare the
        // rows: the card has to size itself for the rows the word wrap really
        // produces, which is what kept the warning from being clipped (parent
        // 26fab4b sized the card one row short and lost the tail).
        for locale in [Locale::En, Locale::ZhCn] {
            let app = rock3a_warning_app(locale, 2, Some(10));
            let rows = render_mission_rows(&app, 33, 30);
            let screen = rows.join("\n");
            let flat = flatten_screen(&rows);
            assert!(
                flat.contains(app.locale.text("storage_health"))
                    && flat.contains(app.locale.text("storage_warning")),
                "{locale:?}: the health state vanished:\n{screen}"
            );
            assert!(
                flat.contains(&format!(
                    "{}:{}",
                    app.locale.text("storage_pre_eol"),
                    eol_text(locale, 2)
                )),
                "{locale:?}: the pre-EOL warning vanished at 31 columns:\n{screen}"
            );
            // Either spelling of the label is fine; what must not vanish is
            // the estimate itself.
            for (full_key, short_key) in [
                ("storage_life_a", "storage_life_a_short"),
                ("storage_life_b", "storage_life_b_short"),
            ] {
                let shown = [full_key, short_key].iter().any(|key| {
                    flat.contains(&format!("{}:10%", app.locale.text(key).replace(' ', "")))
                });
                assert!(
                    shown,
                    "{locale:?}: the '{full_key}' estimate vanished at 31 columns:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn test_render_mission_prefers_the_danger_row_when_the_card_is_short() {
        // The same viewport the review measured: the mission column leaves the
        // storage card 5 rows, which cannot hold identity plus the full
        // telemetry wrap. The danger values have to survive that; a life-time
        // estimate is what may go.
        for locale in [Locale::En, Locale::ZhCn] {
            for pre_eol in [2u8, 3] {
                let app = rock3a_warning_app(locale, pre_eol, Some(10));
                // 21 rows of viewport leave a headroom of 5 rows for the card.
                let rows = render_mission_rows(&app, 33, 21);
                let screen = rows.join("\n");
                let flat = flatten_screen(&rows);
                assert!(
                    flat.contains(&eol_text(locale, pre_eol).replace(' ', "")),
                    "{locale:?} pre_eol={pre_eol}: the pre-EOL value was clipped away \
                     instead of degrading the row:\n{screen}"
                );
                assert!(
                    flat.contains(&app.locale.text("storage_warning").replace(' ', "")),
                    "{locale:?} pre_eol={pre_eol}: the health state was clipped away:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn test_mmc_warning_survives_every_card_width() {
        // The review's scan: the card is sized by render_mission itself, so an
        // estimate that is short of the rows the paragraph produces clips the
        // last row instead of growing. 18..=61 inner columns, both locales,
        // pre-EOL warning and urgent, life-time estimates absent, 10 %, 100 %
        // and over 100 %. 19 of those variants lost the warning.
        for locale in [Locale::En, Locale::ZhCn] {
            for pre_eol in [2u8, 3] {
                for life in [None, Some(10), Some(100), Some(101)] {
                    let app = rock3a_warning_app(locale, pre_eol, life);
                    for inner_width in 18..=61usize {
                        let rows = render_mission_rows(&app, inner_width as u16 + 2, 40);
                        let screen = rows.join("\n");
                        let flat = rows
                            .iter()
                            .map(|row| row.split_whitespace().collect::<Vec<_>>().join(""))
                            .collect::<String>();
                        assert!(
                            flat.contains(&eol_text(locale, pre_eol).replace(' ', "")),
                            "{locale:?} pre_eol={pre_eol} life={life:?} at {inner_width} \
                             columns: the pre-EOL value is not on screen:\n{screen}"
                        );
                        assert!(
                            flat.contains(
                                &app.locale
                                    .text("storage_warning")
                                    .split_whitespace()
                                    .collect::<Vec<_>>()
                                    .join("")
                            ),
                            "{locale:?} pre_eol={pre_eol} life={life:?} at {inner_width} \
                             columns: the health state is not on screen:\n{screen}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_render_mission_card_keeps_the_life_estimates_while_they_fit() {
        // The life-time estimates are the values that give way under pressure,
        // and only when there is no row left for them: while the card has the
        // rows, every value the probe reported stays on screen — including at
        // the 59 and 61 columns that were already correct before the fix.
        for locale in [Locale::En, Locale::ZhCn] {
            for pre_eol in [2u8, 3] {
                let app = rock3a_warning_app(locale, pre_eol, Some(10));
                for inner_width in [31usize, 40, 59, 61] {
                    let rows = render_mission_rows(&app, inner_width as u16 + 2, 40);
                    let screen = rows.join("\n");
                    let flat = flatten_screen(&rows);
                    // Either spelling of the label is right: the shorter one
                    // is what keeps the whole block on one row on a wide card.
                    for (full_key, short_key) in [
                        ("storage_life_a", "storage_life_a_short"),
                        ("storage_life_b", "storage_life_b_short"),
                    ] {
                        let shown = [full_key, short_key].iter().any(|key| {
                            flat.contains(&format!("{}:10%", app.locale.text(key).replace(' ', "")))
                        });
                        assert!(
                            shown,
                            "{locale:?} pre_eol={pre_eol} at {inner_width} columns: \
                             '{full_key}' is not on screen:\n{screen}"
                        );
                    }
                    assert!(
                        flat.contains(&eol_text(locale, pre_eol).replace(' ', "")),
                        "{locale:?} pre_eol={pre_eol} at {inner_width} columns:\n{screen}"
                    );
                }
            }
        }
    }
    #[test]
    fn test_render_mission_narrow_card_drops_life_before_the_warning() {
        // A viewport that leaves the card barely more than its three rows: the
        // danger values survive and a life-time estimate is what goes, so a
        // drop is a decision and never half a word left on the screen.
        for locale in [Locale::En, Locale::ZhCn] {
            let app = rock3a_warning_app(locale, 2, Some(10));
            let rows = render_mission_rows(&app, 33, 21);
            let screen = rows.join("\n");
            let flat = rows
                .iter()
                .map(|row| row.split_whitespace().collect::<Vec<_>>().join(""))
                .collect::<String>();
            assert!(
                flat.contains(&eol_text(locale, 2).replace(' ', "")),
                "{locale:?}: the pre-EOL warning was clipped at the 5-row card:\n{screen}"
            );
            assert!(
                flat.contains("mmcblk0") && flat.contains("58.24GiB"),
                "{locale:?}: the device identity must keep its row:\n{screen}"
            );
        }
    }

    #[test]
    fn test_render_full_tui_with_nvme_zh() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::ZhCn).expect("init app");
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                render(frame, &mut app);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let raw_text = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
        assert!(
            norm_text.contains("存储状态"),
            "Expected '存储状态' in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("Radxa M.2 NVMe SSD 512GB"),
            "Expected 'Radxa M.2 NVMe SSD 512GB' in buffer"
        );
        assert!(norm_text.contains("正常"), "Expected '正常' in buffer");
        assert!(raw_text.contains("38.5 °C"), "Expected '38.5 °C' in buffer");
        // Regression for the live Rock 5B screenshot: on a 100x30 terminal the
        // telemetry card must show the compacted health line (including the
        // available spare) and the read/write line with its threshold, instead
        // of clipping the wrapped overflow row.
        assert!(
            norm_text.contains("状态:正常·温度:38.5°C·已用寿命:2%·备用:100%"),
            "Expected compacted telemetry line 2 in buffer, got: {raw_text}"
        );
        assert!(
            norm_text.contains("读写:Read1.14TiB/Written791.62GiB·阈值10%"),
            "Expected telemetry line 3 with spare threshold in buffer, got: {raw_text}"
        );
    }

    #[test]
    fn test_render_full_tui_no_storage_devices() {
        // Test in Chinese
        {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, Locale::ZhCn).expect("init app");
            app.nvme_status = rsetup_core::NvmeStatus {
                initialized: false,
                devices: vec![],
                message: Some("No NVMe".into()),
            };
            app.mmc_status = rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: Some("No MMC".into()),
            };
            let backend = ratatui::backend::TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render(frame, &mut app);
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let raw_text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
            assert!(
                norm_text.contains("未检测到NVMe或MMC存储设备"),
                "Expected Chinese no-storage message in buffer, got: {raw_text}"
            );
        }

        // Test in English
        {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, Locale::En).expect("init app");
            app.nvme_status = rsetup_core::NvmeStatus {
                initialized: false,
                devices: vec![],
                message: Some("No NVMe".into()),
            };
            app.mmc_status = rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: Some("No MMC".into()),
            };
            let backend = ratatui::backend::TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render(frame, &mut app);
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let raw_text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                raw_text.contains("No NVMe or MMC storage devices detected."),
                "Expected English no-storage message in buffer, got: {raw_text}"
            );
        }
    }

    #[test]
    fn test_render_tui_small_viewport() {
        let viewports = [(60, 18), (40, 12)];
        for (w, h) in viewports {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, Locale::ZhCn).expect("init app");
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            let res = terminal.draw(|frame| {
                render(frame, &mut app);
            });
            assert!(res.is_ok(), "Rendering failed on viewport {w}x{h}");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_tui_app_same_instance_injected_storage_freshness() {
        use rsetup_core::{
            ExecutionPolicy, HardwareError, HealthState, MmcDevice, MmcHealth, MmcStatus,
            NvmeDevice, NvmeSmartLog, NvmeStatus, ProbeMode, StorageReader, StorageStatus,
            TelemetryReadState, TelemetryStatus,
        };
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct FakeStorage(Arc<Mutex<StorageStatus>>);

        impl StorageReader for FakeStorage {
            fn nvme_status(&self) -> Result<NvmeStatus, HardwareError> {
                Ok(self.0.lock().unwrap().nvme.clone())
            }
            fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
                Ok(self.0.lock().unwrap().mmc.clone())
            }
        }

        let initial_storage = StorageStatus {
            nvme: NvmeStatus {
                initialized: true,
                devices: vec![NvmeDevice {
                    path: "/dev/nvme0".into(),
                    name: "nvme0".into(),
                    model: "Test NVMe SSD".into(),
                    serial: "123".into(),
                    firmware: "1".into(),
                    total_bytes: 1000,
                    smart: Some(NvmeSmartLog {
                        critical_warning: 0,
                        temperature_c: 42.0,
                        available_spare_percent: 100,
                        spare_threshold_percent: 10,
                        percentage_used: 5,
                        data_read_bytes: 1000,
                        data_written_bytes: 1000,
                        host_read_commands: 10,
                        host_write_commands: 10,
                        power_on_hours: 1,
                        unsafe_shutdowns: 0,
                        media_errors: 0,
                        num_err_log_entries: 0,
                        warning_flags: vec![],
                    }),
                    telemetry: TelemetryStatus {
                        state: TelemetryReadState::Available,
                        error: None,
                    },
                    health_state: HealthState::Healthy,
                }],
                message: None,
            },
            mmc: MmcStatus {
                initialized: true,
                devices: vec![MmcDevice {
                    name: "mmc0:0001".into(),
                    card_type: "MMC".into(),
                    model: "EMMC_CARD".into(),
                    manufacturer: "Vendor".into(),
                    serial: "456".into(),
                    firmware: "1".into(),
                    block_path: "/dev/mmcblk0".into(),
                    total_bytes: 2000,
                    health: MmcHealth {
                        pre_eol_info: 1,
                        life_time_est_a_percent: Some(10),
                        life_time_est_b_percent: Some(10),
                        warning_flags: vec![],
                    },
                    telemetry: TelemetryStatus {
                        state: TelemetryReadState::Available,
                        error: None,
                    },
                    health_state: HealthState::Healthy,
                }],
                message: None,
            },
        };

        let shared = Arc::new(Mutex::new(initial_storage));
        let fake = FakeStorage(Arc::clone(&shared));

        let controller = Controller::with_storage_reader(
            ProbeMode::Live,
            ExecutionPolicy::DryRun,
            Arc::new(fake),
        );

        let mut app = App::new(controller, Locale::En).expect("init app");
        // On non-Linux host App::new snapshot/controller is synthetic unless forced, but let's check app.nvme_status
        // If non-Linux, Controller with ProbeMode::Live is synthetic if !cfg!(target_os = "linux")
        // But in our test environment (Linux x86_64 or Linux aarch64), let's check:
        // If it is Linux, it queried our fake!
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                app.nvme_status.devices[0]
                    .smart
                    .as_ref()
                    .unwrap()
                    .temperature_c,
                42.0
            );
            assert_eq!(app.mmc_status.devices.len(), 1);

            // Mutate storage state
            {
                let mut state = shared.lock().unwrap();
                if let Some(smart) = &mut state.nvme.devices[0].smart {
                    smart.temperature_c = 78.0;
                }
                state.mmc.devices.clear();
                state.mmc.initialized = false;
            }

            // Refresh app
            app.refresh().expect("app refresh");

            // Verify refreshed state reflects mutation on same App instance
            assert_eq!(
                app.nvme_status.devices[0]
                    .smart
                    .as_ref()
                    .unwrap()
                    .temperature_c,
                78.0
            );
            assert_eq!(app.mmc_status.devices.len(), 0);
            assert!(!app.mmc_status.initialized);

            // Render and verify UI text has 78°C
            let backend = ratatui::backend::TestBackend::new(120, 30);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render_storage_summary(frame, &app, Rect::new(0, 0, 100, 15));
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let raw_text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                raw_text.contains("78.0 °C") || raw_text.contains("78°C"),
                "Expected 78.0 °C in rendered buffer, got: {raw_text}"
            );
        }
    }
}
