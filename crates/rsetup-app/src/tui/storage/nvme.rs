use crate::tui::app::App;
use crate::tui::textfit::fit_block;
use crate::tui::theme::{AMBER, BONE, CORAL, MUTED, SIGNAL};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

/// The rows of one NVMe device, fitted into `max_rows`.  The I/O line is the
/// one that gives way first; the identity and the health state stay together
/// or the device is not rendered at all (`None`).
pub(in crate::tui) fn nvme_device_lines(
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
