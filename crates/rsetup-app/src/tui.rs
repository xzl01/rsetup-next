use crate::i18n::Locale;
use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use rsetup_core::{
    ActionRun, ActionSpec, Controller, DeviceSnapshot, ExecutionPolicy, RiskLevel, SourcePlan,
    SourceStatus,
};
use std::{io, time::Duration};

#[cfg(test)]
pub(in crate::tui) mod fixtures;
mod install;
mod mission;
mod storage;
mod textfit;
mod theme;

use crate::tui::install::backend::system_install_manager;
use crate::tui::install::render::{render_install_footer, render_install_panel};
use crate::tui::install::{InstallFocus, InstallPanel};
use crate::tui::mission::render_mission;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::storage::render_storage_summary;
    use ratatui::Terminal;
    use rsetup_core::{ExecutionPolicy, ProbeMode};

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
