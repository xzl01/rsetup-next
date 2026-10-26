use crate::i18n::Locale;
use crate::tui::App;
use crate::tui::mission::render_mission;
use ratatui::{Terminal, layout::Rect, text::Line};
use rsetup_core::{Controller, ExecutionPolicy, ProbeMode};

/// The Rock 3A device of mmc report §5 T1: its identity line is 61 display
/// columns wide, one more than the 59 columns the storage card gets on a
/// 100-column terminal, which used to push the capacity unit onto a second
/// row.
pub(in crate::tui) fn rock3a_mmc_app(locale: Locale) -> App {
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

pub(in crate::tui) fn line_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The Rock 3A eMMC of the implementation review, this time wearing the
/// warning it really reports: pre-EOL `2`, both life-time estimates at 10%
/// and the derived `Warning` health state.
pub(in crate::tui) fn rock3a_warning_app(locale: Locale, pre_eol: u8, life: Option<u8>) -> App {
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

pub(in crate::tui) fn render_mission_rows(app: &App, width: u16, height: u16) -> Vec<String> {
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

// ---- P2: the pre-EOL warning must never be clipped by the row budget ----

/// The pre-EOL wording the card has to show for `pre_eol`.
pub(in crate::tui) fn eol_text(locale: Locale, pre_eol: u8) -> &'static str {
    let (zh, en) = match pre_eol {
        2 => ("预警(80%)", "Warning(80%)"),
        3 => ("紧急", "Urgent"),
        _ => ("正常", "Normal"),
    };
    if locale.is_zh() { zh } else { en }
}

/// The screen without its borders and padding, so a value that the word
/// wrap moved onto the next row is still found.
pub(in crate::tui) fn flatten_screen(rows: &[String]) -> String {
    rows.iter()
        .flat_map(|row| row.chars())
        .filter(|c| {
            !c.is_whitespace() && !matches!(c, '│' | '─' | '┌' | '┐' | '└' | '┘' | '├' | '┤')
        })
        .collect()
}
