# Task 4.3 Review Package
Base: 4e6110332ffdd816a7cc3ff0feadb23e98e1ce1f
Head: 98735dc4fbb403fd1d5fb4994a914a793d5470e6

## Git Log
98735dc feat(app): unified multi-device storage TUI panel

## Git Diff --stat
 crates/rsetup-app/src/tui.rs | 488 ++++++++++++++++++++++++++++++++++---------
 1 file changed, 385 insertions(+), 103 deletions(-)

## Git Diff
diff --git a/crates/rsetup-app/src/tui.rs b/crates/rsetup-app/src/tui.rs
index 65f99cd..5a2b615 100644
--- a/crates/rsetup-app/src/tui.rs
+++ b/crates/rsetup-app/src/tui.rs
@@ -376,35 +376,65 @@ fn render_header(frame: &mut Frame, app: &App, area: Rect) {
         Paragraph::new(line).block(
             Block::default()
                 .borders(Borders::BOTTOM)
                 .border_style(Style::default().fg(MUTED)),
         ),
         area,
     );
 }
 
 fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
+    // Unified storage card: NVMe devices take 3 content rows each, MMC/SD
+    // devices 2, with one blank row between devices. Long lines wrap inside
+    // the card's inner width (the device line alone wraps to two rows on a
+    // 100-column terminal), so measure the wrapped row count at the card's
+    // width to size the card for what will actually render.
     let has_nvme_devices = app.nvme_status.initialized && !app.nvme_status.devices.is_empty();
-    // The telemetry card needs four content rows plus its two border rows: the
-    // device line alone wraps to two rows on a 100-column terminal, so a shorter
-    // card clips the spare/threshold line instead of showing it.
-    let desired_nvme_height = if has_nvme_devices { 6 } else { 3 };
+    let has_mmc_devices = app.mmc_status.initialized && !app.mmc_status.devices.is_empty();
+    let nvme_count = if has_nvme_devices { app.nvme_status.devices.len() } else { 0 };
+    let mmc_count = if has_mmc_devices { app.mmc_status.devices.len() } else { 0 };
+    let inner_width = (area.width.saturating_sub(2)) as usize;
+    let nvme_rows = if has_nvme_devices {
+        app.nvme_status
+            .devices
+            .iter()
+            .map(|dev| wrapped_rows(&nvme_device_lines(app, dev, inner_width), inner_width))
+            .sum()
+    } else {
+        0
+    };
+    let mmc_rows = if has_mmc_devices {
+        app.mmc_status
+            .devices
+            .iter()
+            .map(|dev| wrapped_rows(&mmc_device_lines(app, dev), inner_width))
+            .sum()
+    } else {
+        0
+    };
+    let device_count = nvme_count + mmc_count;
+    let content_rows = if device_count == 0 {
+        0
+    } else {
+        nvme_rows + mmc_rows + (device_count - 1)
+    };
+    let desired_height = if device_count == 0 { 3 } else { 2 + content_rows };
     // On short viewports keep the two cards above and the service list below
