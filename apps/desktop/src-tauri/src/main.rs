use rsetup_core::{
    ActionError, ActionRun, ActionSpec, ActivityEvent, Controller, DeviceSnapshot, ExecutionPolicy,
    FanCurveApplyResult, FanCurvePlan, FanCurveRequest, FanCurveStatus, GpioStatus, HardwareError,
    LedStatus, OverlayApplyResult, OverlayPlan, OverlayStatus, ProbeMode, RgbLedConfig,
    SourceApplyResult, SourceError, SourcePlan, SourceStatus, SpiFlashApplyResult, SpiFlashPlan,
    SpiFlashRequest, SpiFlashStatus, ThermalStatus, VideoFrame, VideoStatus,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandError {
    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            code: "internal_error",
            message: error.to_string(),
        }
    }
}

impl From<ActionError> for CommandError {
    fn from(error: ActionError) -> Self {
        let code = match &error {
            ActionError::Unknown(_) => "unknown_action",
            ActionError::ConfirmationRequired(_) => "confirmation_required",
            ActionError::Unavailable(_) => "action_unavailable",
            ActionError::RootRequired(_) => "root_required",
            ActionError::AuthorizationCanceled => "authorization_canceled",
            ActionError::Authorization(_, _) => "authorization_failed",
            ActionError::InputRequired(_) => "input_required",
            ActionError::Launch(_) => "internal_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

impl From<SourceError> for CommandError {
    fn from(error: SourceError) -> Self {
        let code = match &error {
            SourceError::UnknownProvider(_) => "unknown_mirror",
            SourceError::Unsupported(_) => "sources_unsupported",
            SourceError::ConfirmationRequired => "confirmation_required",
            SourceError::PlanRequired => "plan_required",
            SourceError::StalePlan => "stale_plan",
            SourceError::RootRequired => "root_required",
            SourceError::AuthorizationCanceled => "authorization_canceled",
            SourceError::Authorization(_) => "authorization_failed",
            SourceError::Io(_) => "internal_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

impl From<HardwareError> for CommandError {
    fn from(error: HardwareError) -> Self {
        let code = match &error {
            HardwareError::Unsupported(_) => "hardware_unsupported",
            HardwareError::InvalidInput(_) => "invalid_hardware_selection",
            HardwareError::Conflict(_) => "hardware_conflict",
            HardwareError::ConfirmationRequired => "confirmation_required",
            HardwareError::PlanRequired => "plan_required",
            HardwareError::StalePlan => "stale_plan",
            HardwareError::RootRequired => "root_required",
            HardwareError::AuthorizationCanceled => "authorization_canceled",
            HardwareError::Authorization(_) => "authorization_failed",
            HardwareError::Io(_) => "internal_error",
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

#[tauri::command]
fn system_snapshot(
    controller: tauri::State<'_, Controller>,
) -> Result<DeviceSnapshot, CommandError> {
    controller.snapshot().map_err(CommandError::internal)
}

#[tauri::command]
fn list_actions(controller: tauri::State<'_, Controller>) -> Vec<ActionSpec> {
    controller.actions()
}

#[tauri::command]
fn list_activity(controller: tauri::State<'_, Controller>) -> Vec<ActivityEvent> {
    controller.activity()
}

#[tauri::command]
fn run_action(
    controller: tauri::State<'_, Controller>,
    action_id: String,
    confirm: bool,
) -> Result<ActionRun, CommandError> {
    controller
        .execute(&action_id, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn source_status(controller: tauri::State<'_, Controller>) -> Result<SourceStatus, CommandError> {
    controller.source_status().map_err(CommandError::from)
}

#[tauri::command]
async fn benchmark_source(
    controller: tauri::State<'_, Controller>,
    provider_id: String,
) -> Result<rsetup_core::MirrorBenchmark, CommandError> {
    let controller = controller.inner().clone();
    tauri::async_runtime::spawn_blocking(move || controller.benchmark_source(&provider_id))
        .await
        .map_err(CommandError::internal)?
        .map_err(CommandError::from)
}

#[tauri::command]
fn plan_sources(
    controller: tauri::State<'_, Controller>,
    provider_id: String,
) -> Result<SourcePlan, CommandError> {
    controller
        .plan_source_change(&provider_id)
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_sources(
    controller: tauri::State<'_, Controller>,
    provider_id: String,
    plan_token: String,
    confirm: bool,
) -> Result<SourceApplyResult, CommandError> {
    controller
        .apply_source_change(&provider_id, &plan_token, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn overlay_status(controller: tauri::State<'_, Controller>) -> Result<OverlayStatus, CommandError> {
    controller.overlay_status().map_err(CommandError::from)
}

#[tauri::command]
async fn authorize_overlay_read(
    controller: tauri::State<'_, Controller>,
) -> Result<OverlayStatus, CommandError> {
    let controller = controller.inner().clone();
    tauri::async_runtime::spawn_blocking(move || controller.authorize_overlay_read())
        .await
        .map_err(|error| CommandError::from(rsetup_core::HardwareError::Io(error.to_string())))?
        .map_err(CommandError::from)
}

#[tauri::command]
fn plan_overlays(
    controller: tauri::State<'_, Controller>,
    selected_ids: Vec<String>,
) -> Result<OverlayPlan, CommandError> {
    controller
        .plan_overlay_change(&selected_ids)
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_overlays(
    controller: tauri::State<'_, Controller>,
    selected_ids: Vec<String>,
    plan_token: String,
    confirm: bool,
) -> Result<OverlayApplyResult, CommandError> {
    controller
        .apply_overlay_change(&selected_ids, &plan_token, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn gpio_status(
    controller: tauri::State<'_, Controller>,
    profile_id: Option<String>,
) -> Result<GpioStatus, CommandError> {
    controller
        .gpio_status_for_profile(profile_id.as_deref())
        .map_err(CommandError::from)
}

#[tauri::command]
fn spi_flash_status(
    controller: tauri::State<'_, Controller>,
) -> Result<SpiFlashStatus, CommandError> {
    controller.spi_flash_status().map_err(CommandError::from)
}

#[tauri::command]
fn plan_spi_flash(
    controller: tauri::State<'_, Controller>,
    request: SpiFlashRequest,
) -> Result<SpiFlashPlan, CommandError> {
    controller
        .plan_spi_flash(&request)
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_spi_flash(
    controller: tauri::State<'_, Controller>,
    request: SpiFlashRequest,
    plan_token: String,
    confirm: bool,
) -> Result<SpiFlashApplyResult, CommandError> {
    controller
        .apply_spi_flash(&request, &plan_token, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn fan_curve_status(
    controller: tauri::State<'_, Controller>,
) -> Result<FanCurveStatus, CommandError> {
    controller.fan_curve_status().map_err(CommandError::from)
}

#[tauri::command]
fn plan_fan_curve(
    controller: tauri::State<'_, Controller>,
    request: FanCurveRequest,
) -> Result<FanCurvePlan, CommandError> {
    controller
        .plan_fan_curve(&request)
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_fan_curve(
    controller: tauri::State<'_, Controller>,
    request: FanCurveRequest,
    plan_token: String,
    confirm: bool,
) -> Result<FanCurveApplyResult, CommandError> {
    controller
        .apply_fan_curve(&request, &plan_token, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn led_status(controller: tauri::State<'_, Controller>) -> Result<LedStatus, CommandError> {
    controller.led_status().map_err(CommandError::from)
}

#[tauri::command]
fn apply_led_trigger(
    controller: tauri::State<'_, Controller>,
    led_id: String,
    trigger: String,
    confirm: bool,
) -> Result<ActionRun, CommandError> {
    controller
        .apply_led_trigger(&led_id, &trigger, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn apply_rgb_led(
    controller: tauri::State<'_, Controller>,
    config: RgbLedConfig,
    confirm: bool,
) -> Result<ActionRun, CommandError> {
    controller
        .apply_rgb_led(&config, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
fn video_status(controller: tauri::State<'_, Controller>) -> Result<VideoStatus, CommandError> {
    controller.video_status().map_err(CommandError::from)
}

#[tauri::command]
fn capture_video_frame(
    controller: tauri::State<'_, Controller>,
    device_id: String,
) -> Result<VideoFrame, CommandError> {
    controller
        .capture_video_frame(&device_id)
        .map_err(CommandError::from)
}

#[tauri::command]
fn thermal_status(controller: tauri::State<'_, Controller>) -> Result<ThermalStatus, CommandError> {
    controller.thermal_status().map_err(CommandError::from)
}

#[tauri::command]
fn apply_thermal_policy(
    controller: tauri::State<'_, Controller>,
    policy: String,
    confirm: bool,
) -> Result<ActionRun, CommandError> {
    controller
        .apply_thermal_policy(&policy, confirm)
        .map_err(CommandError::from)
}

#[tauri::command]
async fn storage_status(
    controller: tauri::State<'_, Controller>,
) -> Result<rsetup_core::StorageStatus, CommandError> {
    let controller = controller.inner().clone();
    tauri::async_runtime::spawn_blocking(move || controller.storage_status())
        .await
        .map_err(CommandError::internal)?
        .map_err(CommandError::from)
}

pub fn command_handler<R: tauri::Runtime>()
-> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        system_snapshot,
        list_actions,
        list_activity,
        run_action,
        source_status,
        benchmark_source,
        plan_sources,
        apply_sources,
        overlay_status,
        authorize_overlay_read,
        plan_overlays,
        apply_overlays,
        gpio_status,
        spi_flash_status,
        plan_spi_flash,
        apply_spi_flash,
        fan_curve_status,
        plan_fan_curve,
        apply_fan_curve,
        led_status,
        apply_led_trigger,
        apply_rgb_led,
        video_status,
        capture_video_frame,
        thermal_status,
        apply_thermal_policy,
        storage_status
    ]
}

fn main() {
    // Opt in explicitly; never infer permission to mutate from launching the GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| arg != "--live-execution" && arg != "--demo" && arg != "--help")
    {
        eprintln!("Usage: rsetup-desktop [--live-execution] [--demo]");
        std::process::exit(2);
    }
    if args.iter().any(|arg| arg == "--help") {
        println!(
            "Usage: rsetup-desktop [--live-execution] [--demo]\nDefault: inspect and preview only. Live operations still require confirmation and administrator authorization."
        );
        return;
    }
    let from_env = Controller::from_environment();
    let mode = if args.iter().any(|arg| arg == "--demo")
        || std::env::var("RSETUP_MODE").as_deref() == Ok("demo")
    {
        ProbeMode::Demo
    } else {
        ProbeMode::Auto
    };
    let controller = if args.iter().any(|arg| arg == "--live-execution") {
        Controller::new(mode, ExecutionPolicy::Live)
    } else if args.iter().any(|arg| arg == "--demo") {
        Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)
    } else {
        from_env
    };
    tauri::Builder::default()
        .manage(controller)
        .invoke_handler(command_handler())
        .run(tauri::generate_context!())
        .expect("failed to run rsetup desktop");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type MockWebview = tauri::WebviewWindow<tauri::test::MockRuntime>;

    /// Every command the shared UI can invoke. Keeping the list explicit lets the
    /// mock runtime authorize real IPC dispatch, and it fails the moment the
    /// production registry silently loses a command the UI still calls.
    const IPC_COMMANDS: &[&str] = &[
        "system_snapshot",
        "list_actions",
        "list_activity",
        "run_action",
        "source_status",
        "benchmark_source",
        "plan_sources",
        "apply_sources",
        "overlay_status",
        "authorize_overlay_read",
        "plan_overlays",
        "apply_overlays",
        "gpio_status",
        "spi_flash_status",
        "plan_spi_flash",
        "apply_spi_flash",
        "fan_curve_status",
        "plan_fan_curve",
        "apply_fan_curve",
        "led_status",
        "apply_led_trigger",
        "apply_rgb_led",
        "video_status",
        "capture_video_frame",
        "thermal_status",
        "apply_thermal_policy",
        "storage_status",
    ];

    /// Commands that answer without arguments; they must return JSON directly.
    const READ_ONLY_COMMANDS: &[&str] = &[
        "system_snapshot",
        "list_actions",
        "list_activity",
        "source_status",
        "overlay_status",
        "gpio_status",
        "spi_flash_status",
        "fan_curve_status",
        "led_status",
        "video_status",
        "thermal_status",
        "storage_status",
    ];

    fn mock_webview(controller: Controller) -> (tauri::App<tauri::test::MockRuntime>, MockWebview) {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        // The mock context ships without capabilities, so authorize the
        // production command set explicitly; otherwise every dispatch is denied
        // before it reaches the handler under test.
        for command in IPC_COMMANDS {
            context.runtime_authority_mut().__allow_command(
                (*command).to_string(),
                tauri::utils::acl::ExecutionContext::Local,
            );
        }
        let app = tauri::test::mock_builder()
            .manage(controller)
            .invoke_handler(command_handler())
            .build(context)
            .expect("mock desktop app builds");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
            .build()
            .expect("mock desktop webview builds");
        (app, webview)
    }

    fn demo_webview() -> (tauri::App<tauri::test::MockRuntime>, MockWebview) {
        mock_webview(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun))
    }

    fn invoke(
        webview: &MockWebview,
        command: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        tauri::test::get_ipc_response(
            webview,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                // The custom-protocol origin is what the packaged desktop shell
                // serves the shared UI from, so dispatch is authorized as local.
                url: "tauri://localhost".parse().expect("valid test origin"),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.to_string(),
            },
        )
        .map(|body| {
            body.deserialize::<serde_json::Value>()
                .expect("successful commands answer with JSON")
        })
    }

    fn error_code(error: &serde_json::Value) -> String {
        error["code"].as_str().unwrap_or_default().to_string()
    }

    /// A storage reader that replays a synthetic contract, and that can be told
    /// to publish a fresh model or to fail. It is only driven on Linux, where a
    /// live controller stops short-circuiting to the demo contract, but it is
    /// compiled everywhere so the fixture itself cannot rot unnoticed.
    struct FakeStorageReader(Arc<Mutex<FakeStorageState>>);

    struct FakeStorageState {
        status: rsetup_core::StorageStatus,
        failure: Option<String>,
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    impl FakeStorageReader {
        fn new(status: rsetup_core::StorageStatus) -> Self {
            Self(Arc::new(Mutex::new(FakeStorageState {
                status,
                failure: None,
            })))
        }

        fn set_first_nvme_model(&self, model: &str) {
            let mut state = self.0.lock().expect("fake storage lock");
            state.status.nvme.devices[0].model = model.to_string();
        }

        fn set_failure(&self, reason: &str) {
            let mut state = self.0.lock().expect("fake storage lock");
            state.failure = Some(reason.to_string());
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    impl rsetup_core::StorageReader for FakeStorageReader {
        fn nvme_status(&self) -> Result<rsetup_core::NvmeStatus, HardwareError> {
            let state = self.0.lock().expect("fake storage lock");
            match &state.failure {
                Some(reason) => Err(HardwareError::Io(reason.clone())),
                None => Ok(state.status.nvme.clone()),
            }
        }

        fn mmc_status(&self) -> Result<rsetup_core::MmcStatus, HardwareError> {
            let state = self.0.lock().expect("fake storage lock");
            match &state.failure {
                Some(reason) => Err(HardwareError::Io(reason.clone())),
                None => Ok(state.status.mmc.clone()),
            }
        }
    }

    /// How Tauri rejects an invoke request before any handler sees it.
    #[derive(Debug, PartialEq)]
    enum DispatchFailure {
        /// The name is absent from the production `generate_handler!` registry.
        MissingCommand,
        /// The runtime authority refused the call before dispatch.
        Denied,
        /// The command ran and answered with its own error.
        Handler,
    }

    /// Tauri 2.12.1 rejects an unregistered command with
    /// `format!("Command {command} not found")` (src/webview/mod.rs, both the
    /// plugin and the handler branch), which splices the command name into the
    /// middle of the sentence. A literal `"Command not found"` substring can
    /// therefore never match, so the guard must rebuild this exact shape.
    fn missing_command_message(command: &str) -> String {
        format!("Command {command} not found")
    }

    fn classify_dispatch_failure(command: &str, error: &serde_json::Value) -> DispatchFailure {
        let text = error.to_string();
        if text.contains(&missing_command_message(command)) {
            DispatchFailure::MissingCommand
        } else if text.contains("not allowed") {
            DispatchFailure::Denied
        } else {
            DispatchFailure::Handler
        }
    }

    /// Returns `None` once the call reached a handler, or the pre-dispatch
    /// failure that stopped it. `assert_command_reaches_a_handler` panics on
    /// exactly the `Some` cases, so asserting this predicate directly proves
    /// which calls the guard accepts and which it rejects.
    fn dispatch_failure(webview: &MockWebview, command: &str) -> Option<DispatchFailure> {
        let error = invoke(webview, command, serde_json::json!({})).err()?;
        match classify_dispatch_failure(command, &error) {
            DispatchFailure::Handler => None,
            failure => Some(failure),
        }
    }

    /// Fails when a command never reached a handler, which is how a dropped or
    /// renamed registry entry shows up here.
    fn assert_command_reaches_a_handler(webview: &MockWebview, command: &str) {
        if let Some(failure) = dispatch_failure(webview, command) {
            panic!("{command} did not reach a handler ({failure:?})");
        }
    }

    #[test]
    fn desktop_registry_exposes_every_shared_ui_command() {
        let (_app, webview) = demo_webview();
        for command in IPC_COMMANDS {
            // A missing argument is fine here; an authorization failure or a
            // missing registry entry is not, because that is what the UI would
            // report for a command it still calls.
            assert_command_reaches_a_handler(&webview, command);
        }
    }

    #[test]
    fn desktop_registry_guard_detects_a_missing_command() {
        let (_app, webview) = demo_webview();
        // Non-vacuity: a name that cannot be registered must be reported as a
        // missing registry entry. This is the same predicate the registry guard
        // panics on, so if a future Tauri changes the wording this test goes red
        // instead of letting the guard silently become a no-op.
        let missing = "desktop_registry_guard_probe";
        let error = invoke(&webview, missing, serde_json::json!({}))
            .expect_err("an unregistered command must be rejected");
        // Pinning the exact value keeps the guard tied to observed behaviour
        // rather than to an assumed phrase.
        assert_eq!(
            error,
            serde_json::json!(missing_command_message(missing)),
            "unexpected rejection payload for an unregistered command"
        );
        assert_eq!(
            dispatch_failure(&webview, missing),
            Some(DispatchFailure::MissingCommand),
            "Tauri no longer reports `Command {{command}} not found`; the registry guard would be vacuous"
        );
        // The guard itself must fire for that name, so a future no-op cannot
        // hide behind the classification check above. The panic hook is muted
        // only for the duration of the captured call.
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let guarded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_command_reaches_a_handler(&webview, missing);
        }));
        std::panic::set_hook(previous_hook);
        assert!(
            guarded.is_err(),
            "the registry guard must fire for an unregistered command"
        );
        // A registered command must not be reported, so the guard cannot be
        // trivially red either.
        assert_command_reaches_a_handler(&webview, "storage_status");
        let registered = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect("a registered command answers");
        assert!(registered["mmc"]["devices"].is_array());
    }

    #[test]
    fn desktop_read_only_commands_answer_with_json() {
        let (_app, webview) = demo_webview();
        for command in READ_ONLY_COMMANDS {
            let value = invoke(&webview, command, serde_json::json!({}))
                .unwrap_or_else(|error| panic!("{command} is not dispatched: {error}"));
            assert!(!value.is_null(), "{command} answered with null");
        }
    }

    #[test]
    fn desktop_storage_ipc_returns_demo_contract() {
        let (_app, webview) = demo_webview();
        let value = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect("storage_status dispatches through the production registry");
        assert!(
            !value["nvme"]["devices"]
                .as_array()
                .expect("nvme devices array")
                .is_empty()
        );
        assert!(
            !value["mmc"]["devices"]
                .as_array()
                .expect("mmc devices array")
                .is_empty()
        );
        assert_eq!(
            value["mmc"]["devices"][0]["telemetry"]["state"],
            "available"
        );
        assert!(value["mmc"]["devices"][0]["healthState"].is_string());
    }

    #[test]
    fn desktop_run_action_stays_synthetic_without_explicit_opt_in() {
        let (_app, webview) = demo_webview();
        let run = invoke(
            &webview,
            "run_action",
            serde_json::json!({ "actionId": "system.inspect", "confirm": true }),
        )
        .expect("catalog action dispatches");
        assert_eq!(run["actionId"], "system.inspect");
        assert_eq!(run["synthetic"], true);
    }

    #[test]
    fn desktop_ipc_maps_failures_to_the_http_error_codes() {
        let (_app, webview) = demo_webview();
        for (command, body, expected) in [
            (
                "run_action",
                serde_json::json!({ "actionId": "does-not-exist", "confirm": true }),
                "unknown_action",
            ),
            (
                "plan_sources",
                serde_json::json!({ "providerId": "does-not-exist" }),
                "unknown_mirror",
            ),
            (
                "plan_overlays",
                serde_json::json!({ "selectedIds": ["not-an-overlay"] }),
                "invalid_hardware_selection",
            ),
            (
                "apply_sources",
                serde_json::json!({ "providerId": "ustc", "planToken": "", "confirm": true }),
                "plan_required",
            ),
        ] {
            let error = invoke(&webview, command, body)
                .expect_err(&format!("{command} must reject this request"));
            assert_eq!(error_code(&error), expected, "{command}: {error}");
            assert!(
                error["message"].as_str().is_some_and(|m| !m.is_empty()),
                "{command}: {error}"
            );
        }
    }

    #[test]
    fn desktop_demo_storage_never_reads_the_host() {
        struct PanickingStorageReader;
        impl rsetup_core::StorageReader for PanickingStorageReader {
            fn nvme_status(&self) -> Result<rsetup_core::NvmeStatus, HardwareError> {
                panic!("demo storage must not inspect NVMe devices");
            }
            fn mmc_status(&self) -> Result<rsetup_core::MmcStatus, HardwareError> {
                panic!("demo storage must not inspect MMC devices");
            }
        }
        let (_app, webview) = mock_webview(Controller::with_storage_reader(
            ProbeMode::Demo,
            ExecutionPolicy::DryRun,
            Arc::new(PanickingStorageReader),
        ));
        let value = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect("demo storage answers without touching the reader");
        assert!(value["mmc"]["devices"][0]["name"].is_string());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_storage_ipc_reports_fresh_data_and_injected_failures() {
        let demo = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)
            .storage_status()
            .expect("demo storage contract");
        let fake = Arc::new(FakeStorageReader::new(demo));
        let controller =
            Controller::with_storage_reader(ProbeMode::Live, ExecutionPolicy::DryRun, fake.clone());
        let (_app, webview) = mock_webview(controller);

        let first = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect("the injected reader answers the first read");
        assert!(first["nvme"]["devices"][0]["model"].is_string());
        assert_eq!(
            first["mmc"]["devices"][0]["telemetry"]["state"],
            "available"
        );

        fake.set_first_nvme_model("fresh-model");
        let second = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect("the injected reader answers the second read");
        assert_eq!(second["nvme"]["devices"][0]["model"], "fresh-model");

        fake.set_failure("injected storage failure");
        let error = invoke(&webview, "storage_status", serde_json::json!({}))
            .expect_err("a failing reader must reject the command");
        assert_eq!(error_code(&error), "internal_error");
        assert_eq!(error["message"], "injected storage failure");
    }

    #[test]
    fn action_errors_keep_the_same_stable_codes_as_http() {
        assert_eq!(
            CommandError::from(ActionError::ConfirmationRequired("Update".into())).code,
            "confirmation_required"
        );
        assert_eq!(
            CommandError::from(ActionError::RootRequired("Update".into())).code,
            "root_required"
        );
        assert_eq!(
            CommandError::from(ActionError::Unavailable("Update".into())).code,
            "action_unavailable"
        );
        assert_eq!(
            CommandError::from(SourceError::UnknownProvider("unknown".into())).code,
            "unknown_mirror"
        );
    }
}
