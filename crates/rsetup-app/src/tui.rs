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
    ActionRun, ActionSpec, Controller, DeviceSnapshot, RiskLevel, SourcePlan, SourceStatus,
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
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
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
    let nvme_rows = if has_nvme_devices {
        app.nvme_status
            .devices
            .iter()
            .map(|dev| wrapped_rows(&nvme_device_lines(app, dev, inner_width), inner_width))
            .sum()
    } else {
        0
    };
    let mmc_rows = if has_mmc_devices {
        app.mmc_status
            .devices
            .iter()
            .map(|dev| wrapped_rows(&mmc_device_lines(app, dev), inner_width))
            .sum()
    } else {
        0
    };
    let error_lines = storage_error_lines(app);
    let error_rows = wrapped_rows(&error_lines, inner_width);
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
    // On short viewports keep the two cards above and the service list below
    // intact rather than growing the storage card past the available room.
    let headroom = area.height.saturating_sub(6 + 6 + 4) as usize;
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
    let cpu = app.snapshot.metrics.cpu_percent.clamp(0.0, 100.0) as u16;
    let memory = percent(
        app.snapshot.metrics.memory_used_bytes,
        app.snapshot.metrics.memory_total_bytes,
    ) as u16;
    let gauges = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    frame.render_widget(
        Gauge::default()
            .block(instrument(app.locale.text("cpu_load")))
            .gauge_style(Style::default().fg(SIGNAL).bg(Color::Rgb(42, 46, 38)))
            .percent(cpu)
            .label(format!("{:.1}%", app.snapshot.metrics.cpu_percent)),
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
            .percent(memory)
            .label(format!("{memory}%")),
        gauges[1],
    );

    let temp = app
        .snapshot
        .metrics
        .temperature_c
        .map(|v| format!("{v:.1} °C"))
        .unwrap_or_else(|| app.locale.text("no_sensor").into());
    let identity = format!(
        "{}\n{} / {}\n{} {}  ·  {} {:.2} {:.2} {:.2}  ·  {} {}",
        app.snapshot.identity.product,
        app.snapshot.identity.soc,
        app.snapshot.identity.architecture,
        app.locale.text("uptime"),
        duration(app.snapshot.metrics.uptime_seconds, app.locale),
        app.locale.text("load_average"),
        app.snapshot.metrics.load_average[0],
        app.snapshot.metrics.load_average[1],
        app.snapshot.metrics.load_average[2],
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

    let error_lines = storage_error_lines(app);
    for err_line in error_lines {
        let rows = wrapped_rows(std::slice::from_ref(&err_line), inner_width);
        if used_rows + rows <= budget {
            lines.push(err_line);
            used_rows += rows;
        }
    }

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
    let mut more = 0usize;
    for (idx, dev) in devices.iter().enumerate() {
        let dev_lines = match dev {
            Device::Nvme(d) => nvme_device_lines(app, d, inner_width),
            Device::Mmc(d) => mmc_device_lines(app, d),
        };
        // A blank row separates consecutive devices, or devices from errors; charge it to the device.
        let needs_sep = idx > 0 || !lines.is_empty();
        let rows = wrapped_rows(&dev_lines, inner_width) + if needs_sep { 1 } else { 0 };
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
    if more > 0 && budget - used_rows >= 1 {
        let more_line = if app.locale.is_zh() {
            format!("... {} {}", app.locale.text("storage_more_devices"), more)
        } else {
            format!("... {} {}", more, app.locale.text("storage_more_devices"))
        };
        lines.push(Line::from(vec![Span::styled(
            more_line,
            Style::default().fg(MUTED),
        )]));
    }

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        area,
    );
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

fn wrapped_rows(lines: &[Line<'static>], inner_width: usize) -> usize {
    // Ratatui wraps each line at the paragraph's inner width; count the
    // rendered rows the way the paragraph will, so the card height and the
    // truncation budget agree with what actually fits.
    if inner_width == 0 {
        return lines.len().max(1);
    }
    lines
        .iter()
        .map(|line| {
            if line.width() == 0 {
                1
            } else {
                (line.width().saturating_sub(1) / inner_width) + 1
            }
        })
        .sum()
}

fn nvme_device_lines(
    app: &App,
    dev: &rsetup_core::NvmeDevice,
    inner_width: usize,
) -> Vec<Line<'static>> {
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

        vec![line1, line2, line3]
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

        vec![line1, line2]
    }
}

