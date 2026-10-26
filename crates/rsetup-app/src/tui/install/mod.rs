pub(in crate::tui) mod backend;
pub(in crate::tui) mod render;

use crate::i18n::Locale;
use rsetup_core::{
    SystemInstallApplyResult, SystemInstallError, SystemInstallManager, SystemInstallPlan,
    SystemInstallRequest, SystemInstallTarget,
};

/// Which part of the panel owns typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum InstallFocus {
    Targets,
    Image,
    Confirm,
}

/// What the panel reports under the field: a core error, or a state that has no
/// core error of its own.
#[derive(Debug)]
enum InstallStatus {
    Error(SystemInstallError),
    Info(&'static str),
    Written,
    /// The target is real but the write was withheld: `--live-execution` was not
    /// given, so the bytes were only counted.
    Planned,
    /// The administrator dismissed the authorization prompt. Kept apart from
    /// `Error` because nothing failed: the write was declined.
    Canceled,
    /// A refusal the privileged helper classified and reported over its error
    /// envelope, carrying the helper's own stable code and explanation.
    Helper {
        code: String,
        message: String,
    },
}

/// The install panel. It owns a core manager, so every gate, every image check
/// and the typed confirmation itself are the core's decisions; the panel only
/// presents them and routes keystrokes.
pub(in crate::tui) struct InstallPanel {
    manager: SystemInstallManager,
    pub(in crate::tui) open: bool,
    targets: Vec<SystemInstallTarget>,
    selected: usize,
    image_path: String,
    pub(in crate::tui) focus: InstallFocus,
    plan: Option<SystemInstallPlan>,
    typed: String,
    status: Option<InstallStatus>,
    result: Option<SystemInstallApplyResult>,
    /// Whether Enter on a matching name really writes (policy live and a real
    /// device backend). The hint is only a label; the core still decides.
    live_write: bool,
    /// Whether that write has to go through the fixed-verb helper instead of
    /// this process. Fixed for the session: neither the policy, the provider nor
    /// the uid can change while the panel is open.
    privileged: bool,
}

impl InstallPanel {
    pub(in crate::tui) fn from_manager(
        manager: SystemInstallManager,
        live_write: bool,
        privileged: bool,
    ) -> Self {
        let mut panel = Self {
            manager,
            live_write,
            privileged,
            open: false,
            targets: Vec::new(),
            selected: 0,
            image_path: String::new(),
            focus: InstallFocus::Targets,
            plan: None,
            typed: String::new(),
            status: None,
            result: None,
        };
        panel.refresh_targets();
        panel
    }

    pub(in crate::tui) fn open_panel(&mut self) {
        self.open = true;
        self.refresh_targets();
    }

    pub(in crate::tui) fn close(&mut self) {
        self.open = false;
        // Leaving the panel drops the plan and the typed name, so a stale token
        // can never be applied from a screen the user has left.
        self.plan = None;
        self.typed.clear();
        self.result = None;
        self.status = None;
    }

    pub(in crate::tui) fn refresh_targets(&mut self) {
        self.plan = None;
        self.result = None;
        self.status = None;
        self.selected = 0;
        match self.manager.targets() {
            Ok(targets) => self.targets = targets,
            Err(error) => {
                self.targets.clear();
                self.status = Some(InstallStatus::Error(error));
            }
        }
    }

    fn selected_target(&self) -> Option<&SystemInstallTarget> {
        self.targets.get(self.selected)
    }

    pub(in crate::tui) fn next_target(&mut self) {
        if self.focus != InstallFocus::Targets {
            return;
        }
        self.selected = (self.selected + 1).min(self.targets.len().saturating_sub(1));
        self.load_plan();
    }

