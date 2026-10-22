use crate::i18n::Locale;
use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Widget, Wrap},
};
use rsetup_core::{
    ActionRun, ActionSpec, BlockDeviceBackend, Controller, DeviceSnapshot, ExecutionPolicy,
    RiskLevel, SourcePlan, SourceStatus, SyntheticBackend, SystemBlockBackend, SystemImageOpener,
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPartition,
    SystemInstallPlan, SystemInstallRequest, SystemInstallTarget, SystemInstallVerificationState,
    SystemMount,
};
use std::{io, time::Duration};

const SIGNAL: Color = Color::Rgb(199, 255, 74);
const AMBER: Color = Color::Rgb(255, 179, 65);
const CORAL: Color = Color::Rgb(255, 90, 73);
const INK: Color = Color::Rgb(16, 18, 15);
const BONE: Color = Color::Rgb(232, 227, 213);
const MUTED: Color = Color::Rgb(139, 145, 128);

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

fn render_storage_summary(frame: &mut Frame, app: &App, area: Rect) {
    let block = instrument(app.locale.text("storage_telemetry"));

    let nvme = if app.nvme_status.initialized {
        app.nvme_status.devices.as_slice()
    } else {
        &[]
    };
    let mmc = if app.mmc_status.initialized {
        app.mmc_status.devices.as_slice()
    } else {
        &[]
    };

    if app.nvme_error.is_none() && app.mmc_error.is_none() && nvme.is_empty() && mmc.is_empty() {
        let msg = Paragraph::new(app.locale.text("storage_not_detected"))
            .style(Style::default().fg(MUTED))
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(msg, area);
        return;
    }

    let budget = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut used_rows = 0usize;

    // NVMe devices all come first, then the MMC/SD devices. When the card is
    // too short for all of them, trailing devices are dropped and a "more
    // devices" hint row is shown instead. Rows are counted after wrapping at
    // the card's inner width, because the budget is in rendered rows.
    enum Device<'a> {
        Nvme(&'a rsetup_core::NvmeDevice),
        Mmc(&'a rsetup_core::MmcDevice),
    }
    let devices: Vec<Device<'_>> = nvme
        .iter()
        .map(Device::Nvme)
        .chain(mmc.iter().map(Device::Mmc))
        .collect();

    // The hint is paid for before a single row is spent, because it is the only
    // thing that can admit a dropped device. An enumeration error used to take
    // every content row it needed first, which left the device loop a budget of
    // zero *and* the card no row for the count: at 33x4 a two-row error left
    // two warning eMMCs neither shown nor counted (mmc third review P2-1). What
    // is held back is the leanest wording of the hint, so the card can always
    // still say the number; a card with no devices has nothing to count and
    // holds nothing back.
    let hint_floor = if devices.is_empty() {
        0
    } else {
        storage_more_hint_floor(app.locale, devices.len(), inner_width).min(budget)
    };

    let error_lines = storage_error_lines(app);
    for err_line in error_lines {
        // An error that will not fit shrinks rather than taking the card: the
        // devices behind it and the count of them still need their rows.
        let room = budget.saturating_sub(used_rows + hint_floor);
        let Some(line) = fit_line_in_rows(&err_line, inner_width, room) else {
            continue;
        };
        let rows = wrapped_rows_capped(std::slice::from_ref(&line), inner_width, room);
        if used_rows + rows <= budget {
            lines.push(line);
            used_rows += rows;
        }
    }
    let mut more = 0usize;
    for (idx, dev) in devices.iter().enumerate() {
        // A blank row separates consecutive devices, or devices from errors; charge it to the device.
        let needs_sep = idx > 0 || !lines.is_empty();
        // Ask each device for the richest block that fits what is left of the
        // card, so a short card degrades its own rows instead of leaving the
        // paragraph to cut the tail off the last one.  When devices follow this
        // one, the hint's rows are held back with it: a device that is dropped
        // has to be accounted for, never vanish in silence (mmc second review
        // P2-2 — two warning eMMCs in a five-row card let the first eat every
        // content row and the second disappeared with no hint).  A device that
        // cannot fit its own danger values then yields entirely (the
        // constructor returns `None`), which falls back to a truthful count
        // rather than a block clipped of its warning.
        let reserve = usize::from(idx + 1 < devices.len()) * hint_floor;
        let allowance = budget.saturating_sub(used_rows + usize::from(needs_sep) + reserve);
        let dev_lines = match dev {
            Device::Nvme(d) => nvme_device_lines(app, d, inner_width, allowance),
            Device::Mmc(d) => mmc_device_lines(app, d, inner_width, allowance),
        };
        let Some(dev_lines) = dev_lines else {
            more = devices.len() - idx;
            break;
        };
        let rows = wrapped_rows_capped(&dev_lines, inner_width, allowance) + usize::from(needs_sep);
        if used_rows + rows > budget {
            more = devices.len() - idx;
            break;
        }
        if needs_sep {
            lines.push(Line::from(""));
        }
        lines.extend(dev_lines);
        used_rows += rows;
    }
    if more > 0 {
        // The reserves above are what leaves room here.  The ladder picks the
        // most it can say *whole* in that room instead of assuming the wording
        // is one row tall (mmc third review P2-2: the hint was reserved and
        // measured as one row, and at 16 inner columns `... 2 more device(s)`
        // is two — the card painted `... 2 more` and dropped `device(s)`).
        let room = budget.saturating_sub(used_rows);
        if let Some(hint) = storage_more_hint(app.locale, more, inner_width, room) {
            lines.push(hint);
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

/// The "more devices" hint in every wording the card might have room for,
/// richest first.
///
/// The hint is the card's only way to admit that it dropped a device, so it has
/// to be paintable *whole*: on the 16 inner columns of an 18-column card the
/// English wording needs two rows, and the one row a short card could spare was
/// cut to `... 2 more` with `device(s)` off the edge.  Callers walk the ladder
/// and take the richest form that fits, and a card that has devices holds back
/// the rows the leanest form needs, so the number always has a home.
fn storage_more_hints(locale: Locale, more: usize) -> [Line<'static>; 3] {
    let muted = Style::default().fg(MUTED);
    let label = locale.text("storage_more_devices");
    let wording = if locale.is_zh() {
        format!("... {label} {more}")
    } else {
        format!("... {more} {label}")
    };
    [wording, format!("... +{more}"), format!("+{more}")]
        .map(|text| Line::from(vec![Span::styled(text, muted)]))
}

/// The rows the leanest honest hint needs at this width, and so the rows a card
/// that has devices holds back before it spends anything on errors or devices.
fn storage_more_hint_floor(locale: Locale, count: usize, inner_width: usize) -> usize {
    let hints = storage_more_hints(locale, count);
    let leanest = hints.last().expect("the hint ladder is not empty");
    wrapped_rows_capped(std::slice::from_ref(leanest), inner_width, MAX_WRAP_ROWS)
}

/// The hint the room can hold whole, richest first; `None` when no wording
/// fits, because a card too narrow for `+N` shows nothing rather than half a
/// word.
fn storage_more_hint(
    locale: Locale,
    more: usize,
    inner_width: usize,
    room: usize,
) -> Option<Line<'static>> {
    storage_more_hints(locale, more).into_iter().find(|line| {
        matches!(
            wrapped_rows(std::slice::from_ref(line), inner_width, room),
            RowFit::Fits(_)
        )
    })
}

/// `line` shrunk to at most `max_rows` rows at `inner_width`, or `None` when
/// even one row of it would not fit.
///
/// The line keeps its head and gives up its tail, cut with the ellipsis
/// [`ellipsize`] marks, so a short card says less of an error instead of
/// everything of an error and nothing of the devices behind it (mmc third
/// review P2-1: an error line charged its rows against the whole card, and the
/// eMMCs below it were neither shown nor counted).  A cut that is announced is
/// a decision; taking every row and leaving the devices uncounted was not.  The
/// spans keep their own styles and the cut lands in the one the ellipsis gives
/// up on, because it is the *text* of the line that is being shortened, not one
/// of its parts.
///
/// The scan starts at the widest head that could still fit and stops at the
/// first candidate that measures small enough, which costs at most one
/// measurement per column of the card's text area.
fn fit_line_in_rows(
    line: &Line<'static>,
    inner_width: usize,
    max_rows: usize,
) -> Option<Line<'static>> {
    let fits = |candidate: &Line<'static>| {
        matches!(
            wrapped_rows(std::slice::from_ref(candidate), inner_width, max_rows),
            RowFit::Fits(_)
        )
    };
    if max_rows == 0 {
        return None;
    }
    if fits(line) {
        return Some(line.clone());
    }
    let total: usize = line.spans.iter().map(|span| span.width()).sum();
    // A head wider than the rows it would be painted on cannot fit whatever the
    // word breaks do, so that is where the scan starts; the empty head is its
    // floor, which is what tells the caller the line has no room at all.
    let ceiling = inner_width.saturating_mul(max_rows).min(total);
    for kept in (0..=ceiling).rev() {
        let mut used = 0usize;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for span in &line.spans {
            if used + span.width() <= kept {
                used += span.width();
                spans.push(span.clone());
                continue;
            }
            let cut = ellipsize(&span.content, kept - used);
            if !cut.is_empty() {
                spans.push(Span::styled(cut, span.style));
            }
            break;
        }
        let candidate = Line::from(spans);
        if fits(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn storage_error_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(err) = &app.nvme_error {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.locale.text("storage_nvme_enum_failed")),
                Style::default().fg(CORAL).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err.to_string(), Style::default().fg(MUTED)),
        ]));
    }
    if let Some(err) = &app.mmc_error {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}: ", app.locale.text("storage_mmc_enum_failed")),
                Style::default().fg(CORAL).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err.to_string(), Style::default().fg(MUTED)),
        ]));
    }
    lines
}

/// The ceiling on the rows any single measurement is allowed to render.  Every
/// caller caps `wrapped_rows` at a screen height; this constant only bounds the
/// pathological case (one enormous line), so a scratch buffer never grows with
/// the input — at most `MAX_WRAP_ROWS` rows times the card width, not a function
/// of the text length.
const MAX_WRAP_ROWS: usize = 512;

/// The outcome of measuring a block against a row budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowFit {
    /// The block occupies exactly `n` rows (and `n` was within the caller's cap).
    Fits(usize),
    /// The block needs more than the caller's cap.  The exact count is
    /// deliberately not claimed: it was never measured past the cap, so a
    /// number here would be a clipped buffer passed off as exact (mmc second
    /// review P3 — the first fix's scratch upper bound was not an upper bound).
    Overflow,
}

/// A line that always paints a cell, used to anchor the row probe of
/// `wrapped_rows`.
const ROW_PROBE: &str = "#";

/// The last row a paragraph paints at `inner_width` inside `height` rows, plus
/// one.
///
/// This is a *lower* bound on the rows the paragraph consumes: a wrapped row
/// that composes to nothing — an empty line, a run of whitespace too wide for
/// the row — paints no cell at all and is invisible here.  Nothing may infer
/// from a smaller count that the paragraph ended; that inference is what
/// `wrapped_rows` avoids with `ROW_PROBE`.
fn painted_rows(lines: &[Line<'static>], inner_width: usize, height: u16) -> usize {
    let width = inner_width.min(u16::MAX as usize) as u16;
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: true })
        .render(buffer.area, &mut buffer);
    (0..height)
        .rfind(|row| (0..width).any(|column| buffer[(column, *row)].symbol() != " "))
        .map_or(0, |last| last as usize + 1)
}