fn mmc_device_lines(app: &App, dev: &rsetup_core::MmcDevice) -> Vec<Line<'static>> {
    // Line 1: [type] block_path · model · manufacturer · capacity
    let type_label = if dev.card_type == "MMC" {
        app.locale.text("storage_emmc")
    } else {
        app.locale.text("storage_sd")
    };
    let line1 = Line::from(vec![
        Span::styled(
            format!("[{}] ", type_label),
            Style::default().fg(BONE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{} · ", dev.block_path), Style::default().fg(BONE)),
        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
        Span::styled(
            format!("{} · ", dev.manufacturer),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            crate::format_bytes(dev.total_bytes),
            Style::default().fg(AMBER),
        ),
    ]);

    // Line 2: Health state or telemetry error/unsupported banner
    let health_label = format!("{}: ", app.locale.text("storage_health"));
    let (health_text, health_color) = match dev.health_state {
        rsetup_core::HealthState::Healthy => (app.locale.text("storage_healthy"), SIGNAL),
        rsetup_core::HealthState::Warning => (app.locale.text("storage_warning"), AMBER),
        rsetup_core::HealthState::Critical => (app.locale.text("storage_critical"), CORAL),
        rsetup_core::HealthState::Unknown => (app.locale.text("storage_unknown"), MUTED),
    };

    let line2 = match dev.telemetry.state {
        rsetup_core::TelemetryReadState::Unavailable => {
            let err_reason = app
                .locale
                .storage_telemetry_error(dev.telemetry.error.as_ref());
            Line::from(vec![
                Span::styled(health_label, Style::default().fg(MUTED)),
                Span::styled(
                    health_text,
                    Style::default()
                        .fg(health_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(
                    format!(
                        "{}: {}",
                        app.locale.text("storage_telemetry_unavailable"),
                        err_reason
                    ),
                    Style::default().fg(MUTED),
                ),
            ])
        }
        rsetup_core::TelemetryReadState::Unsupported => Line::from(vec![
            Span::styled(health_label, Style::default().fg(MUTED)),
            Span::styled(
                health_text,
                Style::default()
                    .fg(health_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(MUTED)),
            Span::styled(
                app.locale.text("storage_telemetry_unsupported"),
                Style::default().fg(MUTED),
            ),
        ]),
        rsetup_core::TelemetryReadState::Available => {
            let life_a_label = format!("{}: ", app.locale.text("storage_life_a"));
            let life_b_label = format!("{}: ", app.locale.text("storage_life_b"));
            let pre_eol_label = format!("{}: ", app.locale.text("storage_pre_eol"));
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
                0 => app.locale.text("storage_eol_undefined"),
                1 => app.locale.text("storage_eol_normal"),
                2 => app.locale.text("storage_eol_warning"),
                3 => app.locale.text("storage_eol_urgent"),
                _ => app.locale.text("storage_eol_undefined"),
            };
            Line::from(vec![
                Span::styled(health_label, Style::default().fg(MUTED)),
                Span::styled(
                    health_text,
                    Style::default()
                        .fg(health_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(life_a_label, Style::default().fg(MUTED)),
                Span::styled(life_a, Style::default().fg(BONE)),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(life_b_label, Style::default().fg(MUTED)),
                Span::styled(life_b, Style::default().fg(BONE)),
                Span::styled(" · ", Style::default().fg(MUTED)),
                Span::styled(pre_eol_label, Style::default().fg(MUTED)),
                Span::styled(eol, Style::default().fg(BONE)),
            ])
        }
    };

    vec![line1, line2]
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

fn percent(value: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        value as f32 / total as f32 * 100.0
    }
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
        assert!(
            raw_text.contains("/dev/mmcblk0"),
            "Expected eMMC device in buffer, got: {raw_text}"
        );
        assert!(
            raw_text.contains("/dev/mmcblk1"),
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
            let lines = mmc_device_lines(&app, dev);
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

    #[derive(Clone)]
    struct EnumerationFixture(
        std::sync::Arc<std::sync::Mutex<(rsetup_core::StorageStatus, bool, bool)>>,
    );

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
}
