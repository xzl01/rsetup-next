use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

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
pub(in crate::tui) fn fit_line_in_rows(
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

/// The ceiling on the rows any single measurement is allowed to render.  Every
/// caller caps `wrapped_rows` at a screen height; this constant only bounds the
/// pathological case (one enormous line), so a scratch buffer never grows with
/// the input — at most `MAX_WRAP_ROWS` rows times the card width, not a function
/// of the text length.
pub(in crate::tui) const MAX_WRAP_ROWS: usize = 512;

/// The outcome of measuring a block against a row budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum RowFit {
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
pub(in crate::tui) fn wrapped_rows(
    lines: &[Line<'static>],
    inner_width: usize,
    cap: usize,
) -> RowFit {
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
pub(in crate::tui) fn wrapped_rows_capped(
    lines: &[Line<'static>],
    inner_width: usize,
    cap: usize,
) -> usize {
    match wrapped_rows(lines, inner_width, cap) {
        RowFit::Fits(n) => n,
        RowFit::Overflow => cap + 1,
    }
}

/// Display width of a plain string, measured exactly the way the paragraph
/// that renders it wraps it.
pub(in crate::tui) fn text_width(text: &str) -> usize {
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
pub(in crate::tui) fn ellipsize(text: &str, max: usize) -> String {
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
pub(in crate::tui) fn join_identity(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The richest candidate block that fits in `max_rows` rows.  Among the
/// candidates that keep the same values the one needing the fewest rows wins,
/// so a card is never padded out with rows it does not need.
pub(in crate::tui) fn fit_block(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Locale;
    use crate::tui::fixtures::{line_text, render_mission_rows, rock3a_warning_app};
    use crate::tui::mmc_device_lines;
    use ratatui::Terminal;

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
}
