use crate::i18n::Locale;
use crate::tui::app::App;
use crate::tui::storage::mmc::mmc_device_lines;
use crate::tui::storage::nvme::nvme_device_lines;
use crate::tui::storage::{render_storage_summary, storage_error_lines};
use crate::tui::textfit::wrapped_rows_capped;
use crate::tui::theme::{BONE, MUTED, SIGNAL, instrument};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Gauge, Paragraph, Wrap},
};

pub(in crate::tui) fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
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
    use rsetup_core::{Controller, ExecutionPolicy, ProbeMode};

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
}
