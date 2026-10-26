pub(in crate::tui) mod mmc;
pub(in crate::tui) mod nvme;

#[cfg(test)]
mod card_tests;

use crate::i18n::Locale;
use crate::tui::app::App;
use crate::tui::textfit::{
    MAX_WRAP_ROWS, RowFit, fit_line_in_rows, wrapped_rows, wrapped_rows_capped,
};
use crate::tui::theme::{CORAL, MUTED, instrument};
use mmc::mmc_device_lines;
use nvme::nvme_device_lines;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

pub(in crate::tui) fn render_storage_summary(frame: &mut Frame, app: &App, area: Rect) {
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

pub(in crate::tui) fn storage_error_lines(app: &App) -> Vec<Line<'static>> {
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
