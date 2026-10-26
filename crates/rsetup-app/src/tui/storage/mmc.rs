use crate::tui::App;
use crate::tui::textfit::{RowFit, ellipsize, fit_block, join_identity, text_width, wrapped_rows};
use crate::tui::theme::{AMBER, BONE, CORAL, MUTED, SIGNAL};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

/// Identity line of an MMC/SD card: `[type] block · model · vendor · capacity`.
///
/// One column too wide and the paragraph wraps the capacity unit onto a
/// second row (mmc report §5 T1), so the identity gives fields up before
/// that can happen: the `/dev/` prefix first, then the vendor, then the
/// model, and only the remainder gets ellipsized. The type badge and the
/// capacity are never given up.
pub(in crate::tui) fn mmc_identity_line(
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

/// The rows of one MMC device, fitted into `max_rows`.  `None` when the
/// identity and a health state cannot both be shown: the caller then counts
/// the device as not rendered instead of showing half of it.
pub(in crate::tui) fn mmc_device_lines(
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

#[cfg(test)]
mod tests {
    use super::mmc_identity_line;
    use crate::i18n::Locale;
    use crate::tui::fixtures::{line_text, rock3a_mmc_app};

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
    // ---- P3: truncation cuts clusters, not code points ----

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
}