    pub(in crate::tui) fn previous_target(&mut self) {
        if self.focus != InstallFocus::Targets {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.load_plan();
    }

    pub(in crate::tui) fn focus_next(&mut self) {
        self.focus = match self.focus {
            InstallFocus::Targets => InstallFocus::Image,
            InstallFocus::Image => InstallFocus::Confirm,
            InstallFocus::Confirm => InstallFocus::Targets,
        };
    }

    pub(in crate::tui) fn push(&mut self, character: char) {
        match self.focus {
            InstallFocus::Targets => {}
            InstallFocus::Image => self.image_path.push(character),
            // The typed name is passed to the core verbatim, trimmed the same
            // way the CLI trims it.
            InstallFocus::Confirm => self.typed.push(character),
        }
    }

    pub(in crate::tui) fn backspace(&mut self) {
        match self.focus {
            InstallFocus::Targets => {}
            InstallFocus::Image => {
                self.image_path.pop();
            }
            InstallFocus::Confirm => {
                self.typed.pop();
            }
        }
    }

    /// Enter: load the plan from the selected target and image, or ask the core
    /// to write.
    pub(in crate::tui) fn confirm(&mut self, locale: &Locale) {
        match self.focus {
            InstallFocus::Targets | InstallFocus::Image => self.load_plan(),
            InstallFocus::Confirm => self.attempt(locale),
        }
    }

    fn load_plan(&mut self) {
        self.result = None;
        let Some(target) = self.selected_target() else {
            self.plan = None;
            self.status = Some(InstallStatus::Info("install_no_targets"));
            return;
        };
        let request = SystemInstallRequest {
            target_id: target.id.clone(),
            image_path: Some(self.image_path.trim().to_owned()),
            image_id: None,
        };
        match self.manager.plan(&request) {
            Ok(plan) => {
                self.plan = Some(plan);
                self.typed.clear();
                self.status = None;
            }
            Err(error) => {
                // A refused plan (running system, mounted target, bad image) is
                // reported exactly as the core phrased it.
                self.plan = None;
                self.status = Some(InstallStatus::Error(error));
            }
        }
    }

    /// The destructive action. The typed name goes to the core untouched: an
    /// empty value is `confirmation_required`, a wrong one is
    /// `acknowledgement_mismatch`, and only the exact target name writes.
    fn attempt(&mut self, _locale: &Locale) {
        let Some(plan) = self.plan.as_ref() else {
            self.status = Some(InstallStatus::Info("install_not_planned"));
            return;
        };
        let request = plan.request.clone();
        let plan_token = plan.plan_token.clone();
        // Same routing as the CLI: an ordinary user's live write is handed to the
        // helper, which asks the administrator, rather than being attempted here
        // and failing on the device node. The typed name still travels with it
        // and is judged by the core on the other side.
        let applied = if self.privileged {
            crate::run_privileged_system_install(&request, &plan_token, &self.typed)
        } else {
            self.manager
                .apply_live(&request, &plan_token, &self.typed)
                .map_err(crate::InstallApplyError::Core)
        };
        match applied {
            Ok(result) => {
                // A dry run reports a real target but writes nothing, so the panel
                // must not announce a write the result itself denies.
                self.status = Some(if result.dry_run {
                    InstallStatus::Planned
                } else {
                    InstallStatus::Written
                });
                self.result = Some(result);
            }
            Err(error) => {
                self.result = None;
                self.status = Some(match error {
                    crate::InstallApplyError::Core(error) => InstallStatus::Error(error),
                    crate::InstallApplyError::AuthorizationCanceled => InstallStatus::Canceled,
                    // The helper classified it; keep its code and wording rather
                    // than collapsing them into a generic failure.
                    crate::InstallApplyError::Helper { code, message } => {
                        InstallStatus::Helper { code, message }
                    }
                });
            }
        }
    }

    /// Affordance only: it mirrors the core's rule using the plan's own expected
    /// device name so the panel can show the destructive key as disabled. The
    /// authoritative judgement is still the core's, which is what `attempt`
    /// calls.
    fn acknowledgement_ready(&self) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            let typed = self.typed.trim();
            !typed.is_empty() && typed == plan.target.id
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{app::App, shell::render};
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use rsetup_core::{
        BlockDeviceBackend, Controller, ExecutionPolicy, ProbeMode, SyntheticBackend,
        SystemImageOpener, SystemInstallPartition, SystemInstallVerificationState, SystemMount,
    };

    /// Scratch directory under `target/`, removed when the test ends.
    struct InstallScratch(std::path::PathBuf);

    impl std::ops::Deref for InstallScratch {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for InstallScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn install_scratch(name: &str) -> InstallScratch {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/system-install-tui-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create scratch directory");
        InstallScratch(root)
    }

    /// A minimal image with a valid MBR.
    fn install_image_bytes() -> Vec<u8> {
        let mut image = vec![0u8; 2 * 1024 * 1024];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x83;
        image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
        image
    }

    /// Counts every byte handed to a device, so a test can prove that nothing
    /// was written when the confirmation was refused. It wraps the demo device
    /// list, so no real block device is involved.
    struct RecordingBackend {
        inner: SyntheticBackend,
        writes: std::sync::Arc<std::sync::Mutex<u64>>,
    }

    impl BlockDeviceBackend for RecordingBackend {
        fn targets(&self) -> std::result::Result<Vec<SystemInstallTarget>, SystemInstallError> {
            self.inner.targets()
        }

        fn mounts(&self) -> std::result::Result<Vec<SystemMount>, SystemInstallError> {
            self.inner.mounts()
        }

        fn write_chunk(
            &self,
            _target: &SystemInstallTarget,
            _offset: u64,
            chunk: &[u8],
        ) -> std::result::Result<(), SystemInstallError> {
            *self.writes.lock().expect("write counter") += chunk.len() as u64;
            Ok(())
        }

        fn reread_partitions(
            &self,
            _target: &SystemInstallTarget,
        ) -> std::result::Result<Vec<SystemInstallPartition>, SystemInstallError> {
            Err(SystemInstallError::Unsupported(
                "the recording backend does not re-read partition tables".into(),
            ))
        }
    }

    fn recording_panel() -> (InstallPanel, std::sync::Arc<std::sync::Mutex<u64>>) {
        let writes = std::sync::Arc::new(std::sync::Mutex::new(0));
        let backend = std::sync::Arc::new(RecordingBackend {
            inner: SyntheticBackend::default(),
            writes: writes.clone(),
        });
        let manager = SystemInstallManager::with_backend(
            backend,
            std::sync::Arc::new(SystemImageOpener),
            true,
        );
        // Never privileged: this panel exists to observe an in-process write.
        (InstallPanel::from_manager(manager, false, false), writes)
    }

    fn demo_app(locale: Locale) -> App {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        App::new(controller, locale).expect("init app")
    }

    fn render_app(app: &mut App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn select_target(panel: &mut InstallPanel, id: &str) {
        panel.selected = panel
            .targets
            .iter()
            .position(|target| target.id == id)
            .unwrap_or_else(|| panic!("{id} is not among the demo targets"));
    }

    fn type_into(panel: &mut InstallPanel, text: &str) {
        for character in text.chars() {
            panel.push(character);
        }
    }

    fn written(writes: &std::sync::Arc<std::sync::Mutex<u64>>) -> u64 {
        *writes.lock().expect("write counter")
    }

    #[test]
    fn install_panel_shows_every_consequence_and_never_writes_before_the_name_matches() {
        let scratch = install_scratch("panel");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        select_target(&mut app.install, "sda");

        // Step 2: type the image path, then load the plan with Enter.
        app.install.focus_next();
        assert_eq!(app.install.focus, InstallFocus::Image);
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        let plan = app.install.plan.as_ref().expect("plan loaded");
        let sha = plan.image.sha256_prefix.clone();
        let capacity = crate::format_bytes(plan.target.size_bytes);

        let text = render_app(&mut app, 140, 60);
        // Target identity, the destroyed partitions, the image identity and both
        // irreversible warnings are on screen before anything can be written.
        assert!(text.contains("/dev/sda"), "{text}");
        assert!(text.contains("RADXA UFS 128GB"), "{text}");
        assert!(text.contains("UFS"), "{text}");
        assert!(text.contains(&capacity), "{text}");
        assert!(text.contains("Existing partitions destroyed (3)"), "{text}");
        assert!(text.contains("sda1"), "{text}");
        assert!(text.contains("SHA-256 (first 1 MiB)"), "{text}");
        assert!(text.contains(&sha), "{text}");
        assert!(text.contains("MBR"), "{text}");
        assert!(
            text.contains("Writing replaces every partition on the target."),
            "{text}"
        );
        assert!(text.contains("This write cannot be rolled back."), "{text}");
        assert!(
            text.contains("Administrator authorization required"),
            "{text}"
        );
        // The destructive key is shown as disabled, and nothing was written.
        assert!(
            text.contains("Disabled: the typed device name does not match"),
            "{text}"
        );
        assert_eq!(
            written(&writes),
            0,
            "nothing may be written before confirming"
        );

        // A boolean-looking value is refused by the core, and still nothing is
        // written.
        app.install.focus_next();
        assert_eq!(app.install.focus, InstallFocus::Confirm);
        type_into(&mut app.install, "true");
        app.install.confirm(&app.locale);
        match &app.install.status {
            Some(InstallStatus::Error(SystemInstallError::AcknowledgementMismatch {
                expected,
                provided,
                ..
            })) => {
                assert_eq!(expected, "sda");
                assert_eq!(provided, "true");
            }
            other => panic!("a boolean must not confirm: {other:?}"),
        }
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("acknowledgement_mismatch"), "{text}");
        assert_eq!(written(&writes), 0);

        // An empty value is not a confirmation either.
        for _ in 0.."true".len() {
            app.install.backspace();
        }
        app.install.confirm(&app.locale);
        assert!(matches!(
            app.install.status,
            Some(InstallStatus::Error(
                SystemInstallError::ConfirmationRequired
            ))
        ));
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("confirmation_required"), "{text}");
        assert_eq!(written(&writes), 0);

        // The exact device name enables the write.
        type_into(&mut app.install, "sda");
        let text = render_app(&mut app, 140, 60);
        assert!(
            !text.contains("Disabled: the typed device name does not match"),
            "{text}"
        );
        assert!(
            text.contains("Dry run: Enter only simulates the write"),
            "{text}"
        );
        app.install.confirm(&app.locale);
        let result = app.install.result.as_ref().expect("the write ran");
        assert_eq!(result.plan.target.id, "sda");
        assert_eq!(result.bytes_written, install_image_bytes().len() as u64);
        assert_eq!(written(&writes), result.bytes_written);
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("Write complete"), "{text}");
        assert!(
            text.contains("Synthetic target: no real device was written"),
            "{text}"
        );
    }

