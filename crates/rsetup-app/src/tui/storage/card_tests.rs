use super::mmc::mmc_device_lines;
use super::{render_storage_summary, storage_error_lines};
use crate::i18n::Locale;
use crate::tui::fixtures::{
    eol_text, flatten_screen, render_mission_rows, rock3a_mmc_app, rock3a_warning_app,
};
use crate::tui::textfit::text_width;
use crate::tui::{app::App, shell::render};
use ratatui::{Terminal, layout::Rect};
use rsetup_core::{Controller, ExecutionPolicy, ProbeMode};

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

#[test]
fn test_render_storage_summary_keeps_mmc_capacity_on_one_row() {
    for locale in [Locale::En, Locale::ZhCn] {
        let app = rock3a_mmc_app(locale);
        let rows = render_storage_rows(&app, 61, 8);
        let screen = rows.join("\n");
        let row = rows
            .iter()
            .find(|row| row.contains("58.24 GiB"))
            .unwrap_or_else(|| panic!("{locale:?}: capacity fell off the device row:\n{screen}"));
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
            let hinted = flat.contains(&format!("1{label}")) || flat.contains(&format!("{label}1"));
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

// ---- second-review findings (mmc round 2): RED first ----

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
        let shown = usize::from(flat.contains("mmcblk0")) + usize::from(flat.contains("mmcblk1"));

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
