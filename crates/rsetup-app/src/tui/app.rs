use crate::i18n::Locale;
use anyhow::{Result, anyhow};
use crossterm::event::KeyCode;
use rsetup_core::{
    ActionRun, ActionSpec, Controller, DeviceSnapshot, ExecutionPolicy, RiskLevel, SourcePlan,
    SourceStatus,
};

use crate::tui::install::backend::system_install_manager;
use crate::tui::install::{InstallFocus, InstallPanel};

pub(in crate::tui) struct App {
    controller: Controller,
    pub(in crate::tui) locale: Locale,
    pub(in crate::tui) snapshot: DeviceSnapshot,
    pub(in crate::tui) actions: Vec<ActionSpec>,
    pub(in crate::tui) selected: usize,
    pub(in crate::tui) confirm_pending: bool,
    pub(in crate::tui) last_run: Option<ActionRun>,
    pub(in crate::tui) source_status: SourceStatus,
    pub(in crate::tui) source_plan: Option<SourcePlan>,
    pub(in crate::tui) source_picker: bool,
    pub(in crate::tui) source_selected: usize,
    pub(in crate::tui) notice: Option<String>,
    pub(in crate::tui) nvme_status: rsetup_core::NvmeStatus,
    pub(in crate::tui) mmc_status: rsetup_core::MmcStatus,
    pub(in crate::tui) nvme_error: Option<rsetup_core::HardwareError>,
    pub(in crate::tui) mmc_error: Option<rsetup_core::HardwareError>,
    pub(in crate::tui) benchmark_rx: Option<
        std::sync::mpsc::Receiver<Result<rsetup_core::MirrorBenchmark, rsetup_core::SourceError>>,
    >,
    pub(in crate::tui) benchmarks: std::collections::BTreeMap<String, rsetup_core::MirrorBenchmark>,
    pub(in crate::tui) install: InstallPanel,
}

impl App {
    pub(in crate::tui) fn new(controller: Controller, locale: Locale) -> Result<Self> {
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

    pub(in crate::tui) fn next(&mut self) {
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

    pub(in crate::tui) fn start_benchmark(&mut self) {
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

    pub(in crate::tui) fn poll_benchmark(&mut self) {
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

    pub(in crate::tui) fn previous(&mut self) {
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
    pub(in crate::tui) fn install_key(&mut self, code: KeyCode) {
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

    pub(in crate::tui) fn refresh(&mut self) -> Result<()> {
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

    pub(in crate::tui) fn request_run(&mut self) {
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

    pub(in crate::tui) fn execute_selected(&mut self) -> Result<()> {
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

    pub(in crate::tui) fn close_source_picker(&mut self) {
        self.source_picker = false;
        self.confirm_pending = false;
        self.source_plan = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::storage::render_storage_summary;
    use ratatui::{Terminal, layout::Rect};
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