/// Rows a word-wrapped paragraph needs for `lines` inside `inner_width`,
/// measured with ratatui's own wrap and capped at `cap` rows.
///
/// Ratatui breaks text at word boundaries, so neither `ceil(width / inner_width)`
/// nor a per-word sum is the row count — both come out a row short as soon as a
/// word cannot start on the tail of a row, and a card sized from a short count
/// clips its last row instead of growing (the row carrying the pre-EOL warning
/// of an eMMC disappeared that way, mmc review P2).  The count is therefore
/// taken from the widget itself, but only within `cap`: the render either fits
/// (`Fits(n)`, exact) or demonstrably runs past `cap` (`Overflow`).  An empty
/// block occupies no rows — distinct from one explicit empty line, which is a
/// real blank row (mmc second review P2-1: charging an empty block a row added
/// phantom blank rows to a healthy one-device card).
fn wrapped_rows(lines: &[Line<'static>], inner_width: usize, cap: usize) -> RowFit {
    if lines.is_empty() {
        return RowFit::Fits(0);
    }
    let widest = lines.iter().map(|line| line.width()).max().unwrap_or(0);
    // A line never wraps while it is not wider than the row it gets, so each is
    // exactly one row; count them without allocating a scratch buffer at all.
    if inner_width == 0 || inner_width >= widest {
        let n = lines.len();
        return if n <= cap {
            RowFit::Fits(n)
        } else {
            RowFit::Overflow
        };
    }
    // Probe two rows past the budget the caller cares about: one to tell
    // "fills the buffer" from "ends inside it", and one more for `ROW_PROBE`.
    // The probe line is what makes the answer trustworthy.  Reading the last
    // painted row off the buffer asks whether the block ended inside the
    // buffer, and a block whose tail composes to blank rows cannot answer it:
    // `["abcde", "", "WARNING"]` at three columns ended its first two rows on
    // the blank row of the empty line and came back `Fits(2)` while `WARNING`
    // was already below the buffer (mmc third review P3 — a blank lookahead is
    // not an end-of-block marker).  `ROW_PROBE` always paints, so its row *is*
    // the block's row count: seeing it on the buffer gives an exact number,
    // not seeing it proves the block runs past the cap.
    let probe = cap.saturating_add(2).min(MAX_WRAP_ROWS) as u16;
    let plain = painted_rows(lines, inner_width, probe);
    let mut anchored = lines.to_vec();
    anchored.push(Line::from(ROW_PROBE));
    let anchored = painted_rows(&anchored, inner_width, probe);
    // The anchored render paints everything the plain one does, so a probe
    // that added nothing was pushed past the buffer: the block needs at least
    // `probe` rows, which is more than the cap by construction.
    if anchored <= plain {
        return RowFit::Overflow;
    }
    let rows = anchored - 1;
    if rows > cap {
        RowFit::Overflow
    } else {
        RowFit::Fits(rows)
    }
}

/// The row count for a block, saturating at `cap + 1` on overflow, for callers
/// that only add it into a height budget which clamps at `cap` anyway.
fn wrapped_rows_capped(lines: &[Line<'static>], inner_width: usize, cap: usize) -> usize {
    match wrapped_rows(lines, inner_width, cap) {
        RowFit::Fits(n) => n,
        RowFit::Overflow => cap + 1,
    }
}

/// The rows of one NVMe device, fitted into `max_rows`.  The I/O line is the
/// one that gives way first; the identity and the health state stay together
/// or the device is not rendered at all (`None`).
fn nvme_device_lines(
    app: &App,
    dev: &rsetup_core::NvmeDevice,
    inner_width: usize,
    max_rows: usize,
) -> Option<Vec<Line<'static>>> {
    let size_str = crate::format_bytes(dev.total_bytes);
    // Line 1: dev.name, dev.path, model, capacity
    let line1 = Line::from(vec![
        Span::styled(
            format!("{} ", dev.name),
            Style::default().fg(BONE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("({}) · ", dev.path), Style::default().fg(MUTED)),
        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
        Span::styled(size_str, Style::default().fg(AMBER)),
    ]);

    // Line 2: Health state, temperature, used endurance, available spare
    let (status_text, status_color) = match dev.health_state {
        rsetup_core::HealthState::Healthy => (app.locale.text("storage_healthy"), SIGNAL),
        rsetup_core::HealthState::Warning => (app.locale.text("storage_warning"), AMBER),
        rsetup_core::HealthState::Critical => (app.locale.text("storage_critical"), CORAL),
        rsetup_core::HealthState::Unknown => (app.locale.text("storage_unknown"), MUTED),
    };
    let status_label = if app.locale.is_zh() {
        "状态: "
    } else {
        "Health: "
    };
    let temp_label = if app.locale.is_zh() {
        "温度: "
    } else {
        "Temp: "
    };
    let spare_label = if app.locale.is_zh() {
        "备用: "
    } else {
        "Spare: "
    };
    let endurance_label = if app.locale.is_zh() {
        "已用寿命: "
    } else {
        "Used Endurance: "
    };
    // "Used Endurance:" is long enough to push the mandatory available-spare
    // value past the 59 columns the mission panel gets on a 100-column
    // terminal, so fall back to the short spelling when the full one does
    // not fit next to the warning flags.
    let short_endurance_label = if app.locale.is_zh() {
        endurance_label
    } else {
        "Used: "
    };

    if let Some(smart) = &dev.smart {
        let is_healthy = dev.health_state == rsetup_core::HealthState::Healthy;
        let build_telemetry_line = |endurance_label: &'static str| -> Line<'static> {
            let mut spans = vec![
                Span::styled(status_label, Style::default().fg(MUTED)),
                Span::styled(
                    status_text,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            if !is_healthy && !smart.warning_flags.is_empty() {
                spans.push(Span::styled(
                    format!(" ({})", smart.warning_flags.join(", ")),
                    Style::default().fg(CORAL),
                ));
            }
            spans.extend(vec![
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(temp_label, Style::default().fg(MUTED)),
                Span::styled(
                    format!("{:.1} °C", smart.temperature_c),
                    Style::default().fg(BONE),
                ),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(endurance_label, Style::default().fg(MUTED)),
                Span::styled(
                    format!("{}%", smart.percentage_used),
                    Style::default().fg(BONE),
                ),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(spare_label, Style::default().fg(MUTED)),
                Span::styled(
                    format!("{}%", smart.available_spare_percent),
                    Style::default().fg(BONE),
                ),
            ]);
            Line::from(spans)
        };
        let full_line = build_telemetry_line(endurance_label);
        let line2 = if full_line.width() > inner_width {
            build_telemetry_line(short_endurance_label)
        } else {
            full_line
        };

        // Line 3: Data read & written plus the available-spare threshold
        let io_label = if app.locale.is_zh() {
            "读写: "
        } else {
            "I/O: "
        };
        let threshold_label = if app.locale.is_zh() {
            "阈值"
        } else {
            "threshold"
        };
        let read_str = crate::format_bytes(smart.data_read_bytes);
        let write_str = crate::format_bytes(smart.data_written_bytes);

        let line3 = Line::from(vec![
            Span::styled(io_label, Style::default().fg(MUTED)),
            Span::styled(
                format!("Read {read_str} / Written {write_str}"),
                Style::default().fg(BONE),
            ),
            Span::styled(" · ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{threshold_label} {}%", smart.spare_threshold_percent),
                Style::default().fg(BONE),
            ),
        ]);

        // The throughput line carries no state worth more than the health
        // state above it, so it is what a short card gives up first.
        let candidates = vec![
            (2, vec![line1.clone(), line2.clone(), line3]),
            (1, vec![line1, line2]),
        ];
        fit_block(&candidates, inner_width, max_rows)
    } else {
        let reason = match &dev.telemetry.error {
            Some(err) => match err.kind {
                rsetup_core::TelemetryErrorKind::PermissionDenied => {
                    if app.locale.is_zh() {
                        format!("权限不足 (code {})", err.code.unwrap_or(13))
                    } else {
                        format!("Permission denied (code {})", err.code.unwrap_or(13))
                    }
                }
                rsetup_core::TelemetryErrorKind::NvmeStatus => {
                    if app.locale.is_zh() {
                        format!("协议状态码 {}", err.code.unwrap_or(0))
                    } else {
                        format!("Protocol status {}", err.code.unwrap_or(0))
                    }
                }
                rsetup_core::TelemetryErrorKind::Io => {
                    if let Some(code) = err.code {
                        if app.locale.is_zh() {
                            format!("I/O 错误 (code {})", code)
                        } else {
                            format!("I/O error (code {})", code)
                        }
                    } else if app.locale.is_zh() {
                        "I/O 错误".to_string()
                    } else {
                        "I/O error".to_string()
                    }
                }
            },
            None => app.locale.text("storage_telemetry_unavailable").to_string(),
        };

        let line2 = Line::from(vec![
            Span::styled(status_label, Style::default().fg(MUTED)),
            Span::styled(
                status_text,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(MUTED)),
            Span::styled(
                format!(
                    "{}: {}",
                    app.locale.text("storage_telemetry_unavailable"),
                    reason
                ),
                Style::default().fg(MUTED),
            ),
        ]);

        fit_block(&[(1, vec![line1, line2])], inner_width, max_rows)
    }
}

/// Display width of a plain string, measured exactly the way the paragraph
/// that renders it wraps it.
fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// Truncate `text` to at most `max` display columns, marking the cut with a
/// single ellipsis. Returns an empty string when even the ellipsis would not
/// fit, so a caller never renders a lone `…`.
///
/// The cut advances one user-perceived character at a time, taken straight from
/// ratatui's segmentation: `Span::styled_graphemes` runs the real
/// `UnicodeSegmentation` (extended grapheme clusters) that the widget itself
/// lays its rows out with, so a combining mark, a variation selector or a zero
/// width joiner travels with the glyph it hangs off.  A cluster's width is not
/// the sum of the widths of the characters in it, so each *complete* candidate
/// — prefix + cluster + ellipsis — is measured rather than a running per-
/// character sum; cutting mid-cluster would both miscount and strand a mark that
/// cannot render on its own.  A hand-rolled block list (the first fix) still left
/// an orphan joiner after `A`+ZWJ+ZWJ and split Indic conjuncts off a matra (mmc
/// second review P3-4); using the widget's own segmentation is why it can no
/// longer.  `test_ellipsize_cuts_on_a_real_grapheme_boundary` pins the cut on
/// that same public API, so the helper cannot certify itself.
fn ellipsize(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_owned();
    }
    if max < 2 {
        return String::new();
    }
    let mut taken = String::new();
    // The temporary `Span` lives for the whole loop, so each symbol borrow stays
    // valid while it is measured and appended.
    for grapheme in Span::raw(text).styled_graphemes(Style::default()) {
        let cluster = grapheme.symbol;
        if text_width(&format!("{taken}{cluster}…")) > max {
            break;
        }
        taken.push_str(cluster);
    }
    taken.push('…');
    taken
}

/// Join the non-empty identity fields of a device with the card separator.
///
/// A device whose sysfs read failed carries empty `model`/`manufacturer`
/// fields; joining them verbatim would print dangling ` · ` separators.
fn join_identity(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Identity line of an MMC/SD card: `[type] block · model · vendor · capacity`.
///
/// One column too wide and the paragraph wraps the capacity unit onto a
/// second row (mmc report §5 T1), so the identity gives fields up before
/// that can happen: the `/dev/` prefix first, then the vendor, then the
/// model, and only the remainder gets ellipsized. The type badge and the
/// capacity are never given up.
fn mmc_identity_line(
    type_label: &str,
    dev: &rsetup_core::MmcDevice,
    inner_width: usize,
) -> Line<'static> {
    let badge = format!("[{type_label}] ");
    let capacity = crate::format_bytes(dev.total_bytes);
    let short_block = dev
        .block_path
        .strip_prefix("/dev/")
        .unwrap_or(dev.block_path.as_str());
    let detail_full = join_identity(&[&dev.block_path, &dev.model]);
    let detail_short = join_identity(&[short_block, &dev.model]);
    let vendor = dev.manufacturer.trim();
    let vendor = (!vendor.is_empty()).then(|| vendor.to_string());
    let block_only = join_identity(&[short_block]);

    // The candidate is measured as the complete row it produces.  The width of
    // a joined string is not the sum of the widths of its parts as soon as one
    // of them carries combining marks or variation selectors.
    let width_of = |detail: &str, vendor: Option<&str>| -> usize {
        let parts = vec![detail, vendor.unwrap_or(""), &capacity]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        text_width(&format!("{badge}{}", join_identity(&parts)))
    };

    let candidates = [
        (detail_full, vendor.clone()),
        (detail_short.clone(), vendor.clone()),
        (detail_short.clone(), None),
        (block_only, None),
    ];
    let chosen = candidates
        .into_iter()
        .find(|(detail, vendor)| width_of(detail, vendor.as_deref()) <= inner_width);
    let (detail, vendor) = chosen.unwrap_or_else(|| {
        // Even the leanest identity is too wide: ellipsize it so the badge
        // and the capacity keep their one row. The candidate that has to fit
        // is the complete row, not the identity on its own, so the room is
        // taken back until the row the card renders really fits.
        let mut room = inner_width;
        let detail = loop {
            let candidate = ellipsize(&detail_short, room);
            if room == 0 || text_width(&format!("{badge}{candidate} · {capacity}")) <= inner_width
            {
                break candidate;
            }
            room -= 1;
        };
        (detail, None)
    });

    let mut spans = vec![Span::styled(
        badge,
        Style::default().fg(BONE).add_modifier(Modifier::BOLD),
    )];
    if !detail.is_empty() {
        spans.push(Span::styled(
            format!("{detail} · "),
            Style::default().fg(BONE),
        ));
    }
    if let Some(vendor) = vendor {
        spans.push(Span::styled(
            format!("{vendor} · "),
            Style::default().fg(MUTED),
        ));
    }
    spans.push(Span::styled(capacity, Style::default().fg(AMBER)));
    Line::from(spans)
}

/// One `Health: Warning`-style segment of the MMC health block.
type Segment = Vec<Span<'static>>;

/// Reads `[Health: Warning] [SLC: 10%]` as `Health: Warning · SLC: 10%`.
fn join_segments(segments: &[Segment]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        }
        spans.extend(segment.iter().cloned());
    }
    Line::from(spans)
}