-    // intact rather than growing the telemetry card past the available room.
-    let nvme_headroom = area.height.saturating_sub(6 + 6 + 4);
-    let nvme_height = desired_nvme_height.min(nvme_headroom).max(3);
+    // intact rather than growing the storage card past the available room.
+    let headroom = area.height.saturating_sub(6 + 6 + 4) as usize;
+    let height = desired_height.min(headroom).max(3);
     let rows = Layout::default()
         .direction(Direction::Vertical)
         .constraints([
             Constraint::Length(6),
             Constraint::Length(6),
-            Constraint::Length(nvme_height),
+            Constraint::Length(height as u16),
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
@@ -452,21 +482,21 @@ fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
         temp
     );
     frame.render_widget(
         Paragraph::new(identity)
             .style(Style::default().fg(BONE))
             .block(instrument(app.locale.text("device_core")))
             .wrap(Wrap { trim: true }),
         rows[1],
     );
 
-    render_nvme_summary(frame, app, rows[2]);
+    render_storage_summary(frame, app, rows[2]);
 
     let services = app
         .snapshot
         .services
         .iter()
         .map(|service| {
             format!(
                 "{}  ·  {}  ·  {}",
                 app.locale.service_state(service.state),
                 app.locale.service_label(&service.id, &service.label),
@@ -477,120 +507,260 @@ fn render_mission(frame: &mut Frame, app: &App, area: Rect) {
         .join("\n");
     frame.render_widget(
         Paragraph::new(services)
             .style(Style::default().fg(MUTED))
             .block(instrument(app.locale.text("service_signals")))
             .wrap(Wrap { trim: true }),
         rows[3],
     );
 }
 
-fn render_nvme_summary(frame: &mut Frame, app: &App, area: Rect) {
-    let block = instrument(app.locale.text("nvme_telemetry"));
-    if !app.nvme_status.initialized || app.nvme_status.devices.is_empty() {
-        let msg = Paragraph::new(app.locale.text("nvme_not_detected"))
+fn render_storage_summary(frame: &mut Frame, app: &App, area: Rect) {
+    let block = instrument(app.locale.text("storage_telemetry"));
+
+    let nvme = if app.nvme_status.initialized {
+        app.nvme_status.devices.as_slice()
+    } else {
+        &[]
+    };
+    let mmc = if app.mmc_status.initialized {
+        app.mmc_status.devices.as_slice()
+    } else {
+        &[]
+    };
+
+    if nvme.is_empty() && mmc.is_empty() {
+        let msg = Paragraph::new(app.locale.text("storage_not_detected"))
             .style(Style::default().fg(MUTED))
             .block(block)
             .wrap(Wrap { trim: true });
         frame.render_widget(msg, area);
         return;
     }
 
-    let mut lines = Vec::new();
-    for (idx, dev) in app.nvme_status.devices.iter().enumerate() {
+    // NVMe devices all come first, then the MMC/SD devices. When the card is
+    // too short for all of them, trailing devices are dropped and a "more
+    // devices" hint row is shown instead. Rows are counted after wrapping at
+    // the card's inner width, because the budget is in rendered rows.
+    enum Device<'a> {
+        Nvme(&'a rsetup_core::NvmeDevice),
+        Mmc(&'a rsetup_core::MmcDevice),
+    }
+    let devices: Vec<Device<'_>> = nvme
+        .iter()
+        .map(Device::Nvme)
+        .chain(mmc.iter().map(Device::Mmc))
+        .collect();
+    let budget = area.height.saturating_sub(2) as usize;
+    let inner_width = area.width.saturating_sub(2) as usize;
+    let mut lines: Vec<Line<'static>> = Vec::new();
+    let mut used_rows = 0usize;
+    let mut more = 0usize;
+    for (idx, dev) in devices.iter().enumerate() {
+        let dev_lines = match dev {
+            Device::Nvme(d) => nvme_device_lines(app, d, inner_width),
+            Device::Mmc(d) => mmc_device_lines(app, d),
+        };
+        // A blank row separates consecutive devices; charge it to the second one.
+        let rows = wrapped_rows(&dev_lines, inner_width) + if idx > 0 { 1 } else { 0 };
+        if used_rows + rows > budget {
+            more = devices.len() - idx;
+            break;
+        }
         if idx > 0 {
             lines.push(Line::from(""));
         }
-        let size_str = crate::format_bytes(dev.total_bytes);
-        // Line 1: dev.name, dev.path, model, capacity
-        lines.push(Line::from(vec![
-            Span::styled(format!("{} ", dev.name), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
-            Span::styled(format!("({}) · ", dev.path), Style::default().fg(MUTED)),
-            Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
-            Span::styled(size_str, Style::default().fg(AMBER)),
-        ]));
-
-        // Line 2: Health state, temperature, used endurance, available spare
-        let is_healthy = dev.smart.critical_warning == 0 && dev.smart.warning_flags.is_empty();
-        let (status_text, status_color) = if is_healthy {
-            (app.locale.text("nvme_healthy"), SIGNAL)
-        } else {
-            (app.locale.text("nvme_warning"), CORAL)
-        };
-        let status_label = if app.locale.is_zh() { "状态: " } else { "Health: " };
-        let temp_label = if app.locale.is_zh() { "温度: " } else { "Temp: " };
-        let spare_label = if app.locale.is_zh() { "备用: " } else { "Spare: " };
-        let endurance_label = if app.locale.is_zh() { "已用寿命: " } else { "Used Endurance: " };
-        // "Used Endurance:" is long enough to push the mandatory available-spare
-        // value past the 59 columns the mission panel gets on a 100-column
-        // terminal, so fall back to the short spelling when the full one does
-        // not fit next to the warning flags.
-        let short_endurance_label = if app.locale.is_zh() { endurance_label } else { "Used: " };
-        let inner_width = area.width.saturating_sub(2) as usize;
-
-        let build_telemetry_line = |endurance_label: &'static str| -> Line<'static> {
-            let mut spans = vec![
-                Span::styled(status_label, Style::default().fg(MUTED)),
-                Span::styled(status_text, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
-            ];
-            if !is_healthy && !dev.smart.warning_flags.is_empty() {
-                spans.push(Span::styled(
-                    format!(" ({})", dev.smart.warning_flags.join(", ")),
-                    Style::default().fg(CORAL),
-                ));
-            }
-            spans.extend(vec![
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(temp_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{:.1} °C", dev.smart.temperature_c), Style::default().fg(BONE)),
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(endurance_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{}%", dev.smart.percentage_used), Style::default().fg(BONE)),
-                Span::styled(" · ", Style::default().fg(MUTED)),
-                Span::styled(spare_label, Style::default().fg(MUTED)),
-                Span::styled(format!("{}%", dev.smart.available_spare_percent), Style::default().fg(BONE)),
-            ]);
-            Line::from(spans)
-        };
-        let full_line = build_telemetry_line(endurance_label);
-        let line2 = if full_line.width() > inner_width {
-            build_telemetry_line(short_endurance_label)
+        lines.extend(dev_lines);
+        used_rows += rows;
+    }
+    if more > 0 && budget - used_rows >= 1 {
+        let more_line = if app.locale.is_zh() {
+            format!("... {} {}", app.locale.text("storage_more_devices"), more)
         } else {
-            full_line
+            format!("... {} {}", more, app.locale.text("storage_more_devices"))
         };
-        lines.push(line2);
-
-        // Line 3: Data read & written plus the available-spare threshold
-        let io_label = if app.locale.is_zh() { "读写: " } else { "I/O: " };
-        let threshold_label = if app.locale.is_zh() { "阈值" } else { "threshold" };
-        let read_str = crate::format_bytes(dev.smart.data_read_bytes);
-        let write_str = crate::format_bytes(dev.smart.data_written_bytes);
-
-        lines.push(Line::from(vec![
-            Span::styled(io_label, Style::default().fg(MUTED)),
-            Span::styled(format!("Read {read_str} / Written {write_str}"), Style::default().fg(BONE)),
-            Span::styled(" · ", Style::default().fg(MUTED)),
-            Span::styled(
-                format!("{threshold_label} {}%", dev.smart.spare_threshold_percent),
-                Style::default().fg(BONE),
-            ),
-        ]));
+        lines.push(Line::from(vec![Span::styled(
+            more_line,
+            Style::default().fg(MUTED),
+        )]));
     }
 
     frame.render_widget(
         Paragraph::new(lines)
             .block(block)
             .wrap(Wrap { trim: true }),
         area,
     );
 }
 
+fn wrapped_rows(lines: &[Line<'static>], inner_width: usize) -> usize {
+    // Ratatui wraps each line at the paragraph's inner width; count the
+    // rendered rows the way the paragraph will, so the card height and the
+    // truncation budget agree with what actually fits.
+    if inner_width == 0 {
+        return lines.len().max(1);
+    }
+    lines
+        .iter()
+        .map(|line| {
+            if line.width() == 0 {
+                1
+            } else {
+                (line.width().saturating_sub(1) / inner_width) + 1
+            }
+        })
+        .sum()
+}
+
+fn nvme_device_lines(
+    app: &App,
+    dev: &rsetup_core::NvmeDevice,
+    inner_width: usize,
+) -> Vec<Line<'static>> {
+    let size_str = crate::format_bytes(dev.total_bytes);
+    // Line 1: dev.name, dev.path, model, capacity
+    let line1 = Line::from(vec![
+        Span::styled(format!("{} ", dev.name), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
+        Span::styled(format!("({}) · ", dev.path), Style::default().fg(MUTED)),
+        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
+        Span::styled(size_str, Style::default().fg(AMBER)),
+    ]);
+
+    // Line 2: Health state, temperature, used endurance, available spare
+    let is_healthy = dev.smart.critical_warning == 0 && dev.smart.warning_flags.is_empty();
+    let (status_text, status_color) = if is_healthy {
+        (app.locale.text("nvme_healthy"), SIGNAL)
+    } else {
+        (app.locale.text("nvme_warning"), CORAL)
+    };
+    let status_label = if app.locale.is_zh() { "状态: " } else { "Health: " };
+    let temp_label = if app.locale.is_zh() { "温度: " } else { "Temp: " };
+    let spare_label = if app.locale.is_zh() { "备用: " } else { "Spare: " };
+    let endurance_label = if app.locale.is_zh() { "已用寿命: " } else { "Used Endurance: " };
+    // "Used Endurance:" is long enough to push the mandatory available-spare
+    // value past the 59 columns the mission panel gets on a 100-column
+    // terminal, so fall back to the short spelling when the full one does
+    // not fit next to the warning flags.
+    let short_endurance_label = if app.locale.is_zh() { endurance_label } else { "Used: " };
+
+    let build_telemetry_line = |endurance_label: &'static str| -> Line<'static> {
+        let mut spans = vec![
+            Span::styled(status_label, Style::default().fg(MUTED)),
+            Span::styled(status_text, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
+        ];
+        if !is_healthy && !dev.smart.warning_flags.is_empty() {
+            spans.push(Span::styled(
+                format!(" ({})", dev.smart.warning_flags.join(", ")),
+                Style::default().fg(CORAL),
+            ));
+        }
+        spans.extend(vec![
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(temp_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{:.1} °C", dev.smart.temperature_c), Style::default().fg(BONE)),
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(endurance_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{}%", dev.smart.percentage_used), Style::default().fg(BONE)),
+            Span::styled(" · ", Style::default().fg(MUTED)),
+            Span::styled(spare_label, Style::default().fg(MUTED)),
+            Span::styled(format!("{}%", dev.smart.available_spare_percent), Style::default().fg(BONE)),
+        ]);
+        Line::from(spans)
+    };
+    let full_line = build_telemetry_line(endurance_label);
+    let line2 = if full_line.width() > inner_width {
+        build_telemetry_line(short_endurance_label)
+    } else {
+        full_line
+    };
+
+    // Line 3: Data read & written plus the available-spare threshold
+    let io_label = if app.locale.is_zh() { "读写: " } else { "I/O: " };
+    let threshold_label = if app.locale.is_zh() { "阈值" } else { "threshold" };
+    let read_str = crate::format_bytes(dev.smart.data_read_bytes);
+    let write_str = crate::format_bytes(dev.smart.data_written_bytes);
+
+    let line3 = Line::from(vec![
+        Span::styled(io_label, Style::default().fg(MUTED)),
+        Span::styled(format!("Read {read_str} / Written {write_str}"), Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(
+            format!("{threshold_label} {}%", dev.smart.spare_threshold_percent),
+            Style::default().fg(BONE),
+        ),
+    ]);
+
+    vec![line1, line2, line3]
+}
+
+fn mmc_device_lines(app: &App, dev: &rsetup_core::MmcDevice) -> Vec<Line<'static>> {
+    // Line 1: [type] block_path · model · manufacturer · capacity
+    let type_label = if dev.card_type == "MMC" {
+        app.locale.text("storage_emmc")
+    } else {
+        app.locale.text("storage_sd")
+    };
+    let line1 = Line::from(vec![
+        Span::styled(format!("[{}] ", type_label), Style::default().fg(BONE).add_modifier(Modifier::BOLD)),
+        Span::styled(format!("{} · ", dev.block_path), Style::default().fg(BONE)),
+        Span::styled(format!("{} · ", dev.model), Style::default().fg(BONE)),
+        Span::styled(format!("{} · ", dev.manufacturer), Style::default().fg(MUTED)),
+        Span::styled(crate::format_bytes(dev.total_bytes), Style::default().fg(AMBER)),
+    ]);
+
+    // Line 2: Health state, SLC/MLC life estimates, pre-EOL warning
+    let health_label = format!("{}: ", app.locale.text("storage_health"));
+    let life_a_label = format!("{}: ", app.locale.text("storage_life_a"));
+    let life_b_label = format!("{}: ", app.locale.text("storage_life_b"));
+    let pre_eol_label = format!("{}: ", app.locale.text("storage_pre_eol"));
+    let (health_text, health_color) =
+        if !dev.health.warning_flags.is_empty() || dev.health.pre_eol_info == 3 {
+            (app.locale.text("storage_critical"), CORAL)
+        } else if dev.health.pre_eol_info == 2 {
+            (app.locale.text("storage_warning"), AMBER)
+        } else {
+            (app.locale.text("storage_healthy"), SIGNAL)
+        };
+    let life_a = dev
+        .health
+        .life_time_est_a_percent
+        .map(|p| format!("{p}%"))
+        .unwrap_or_else(|| app.locale.text("storage_na").to_string());
+    let life_b = dev
+        .health
+        .life_time_est_b_percent
+        .map(|p| format!("{p}%"))
+        .unwrap_or_else(|| app.locale.text("storage_na").to_string());
+    let eol = match dev.health.pre_eol_info {
+        0 => app.locale.text("storage_eol_undefined"),
+        1 => app.locale.text("storage_eol_normal"),
+        2 => app.locale.text("storage_eol_warning"),
+        3 => app.locale.text("storage_eol_urgent"),
+        _ => app.locale.text("storage_eol_undefined"),
+    };
+    let line2 = Line::from(vec![
+        Span::styled(health_label, Style::default().fg(MUTED)),
+        Span::styled(health_text, Style::default().fg(health_color).add_modifier(Modifier::BOLD)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(life_a_label, Style::default().fg(MUTED)),
+        Span::styled(life_a, Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(life_b_label, Style::default().fg(MUTED)),
+        Span::styled(life_b, Style::default().fg(BONE)),
+        Span::styled(" · ", Style::default().fg(MUTED)),
+        Span::styled(pre_eol_label, Style::default().fg(MUTED)),
+        Span::styled(eol, Style::default().fg(BONE)),
+    ]);
+
+    vec![line1, line2]
+}
+
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
@@ -876,67 +1046,67 @@ mod tests {
         let mut app = App::new(controller, Locale::En).expect("init app");
         // Demo mode returns 2 MMC devices (eMMC + SD).
         assert!(app.mmc_status.initialized);
         assert_eq!(app.mmc_status.devices.len(), 2);
         app.refresh().expect("refresh app");
         assert!(app.mmc_status.initialized);
         assert_eq!(app.mmc_status.devices.len(), 2);
     }
 
     #[test]
-    fn test_render_nvme_summary_demo() {
+    fn test_render_storage_summary_demo() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let app = App::new(controller, Locale::ZhCn).expect("init app");
         let backend = ratatui::backend::TestBackend::new(80, 25);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 80, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
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
-    fn test_render_nvme_summary_fits_available_spare() {
+    fn test_render_storage_summary_fits_available_spare() {
         // Real device geometry: left panel is 61% of a 100-column terminal,
-        // so the NVMe box has 61 display columns (59 inner columns).
+        // so the storage box has 61 display columns (59 inner columns).
         for (locale, spare_label, threshold_label) in
             [(Locale::ZhCn, "备用", "阈值"), (Locale::En, "Spare", "threshold")]
         {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, locale).expect("init app");
             let mut dev = app.nvme_status.devices[0].clone();
             // Mirror the real Rock 5B device: 100% spare against a 1% threshold.
             dev.smart.available_spare_percent = 100;
             dev.smart.spare_threshold_percent = 1;
             app.nvme_status.devices = vec![dev];
 
             let backend = ratatui::backend::TestBackend::new(61, 6);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
-                    render_nvme_summary(frame, &app, Rect::new(0, 0, 61, 6));
+                    render_storage_summary(frame, &app, Rect::new(0, 0, 61, 6));
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
 
             assert!(
                 norm_text.contains(&format!("{spare_label}:100%")),
                 "Expected available spare '{spare_label}: 100%' inside 59 columns, got: {raw_text:?}"
             );
@@ -945,29 +1115,29 @@ mod tests {
                 "Expected spare threshold '1%' (label '{threshold_label}') in buffer, got: {raw_text:?}"
             );
             assert!(
                 norm_text.contains(threshold_label),
                 "Expected threshold label '{threshold_label}' in buffer, got: {raw_text:?}"
             );
         }
     }
 
     #[test]
-    fn test_render_nvme_summary_uses_full_endurance_label_when_wide() {
+    fn test_render_storage_summary_uses_full_endurance_label_when_wide() {
         let render_norm = |locale: Locale, width: u16| -> String {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let app = App::new(controller, locale).expect("init app");
             let backend = ratatui::backend::TestBackend::new(width, 6);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
-                    render_nvme_summary(frame, &app, Rect::new(0, 0, width, 6));
+                    render_storage_summary(frame, &app, Rect::new(0, 0, width, 6));
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
@@ -997,134 +1167,246 @@ mod tests {
         let backend = ratatui::backend::TestBackend::new(100, 30);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 render(frame, &mut app);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
-        assert!(norm_text.contains("NVMe存储遥测"), "Expected 'NVMe 存储遥测' in buffer, got: {raw_text}");
+        assert!(norm_text.contains("存储状态"), "Expected '存储状态' in buffer, got: {raw_text}");
         assert!(raw_text.contains("Radxa M.2 NVMe SSD 512GB"), "Expected 'Radxa M.2 NVMe SSD 512GB' in buffer");
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
-    fn test_render_full_tui_uninitialized_nvme() {
+    fn test_render_full_tui_no_storage_devices() {
         // Test in Chinese
         {
             let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
             let mut app = App::new(controller, Locale::ZhCn).expect("init app");
             app.nvme_status = rsetup_core::NvmeStatus {
                 initialized: false,
                 devices: vec![],
                 message: Some("No NVMe".into()),
             };
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("No MMC".into()),
+            };
             let backend = ratatui::backend::TestBackend::new(100, 30);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
                     render(frame, &mut app);
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
             assert!(
-                norm_text.contains("未检测到NVMe存储设备，模块未激活"),
-                "Expected Chinese uninitialized message in buffer, got: {raw_text}"
+                norm_text.contains("未检测到NVMe或MMC存储设备"),
+                "Expected Chinese no-storage message in buffer, got: {raw_text}"
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
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: Some("No MMC".into()),
+            };
             let backend = ratatui::backend::TestBackend::new(100, 30);
             let mut terminal = Terminal::new(backend).expect("init test terminal");
             terminal
                 .draw(|frame| {
                     render(frame, &mut app);
                 })
                 .expect("draw");
             let buffer = terminal.backend().buffer();
             let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
             assert!(
-                raw_text.contains("No NVMe storage devices detected; module is uninitialized."),
-                "Expected English uninitialized message in buffer, got: {raw_text}"
+                raw_text.contains("No NVMe or MMC storage devices detected."),
+                "Expected English no-storage message in buffer, got: {raw_text}"
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
-    fn test_render_nvme_summary_warning_state() {
+    fn test_render_storage_summary_warning_state() {
         let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
         let mut app = App::new(controller, Locale::ZhCn).expect("init app");
         let mut dev = app.nvme_status.devices[0].clone();
         dev.smart.critical_warning = 0x03;
         dev.smart.warning_flags = vec!["spare_below_threshold".into(), "temperature_exceeded".into()];
         app.nvme_status.devices = vec![dev];
 
         let backend = ratatui::backend::TestBackend::new(100, 10);
         let mut terminal = Terminal::new(backend).expect("init test terminal");
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 100, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
         assert!(norm_text.contains("告警"), "Expected '告警' in buffer, got: {raw_text}");
         assert!(raw_text.contains("spare_below_threshold"), "Expected 'spare_below_threshold' in buffer");
         assert!(raw_text.contains("temperature_exceeded"), "Expected 'temperature_exceeded' in buffer");
 
         // Also test English locale
         app.locale = Locale::En;
         terminal
             .draw(|frame| {
                 let area = Rect::new(0, 0, 100, 6);
-                render_nvme_summary(frame, &app, area);
+                render_storage_summary(frame, &app, area);
             })
             .expect("draw");
         let buffer = terminal.backend().buffer();
         let text_en = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
         assert!(text_en.contains("Warning"), "Expected 'Warning' in buffer, got: {text_en}");
         assert!(text_en.contains("spare_below_threshold"), "Expected 'spare_below_threshold' in buffer");
     }
+
+    #[test]
+    fn test_render_storage_summary_mmc_only() {
+        // MMC devices only: NVMe uninitialized, no not-detected notice shown.
+        // The full TUI at 100x20 clamps the card to its minimum height, so the
+        // card is rendered directly on a large area to check its contents.
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::ZhCn).expect("init app");
+        app.nvme_status = rsetup_core::NvmeStatus {
+            initialized: false,
+            devices: vec![],
+            message: None,
+        };
+        let backend = ratatui::backend::TestBackend::new(100, 20);
+        let mut terminal = Terminal::new(backend).expect("init test terminal");
+        terminal
+            .draw(|frame| {
+                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
+        assert!(raw_text.contains("FE4MB4"), "Expected eMMC model in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk0"), "Expected eMMC path in buffer, got: {raw_text}");
+        assert!(raw_text.contains("SC64G"), "Expected SD model in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk1"), "Expected SD path in buffer, got: {raw_text}");
+        assert!(raw_text.contains("10%"), "Expected eMMC life value in buffer, got: {raw_text}");
+        assert!(norm_text.contains("存储状态"), "Expected card title in buffer, got: {raw_text}");
+
+        // English locale: type labels and the N/A life values for the SD card.
+        app.locale = Locale::En;
+        terminal
+            .draw(|frame| {
+                render_storage_summary(frame, &app, Rect::new(0, 0, 80, 12));
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_en = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        assert!(raw_en.contains("eMMC"), "Expected 'eMMC' label in buffer, got: {raw_en}");
+        assert!(raw_en.contains("SD Card"), "Expected 'SD Card' label in buffer, got: {raw_en}");
+        assert!(raw_en.contains("N/A"), "Expected 'N/A' life for SD card, got: {raw_en}");
+    }
+
+    #[test]
+    fn test_render_storage_summary_multi_device_all_visible_when_tall() {
+        // On a tall terminal the card has room for all demo devices:
+        // 1 NVMe + 2 MMC must all be visible without truncation.
+        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+        let mut app = App::new(controller, Locale::En).expect("init app");
+        let backend = ratatui::backend::TestBackend::new(100, 40);
+        let mut terminal = Terminal::new(backend).expect("init test terminal");
+        terminal
+            .draw(|frame| {
+                render(frame, &mut app);
+            })
+            .expect("draw");
+        let buffer = terminal.backend().buffer();
+        let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+        assert!(raw_text.contains("nvme0"), "Expected NVMe device in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk0"), "Expected eMMC device in buffer, got: {raw_text}");
+        assert!(raw_text.contains("/dev/mmcblk1"), "Expected SD device in buffer, got: {raw_text}");
+    }
+
+    #[test]
+    fn test_render_storage_summary_empty() {
+        for locale in [Locale::ZhCn, Locale::En] {
+            let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
+            let mut app = App::new(controller, locale).expect("init app");
+            app.nvme_status = rsetup_core::NvmeStatus {
+                initialized: false,
+                devices: vec![],
+                message: None,
+            };
+            app.mmc_status = rsetup_core::MmcStatus {
+                initialized: false,
+                devices: vec![],
+                message: None,
+            };
+            let backend = ratatui::backend::TestBackend::new(100, 20);
+            let mut terminal = Terminal::new(backend).expect("init test terminal");
+            terminal
+                .draw(|frame| {
+                    render_storage_summary(frame, &app, Rect::new(0, 0, 80, 10));
+                })
+                .expect("draw");
+            let buffer = terminal.backend().buffer();
+            let raw_text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
+            let norm_text = raw_text.split_whitespace().collect::<Vec<_>>().join("");
+            if locale == Locale::ZhCn {
+                assert!(
+                    norm_text.contains("未检测到NVMe或MMC存储设备"),
+                    "Expected Chinese no-storage message in buffer, got: {raw_text}"
+                );
+            } else {
+                assert!(
+                    raw_text.contains("No NVMe or MMC storage devices detected."),
+                    "Expected English no-storage message in buffer, got: {raw_text}"
+                );
+            }
+        }
+    }
 }
 