    #[test]
    fn install_key_routes_typing_listing_and_confirmation() {
        let (panel, writes) = recording_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        app.install.open_panel();

        // TAB moves the focus; characters land in the focused field, including
        // the J/K/R keys that drive the list.
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Image);
        for character in "/tmp/rock/r.img".chars() {
            app.install_key(KeyCode::Char(character));
        }
        assert_eq!(app.install.image_path, "/tmp/rock/r.img");
        app.install_key(KeyCode::Backspace);
        assert_eq!(app.install.image_path, "/tmp/rock/r.im");

        // The missing image is reported by the core when the plan is loaded.
        app.install_key(KeyCode::Enter);
        assert!(
            matches!(
                app.install.status,
                Some(InstallStatus::Error(SystemInstallError::InvalidInput(_)))
            ),
            "{:?}",
            app.install.status
        );

        // Back on the list, J/K move the selection and R reloads the devices.
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Confirm);
        app.install_key(KeyCode::Tab);
        assert_eq!(app.install.focus, InstallFocus::Targets);
        let before = app.install.selected;
        app.install_key(KeyCode::Char('j'));
        assert_ne!(app.install.selected, before);
        app.install_key(KeyCode::Char('k'));
        assert_eq!(app.install.selected, before);
        app.install_key(KeyCode::Char('r'));
        assert!(app.install.plan.is_none());
        assert!(!app.install.targets.is_empty());
        assert_eq!(written(&writes), 0);
    }

    /// A finished write whose verification is one of the two shapes core reports as
    /// "unsupported": a compressed image (the table was re-read, there was nothing
    /// to compare it against) and a host that cannot re-read a table at all.
    fn finished_install(locale: Locale, observed: bool) -> App {
        finished_install_with(
            locale,
            rsetup_core::SystemInstallVerification {
                state: SystemInstallVerificationState::Unsupported,
                expected: Vec::new(),
                observed: observed.then(|| {
                    vec![rsetup_core::SystemInstallPartition {
                        name: "sda1".into(),
                        number: 1,
                        start_bytes: 1024 * 1024,
                        size_bytes: 4 * 1024 * 1024,
                        mount_point: None,
                    }]
                }),
                detail: Some(if observed {
                    "the image is compressed and was not parsed before writing, so the re-read partition table was not compared".into()
                } else {
                    "this host cannot re-read partition tables".into()
                }),
            },
        )
    }

    /// A finished write whose verification is supplied by the test.
    fn finished_install_with(
        locale: Locale,
        verification: rsetup_core::SystemInstallVerification,
    ) -> App {
        // A unique scratch name per call: these tests run in parallel in one
        // process, and a shared directory would be removed by whichever finishes
        // first.
        static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The counter alone makes the name unique; the shape no longer needs to be
        // part of it.
        let scratch = install_scratch(&format!("verification-{call}"));
        let image = scratch.join("system.img");
        std::fs::write(&image, install_image_bytes()).expect("write the fixture image");
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(locale);
        app.install = panel;
        select_target(&mut app.install, "sda");
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        app.install.focus_next();
        type_into(&mut app.install, "sda");
        app.install.confirm(&app.locale);
        let mut result = app.install.result.clone().unwrap_or_else(|| {
            panic!(
                "the demo write produced a result: status={:?} plan={:?}",
                app.install.status,
                app.install
                    .plan
                    .as_ref()
                    .map(|plan| plan.plan_token.clone())
            )
        });
        // Keep the run itself: the write is done, only the verification differs.
        result.verification = verification;
        app.install.result = Some(result);
        app
    }

    /// The board's case, rendered: the kernel refused the re-read (EINVAL) after a
    /// write that did complete. The panel must not print the mismatch wording, and
    /// the honest reason has to reach the operator.
    #[test]
    fn a_failed_reread_is_rendered_as_unverified_not_as_a_mismatch() {
        /// Compare on letters and digits only: the panel folds long lines and the
        /// folds carry border cells between the halves.
        fn letters(text: &str) -> String {
            text.chars()
                .filter(|character| character.is_alphanumeric())
                .collect()
        }
        for locale in [Locale::En, Locale::ZhCn] {
            let failed = render_app(
                &mut finished_install_with(
                    locale,
                    rsetup_core::SystemInstallVerification {
                        state: SystemInstallVerificationState::Unsupported,
                        expected: Vec::new(),
                        observed: None,
                        detail: Some(
                            "the kernel could not re-read the partition table of /dev/sda after the image was written, so the write could not be verified: unable to write the system image: the kernel did not re-read /dev/sda: Invalid argument (os error 22)".into(),
                        ),
                    },
                ),
                150,
                60,
            );
            let failed_letters = letters(&failed);
            for expected in [
                "could not re-read the partition table of /dev/sda",
                "after the image was written",
                "could not be verified",
                "did not re-read /dev/sda",
                "Invalid argument (os error 22)",
            ] {
                assert!(
                    failed_letters.contains(&letters(expected)),
                    "{locale:?} must render {expected:?}: {failed}"
                );
            }
            for forbidden in [
                locale.text("install_verification_mismatched"),
                "differs from the image",
                locale.text("install_verification_compressed_not_compared"),
            ] {
                assert!(
                    !failed_letters.contains(&letters(forbidden)),
                    "{locale:?} must not render {forbidden:?}: {failed}"
                );
            }
            // And it stays a success: the image was written.
            assert!(
                failed_letters.contains(&letters(locale.text("install_written"))),
                "{failed}"
            );
        }
    }

    /// Both `Unsupported` shapes, rendered: the compressed image says why it could
    /// not be compared, the platform case keeps the general wording, and neither is
    /// rendered as a failure.
    #[test]
    fn a_compressed_image_explains_that_the_table_was_not_compared() {
        // Wide CJK characters leave a filler cell between glyphs and long lines
        // fold, so both the rendered text and the expected fragments are compared
        // with their whitespace removed.
        fn squash(text: &str) -> String {
            text.split_whitespace().collect::<Vec<_>>().join("")
        }
        for locale in [Locale::En, Locale::ZhCn] {
            let compressed = squash(&render_app(&mut finished_install(locale, true), 150, 60));
            let compressed_key = match locale {
                Locale::ZhCn => "压缩镜像的布局未在写入前解析",
                Locale::En => "its layout was not parsed before writing",
            };
            assert!(
                compressed.contains(&squash(compressed_key)),
                "{locale:?} must say the layout was not compared: {compressed}"
            );
            assert!(
                compressed.contains(&squash("the image is compressed and was not parsed")),
                "{locale:?} must show the core's own reason: {compressed}"
            );
            assert!(
                !compressed.contains(&squash("当前主机无法重读分区表")),
                "the platform wording is wrong for a compressed image: {compressed}"
            );
            // The write is still presented as a success.
            assert!(
                compressed.contains(&squash(locale.text("install_written"))),
                "{compressed}"
            );
            assert!(
                !compressed.contains("install_unsupported"),
                "no error code is shown for a successful write: {compressed}"
            );

            let platform = squash(&render_app(&mut finished_install(locale, false), 150, 60));
            let platform_key = match locale {
                Locale::ZhCn => "当前主机无法重读分区表",
                Locale::En => "cannot re-read partition tables",
            };
            assert!(
                platform.contains(&squash(platform_key)),
                "{locale:?} keeps the general reason for a host that cannot re-read: {platform}"
            );
            assert!(
                !platform.contains(&squash(compressed_key)),
                "{locale:?} must not use the compressed wording here: {platform}"
            );
        }
    }

    /// The success semantics are checked on the model too: the run is a success and
    /// the panel reports a finished write, not an error.
    #[test]
    fn an_uncomparable_verification_is_not_a_failure() {
        for observed in [true, false] {
            let app = finished_install(Locale::En, observed);
            let result = app.install.result.as_ref().expect("result");
            assert_eq!(result.run.status, rsetup_core::ActionStatus::Succeeded);
            assert!(matches!(app.install.status, Some(InstallStatus::Written)));
            assert_eq!(
                result.verification.state,
                SystemInstallVerificationState::Unsupported
            );
        }
    }

    #[test]
    fn install_panel_requires_a_plan_before_writing() {
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        panel.focus = InstallFocus::Confirm;
        type_into(&mut panel, "sda");
        panel.confirm(&Locale::En);
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Info("install_not_planned"))
            ),
            "{:?}",
            panel.status
        );
        assert_eq!(written(&writes), 0);
    }

    /// The reported defect, at this surface: when the panel is told the write has
    /// to be delegated, Enter must not touch the in-process backend at all.
    ///
    /// The helper is absent on a development host, so delegation is observable as
    /// `root_required` before `pkexec` is reached and the write counter stays at
    /// zero. A board that ships the helper, or a run as root, skips instead of
    /// starting a real authorization prompt.
    #[test]
    fn a_privileged_panel_delegates_instead_of_writing_in_process() {
        if std::path::Path::new(crate::PRIVILEGED_HELPER).is_file()
            || crate::effective_uid() == Some(0)
        {
            return;
        }
        let scratch = install_scratch("panel-privileged");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.privileged = true;
        panel.open_panel();
        panel.selected = panel
            .targets
            .iter()
            .position(|target| target.id == "sda")
            .expect("the demo target list carries sda");
        panel.focus_next();
        type_into(&mut panel, &image.display().to_string());
        panel.confirm(&Locale::En);
        assert!(panel.plan.is_some(), "{:?}", panel.status);

        panel.focus_next();
        type_into(&mut panel, "sda");
        panel.confirm(&Locale::En);

        assert_eq!(
            written(&writes),
            0,
            "a delegated write must not reach the in-process backend"
        );
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Error(SystemInstallError::RootRequired))
            ),
            "{:?}",
            panel.status
        );
    }

    #[test]
    fn install_panel_is_localized_and_marks_the_boot_device() {
        let scratch = install_scratch("panel-zh");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::ZhCn);
        app.install = panel;
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);

        let text = render_app(&mut app, 140, 60);
        let squashed = text.split_whitespace().collect::<Vec<_>>().join("");
        assert!(squashed.contains("系统镜像安装"), "{squashed}");
        assert!(squashed.contains("1选择目标设备"), "{squashed}");
        assert!(squashed.contains("将被清除的现有分区"), "{squashed}");
        assert!(
            squashed.contains("写入会替换目标设备上的全部分区。"),
            "{squashed}"
        );
        assert!(squashed.contains("此写入无法回滚。"), "{squashed}");
        assert!(
            squashed.contains("已禁用：键入的设备名尚未匹配目标"),
            "{squashed}"
        );
        assert!(squashed.contains("承载正在运行的系统"), "{squashed}");
        assert!(squashed.contains("可移动介质"), "{squashed}");
        assert!(squashed.contains("SHA-256（前1MiB）"), "{squashed}");
    }

    #[test]
    fn install_panel_refuses_the_device_that_hosts_the_running_system() {
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        select_target(&mut panel, "mmcblk1");
        panel.load_plan();
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Error(SystemInstallError::RunningSystem(_)))
            ),
            "{:?}",
            panel.status
        );
        assert!(
            panel.plan.is_none(),
            "a refused device has no plan to apply"
        );
        assert_eq!(written(&writes), 0);

        // There is no plan, so even the exact typed name cannot write anything.
        panel.focus = InstallFocus::Confirm;
        type_into(&mut panel, "mmcblk1");
        panel.confirm(&Locale::En);
        assert!(
            matches!(
                panel.status,
                Some(InstallStatus::Info("install_not_planned"))
            ),
            "{:?}",
            panel.status
        );
        assert_eq!(written(&writes), 0);

        // The list still shows why that device is refused.
        let mut app = demo_app(Locale::En);
        app.install = panel;
        app.install.open = true;
        let text = render_app(&mut app, 140, 60);
        assert!(text.contains("/dev/mmcblk1"), "{text}");
        assert!(text.contains("Hosts the running system"), "{text}");
    }

    #[test]
    fn install_panel_edits_only_the_focused_field() {
        let (mut panel, _) = recording_panel();
        panel.open_panel();
        panel.focus_next();
        type_into(&mut panel, "/tmp");
        assert_eq!(panel.image_path, "/tmp");
        panel.backspace();
        assert_eq!(panel.image_path, "/tm");

        panel.focus_next();
        type_into(&mut panel, "sda");
        assert_eq!(panel.typed, "sda");
        panel.backspace();
        assert_eq!(panel.typed, "sd");

        // J/K only move the device selection while the list owns the focus.
        let before = panel.selected;
        panel.next_target();
        assert_eq!(panel.selected, before);
        panel.focus_next();
        assert_eq!(panel.focus, InstallFocus::Targets);
        panel.next_target();
        assert_ne!(panel.selected, before);

        // Leaving the panel drops the plan and the typed name.
        panel.close();
        assert!(panel.plan.is_none());
        assert!(panel.typed.is_empty());
        assert!(!panel.open);
    }

    #[test]
    fn install_panel_typing_is_trimmed_like_the_cli() {
        let scratch = install_scratch("panel-trim");
        let image = scratch.join("radxa.img");
        std::fs::write(&image, install_image_bytes()).expect("write fixture image");
        let (mut panel, writes) = recording_panel();
        panel.open_panel();
        let mut app = demo_app(Locale::En);
        app.install = panel;
        select_target(&mut app.install, "sda");
        app.install.focus_next();
        type_into(&mut app.install, &image.display().to_string());
        app.install.confirm(&app.locale);
        app.install.focus_next();
        // Spaces around the name are trimmed, exactly as the CLI trims stdin.
        type_into(&mut app.install, "  sda  ");
        app.install.confirm(&app.locale);
        assert!(app.install.result.is_some(), "{:?}", app.install.status);
        assert!(written(&writes) > 0);
    }
}