/// Candidate health blocks of one MMC device, each with the number of values
/// it keeps, richest first.
///
/// The health state is never given up. While the pre-EOL byte reports a
/// warning or an urgent state its value is not offered away either: the
/// life-time estimates are the least critical of the four values, so they are
/// what the card trades first (mmc review P2 — the card has to degrade by
/// decision, never by clipping the tail of a row).
fn mmc_health_candidates(app: &App, dev: &rsetup_core::MmcDevice) -> Vec<(u8, Vec<Line<'static>>)> {
    let health_label = format!("{}: ", app.locale.text("storage_health"));
    let (health_text, health_color) = match dev.health_state {
        rsetup_core::HealthState::Healthy => (app.locale.text("storage_healthy"), SIGNAL),
        rsetup_core::HealthState::Warning => (app.locale.text("storage_warning"), AMBER),
        rsetup_core::HealthState::Critical => (app.locale.text("storage_critical"), CORAL),
        rsetup_core::HealthState::Unknown => (app.locale.text("storage_unknown"), MUTED),
    };
    let health_segment: Segment = vec![
        Span::styled(health_label, Style::default().fg(MUTED)),
        Span::styled(
            health_text,
            Style::default()
                .fg(health_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut candidates: Vec<(u8, Vec<Line<'static>>)> = Vec::new();
    match dev.telemetry.state {
        rsetup_core::TelemetryReadState::Unavailable => {
            let err_reason = app
                .locale
                .storage_telemetry_error(dev.telemetry.error.as_ref());
            let mut spans = health_segment;
            spans.extend([
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(
                    format!(
                        "{}: {}",
                        app.locale.text("storage_telemetry_unavailable"),
                        err_reason
                    ),
                    Style::default().fg(MUTED),
                ),
            ]);
            candidates.push((2, vec![Line::from(spans)]));
        }
        rsetup_core::TelemetryReadState::Unsupported => {
            let mut spans = health_segment;
            spans.extend([
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(
                    app.locale.text("storage_telemetry_unsupported"),
                    Style::default().fg(MUTED),
                ),
            ]);
            candidates.push((2, vec![Line::from(spans)]));
        }
        rsetup_core::TelemetryReadState::Available => {
            let format_life = |value: Option<u8>| -> String {
                match value {
                    Some(100) => "90–100%".to_string(),
                    Some(101) => ">100%".to_string(),
                    Some(p) => format!("{p}%"),
                    None => app.locale.text("storage_na").to_string(),
                }
            };
            let life_a = format_life(dev.health.life_time_est_a_percent);
            let life_b = format_life(dev.health.life_time_est_b_percent);
            let eol = match dev.health.pre_eol_info {
                2 => app.locale.text("storage_eol_warning"),
                3 => app.locale.text("storage_eol_urgent"),
                1 => app.locale.text("storage_eol_normal"),
                _ => app.locale.text("storage_eol_undefined"),
            };
            let eol_carries_news = matches!(dev.health.pre_eol_info, 2 | 3);
            let life_segment = |key: &str, value: String| -> Segment {
                vec![
                    Span::styled(
                        format!("{}: ", app.locale.text(key)),
                        Style::default().fg(MUTED),
                    ),
                    Span::styled(value, Style::default().fg(BONE)),
                ]
            };
            let eol_segment = |value: &str| -> Segment {
                vec![
                    Span::styled(
                        format!("{}: ", app.locale.text("storage_pre_eol")),
                        Style::default().fg(MUTED),
                    ),
                    Span::styled(value.to_string(), Style::default().fg(BONE)),
                ]
            };
            let all = |short: bool| -> Vec<Segment> {
                let (life_a_key, life_b_key) = if short {
                    ("storage_life_a_short", "storage_life_b_short")
                } else {
                    ("storage_life_a", "storage_life_b")
                };
                vec![
                    health_segment.clone(),
                    life_segment(life_a_key, life_a.clone()),
                    life_segment(life_b_key, life_b.clone()),
                    eol_segment(eol),
                ]
            };
            let without_eol = |short: bool| -> Vec<Segment> {
                let (life_a_key, life_b_key) = if short {
                    ("storage_life_a_short", "storage_life_b_short")
                } else {
                    ("storage_life_a", "storage_life_b")
                };
                vec![
                    health_segment.clone(),
                    life_segment(life_a_key, life_a.clone()),
                    life_segment(life_b_key, life_b.clone()),
                ]
            };
            // Effective values counted by field, so a Normal/undefined pre-EOL
            // never wins a row it says nothing about: health (1) plus the two
            // life-time estimates (2); the pre-EOL adds a value only while it
            // reports a warning or an urgent state.  `fit_block` then takes the
            // fewest rows among the candidates that keep the same values, so a
            // roomy card is not padded to a second row to fit a "Normal" it
            // does not need to say (mmc second review P3-5: `pre_eol=1`, life
            // 100 %, 59 columns picked two rows including Normal while a single
            // row held the same three values).
            let all_values = 3 + u8::from(eol_carries_news);
            if eol_carries_news {
                // Everything the probe reported, on one row (full then short labels).
                for segments in [all(false), all(true)] {
                    candidates.push((all_values, vec![join_segments(&segments)]));
                }
                // The same values with the pre-EOL segment on a row of its own:
                // a deliberate break beats a ragged wrap of the tail.
                for short in [false, true] {
                    candidates.push((
                        all_values,
                        vec![
                            join_segments(&without_eol(short)),
                            join_segments(&[eol_segment(eol)]),
                        ],
                    ));
                }
                // Under pressure the life-time estimates give way; a warning or
                // an urgent state is never given up (mmc review P2).
                let danger = vec![health_segment.clone(), eol_segment(eol)];
                candidates.push((2, vec![join_segments(&danger)]));
                candidates.push((
                    2,
                    vec![
                        join_segments(std::slice::from_ref(&health_segment)),
                        join_segments(&[eol_segment(eol)]),
                    ],
                ));
            } else {
                // A Normal/undefined pre-EOL carries no information the health
                // state does not already give, so it is worth no value of its
                // own: with or without it the block scores the same (health plus
                // the two life estimates).  `fit_block` then keeps as much of the
                // card's wording as it gets for free — the full and the short
                // spelling *with* the pre-EOL come first, so they win whenever
                // they cost no extra row (the compact Chinese line and the short
                // English line at 61 columns keep "Normal"); the same block
                // *without* the pre-EOL ties on value but is narrower, so the
                // moment the pre-EOL would buy a row for itself it is dropped
                // rather than padding the card to a second row for a word that
                // says nothing (mmc second review P3-5: `pre_eol=1`, life 100 %,
                // 59 columns took two rows including Normal while one row held
                // the same three values).  Health alone is the leanest fallback.
                for segments in [all(false), all(true), without_eol(false), without_eol(true)] {
                    candidates.push((all_values, vec![join_segments(&segments)]));
                }
                candidates.push((
                    1,
                    vec![join_segments(std::slice::from_ref(&health_segment))],
                ));
            }
        }
    }
    candidates
}

/// The richest candidate block that fits in `max_rows` rows.  Among the
/// candidates that keep the same values the one needing the fewest rows wins,
/// so a card is never padded out with rows it does not need.
fn fit_block(
    candidates: &[(u8, Vec<Line<'static>>)],
    inner_width: usize,
    max_rows: usize,
) -> Option<Vec<Line<'static>>> {
    let rows_of = |block: &Vec<Line<'static>>| -> usize {
        match wrapped_rows(block, inner_width, max_rows) {
            RowFit::Fits(n) => n,
            RowFit::Overflow => usize::MAX,
        }
    };
    let richest = candidates
        .iter()
        .filter(|(_, block)| rows_of(block) != usize::MAX)
        .map(|(values, _)| *values)
        .max()?;
    candidates
        .iter()
        .filter(|(values, _)| *values == richest)
        .min_by_key(|(_, block)| rows_of(block))
        .map(|(_, block)| block.clone())
}

/// The rows of one MMC device, fitted into `max_rows`.  `None` when the
/// identity and a health state cannot both be shown: the caller then counts
/// the device as not rendered instead of showing half of it.
fn mmc_device_lines(
    app: &App,
    dev: &rsetup_core::MmcDevice,
    inner_width: usize,
    max_rows: usize,
) -> Option<Vec<Line<'static>>> {
    let type_label = if dev.card_type == "MMC" {
        app.locale.text("storage_emmc")
    } else {
        app.locale.text("storage_sd")
    };
    let identity = mmc_identity_line(type_label, dev, inner_width);
    let identity_rows = match wrapped_rows(std::slice::from_ref(&identity), inner_width, max_rows) {
        RowFit::Fits(n) => n,
        RowFit::Overflow => return None,
    };
    let block = fit_block(
        &mmc_health_candidates(app, dev),
        inner_width,
        max_rows.checked_sub(identity_rows)?,
    )?;
    let mut lines = vec![identity];
    lines.extend(block);
    Some(lines)
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

fn instrument(title: &str) -> Block<'_> {
    Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(76, 82, 68)))
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
    fn test_render_storage_summary_demo() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let app = App::new(controller, Locale::ZhCn).expect("init app");
        let backend = ratatui::backend::TestBackend::new(80, 25);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 80, 6);
                render_storage_summary(frame, &app, area);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        let norm_text = text.split_whitespace().collect::<Vec<_>>().join("");
        assert!(text.contains("Radxa M.2 NVMe SSD 512GB") || text.contains("nvme0"));
        assert!(text.contains("38.5") || text.contains("温度"));
        // Available spare and its threshold must both be on screen, not clipped.
        assert!(
            norm_text.contains("备用:100%"),
            "Expected available spare '备用: 100%' in buffer, got: {text:?}"
        );
        assert!(
            norm_text.contains("阈值10%"),
            "Expected spare threshold '阈值 10%' in buffer, got: {text:?}"
        );
    }

    #[test]
    fn test_render_storage_summary_fits_available_spare() {
        // Real device geometry: left panel is 61% of a 100-column terminal,
        // so the storage box has 61 display columns (59 inner columns).
        for (locale, spare_label, threshold_label) in [
            (Locale::ZhCn, "备用", "阈值"),
            (Locale::En, "Spare", "threshold"),
        ] {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, locale).expect("init app");
            let mut dev = app.nvme_status.devices[0].clone();
            // Mirror the real Rock 5B device: 100% spare against a 1% threshold.
            if let Some(smart) = &mut dev.smart {
                smart.available_spare_percent = 100;
                smart.spare_threshold_percent = 1;
            }
            app.nvme_status.devices = vec![dev];
            // This case is about the *width* an NVMe block needs for its
            // available-spare threshold (59 inner columns), so it is measured on
            // the NVMe alone.  Leaving the demo MMC devices in place would turn
            // it into the multi-device accounting case the second review opened
            // (a card too short for every device must leave a row for the
            // "more devices" hint instead of letting the first device eat all of
            // them), where an NVMe that degrades gives up its I/O row — spare
            // threshold included (mmc second review P2-2 / P3-6).  That
            // accounting is pinned by `test_second_warning_device_is_accounted_for`.
            app.mmc_status = rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: None,
            };

            let backend = ratatui::backend::TestBackend::new(61, 6);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render_storage_summary(frame, &app, Rect::new(0, 0, 61, 6));
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
                norm_text.contains(&format!("{spare_label}:100%")),
                "Expected available spare '{spare_label}: 100%' inside 59 columns, got: {raw_text:?}"
            );
            assert!(
                raw_text.contains("1%"),
                "Expected spare threshold '1%' (label '{threshold_label}') in buffer, got: {raw_text:?}"
            );
            assert!(
                norm_text.contains(threshold_label),
                "Expected threshold label '{threshold_label}' in buffer, got: {raw_text:?}"
            );
        }
    }

    #[test]
    fn test_render_storage_summary_uses_full_endurance_label_when_wide() {
        let render_norm = |locale: Locale, width: u16| -> String {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let app = App::new(controller, locale).expect("init app");
            let backend = ratatui::backend::TestBackend::new(width, 6);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render_storage_summary(frame, &app, Rect::new(0, 0, width, 6));
                })
                .expect("draw");
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("")
        };

        // 59 inner columns cannot hold "Used Endurance:" next to the mandatory
        // available-spare value, so the short spelling is used there.
        let narrow = render_norm(Locale::En, 61);
        assert!(
            narrow.contains("Used:2%"),
            "Expected compact endurance label at 59 columns, got: {narrow}"
        );
        assert!(
            narrow.contains("Spare:100%"),
            "Expected available spare at 59 columns, got: {narrow}"
        );

        // With room to spare the full label is kept.
        let wide = render_norm(Locale::En, 90);
        assert!(
            wide.contains("UsedEndurance:2%"),
            "Expected full 'Used Endurance' label on a wide panel, got: {wide}"
        );
    }

    /// The Rock 3A device of mmc report §5 T1: its identity line is 61 display
    /// columns wide, one more than the 59 columns the storage card gets on a
    /// 100-column terminal, which used to push the capacity unit onto a second
    /// row.
    fn rock3a_mmc_app(locale: Locale) -> App {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, locale).expect("init app");
        let mut dev = app.mmc_status.devices[0].clone();
        dev.block_path = "/dev/mmcblk0".into();
        dev.card_type = "MMC".into();
        dev.model = "064G02".into();
        dev.manufacturer = "Toshiba (0x000011)".into();
        dev.total_bytes = 62_537_072_640; // 58.24 GiB
        app.mmc_status.devices = vec![dev];
        app.nvme_status.devices.clear();
        app.nvme_status.initialized = false;
        app
    }

    /// Render the storage card alone and return its rows as plain text.
    fn render_storage_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                render_storage_summary(frame, app, Rect::new(0, 0, width, height));
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn test_render_storage_summary_keeps_mmc_capacity_on_one_row() {
        for locale in [Locale::En, Locale::ZhCn] {
            let app = rock3a_mmc_app(locale);
            let rows = render_storage_rows(&app, 61, 8);
            let screen = rows.join("\n");
            let row = rows
                .iter()
                .find(|row| row.contains("58.24 GiB"))
                .unwrap_or_else(|| {
                    panic!("{locale:?}: capacity fell off the device row:\n{screen}")
                });
            assert!(
                row.contains("[eMMC] mmcblk0"),
                "{locale:?}: the capacity must share its row with the device badge, got {row:?} in:\n{screen}"
            );
            assert!(
                !row.contains("/dev/"),
                "{locale:?}: the /dev/ prefix is the first field to give up, got {row:?}"
            );
        }
    }

    #[test]
    fn test_render_storage_summary_mmc_identity_degrades_by_width() {
        // A wide card keeps everything the probe reported.
        let wide = render_storage_rows(&rock3a_mmc_app(Locale::En), 100, 8).join("\n");
        assert!(
            wide.contains("[eMMC] /dev/mmcblk0 · 064G02 · Toshiba (0x000011) · 58.24 GiB"),
            "Expected the full identity line on a wide card, got:\n{wide}"
        );

        // 59 inner columns give up the /dev/ prefix but keep the vendor name.
        let narrow = render_storage_rows(&rock3a_mmc_app(Locale::En), 61, 8).join("\n");
        assert!(
            narrow.contains("[eMMC] mmcblk0 · 064G02 · Toshiba (0x000011) · 58.24 GiB"),
            "Expected the /dev/-less identity line at 59 columns, got:\n{narrow}"
        );
    }

    #[test]
    fn test_render_storage_summary_mmc_telemetry_line_fits_narrow_card() {
        // The Chinese spelling is short enough to keep the whole line, the
        // pre-EOL segment included.
        let zh = render_storage_rows(&rock3a_mmc_app(Locale::ZhCn), 61, 8)
            .into_iter()
            // A double-width glyph leaves a blank cell behind in the buffer,
            // so compare the line without its padding.
            .map(|row| row.replace(' ', ""))
            .find(|row| row.contains("SLC寿命"))
            .expect("Chinese telemetry line");
        assert!(
            zh.contains("SLC寿命:10%") && zh.contains("预警:正常"),
            "Expected the full Chinese telemetry line, got {zh:?}"
        );

        // English needs the short life-time spelling to keep pre-EOL on the
        // same row.
        let en = render_storage_rows(&rock3a_mmc_app(Locale::En), 61, 8)
            .into_iter()
            .find(|row| row.contains("SLC"))
            .expect("English telemetry line");
        assert!(
            en.contains("SLC: 10%") && en.contains("Pre-EOL: Normal"),
            "Expected the compact English telemetry line, got {en:?}"
        );
    }

    #[test]
    fn test_mmc_identity_line_fits_every_inner_width() {
        let app = rock3a_mmc_app(Locale::En);
        let dev = &app.mmc_status.devices[0];
        // The badge and the capacity alone take 16 columns; nothing narrower
        // than that can be rendered, so the ladder only answers for the card
        // widths a real terminal can offer.
        for inner_width in 16..=61usize {
            let line = mmc_identity_line("eMMC", dev, inner_width);
            assert!(
                line.width() <= inner_width,
                "identity line is {} columns wide for a {inner_width}-column card",
                line.width()
            );
        }
        // Below that floor the capacity survives and only the identity goes.
        for inner_width in 1..16usize {
            let line = mmc_identity_line("eMMC", dev, inner_width);
            assert_eq!(
                line.spans.last().expect("capacity span").content.as_ref(),
                "58.24 GiB",
                "the capacity must survive a {inner_width}-column card"
            );
        }
    }

    #[test]
    fn test_mmc_identity_line_skips_empty_identity_fields() {
        // A device whose sysfs read failed reports no model and no vendor; its
        // line must not carry dangling separators.
        let mut app = rock3a_mmc_app(Locale::En);
        {
            let dev = &mut app.mmc_status.devices[0];
            dev.model.clear();
            dev.manufacturer.clear();
        }
        let line = mmc_identity_line("SD Card", &app.mmc_status.devices[0], 59);
        assert_eq!(line_text(&line), "[SD Card] /dev/mmcblk0 · 58.24 GiB");
    }

    // ---- P2: the pre-EOL warning must never be clipped by the row budget ----

    /// Rows the paragraph widget really consumes for `lines` at `inner_width`.
    /// This is the oracle the height budget has to agree with: it is produced
    /// by rendering the very same `Paragraph` the card renders.
    fn measured_rows(lines: &[Line<'static>], inner_width: usize) -> usize {
        let width = inner_width.max(1) as u16;
        let height = 64u16;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(lines.to_vec()).wrap(Wrap { trim: true }),
                    Rect::new(0, 0, width, height),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .rfind(|row| (0..width).any(|col| buffer[(col, *row)].symbol() != " "))
            .map(|last| last as usize + 1)
            .unwrap_or(0)
    }

    /// The Rock 3A eMMC of the implementation review, this time wearing the
    /// warning it really reports: pre-EOL `2`, both life-time estimates at 10%
    /// and the derived `Warning` health state.
    fn rock3a_warning_app(locale: Locale, pre_eol: u8, life: Option<u8>) -> App {
        let mut app = rock3a_mmc_app(locale);
        let dev = &mut app.mmc_status.devices[0];
        dev.health.pre_eol_info = pre_eol;
        dev.health.life_time_est_a_percent = life;
        dev.health.life_time_est_b_percent = life;
        dev.health_state = if pre_eol >= 2 {
            rsetup_core::HealthState::Warning
        } else {
            rsetup_core::HealthState::Healthy
        };
        app
    }

    /// The pre-EOL wording the card has to show for `pre_eol`.
    fn eol_text(locale: Locale, pre_eol: u8) -> &'static str {
        let (zh, en) = match pre_eol {
            2 => ("预警(80%)", "Warning(80%)"),
            3 => ("紧急", "Urgent"),
            _ => ("正常", "Normal"),
        };
        if locale.is_zh() { zh } else { en }
    }

    fn render_mission_rows(app: &App, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                render_mission(frame, app, Rect::new(0, 0, width, height));
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|row| {
                (0..width)
                    .map(|col| buffer[(col, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn test_wrapped_rows_matches_the_paragraph_word_wrap() {
        // The height budget has to count the rows the paragraph really uses.
        // An estimate by `ceil(width / inner_width)` is one row short as soon
        // as a word cannot start on the tail of a row, and the card then clips
        // its last row instead of growing.
        for locale in [Locale::En, Locale::ZhCn] {
            for pre_eol in [1u8, 2, 3] {
                for life in [None, Some(10), Some(100), Some(101)] {
                    let app = rock3a_warning_app(locale, pre_eol, life);
                    let dev = &app.mmc_status.devices[0];
                    for inner_width in 16..=61usize {
                        let lines = mmc_device_lines(&app, dev, inner_width, usize::MAX)
                            .expect("the card offers the eMMC a row of its own");
                        let budget = match wrapped_rows(&lines, inner_width, 64) {
                            RowFit::Fits(n) => n,
                            RowFit::Overflow => panic!(
                                "{locale:?} pre_eol={pre_eol} life={life:?} at {inner_width}: a \
                                 device block should not need 64 rows"
                            ),
                        };
                        let measured = measured_rows(&lines, inner_width);
                        assert_eq!(
                            budget,
                            measured,
                            "{locale:?} pre_eol={pre_eol} life={life:?} at {inner_width} columns: \
                             budget {budget} rows, paragraph used {measured}\nlines: {:?}",
                            lines.iter().map(line_text).collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
    }

    /// The screen without its borders and padding, so a value that the word
    /// wrap moved onto the next row is still found.
    fn flatten_screen(rows: &[String]) -> String {
        rows.iter()
            .flat_map(|row| row.chars())
            .filter(|c| {
                !c.is_whitespace() && !matches!(c, '│' | '─' | '┌' | '┐' | '└' | '┘' | '├' | '┤')
            })
            .collect()
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
    fn test_render_storage_summary_warning_state() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::ZhCn).expect("init app");
        let mut dev = app.nvme_status.devices[0].clone();
        if let Some(smart) = &mut dev.smart {
            smart.critical_warning = 0;
            smart.warning_flags = vec![
                "spare_below_threshold".into(),
                "temperature_exceeded".into(),
            ];
        }
        dev.health_state = rsetup_core::HealthState::Warning;
        app.nvme_status.devices = vec![dev];

        let backend = ratatui::backend::TestBackend::new(100, 10);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 100, 6);
                render_storage_summary(frame, &app, area);
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
            norm_text.contains("告警"),
            "Expected '告警' in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("spare_below_threshold"),
            "Expected 'spare_below_threshold' in buffer"
        );
        assert!(
            raw_text.contains("temperature_exceeded"),
            "Expected 'temperature_exceeded' in buffer"
        );

        // Also test English locale
        app.locale = Locale::En;
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 100, 6);
                render_storage_summary(frame, &app, area);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text_en = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text_en.contains("Warning"),
            "Expected 'Warning' in buffer, got: {text_en}"
        );
        assert!(
            text_en.contains("spare_below_threshold"),
            "Expected 'spare_below_threshold' in buffer"
        );
    }

    #[test]
    fn test_render_storage_summary_mmc_only() {
        // MMC devices only: NVMe uninitialized, no not-detected notice shown.
        // The full TUI at 100x20 clamps the card to its minimum height, so the
        // card is rendered directly on a large area to check its contents.
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::ZhCn).expect("init app");
        app.nvme_status = rsetup_core::NvmeStatus {
            initialized: false,
            devices: vec![],
            message: None,
        };
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
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
            raw_text.contains("FE4MB4"),
            "Expected eMMC model in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("/dev/mmcblk0"),
            "Expected eMMC path in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("SC64G"),
            "Expected SD model in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("/dev/mmcblk1"),
            "Expected SD path in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("10%"),
            "Expected eMMC life value in buffer, got: {raw_text}"
        );
        assert!(
            norm_text.contains("存储状态"),
            "Expected card title in buffer, got: {raw_text}"
        );

        // English locale: type labels and the N/A life values for the SD card.
        app.locale = Locale::En;
        terminal
            .draw(|frame| {
                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let raw_en = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            raw_en.contains("eMMC"),
            "Expected 'eMMC' label in buffer, got: {raw_en}"
        );
        assert!(
            raw_en.contains("SD Card"),
            "Expected 'SD Card' label in buffer, got: {raw_en}"
        );
        assert!(
            raw_en.contains("Health telemetry unsupported"),
            "Expected 'Health telemetry unsupported' for SD card, got: {raw_en}"
        );
    }

    #[test]
    fn test_render_storage_summary_multi_device_all_visible_when_tall() {
        // On a tall terminal the card has room for all demo devices:
        // 1 NVMe + 2 MMC must all be visible without truncation.
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::En).expect("init app");
        let backend = ratatui::backend::TestBackend::new(100, 40);
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
            raw_text.contains("nvme0"),
            "Expected NVMe device in buffer, got: {raw_text}"
        );
        // The card is 61 columns wide in the full layout, so both MMC lines
        // give up the /dev/ prefix instead of wrapping the capacity (§5 T1).
        assert!(
            raw_text.contains("mmcblk0"),
            "Expected eMMC device in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("mmcblk1"),
            "Expected SD device in buffer, got: {raw_text}"
        );
    }

    #[test]
    fn test_render_storage_summary_empty() {
        for locale in [Locale::ZhCn, Locale::En] {
            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
            let mut app = App::new(controller, locale).expect("init app");
            app.nvme_status = rsetup_core::NvmeStatus {
                initialized: false,
                devices: vec![],
                message: None,
            };
            app.mmc_status = rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: None,
            };
            let backend = ratatui::backend::TestBackend::new(100, 20);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render_storage_summary(frame, &app, Rect::new(0, 0, 80, 10));
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let raw_text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
            if locale == Locale::ZhCn {
                assert!(
                    norm_text.contains("未检测到NVMe或MMC存储设备"),
                    "Expected Chinese no-storage message in buffer, got: {raw_text}"
                );
            } else {
                assert!(
                    raw_text.contains("No NVMe or MMC storage devices detected."),
                    "Expected English no-storage message in buffer, got: {raw_text}"
                );
            }
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

    #[test]
    fn mmc_unavailable_in_tui_lines_and_render() {
        use rsetup_core::{
            HealthState, MmcHealth, TelemetryError, TelemetryErrorKind, TelemetryReadState,
            TelemetryStatus,
        };

        for (locale, reason) in [
            (Locale::En, "Permission denied (13)"),
            (Locale::ZhCn, "权限不足 (13)"),
        ] {
            let mut app = App::new(
                Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
                locale,
            )
            .expect("init app");
            assert!(!app.mmc_status.devices.is_empty());
            {
                let dev = &mut app.mmc_status.devices[0];
                dev.health = MmcHealth::default();
                dev.health_state = HealthState::Unknown;
                dev.telemetry = TelemetryStatus {
                    state: TelemetryReadState::Unavailable,
                    error: Some(TelemetryError {
                        kind: TelemetryErrorKind::PermissionDenied,
                        code: Some(13),
                    }),
                };
            }

            // Test lines directly
            let dev = &app.mmc_status.devices[0];
            let lines = mmc_device_lines(&app, dev, usize::MAX, usize::MAX)
                .expect("an unreadable card still reports its state");
            let combined_lines_text = lines
                .iter()
                .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
                .collect::<Vec<_>>()
                .join(" ");

            assert!(
                combined_lines_text.contains(reason),
                "Expected '{reason}' in lines, got: {combined_lines_text}"
            );
            assert!(!combined_lines_text.contains("SLC"));
            assert!(!combined_lines_text.contains("MLC"));

            // Test render_storage_summary TestBackend
            let backend = ratatui::backend::TestBackend::new(120, 30);
            let mut terminal = Terminal::new(backend).expect("init test terminal");
            terminal
                .draw(|frame| {
                    render_storage_summary(frame, &app, Rect::new(0, 0, 120, 30));
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            let raw_text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
            let norm_reason = reason.split_whitespace().collect::<Vec<_>>().join("");
            assert!(
                norm_text.contains(&norm_reason),
                "Expected '{reason}' (norm '{norm_reason}') in rendered buffer, got:\n{raw_text}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[derive(Clone)]
    struct EnumerationFixture(
        std::sync::Arc<std::sync::Mutex<(rsetup_core::StorageStatus, bool, bool)>>,
    );

    #[cfg(target_os = "linux")]
    impl rsetup_core::StorageReader for EnumerationFixture {
        fn nvme_status(&self) -> Result<rsetup_core::NvmeStatus, rsetup_core::HardwareError> {
            let state = self.0.lock().unwrap();
            if state.1 {
                Err(rsetup_core::HardwareError::Io(
                    "nvme-enumeration-eio".into(),
                ))
            } else {
                Ok(state.0.nvme.clone())
            }
        }
        fn mmc_status(&self) -> Result<rsetup_core::MmcStatus, rsetup_core::HardwareError> {
            let state = self.0.lock().unwrap();
            if state.2 {
                Err(rsetup_core::HardwareError::Io("mmc-enumeration-eio".into()))
            } else {
                Ok(state.0.mmc.clone())
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn storage_card_text(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_storage_summary(frame, app, Rect::new(0, 0, 120, 40));
            })
            .unwrap();
        let raw = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        raw.split_whitespace().collect::<Vec<_>>().join("")
    }

    #[cfg(target_os = "linux")]
    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join("")
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn storage_enumeration_errors_are_not_no_devices() {
        use std::sync::{Arc, Mutex};
        let demo = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)
            .storage_status()
            .unwrap();
        let shared = Arc::new(Mutex::new((demo, true, true)));
        let controller = Controller::with_storage_reader(
            ProbeMode::Live,
            ExecutionPolicy::DryRun,
            Arc::new(EnumerationFixture(shared)),
        );
        let app = App::new(controller, Locale::En).unwrap();
        let text = storage_card_text(&app);
        assert!(
            text.contains(&norm("NVMe device enumeration failed")),
            "{text}"
        );
        assert!(
            text.contains(&norm("MMC device enumeration failed")),
            "{text}"
        );
        assert!(
            !text.contains(&norm(app.locale.text("storage_not_detected"))),
            "{text}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn storage_enumeration_state_transitions_and_screens() {
        use std::sync::{Arc, Mutex};
        for locale in [Locale::En, Locale::ZhCn] {
            let demo = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)
                .storage_status()
                .unwrap();
            let nvme_model = demo.nvme.devices[0].model.clone();
            let mmc_model = demo.mmc.devices[0].model.clone();

            // 1. Double empty success -> shows storage_not_detected
            let mut empty_demo = demo.clone();
            empty_demo.nvme.devices.clear();
            empty_demo.mmc.devices.clear();
            let shared = Arc::new(Mutex::new((empty_demo, false, false)));
            let controller = Controller::with_storage_reader(
                ProbeMode::Live,
                ExecutionPolicy::DryRun,
                Arc::new(EnumerationFixture(shared.clone())),
            );
            let mut app = App::new(controller, locale).unwrap();
            let text = storage_card_text(&app);
            assert!(
                text.contains(&norm(app.locale.text("storage_not_detected"))),
                "Expected empty message in: {text}"
            );
            assert!(app.nvme_error.is_none());
            assert!(app.mmc_error.is_none());

            // 2. NVMe failure / MMC empty -> error line, no storage_not_detected
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = true; // nvme fail
                guard.2 = false; // mmc ok
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_some());
            assert!(app.mmc_error.is_none());
            let text = storage_card_text(&app);
            assert!(
                text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                !text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(
                !text.contains(&norm(app.locale.text("storage_not_detected"))),
                "{text}"
            );

            // 3. MMC failure / NVMe has devices -> MMC error line, NVMe model visible
            {
                let mut guard = shared.lock().unwrap();
                guard.0 = demo.clone();
                guard.1 = false; // nvme ok
                guard.2 = true; // mmc fail
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_none());
            assert!(app.mmc_error.is_some());
            let text = storage_card_text(&app);
            assert!(
                !text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(text.contains(&norm(&nvme_model)), "{text}");
            assert!(
                !text.contains(&norm(app.locale.text("storage_not_detected"))),
                "{text}"
            );

            // 4. NVMe failure / MMC has devices -> NVMe error line, MMC model visible
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = true; // nvme fail
                guard.2 = false; // mmc ok
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_some());
            assert!(app.mmc_error.is_none());
            let text = storage_card_text(&app);
            assert!(
                text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                !text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(text.contains(&norm(&mmc_model)), "{text}");
            assert!(!text.contains(&norm(&nvme_model)), "{text}");
            assert!(
                !text.contains(&norm(app.locale.text("storage_not_detected"))),
                "{text}"
            );

            // 5. Both fail -> both errors visible, neither model visible
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = true; // nvme fail
                guard.2 = true; // mmc fail
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_some());
            assert!(app.mmc_error.is_some());
            let text = storage_card_text(&app);
            assert!(
                text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(!text.contains(&norm(&nvme_model)), "{text}");
            assert!(!text.contains(&norm(&mmc_model)), "{text}");
            assert!(
                !text.contains(&norm(app.locale.text("storage_not_detected"))),
                "{text}"
            );

            // 6. Fail -> Refresh success (clears error, restores devices)
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = false;
                guard.2 = false;
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_none());
            assert!(app.mmc_error.is_none());
            let text = storage_card_text(&app);
            assert!(
                !text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                !text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(text.contains(&norm(&nvme_model)), "{text}");
            assert!(text.contains(&norm(&mmc_model)), "{text}");

            // 7. Success -> Refresh fail (clears old devices, shows error)
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = true;
                guard.2 = true;
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_some());
            assert!(app.mmc_error.is_some());
            let text = storage_card_text(&app);
            assert!(
                text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(!text.contains(&norm(&nvme_model)), "{text}");
            assert!(!text.contains(&norm(&mmc_model)), "{text}");

            // 8. Single disk telemetry unavailable but query Ok -> shows telemetry error, NOT enumeration error
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = false;
                guard.2 = false;
                guard.0 = demo.clone();
                guard.0.nvme.devices[0].smart = None;
                guard.0.nvme.devices[0].telemetry = rsetup_core::TelemetryStatus {
                    state: rsetup_core::TelemetryReadState::Unavailable,
                    error: Some(rsetup_core::TelemetryError {
                        kind: rsetup_core::TelemetryErrorKind::PermissionDenied,
                        code: Some(13),
                    }),
                };
            }
            app.refresh().unwrap();
            assert!(app.nvme_error.is_none());
            assert!(app.mmc_error.is_none());
            let text = storage_card_text(&app);
            assert!(
                !text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                "{text}"
            );
            assert!(
                !text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                "{text}"
            );
            assert!(text.contains(&norm(&nvme_model)), "{text}");
            assert!(
                text.contains(&norm(app.locale.text("storage_permission_denied"))),
                "{text}"
            );

            // 9. Full screen render test on 120x40 and 100x28 TestBackend
            {
                let mut guard = shared.lock().unwrap();
                guard.1 = true;
                guard.2 = true;
            }
            app.refresh().unwrap();

            for (w, h) in [(120, 40), (100, 28)] {
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                terminal
                    .draw(|frame| {
                        render(frame, &mut app);
                    })
                    .unwrap();
                let full_text = norm(
                    &terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>(),
                );
                assert!(
                    full_text.contains(&norm(app.locale.text("storage_nvme_enum_failed"))),
                    "Expected NVMe enum error on {w}x{h}, got:\n{full_text}"
                );
                assert!(
                    full_text.contains(&norm(app.locale.text("storage_mmc_enum_failed"))),
                    "Expected MMC enum error on {w}x{h}, got:\n{full_text}"
                );
            }
        }
    }

    #[test]
    fn test_mmc_warning_degrades_by_decision_not_by_clipping() {
        // The reviewed card: 31 columns of text.  With four rows of room the
        // card shows everything the probe reported; with three, the life-time
        // estimates give way and the pre-EOL warning keeps its row — a layout
        // that was picked, never a tail left dangling off the last row.
        let app = rock3a_warning_app(Locale::En, 2, Some(10));
        let content = |height: u16| {
            render_storage_rows(&app, 33, height)
                .into_iter()
                .skip(1)
                .take_while(|row| !row.starts_with('\u{2514}'))
                .map(|row| {
                    row.trim_start_matches('\u{2502}')
                        .trim_end_matches('\u{2502}')
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            content(6),
            [
                "[eMMC] mmcblk0 \u{b7} 58.24 GiB",
                "Health: Warning \u{b7} SLC Life: 10%",
                "\u{b7} MLC Life: 10% \u{b7} Pre-EOL:",
                "Warning(80%)",
            ],
            "the four-row card"
        );
        assert_eq!(
            content(5),
            [
                "[eMMC] mmcblk0 \u{b7} 58.24 GiB",
                "Health: Warning \u{b7} Pre-EOL:",
                "Warning(80%)",
            ],
            "the three-row card must trade the life-time estimates, not the warning"
        );
    }

    #[test]
    fn test_mmc_card_without_room_counts_the_device_out_instead_of_halving_it() {
        // Two content rows cannot hold the identity and the pre-EOL warning of
        // the same eMMC at 31 columns, and dropping the warning to make them
        // fit is exactly what the review objects to.  Then the honest answer
        // is "a device is not shown", and the number in the hint has to be the
        // truth; with the third row back, the warning returns.
        for locale in [Locale::En, Locale::ZhCn] {
            let app = rock3a_warning_app(locale, 2, Some(10));
            let label = app.locale.text("storage_more_devices").replace(' ', "");
            let mut outcomes = Vec::new();
            for height in [3u16, 4, 5] {
                let rows = render_storage_rows(&app, 33, height);
                let flat = flatten_screen(&rows);
                let value = eol_text(locale, 2).replace(' ', "");
                let hinted =
                    flat.contains(&format!("1{label}")) || flat.contains(&format!("{label}1"));
                assert!(
                    flat.contains(&value) || hinted,
                    "{locale:?} at a {height}-row card shows neither the warning nor \
                     the fact that the device is missing:\n{}",
                    rows.join("\n")
                );
                // Half a value is never painted: if the warning is not whole,
                // the device is not on the card at all.
                if !flat.contains(&value) {
                    assert!(
                        !flat.contains(&value[..value.len() - 1]),
                        "{locale:?} at a {height}-row card shows a fragment of the \
                         pre-EOL value:\n{}",
                        rows.join("\n")
                    );
                }
                outcomes.push(flat.contains(&value));
                assert!(hinted || flat.contains(&value));
            }
            // A single content row can never hold the identity and the warning
            // of the same device at 31 columns; from there on an extra row may
            // only ever add, never take away.
            assert!(
                !outcomes[0],
                "{locale:?}: one content row cannot hold a device and its warning"
            );
            assert!(
                !outcomes.windows(2).any(|pair| pair[0] && !pair[1]),
                "{locale:?}: a taller card showed less than a shorter one: {outcomes:?}"
            );
            assert!(
                outcomes.iter().any(|shown| *shown),
                "{locale:?}: the warning never came back, however tall the card got"
            );
            assert!(
                outcomes.iter().any(|shown| !shown),
                "{locale:?}: no card was ever too short, so the hint went untested"
            );
        }
    }

    // ---- P3: truncation cuts clusters, not code points ----

    #[test]
    fn test_ellipsize_never_splits_a_grapheme() {
        // The review's case: a heart with U+FE0F is one glyph, and the
        // per-character sum handed back five columns for a budget of three
        // (mmc review P3).  The cluster boundary is taken from the public
        // ratatui segmentation (`oracle_clusters`), not from the helper under
        // test, so the test cannot let the helper certify itself.  A Linux
        // block path is ASCII, so this pins the helper's robustness, not a
        // defect the Rock 3A probe can hit.
        let cases = [
            (
                "\u{2764}\u{fe0f}\u{2764}\u{fe0f}\u{2764}\u{fe0f}",
                "heart + VS16",
            ),
            (
                "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
                "zero width joined family",
            ),
            (
                "\u{1f1e8}\u{1f1f3}\u{1f1e9}\u{1f1ea}",
                "regional indicator flags",
            ),
            (
                "1\u{fe0f}\u{20e3}2\u{fe0f}\u{20e3}3\u{fe0f}\u{20e3}",
                "keycaps",
            ),
            ("e\u{301}mos\u{301}torage", "combining marks in words"),
            ("\u{1f44d}\u{1f3fd}\u{1f44d}\u{1f3fd}", "skin tone modifier"),
            ("\u{5b58}\u{50a8}\u{82af}\u{7247}", "double width text"),
            ("mmcblk0", "plain ascii"),
        ];
        for (text, label) in cases {
            assert_eq!(
                oracle_clusters(text).concat(),
                text,
                "{label}: the segmentation dropped or duplicated bytes"
            );
            let clusters = oracle_clusters(text);
            for max in 0..=text_width(text) + 2 {
                let cut = ellipsize(text, max);
                assert!(
                    text_width(&cut) <= max,
                    "{label}: {text:?} at {max} columns came back {} wide: {cut:?}",
                    text_width(&cut)
                );
                let kept = cut.strip_suffix('\u{2026}').unwrap_or(&cut);
                assert!(
                    text.starts_with(kept),
                    "{label}: {cut:?} is not a prefix of {text:?}"
                );
                // The kept text is a whole number of user-perceived characters:
                // its cluster list is a prefix of the full cluster list, so the
                // cut never lands inside a cluster (base and its mark/joiner
                // always travel together).
                let kept_clusters = oracle_clusters(kept);
                assert!(
                    clusters.starts_with(kept_clusters.as_slice()),
                    "{label}: {kept:?} ends inside a user-perceived character (oracle {clusters:?})"
                );
                // An empty result is the documented answer for a budget that
                // cannot hold even the ellipsis.
                if kept != text && !cut.is_empty() {
                    assert!(
                        cut.ends_with('\u{2026}'),
                        "{label}: {cut:?} was cut from {text:?} without an ellipsis"
                    );
                }
            }
        }
    }

    #[test]
    fn test_mmc_identity_line_cuts_a_wide_identity_on_a_cluster_boundary() {
        // A vendor string can carry emoji or combining marks even though the
        // block path itself is ASCII.  The identity row is the row the card
        // never wraps, so the row the ellipsis produces has to stay inside the
        // card at every width a real terminal can offer.
        let mut app = rock3a_mmc_app(Locale::En);
        {
            let dev = &mut app.mmc_status.devices[0];
            dev.model = "\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}".into();
            dev.manufacturer = "\u{5b58}\u{50a8}\u{82af}\u{7247} (0x000011)".into();
        }
        let dev = &app.mmc_status.devices[0];
        for inner_width in 18..=61usize {
            let line = mmc_identity_line("eMMC", dev, inner_width);
            assert!(
                line.width() <= inner_width,
                "identity line is {} columns wide for a {inner_width}-column card: {:?}",
                line.width(),
                line_text(&line)
            );
        }
    }

    // ---- second-review findings (mmc round 2): RED first ----

    /// Independent oracle: the rows ratatui's own word wrap produces, measured
    /// in a buffer far taller than any device block so nothing clips.  It does
    /// not call the code under test.
    fn oracle_rows(lines: &[Line<'static>], inner_width: usize) -> usize {
        let width = inner_width.max(1) as u16;
        let height = 400u16;
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("init test terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(lines.to_vec()).wrap(Wrap { trim: true }),
                    Rect::new(0, 0, width, height),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .rfind(|row| (0..width).any(|col| buffer[(col, *row)].symbol() != " "))
            .map(|last| last as usize + 1)
            .unwrap_or(0)
    }

    /// The public ratatui segmentation — the oracle the truncation has to cut
    /// on (UnicodeSegmentation under the hood), not the helper under test.
    fn oracle_clusters(text: &str) -> Vec<String> {
        Span::raw(text)
            .styled_graphemes(Style::default())
            .map(|g| g.symbol.to_string())
            .collect()
    }

    #[test]
    fn test_wrapped_rows_of_an_empty_block_is_zero() {
        // An empty block occupies no rows.  Charging it one row is what made a
        // healthy single-device card grow two phantom blank rows (second-review
        // P2-1: `wrapped_rows(&[], w)` returned 1, `render_mission` then added
        // an error row and a separator on top).
        assert_eq!(
            wrapped_rows(&[], 31, usize::MAX),
            RowFit::Fits(0),
            "no lines must be zero rows"
        );
        assert_eq!(
            wrapped_rows(&[], 0, usize::MAX),
            RowFit::Fits(0),
            "no lines at zero width is zero rows"
        );
        assert_eq!(
            wrapped_rows(&[Line::from("")], 31, usize::MAX),
            RowFit::Fits(1),
            "a single explicit empty line is a real blank row, not zero"
        );
        // And the healthy card at 33x30 paints no empty rows it did not need.
        let app = rock3a_warning_app(Locale::En, 1, Some(10));
        let rows = render_mission_rows(&app, 33, 30);
        let title = app.locale.text("storage_telemetry");
        let inner = card_inner_rows(&rows, title);
        assert!(
            !inner.iter().any(|row| row.is_empty()),
            "a healthy device card painted empty rows it did not need:\n{}",
            rows.join("\n")
        );
    }

    /// The rows strictly inside the storage card (between its top and bottom
    /// border) of a `render_mission_rows` capture, located by its title.
    fn card_inner_rows(rows: &[String], title: &str) -> Vec<String> {
        let start = rows
            .iter()
            .position(|row| row.starts_with('\u{250C}') && row.contains(title))
            .expect("the storage card has a titled top border");
        let end = start
            + rows[start + 1..]
                .iter()
                .position(|row| row.starts_with('\u{2514}'))
                .expect("the storage card is closed");
        rows[start + 1..end]
            .iter()
            .map(|row| row.trim_start_matches('\u{2502}').trim_end().to_string())
            .collect()
    }

    #[test]
    fn test_second_warning_device_is_accounted_for() {
        // Two warning eMMCs in a card too short for both (second-review P2-2:
        // the first device used to eat every content row and the second
        // disappeared with no trace).  A card may show none of them and say so —
        // "fall back the whole block to a truthful count" is one of the honest
        // outcomes the review accepts — but it must never drop a device in
        // silence, and any device it does show shows its warning whole.
        for locale in [Locale::En, Locale::ZhCn] {
            let mut app = rock3a_warning_app(locale, 2, Some(10));
            let first = app.mmc_status.devices[0].clone();
            let mut second = first.clone();
            second.block_path = "/dev/mmcblk1".into();
            app.mmc_status.devices = vec![first, second];

            let label = app.locale.text("storage_more_devices").replace(' ', "");
            let rows = render_storage_rows(&app, 33, 5);
            let screen = rows.join("\n");
            let flat: String = rows
                .iter()
                .flat_map(|row| row.chars())
                .filter(|c| !c.is_whitespace())
                .collect();
            // The "more devices" label only ever appears in the hint line.
            let hinted = flat.contains(&label);
            let shown =
                usize::from(flat.contains("mmcblk0")) + usize::from(flat.contains("mmcblk1"));

            // No device vanishes in silence: either both are on screen, or the
            // card accounts for the ones it dropped.
            assert!(
                hinted || shown == 2,
                "{locale:?}: a 33x5 card with two warning devices neither shows both \
                 devices nor accounts for the ones it dropped:\n{screen}"
            );
            // If it shows a device, the pre-EOL warning is whole — never a
            // fragment left at the edge of the card.
            if shown > 0 {
                let value = eol_text(locale, 2).replace(' ', "");
                let fragment: String = value.chars().take(value.chars().count() - 1).collect();
                assert!(
                    !flat.contains(&fragment) || flat.contains(&value),
                    "{locale:?}: a fragment of the pre-EOL warning was painted:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn test_wrapped_rows_tracks_ratatui_on_adversarial_content() {
        // The scratch upper bound in the first fix was not an upper bound: it
        // under-counted, silently clipped the scratch render, and returned a
        // too-small row count that the card then trusted (second-review P3-3).
        // The budget must agree with ratatui on content a device block can
        // actually produce: leading/double whitespace and non-breaking runs.
        let cases: Vec<(Vec<Line<'static>>, usize)> = vec![
            (vec![Line::from("  abc  def")], 3),
            (vec![Line::from("中".repeat(31))], 5),
            (vec![Line::from("x ".repeat(40))], 5),
            (vec![Line::from("aa bb cc dd ee ff")], 4),
        ];
        for (block, width) in cases {
            let budget = match wrapped_rows(&block, width, 511) {
                RowFit::Fits(n) => n,
                RowFit::Overflow => panic!(
                    "these blocks need far fewer than 511 rows; at width {width}: lines {:?}",
                    block.iter().map(line_text).collect::<Vec<_>>()
                ),
            };
            let oracle = oracle_rows(&block, width);
            assert_eq!(
                budget,
                oracle,
                "budget vs ratatui at width {width}: lines {:?}",
                block.iter().map(line_text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_wrapped_rows_is_resource_bounded() {
        // A line no device produces: ~130 KB of one-word filler.  Measuring it
        // must not allocate a scratch buffer that grows with the text — the
        // first fix's per-word upper bound implied a buffer up to 65535 rows
        // tall (order 150 MB at 59 columns) for exactly this input.  The
        // measurement is capped at the caller's budget (never past
        // MAX_WRAP_ROWS): a small cap yields `Overflow` without rendering more
        // than the cap, and never passes a clipped count off as the answer.
        let block = vec![Line::from("x ".repeat(65_535))];
        assert_eq!(
            wrapped_rows(&block, 59, 5),
            RowFit::Overflow,
            "an enormous line must overflow a five-row budget, not return a clipped count"
        );
        // The empty block is zero rows even against an unbounded cap.
        assert_eq!(wrapped_rows(&[], 59, usize::MAX), RowFit::Fits(0));
        // A single word wider than the row is split on ratatui's own count, not
        // on a hand-rolled one.
        let wide = vec![Line::from("x".repeat(20))];
        assert_eq!(
            wrapped_rows(&wide, 3, 64),
            RowFit::Fits(oracle_rows(&wide, 3)),
            "a word wider than the row is split on ratatui's count"
        );
    }

    #[test]
    fn test_ellipsize_cuts_on_a_real_grapheme_boundary() {
        // The second review's counterexamples against the hand-rolled
        // segmentation: a base followed by a run of zero width joiners, and
        // Indic conjuncts.  The boundary oracle is the public ratatui
        // segmentation, so the helper cannot certify itself (P3-4).
        let cases = [
            ("A\u{200d}\u{200d}BBBB", "A + ZWJ + ZWJ + word"),
            ("\u{2764}\u{fe0f}\u{2764}\u{fe0f}", "heart + VS16"),
            (
                "\u{1f1e8}\u{1f1f3}\u{1f1e9}\u{1f1ea}",
                "regional indicator flags",
            ),
            ("e\u{301}mos\u{301}torage", "combining marks"),
            (
                "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
                "partial ZWJ family",
            ),
            (
                "\u{915}\u{94d}\u{915}\u{94d}\u{915}\u{94d}\u{915}",
                "devanagari conjuncts",
            ),
        ];
        for (text, label) in cases {
            let clusters = oracle_clusters(text);
            for max in 0..=text_width(text) + 2 {
                let cut = ellipsize(text, max);
                assert!(
                    text_width(&cut) <= max,
                    "{label}: {text:?} at {max} columns came back {} wide: {cut:?}",
                    text_width(&cut)
                );
                let kept = cut.strip_suffix('\u{2026}').unwrap_or(&cut);
                let kept_clusters = oracle_clusters(kept);
                assert!(
                    clusters.starts_with(kept_clusters.as_slice()),
                    "{label}: kept {kept:?} is not a whole-cluster prefix of {text:?} \
                     (oracle {clusters:?})"
                );
            }
        }
    }

    #[test]
    fn test_wrapped_rows_never_reads_a_blank_row_as_the_end_of_a_block() {
        // The third review's counterexample against the blank lookahead: the
        // scratch render of `["abcde", "", "WARNING"]` ends its first two rows
        // on the blank row of the empty line, and reading the last painted row
        // off the buffer called that a two-row fit while `WARNING` was already
        // below the buffer (P3 — `Fits(2)` where only `Overflow` is true).
        let blank_then_more = vec![Line::from("abcde"), Line::from(""), Line::from("WARNING")];
        for cap in 0..6 {
            assert_eq!(
                wrapped_rows(&blank_then_more, 3, cap),
                RowFit::Overflow,
                "six rows of content never fit a {cap}-row budget, whatever row the buffer ends on"
            );
        }
        // Once the rows really exist the same block is measured exactly: two
        // rows for `abcde`, the empty line's own row, three for `WARNING`.
        assert_eq!(
            wrapped_rows(&blank_then_more, 3, 6),
            RowFit::Fits(6),
            "the block is six rows at three columns"
        );
        // The blind spot is not limited to a clipped tail: a block that ends
        // with a blank row shows one painted row less than it costs, and a card
        // sized from that number is a row too short.
        let ends_blank = vec![Line::from("abcdef"), Line::from("")];
        assert_eq!(
            wrapped_rows(&ends_blank, 3, 2),
            RowFit::Overflow,
            "the block is three rows, the last one blank"
        );
        assert_eq!(wrapped_rows(&ends_blank, 3, 3), RowFit::Fits(3));
        // An explicit blank line is a row, whatever the width, and it is the
        // block that paints nothing that must stay at zero rows.
        assert_eq!(
            wrapped_rows(&[Line::from(""), Line::from("")], 3, 4),
            RowFit::Fits(2)
        );
        assert_eq!(wrapped_rows(&[Line::from("")], 3, 0), RowFit::Overflow);
        assert_eq!(wrapped_rows(&[], 3, 0), RowFit::Fits(0));
    }

    /// Every wording [`storage_more_hints`] offers for `more`, in the same
    /// order, rebuilt from the i18n text rather than from the helper under test.
    fn hint_wordings(locale: Locale, more: usize) -> Vec<String> {
        let label = locale.text("storage_more_devices");
        let full = if locale.is_zh() {
            format!("... {label} {more}")
        } else {
            format!("... {more} {label}")
        };
        vec![full, format!("... +{more}"), format!("+{more}")]
    }

    /// The text of a card row without its borders, blank rows or the gaps a
    /// double-width glyph leaves in the buffer: a Chinese character fills a
    /// second cell, which reads as a space here and is no defect of the card.
    fn no_space(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// The single content row of a storage card, without its borders.
    fn only_content_row(rows: &[String]) -> String {
        rows.iter()
            .filter(|row| !row.starts_with('\u{250C}') && !row.starts_with('\u{2514}'))
            .map(|row| {
                row.trim_start_matches('\u{2502}')
                    .trim_end_matches('\u{2502}')
                    .trim()
                    .to_string()
            })
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_more_devices_hint_is_painted_whole_at_every_width() {
        // The hint is what admits a dropped device, so half of it is worse than
        // nothing: at the 16 inner columns of an 18x3 card the English wording
        // is two rows tall and the one row the card had was painted as
        // `... 2 more` with `device(s)` off the edge (mmc third review P2-2).
        // What the card may show is a whole wording, and the richest one the
        // room can hold.
        for locale in [Locale::En, Locale::ZhCn] {
            let mut app = rock3a_warning_app(locale, 2, Some(10));
            let first = app.mmc_status.devices[0].clone();
            let mut second = first.clone();
            second.block_path = "/dev/mmcblk1".into();
            app.mmc_status.devices = vec![first, second];
            for width in 18..=61u16 {
                let inner_width = usize::from(width) - 2;
                // One content row: nothing but the hint can be on it, so the
                // row is either a whole wording or a defect.
                let rows = render_storage_rows(&app, width, 3);
                let screen = rows.join("\n");
                let wordings = hint_wordings(locale, 2);
                let content = no_space(&only_content_row(&rows));
                let whole: Vec<String> = wordings.iter().map(|w| no_space(w)).collect();
                assert!(
                    whole.contains(&content),
                    "{locale:?} at a {width}x3 card painted {:?}, which is no whole wording \
                     the hint offers ({wordings:?}):\n{screen}",
                    only_content_row(&rows),
                );
                let richest = wordings
                    .iter()
                    .find(|wording| text_width(wording) <= inner_width)
                    .expect("the leanest wording is two columns");
                assert_eq!(
                    content,
                    no_space(richest),
                    "{locale:?} at a {width}x3 card: the widest wording that fits one row is \
                     {richest:?}, the card showed {:?}:\n{screen}",
                    only_content_row(&rows),
                );
            }
        }
    }

    #[test]
    fn test_mission_counts_the_devices_a_narrow_card_cannot_show() {
        // The same rule one level up, on the shape `render_mission` picks for
        // itself: an 19-row mission column leaves the storage card three rows,
        // one of them for the count of the eMMCs the card has no rows for, and
        // that row is a whole wording rather than a word cut at the border.
        for locale in [Locale::En, Locale::ZhCn] {
            let mut app = rock3a_warning_app(locale, 2, Some(10));
            let first = app.mmc_status.devices[0].clone();
            let mut second = first.clone();
            second.block_path = "/dev/mmcblk1".into();
            app.mmc_status.devices = vec![first, second];
            for width in [18u16, 21] {
                let rows = render_mission_rows(&app, width, 19);
                let screen = rows.join("\n");
                let flat = flatten_screen(&rows);
                let title = app.locale.text("storage_telemetry");
                assert!(
                    flat.contains(&no_space(app.locale.text("service_signals"))),
                    "{locale:?} at a {width}-column mission column lost the service list that \
                     sits under the storage card:\n{screen}"
                );
                assert!(
                    flat.contains(&no_space(title)),
                    "{locale:?} at a {width}-column mission column has no storage card:\n{screen}"
                );
                assert_eq!(
                    flat.matches("[eMMC]").count(),
                    0,
                    "{locale:?} at a {width}-column mission column shows a device the three-row \
                     card cannot hold a warning for:\n{screen}"
                );
                assert!(
                    hint_wordings(locale, 2)
                        .iter()
                        .any(|wording| flat.contains(&no_space(wording))),
                    "{locale:?} at a {width}-column mission column: the two eMMCs are neither \
                     shown nor counted:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn test_error_rows_leave_the_device_count_visible() {
        // The third review's P2-1: with an enumeration error and devices behind
        // it, a short card let the error take every content row, the device
        // loop ran on a budget of zero, and the row that would have counted the
        // dropped devices was gone too — the eMMCs were neither shown nor
        // missed.  Whatever the card's shape, a device the card does not show
        // has to be in the hint.
        for locale in [Locale::En, Locale::ZhCn] {
            let mut app = rock3a_warning_app(locale, 2, Some(10));
            let first = app.mmc_status.devices[0].clone();
            let mut second = first.clone();
            second.block_path = "/dev/mmcblk1".into();
            app.mmc_status.devices = vec![first, second];
            app.nvme_error = Some(rsetup_core::HardwareError::Io(
                "nvme-enumeration-eio".into(),
            ));
            let label = app.locale.text("storage_more_devices");
            for width in [18u16, 25, 33, 45, 61] {
                for height in 3..=16u16 {
                    let rows = render_storage_rows(&app, width, height);
                    let screen = rows.join("\n");
                    // Borders and the gaps of double-width glyphs go, the words
                    // stay: a hint the paragraph moved onto a second row is one
                    // hint, not a fragment of one.
                    let flat = flatten_screen(&rows);
                    // A device that is on the card is on it with its identity
                    // row, and that row always carries the badge; on a narrow
                    // card the block path inside it is what gives way, so
                    // counting names would see a shown device as a lost one.
                    let shown = flat.matches("[eMMC]").count();
                    let dropped = app.mmc_status.devices.len() - shown;
                    // Any wording of the hint counts as the card admitting it
                    // dropped a device, as long as the number it says is the
                    // number of devices really missing.
                    let counted = (1..=app.mmc_status.devices.len())
                        .filter(|n| {
                            hint_wordings(locale, *n)
                                .iter()
                                .any(|wording| flat.contains(&wording.replace(' ', "")))
                        })
                        .collect::<Vec<_>>();
                    let expected_count = [dropped];
                    let expected: &[usize] = if dropped == 0 { &[] } else { &expected_count };
                    assert_eq!(
                        counted.as_slice(),
                        expected,
                        "{locale:?} at a {width}x{height} card: {shown} of {} device(s) are on \
                         screen and the card counts {counted:?} — a dropped device has to be \
                         named by the hint ({label:?} or one of its short forms), a shown one \
                         must not be counted twice:\n{screen}",
                        app.mmc_status.devices.len(),
                    );
                }
            }
        }
    }

    #[test]
    fn test_error_and_device_count_share_a_short_card() {
        // The exact shape the review measured: 33 columns, four rows, one
        // enumeration error and two warning eMMCs.  The error keeps the row it
        // can hold with its tail given up under an ellipsis, the devices take
        // the row that counts them, and neither is left out in silence (mmc
        // third review P2-1).
        for locale in [Locale::En, Locale::ZhCn] {
            let mut app = rock3a_warning_app(locale, 2, Some(10));
            let first = app.mmc_status.devices[0].clone();
            let mut second = first.clone();
            second.block_path = "/dev/mmcblk1".into();
            app.mmc_status.devices = vec![first, second];
            app.nvme_error = Some(rsetup_core::HardwareError::Io(
                "nvme-enumeration-eio".into(),
            ));
            let rows = render_storage_rows(&app, 33, 4);
            let screen = rows.join("\n");
            let content = only_content_row(&rows);
            let flat: String = content.chars().filter(|c| !c.is_whitespace()).collect();
            let error_flat: String = storage_error_lines(&app)
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect::<String>()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let first_row: String = content
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            // The card's first row is the error with its tail given up, and the
            // ellipsis says so: a cut that is announced is a decision, a word
            // sawn in half is a defect, and dropping the error to keep a device
            // it does not show is what the review rejected.
            assert!(
                first_row.ends_with('\u{2026}'),
                "{locale:?}: the shortened error does not say it was shortened:\n{screen}"
            );
            let head = first_row.trim_end_matches('\u{2026}');
            assert!(
                !head.is_empty() && error_flat.starts_with(head),
                "{locale:?}: {first_row:?} is not the beginning of the error \
                 {error_flat:?}:\n{screen}"
            );
            assert!(
                head.len() < error_flat.len(),
                "{locale:?}: the error kept all of its text and still an ellipsis:\n{screen}"
            );
            assert!(
                hint_wordings(locale, 2)
                    .iter()
                    .any(|wording| flat.contains(&wording.replace(' ', ""))),
                "{locale:?}: the two dropped eMMCs are not counted on the card:\n{screen}"
            );
        }
    }
}
