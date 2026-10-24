use rsetup_core::{
    ActionError, ActionRun, ActionSpec, ActivityEvent, BlockDeviceBackend, Controller,
    DeviceSnapshot, ExecutionPolicy, FanCurveApplyResult, FanCurvePlan, FanCurveRequest,
    FanCurveStatus, GpioStatus, HardwareError, LedStatus, OverlayApplyResult, OverlayPlan,
    OverlayStatus, ProbeMode, RgbLedConfig, SourceApplyResult, SourceError, SourcePlan,
    SourceStatus, SpiFlashApplyResult, SpiFlashPlan, SpiFlashRequest, SpiFlashStatus,
    SyntheticBackend, SystemBlockBackend, SystemImageOpener, SystemInstallApplyResult,
    SystemInstallError, SystemInstallManager, SystemInstallPartition, SystemInstallPlan,
    SystemInstallRequest, SystemInstallTarget, SystemMount, ThermalStatus, VideoFrame, VideoStatus,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::File,
    hash::{BuildHasher, Hash, Hasher},
    io::Read,
    path::{Path as FsPath, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

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

/// Mirrors `crates/rsetup-app/src/main.rs::system_install_error_code`: the CLI,
/// the HTTP surface, the TUI and this crate report one vocabulary.
impl From<SystemInstallError> for CommandError {
    fn from(error: SystemInstallError) -> Self {
        let code = match &error {
            SystemInstallError::Unsupported(_) => "install_unsupported",
            SystemInstallError::InvalidInput(_) => "invalid_install_request",
            SystemInstallError::ConfirmationRequired => "confirmation_required",
            SystemInstallError::AcknowledgementMismatch { .. } => "acknowledgement_mismatch",
            SystemInstallError::PlanRequired => "plan_required",
            SystemInstallError::StalePlan => "stale_plan",
            SystemInstallError::RootRequired => "root_required",
            SystemInstallError::Conflict(_) => "install_conflict",
            SystemInstallError::RunningSystem(_) => "writes_running_system",
            SystemInstallError::Io(_) => "internal_error",
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

// ---------------------------------------------------------------------------
// System image install: a fixed image catalogue and an explicit live selection
// ---------------------------------------------------------------------------

/// Directory the desktop shell may read install images from. The UI never submits
/// a filesystem path: it submits an id, and only a plain file name inside this
/// directory is accepted. Mirrors the HTTP surface in `crates/rsetup-app`.
const DEFAULT_IMAGE_DIRECTORY: &str = "/var/lib/rsetup-next/images";
const IMAGE_DIRECTORY_ENV: &str = "RSETUP_IMAGE_DIR";

/// Test-only override for the catalogue directory. `set_var` is process-wide and
/// racy; this is set once, before the first app is built.
static IMAGE_DIRECTORY_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

#[cfg(test)]
fn set_image_directory(directory: PathBuf) {
    let _ = IMAGE_DIRECTORY_OVERRIDE.set(directory);
}

fn image_directory() -> PathBuf {
    if let Some(directory) = IMAGE_DIRECTORY_OVERRIDE.get() {
        return directory.clone();
    }
    std::env::var_os(IMAGE_DIRECTORY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_IMAGE_DIRECTORY))
}

/// Resolves an image id to a file inside the catalogue directory. Every way the
/// UI could escape the directory is rejected, and the name has to look like an
/// image before the core ever sees it.
fn catalog_image_path(directory: &FsPath, image_id: &str) -> Result<PathBuf, SystemInstallError> {
    let invalid = |detail: &str| SystemInstallError::InvalidInput(detail.to_owned());
    if image_id.is_empty() {
        return Err(invalid("an image id is required"));
    }
    if image_id.len() > 255 {
        return Err(invalid("the image id is too long"));
    }
    if image_id.contains('/') || image_id.contains('\\') || image_id.chars().any(char::is_control) {
        return Err(invalid(
            "the image id must be a plain file name, not a path",
        ));
    }
    if image_id == "." || image_id == ".." {
        return Err(invalid(
            "the image id must be a plain file name, not a path",
        ));
    }
    if !(image_id.ends_with(".img") || image_id.ends_with(".img.xz")) {
        return Err(invalid("the image id must end with .img or .img.xz"));
    }
    let path = directory.join(image_id);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| invalid(&format!("no such image in the image directory: {error}")))?;
    if !metadata.file_type().is_file() {
        return Err(invalid(
            "the image must be a regular file, not a symlink or a directory",
        ));
    }
    std::fs::canonicalize(&path)
        .map_err(|error| invalid(&format!("the image path cannot be resolved: {error}")))
}

/// Marker the refusing writer leaves behind: the core wraps a failed write into
/// `Io(...)`, so the surface recognises its own refusal and reports the policy.
const LIVE_REQUIRED_MARKER: &str = "live execution is not enabled on this surface";

/// Refuses every write. The desktop shell never simulates a write, so the UI
/// cannot mistake a simulated image write for a real one; reads still come from
/// the real (or demo) provider, so the consequences it shows are real.
struct RefusingWriter {
    inner: Arc<dyn BlockDeviceBackend>,
}

impl BlockDeviceBackend for RefusingWriter {
    fn targets(&self) -> Result<Vec<SystemInstallTarget>, SystemInstallError> {
        self.inner.targets()
    }

    fn mounts(&self) -> Result<Vec<SystemMount>, SystemInstallError> {
        self.inner.mounts()
    }

    fn write_chunk(
        &self,
        _target: &SystemInstallTarget,
        _offset: u64,
        _chunk: &[u8],
    ) -> Result<(), SystemInstallError> {
        Err(SystemInstallError::Unsupported(LIVE_REQUIRED_MARKER.into()))
    }

    fn reread_partitions(
        &self,
        _target: &SystemInstallTarget,
    ) -> Result<Vec<SystemInstallPartition>, SystemInstallError> {
        Err(SystemInstallError::Unsupported(
            "live execution is not enabled on this surface; the partition table was not re-read"
                .into(),
        ))
    }
}

#[derive(Debug, Clone, Copy)]
struct InstallMode {
    /// Demo contract instead of the real devices.
    synthetic: bool,
    /// The controller's execution policy: the same value the CLI derives from
    /// `--live-execution`. This crate never keeps a policy of its own.
    live: bool,
}

/// Path of the root-only helper and the authorizer that starts it. Both come from
/// `rsetup-core`, which owns the helper protocol, so this shell cannot drift from
/// the CLI's idea of which binary and which authorizer are meant.
const PRIVILEGED_HELPER: &str = rsetup_core::privileged_helper_path();
const PKEXEC: &str = rsetup_core::pkexec_path();

/// Whether a real write has to be delegated to that helper. The rule lives in
/// `rsetup-core` so every surface decides it once.
fn install_needs_privilege(live: bool, synthetic: bool, effective_uid: Option<u32>) -> bool {
    rsetup_core::install_needs_privilege(live, synthetic, effective_uid)
}

/// The caller's effective uid, from the core that already calls `geteuid`.
///
/// This shell does not depend on `libc`, and declaring `geteuid` by hand here was
/// a third copy of a binding the core already owns.
fn effective_uid() -> Option<u32> {
    rsetup_core::effective_uid()
}

/// Runs the helper's fixed install verb and parses its typed result.
///
/// The token is the preview this process issued. The helper treats it as the
/// advisory preview it is, then issues and consumes its own before writing, so
/// nothing here can widen what may be written.
/// Reads the helper's error envelope: `{"error":{"code","message"}}`.
fn helper_error_envelope(stdout: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let error = value.get("error")?;
    let code = error.get("code")?.as_str()?.to_owned();
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some((code, message))
}

/// Maps a code that crossed the helper boundary back into the closed vocabulary.
///
/// `CommandError::code` is a `&'static str` at every other construction site, so
/// the owned code is matched against the shared vocabulary. An unrecognised code
/// keeps the pre-existing generic classification rather than being guessed into
/// the wrong class.
fn install_code_static(code: &str) -> &'static str {
    match code {
        "install_unsupported" => "install_unsupported",
        "invalid_install_request" => "invalid_install_request",
        "confirmation_required" => "confirmation_required",
        "acknowledgement_mismatch" => "acknowledgement_mismatch",
        "plan_required" => "plan_required",
        "stale_plan" => "stale_plan",
        "root_required" => "root_required",
        "install_conflict" => "install_conflict",
        "writes_running_system" => "writes_running_system",
        "authorization_canceled" => "authorization_canceled",
        _ => "internal_error",
    }
}

fn run_privileged_system_install(
    request: &SystemInstallRequest,
    plan_token: &str,
    acknowledgement: &str,
) -> Result<SystemInstallApplyResult, CommandError> {
    if !FsPath::new(PRIVILEGED_HELPER).is_file() {
        return Err(CommandError {
            code: "root_required",
            message: "writing a system image requires root privileges".into(),
        });
    }
    let Some(image_path) = request.image_path.as_deref() else {
        return Err(CommandError {
            code: "plan_required",
            message: "preview the system image write before applying it".into(),
        });
    };
    let output = Command::new(PKEXEC)
        .args([
            PRIVILEGED_HELPER,
            "system-install-apply",
            &request.target_id,
            image_path,
            plan_token,
            acknowledgement,
            "--confirmed",
        ])
        .output()
        .map_err(CommandError::internal)?;
    if output.status.code() == Some(126) {
        // polkit's "not authorized": the administrator dismissed the prompt.
        // That is a decision, so it keeps the same code the other privileged
        // commands report instead of looking like a failed write.
        return Err(CommandError {
            code: "authorization_canceled",
            message: "Administrator authorization canceled.".into(),
        });
    }
    if !output.status.success() {
        // The helper classifies its own failures, so its code is reused rather
        // than flattened into one generic error. A helper built before the
        // envelope, or one that died before printing it, still degrades to the
        // exit status and stderr.
        if let Some((code, message)) = helper_error_envelope(&output.stdout) {
            return Err(CommandError {
                code: install_code_static(&code),
                message,
            });
        }
        return Err(CommandError {
            code: "internal_error",
            message: format!(
                "the privileged helper failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|error| CommandError {
        code: "internal_error",
        message: format!("invalid helper response: {error}"),
    })
}

// ---------------------------------------------------------------------------
// Desktop image picking: the Rust side owns the path
// ---------------------------------------------------------------------------

/// Prefix of every handle this process issues for a picked image.
const PICK_HANDLE_PREFIX: &str = "pick-";
/// How long a picked image stays usable: long enough for a preview, a typed
/// confirmation and a write, short enough that a stale handle stops working.
const PICK_HANDLE_TTL: Duration = Duration::from_secs(600);
/// Random bytes behind a handle (16 bytes render as 32 hex characters).
const PICK_HANDLE_RANDOM_BYTES: usize = 16;

/// Opens the platform's native file chooser. It is a trait so registration and
/// validation can be tested without a dialog, and it is only ever called from
/// Rust: the WebView gets no dialog permission, and the file is chosen by the user
/// in this process rather than named by the page.
trait ImagePicker: Send + Sync + 'static {
    /// `Ok(None)` means the user cancelled, which is not an error. `Err` carries
    /// a readable reason, e.g. that no dialog helper is installed.
    fn pick(&self) -> Result<Option<PathBuf>, String>;
}

/// The real picker: hands the request to the platform's own dialog helper, so no
/// dialog dependency (and no dialog permission for the WebView) is introduced.
///
/// The helper's filter is a convenience only — the rules that decide what may be
/// written are enforced in Rust afterwards, so a helper that ignores its filter
/// cannot widen anything.
struct SystemImagePicker;

impl ImagePicker for SystemImagePicker {
    fn pick(&self) -> Result<Option<PathBuf>, String> {
        platform_pick_image()
    }
}

/// Runs a dialog helper and reads one path from its standard output.
///
/// A cancelled dialog is a helper that exits non-zero with nothing to say, or one
/// that succeeds with empty output: both are reported as "cancelled". Anything a
/// helper explains on stderr is a real failure, so a missing or broken helper is
/// never silently reported as a cancelled selection.
fn run_dialog_helper(program: &str, args: &[&str]) -> Result<Option<PathBuf>, String> {
    let output = match Command::new(program).args(args).output() {
        Ok(output) => output,
        Err(error) => return Err(format!("{program} is not available: {error}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() && !(stdout.is_empty() && stderr.is_empty()) {
        return Err(format!("{program} failed: {stderr}"));
    }
    if stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(stdout)))
}

#[cfg(target_os = "macos")]
fn platform_pick_image() -> Result<Option<PathBuf>, String> {
    // `choose file` raises -128 when the user cancels; that is turned into empty
    // output so it is reported as a cancelled selection.
    const SCRIPT: &str = "try\nset chosen to choose file with prompt \"Choose a system image (.img or .img.xz)\" of type {\"img\", \"xz\"}\nreturn POSIX path of chosen\non error number -128\nreturn \"\"\nend try";
    run_dialog_helper("osascript", &["-e", SCRIPT])
}

#[cfg(target_os = "linux")]
fn platform_pick_image() -> Result<Option<PathBuf>, String> {
    // The two common dialog helpers, tried in turn; the last explanation is
    // reported if neither is available.
    let candidates: [(&str, &[&str]); 2] = [
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Choose a system image",
                "--file-filter=Images | *.img *.img.xz",
                "--file-filter=All files | *",
            ],
        ),
        (
            "kdialog",
            &["--getopenfilename", ".", "Images (*.img *.img.xz)"],
        ),
    ];
    let mut last = None;
    for (program, args) in candidates {
        match run_dialog_helper(program, args) {
            Ok(result) => return Ok(result),
            Err(reason) => last = Some(reason),
        }
    }
    Err(last.unwrap_or_else(|| "no file chooser is available".to_owned()))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_pick_image() -> Result<Option<PathBuf>, String> {
    // Honest refusal instead of an untested dialog implementation: this host has
    // no picker, so the user still has the catalogue route.
    Err(
        "choosing an image file is implemented on Linux (zenity or kdialog) and macOS only; copy the image into the image directory and pick it from the list instead"
            .to_owned(),
    )
}

/// True for exactly the shape this process issues: `pick-`, 32 lowercase hex
/// characters, a dash, and a display slug. A path-shaped string can never match,
/// and a hand-written handle still has to exist in the registry below.
fn valid_pick_handle(handle: &str) -> bool {
    let Some(rest) = handle.strip_prefix(PICK_HANDLE_PREFIX) else {
        return false;
    };
    let Some((random, slug)) = rest.split_once('-') else {
        return false;
    };
    random.len() == PICK_HANDLE_RANDOM_BYTES * 2
        && random
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        && !slug.is_empty()
        && slug.len() <= 64
        && slug.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// Random bytes for a handle. Without `/dev/urandom` this falls back to the
/// hasher seed, which the standard library itself seeds from the OS: the random
/// part only has to be unguessable, because acceptance is decided by the registry
/// lookup, never by the handle alone.
fn pick_handle_random() -> [u8; PICK_HANDLE_RANDOM_BYTES] {
    let mut bytes = [0u8; PICK_HANDLE_RANDOM_BYTES];
    if let Ok(mut urandom) = File::open("/dev/urandom")
        && urandom.read_exact(&mut bytes).is_ok()
    {
        return bytes;
    }
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    std::process::id().hash(&mut hasher);
    (
        std::time::SystemTime::now(),
        &bytes as *const [u8; PICK_HANDLE_RANDOM_BYTES],
    )
        .hash(&mut hasher);
    hasher.finish().hash(&mut hasher);
    let first = hasher.finish();
    hasher.finish().hash(&mut hasher);
    let second = hasher.finish();
    bytes[..8].copy_from_slice(&first.to_le_bytes());
    bytes[8..].copy_from_slice(&second.to_le_bytes());
    bytes
}

fn hex_of(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// A picked image, plus the handle that is the only name a request may use for it.
struct PickedImage {
    path: PathBuf,
    picked_at: SystemTime,
}

/// Images the user chose through the Rust-side dialog. Their paths are held here,
/// not in anything the WebView sends; the WebView holds handles, and a handle is
/// accepted only if this process issued it and it has not expired. What is kept
/// from the page is the ability to *name* a path, not the ability to read one: the
/// plan echoes the resolved `image.path` like every other surface.
struct PickedImages {
    entries: Mutex<HashMap<String, PickedImage>>,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
}

impl PickedImages {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            now: Box::new(SystemTime::now),
        }
    }

    #[cfg(test)]
    fn with_clock(now: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            now: Box::new(now),
        }
    }

    /// Registers a path that already passed the image rules, and returns the
    /// handle plus the display metadata the WebView may show.
    fn insert(&self, path: PathBuf, name: &str) -> String {
        let slug: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                    character
                } else {
                    '-'
                }
            })
            .take(48)
            .collect();
        let handle = format!(
            "{PICK_HANDLE_PREFIX}{}-{}",
            hex_of(&pick_handle_random()),
            slug
        );
        let mut entries = self.entries.lock().expect("picked image lock");
        entries.insert(
            handle.clone(),
            PickedImage {
                path,
                picked_at: (self.now)(),
            },
        );
        handle
    }

    /// Resolves a handle to the path this process picked. A handle this process
    /// did not issue, one whose shape is wrong, and one that expired are all
    /// refused — and refused *here*, never by being treated as a file name or a
    /// path inside the catalogue directory.
    fn resolve(&self, handle: &str) -> Result<PathBuf, SystemInstallError> {
        let invalid = |detail: &str| SystemInstallError::InvalidInput(detail.to_owned());
        if !valid_pick_handle(handle) {
            return Err(invalid(
                "the image handle is not one this process issued; choose the image through the desktop shell",
            ));
        }
        let entries = self.entries.lock().expect("picked image lock");
        let Some(entry) = entries.get(handle) else {
            return Err(invalid(
                "the image handle is not one this process issued; choose the image through the desktop shell",
            ));
        };
        let age = (self.now)()
            .duration_since(entry.picked_at)
            .unwrap_or_default();
        if age > PICK_HANDLE_TTL {
            return Err(invalid(
                "the image selection has expired; choose the image again",
            ));
        }
        Ok(entry.path.clone())
    }
}

/// The image rules the CLI applies before it will plan: absolute, a regular file,
/// not a symlink, not empty, and named like an image. Whether the image sits on
/// the target device is left to the core, which knows the mounts and the running
/// system.
fn validate_picked_image(path: &FsPath) -> Result<(PathBuf, String, u64), SystemInstallError> {
    let invalid = |detail: &str| SystemInstallError::InvalidInput(detail.to_owned());
    if !path.is_absolute() {
        return Err(invalid("the chosen image must be an absolute path"));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| invalid(&format!("the chosen image cannot be read: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(invalid(
            "the chosen image must be a regular file, not a symlink or a directory",
        ));
    }
    if metadata.len() == 0 {
        return Err(invalid("the chosen image is empty"));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("the chosen image name cannot be read"))?
        .to_owned();
    if !(name.ends_with(".img") || name.ends_with(".img.xz")) {
        return Err(invalid("the chosen image must end with .img or .img.xz"));
    }
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| invalid(&format!("the chosen image cannot be resolved: {error}")))?;
    Ok((resolved, name, metadata.len()))
}

fn install_manager(mode: InstallMode) -> SystemInstallManager {
    let backend: Arc<dyn BlockDeviceBackend> = if mode.synthetic {
        Arc::new(SyntheticBackend::default())
    } else {
        Arc::new(SystemBlockBackend::new(None))
    };
    let backend: Arc<dyn BlockDeviceBackend> = if mode.live {
        backend
    } else {
        Arc::new(RefusingWriter { inner: backend })
    };
    SystemInstallManager::with_backend(backend, Arc::new(SystemImageOpener), mode.synthetic)
}

/// One core manager per app instance: an image token issued by `plan` is consumed
/// by the same manager in `apply`, exactly as the CLI and the TUI do it in one
/// session. Tokens stay opaque here — the core issues and checks them.
struct InstallSurface {
    controller: Controller,
    image_directory: PathBuf,
    mode: OnceLock<InstallMode>,
    manager: OnceLock<SystemInstallManager>,
    /// Images the user picked through the Rust-side dialog. Their paths are held
    /// here rather than in anything the WebView sends: a page can only point the
    /// core at a picked file through a handle. (The plan response still echoes the
    /// resolved `image.path`, as it does on every surface.)
    picks: PickedImages,
    /// Injectable so the registration and validation logic is testable without a
    /// real dialog.
    picker: Arc<dyn ImagePicker>,
}

impl InstallSurface {
    fn new(controller: Controller) -> Self {
        Self::with_picker(controller, Arc::new(SystemImagePicker))
    }

    #[cfg(test)]
    fn new_for_tests(
        controller: Controller,
        picker: Arc<dyn ImagePicker>,
        now: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> Self {
        let mut surface = Self::with_picker(controller, picker);
        surface.picks = PickedImages::with_clock(now);
        surface
    }

    fn with_picker(controller: Controller, picker: Arc<dyn ImagePicker>) -> Self {
        Self {
            controller,
            image_directory: image_directory(),
            mode: OnceLock::new(),
            manager: OnceLock::new(),
            picks: PickedImages::new(),
            picker,
        }
    }

    /// The provider (demo or real) comes from the controller's own snapshot and
    /// the write policy from its execution policy, so all four surfaces decide
    /// this once, in the same place.
    fn mode(&self) -> Result<InstallMode, CommandError> {
        if let Some(mode) = self.mode.get() {
            return Ok(*mode);
        }
        let synthetic = self
            .controller
            .snapshot()
            .map_err(CommandError::internal)?
            .synthetic;
        let mode = InstallMode {
            synthetic,
            live: self.controller.policy() == ExecutionPolicy::Live,
        };
        let _ = self.mode.set(mode);
        Ok(mode)
    }

    fn live_allowed(&self) -> Result<bool, CommandError> {
        Ok(self.mode()?.live)
    }

    fn manager(&self) -> Result<&SystemInstallManager, CommandError> {
        let mode = self.mode()?;
        Ok(self.manager.get_or_init(|| install_manager(mode)))
    }

    /// Writes the image the way the CLI does: here when this process may, and
    /// through the fixed-verb helper when it may not.
    ///
    /// The shell is meant to run unprivileged, so without this a real write from
    /// the desktop would be the one apply in the product that never asks the
    /// administrator — opening the device would just fail. The helper re-plans,
    /// re-gates and re-judges the typed name on the other side.
    fn apply(
        &self,
        request: &SystemInstallRequest,
        plan_token: &str,
        acknowledgement: &str,
    ) -> Result<SystemInstallApplyResult, CommandError> {
        let mode = self.mode()?;
        if install_needs_privilege(mode.live, mode.synthetic, effective_uid()) {
            return run_privileged_system_install(request, plan_token, acknowledgement);
        }
        self.manager()?
            .apply_live(request, plan_token, acknowledgement)
            .map_err(|error| self.install_error(error))
    }

    /// Turns a UI supplied image id into the absolute catalogue path the core
    /// validates. The core resolves ids only when a catalogue is configured, so
    /// the surface resolves the id here and hands the core a path inside its own
    /// fixed directory.
    fn request(
        &self,
        target_id: &str,
        image_id: &str,
    ) -> Result<SystemInstallRequest, SystemInstallError> {
        // Two, and only two, ways a path can be named: a handle this process
        // issued for a file the user chose in the native dialog, or a plain file
        // name inside the fixed catalogue directory. A handle-shaped string is
        // never treated as a file name, and a path-shaped string is never treated
        // as a handle — neither falls back to the other.
        let image_path = if image_id.starts_with(PICK_HANDLE_PREFIX) {
            self.picks.resolve(image_id)?
        } else {
            catalog_image_path(&self.image_directory, image_id)?
        };
        Ok(SystemInstallRequest {
            target_id: target_id.to_owned(),
            image_path: Some(image_path.display().to_string()),
            // The id travels with the request so the plan can echo it back and a
            // client can build its apply payload from the plan response alone.
            // The core ignores it whenever a path is given, and the token binding
            // uses the resolved path, size and hash — not this label — so the
            // echoed id can never widen what may be written.
            image_id: Some(image_id.to_owned()),
        })
    }

    /// Opens the native file chooser from Rust and keeps the resulting path here.
    /// The pick answer carries an opaque handle plus display metadata and no path;
    /// the plan response will echo the resolved `image.path`, as it does for a
    /// catalogue id.
    fn pick_image(&self) -> Result<PickedImageResponse, CommandError> {
        let picked = self.picker.pick().map_err(|detail| CommandError {
            code: "install_unsupported",
            message: detail,
        })?;
        // A cancelled dialog is an answer, not a failure: nothing was chosen and
        // nothing will be written.
        let Some(path) = picked else {
            return Ok(PickedImageResponse::cancelled());
        };
        let (path, name, size_bytes) = validate_picked_image(&path).map_err(CommandError::from)?;
        let handle = self.picks.insert(path, &name);
        Ok(PickedImageResponse {
            cancelled: false,
            handle: Some(handle),
            name: Some(name),
            size_bytes: Some(size_bytes),
        })
    }

    /// Maps a core error, recognising this surface's own write refusal so a dry
    /// run reports the policy instead of "a partial image was written".
    fn install_error(&self, error: SystemInstallError) -> CommandError {
        if !self.live_allowed().unwrap_or(true) && error.to_string().contains(LIVE_REQUIRED_MARKER)
        {
            return CommandError {
                code: "install_unsupported",
                message: "Live execution is not enabled: restart the desktop shell with --live-execution to write an image. Nothing was written.".into(),
            };
        }
        CommandError::from(error)
    }

    /// Only ids `apply` would accept are listed, so the UI cannot be handed an id
    /// that would be rejected later.
    fn catalog(&self) -> Vec<CatalogImage> {
        let Ok(entries) = std::fs::read_dir(&self.image_directory) else {
            return Vec::new();
        };
        let mut images = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let id = entry.file_name().to_str()?.to_owned();
                if catalog_image_path(&self.image_directory, &id).is_err() {
                    return None;
                }
                Some(CatalogImage {
                    id,
                    size_bytes: entry.metadata().ok()?.len(),
                })
            })
            .collect::<Vec<_>>();
        images.sort_by(|left, right| left.id.cmp(&right.id));
        images
    }
}

/// What the WebView gets for a pick. Exactly one of the two shapes is filled in: a
/// cancelled dialog carries no handle at all, and a successful pick carries a
/// handle and display metadata but no path.
///
/// That guarantee is scoped to this answer: the *pick response* never carries a
/// path. The plan response does echo the resolved `image.path`, exactly as the
/// catalogue, CLI, TUI and HTTP surfaces do — a page can read a path, it just
/// cannot name one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PickedImageResponse {
    cancelled: bool,
    handle: Option<String>,
    name: Option<String>,
    size_bytes: Option<u64>,
}

impl PickedImageResponse {
    fn cancelled() -> Self {
        Self {
            cancelled: true,
            handle: None,
            name: None,
            size_bytes: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogImage {
    id: String,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallOptions {
    /// Whether a write is permitted at all. When false the shell refuses the
    /// write instead of simulating one.
    live_allowed: bool,
    targets: Vec<SystemInstallTarget>,
    images: Vec<CatalogImage>,
}

#[tauri::command]
async fn system_install_targets(
    surface: tauri::State<'_, Arc<InstallSurface>>,
) -> Result<InstallOptions, CommandError> {
    let surface = surface.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let targets = surface
            .manager()?
            .targets()
            .map_err(|error| surface.install_error(error))?;
        Ok(InstallOptions {
            live_allowed: surface.live_allowed()?,
            targets,
            images: surface.catalog(),
        })
    })
    .await
    .map_err(CommandError::internal)?
}

/// Opens the native image picker in Rust. The WebView cannot name a path: it asks
/// this process to ask the user, and receives an opaque handle back.
#[tauri::command]
async fn system_install_pick_image(
    surface: tauri::State<'_, Arc<InstallSurface>>,
) -> Result<PickedImageResponse, CommandError> {
    let surface = surface.inner().clone();
    tauri::async_runtime::spawn_blocking(move || surface.pick_image())
        .await
        .map_err(CommandError::internal)?
}

#[tauri::command]
async fn system_install_plan(
    surface: tauri::State<'_, Arc<InstallSurface>>,
    target_id: String,
    image_id: String,
) -> Result<SystemInstallPlan, CommandError> {
    let surface = surface.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let request = surface
            .request(&target_id, &image_id)
            .map_err(|error| surface.install_error(error))?;
        surface
            .manager()?
            .plan(&request)
            .map_err(|error| surface.install_error(error))
    })
    .await
    .map_err(CommandError::internal)?
}

#[tauri::command]
async fn system_install_apply(
    surface: tauri::State<'_, Arc<InstallSurface>>,
    target_id: String,
    image_id: String,
    plan_token: String,
    acknowledgement: String,
) -> Result<SystemInstallApplyResult, CommandError> {
    let surface = surface.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let request = surface
            .request(&target_id, &image_id)
            .map_err(|error| surface.install_error(error))?;
        // The token is opaque: it is the core's to issue and to check. The
        // acknowledgement is judged inside the core, so the shell cannot drift
        // from the CLI, the TUI or the HTTP surface.
        surface.apply(&request, &plan_token, &acknowledgement)
    })
    .await
    .map_err(CommandError::internal)?
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
        storage_status,
        system_install_targets,
        system_install_pick_image,
        system_install_plan,
        system_install_apply
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
    let install_surface = Arc::new(InstallSurface::new(controller.clone()));
    tauri::Builder::default()
        .manage(controller)
        .manage(install_surface)
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
        "system_install_targets",
        "system_install_pick_image",
        "system_install_plan",
        "system_install_apply",
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
        "system_install_targets",
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
        // The registry test dispatches every command, including the image picker.
        // A scripted answer keeps that synchronous and never opens a dialog, so no
        // test can block waiting for a human to dismiss one.
        let surface = Arc::new(InstallSurface::with_picker(
            controller.clone(),
            ScriptedPicker::cancelling(),
        ));
        let app = tauri::test::mock_builder()
            .manage(controller)
            .manage(surface)
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

    /// Fixture catalogue directory under `target/`, shared by every install test
    /// in this process. Only plain files with an image name are listed, and the
    /// core never sees a UI supplied path.
    fn install_fixture_directory() -> PathBuf {
        static DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        DIRECTORY
            .get_or_init(|| {
                let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../target/desktop-install-tests");
                std::fs::create_dir_all(&directory).expect("create the fixture directory");
                let mut image = vec![0u8; 2 * 1024 * 1024];
                image[510] = 0x55;
                image[511] = 0xAA;
                image[446 + 4] = 0x83;
                image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
                image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
                std::fs::write(directory.join("demo.img"), &image)
                    .expect("write the fixture image");
                std::fs::write(directory.join("notes.txt"), b"not an image")
                    .expect("write the non-image fixture");
                // A second image, so "preview one, apply for another" is a real
                // image change without mutating a fixture another test reads.
                let mut other = vec![0u8; 4 * 1024 * 1024];
                other[510] = 0x55;
                other[511] = 0xAA;
                other[446 + 4] = 0x83;
                other[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
                other[446 + 12..446 + 16].copy_from_slice(&4096u32.to_le_bytes());
                std::fs::write(directory.join("other.img"), &other)
                    .expect("write the second fixture image");
                set_image_directory(directory.clone());
                directory
            })
            .clone()
    }

    #[test]
    fn desktop_system_install_requires_an_acknowledgement() {
        let directory = install_fixture_directory();
        let (_app, webview) = demo_webview();

        // The app never hides its policy: a dry-run shell says so up front.
        let options = invoke(&webview, "system_install_targets", serde_json::json!({}))
            .expect("system_install_targets dispatches");
        assert_eq!(
            options["liveAllowed"],
            serde_json::json!(false),
            "{options}"
        );
        let ids = options["images"]
            .as_array()
            .expect("images array")
            .iter()
            .map(|image| image["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["demo.img".to_string(), "other.img".to_string()],
            "the catalogue listing is exactly what apply accepts: {options}"
        );

        // Only catalogue ids are accepted; a path or a traversal id is rejected by
        // this crate, before the core is reached.
        for image_id in [
            "../demo.img",
            "sub/demo.img",
            "notes.txt",
            "",
            "/etc/passwd",
        ] {
            let error = invoke(
                &webview,
                "system_install_plan",
                serde_json::json!({"targetId": "sda", "imageId": image_id}),
            )
            .expect_err(&format!("{image_id:?} must be rejected"));
            assert_eq!(
                error_code(&error),
                "invalid_install_request",
                "{image_id:?}: {error}"
            );
        }

        let plan = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        )
        .expect("system_install_plan dispatches");
        let expected = std::fs::canonicalize(directory.join("demo.img")).unwrap();
        assert_eq!(
            plan["image"]["path"].as_str(),
            Some(expected.display().to_string().as_str()),
            "{plan}"
        );
        assert_eq!(plan["destroys"].as_array().unwrap().len(), 3, "{plan}");
        let token = plan["planToken"].as_str().expect("plan token").to_string();

        // A missing acknowledgement and a wrong one are the core's own verdicts.
        for (acknowledgement, expected_code) in [
            ("", "confirmation_required"),
            ("true", "acknowledgement_mismatch"),
            ("mmcblk0", "acknowledgement_mismatch"),
        ] {
            let error = invoke(
                &webview,
                "system_install_apply",
                serde_json::json!({
                    "targetId": "sda",
                    "imageId": "demo.img",
                    "planToken": token,
                    "acknowledgement": acknowledgement
                }),
            )
            .expect_err(&format!("{acknowledgement:?} must not confirm"));
            assert_eq!(
                error_code(&error),
                expected_code,
                "{acknowledgement:?}: {error}"
            );
        }

        // Even the exact device name cannot write here: the shell refuses instead
        // of simulating a write, so the UI cannot believe it installed a system.
        let error = invoke(
            &webview,
            "system_install_apply",
            serde_json::json!({
                "targetId": "sda",
                "imageId": "demo.img",
                "planToken": token,
                "acknowledgement": "sda"
            }),
        )
        .expect_err("a dry-run shell refuses the write");
        assert_eq!(error_code(&error), "install_unsupported", "{error}");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.to_lowercase().contains("live execution")),
            "{error}"
        );
    }

    #[test]
    fn desktop_system_install_applies_with_the_live_policy() {
        let _directory = install_fixture_directory();
        // Demo provider plus the live policy: the write is real code but lands in
        // the demo provider, never on a device.
        let (_app, webview) = mock_webview(Controller::new(ProbeMode::Demo, ExecutionPolicy::Live));

        let options = invoke(&webview, "system_install_targets", serde_json::json!({}))
            .expect("system_install_targets dispatches");
        assert_eq!(options["liveAllowed"], serde_json::json!(true), "{options}");

        let plan = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        )
        .expect("system_install_plan dispatches");
        // The plan echoes the identity apply accepts, so the payload below is
        // built from the response alone.
        assert_eq!(plan["image"]["id"], serde_json::json!("demo.img"), "{plan}");
        assert_eq!(plan["target"]["id"], serde_json::json!("sda"), "{plan}");
        let token = plan["planToken"].as_str().expect("plan token").to_string();

        let result = invoke(
            &webview,
            "system_install_apply",
            serde_json::json!({
                "targetId": plan["target"]["id"].clone(),
                "imageId": plan["image"]["id"].clone(),
                "planToken": token,
                "acknowledgement": plan["target"]["id"].clone()
            }),
        )
        .expect("the live policy writes into the demo provider");
        assert_eq!(
            result["bytesWritten"],
            serde_json::json!(2 * 1024 * 1024),
            "{result}"
        );
        assert_eq!(result["plan"]["image"]["id"], serde_json::json!("demo.img"));
        assert_eq!(result["run"]["synthetic"], serde_json::json!(true));

        // Preview one image, then apply for another: the token binds the image
        // identity, so the write is refused instead of landing the wrong image.
        let mismatch_plan = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        )
        .expect("system_install_plan dispatches");
        let error = invoke(
            &webview,
            "system_install_apply",
            serde_json::json!({
                "targetId": mismatch_plan["target"]["id"].clone(),
                "imageId": "other.img",
                "planToken": mismatch_plan["planToken"].clone(),
                "acknowledgement": mismatch_plan["target"]["id"].clone()
            }),
        )
        .expect_err("an image change invalidates the plan");
        assert_eq!(error_code(&error), "stale_plan", "{error}");

        // The core's running-system gate holds over IPC too.
        let error = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "mmcblk1", "imageId": "demo.img"}),
        )
        .expect_err("the boot device is refused");
        assert_eq!(error_code(&error), "writes_running_system", "{error}");
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
        // The reason has to reach the caller, but the exact rendering belongs to
        // the core's `HardwareError` Display and is shared with the HTTP surface.
        // Pinning the bare text here asserted a message no surface produces.
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("injected storage failure"),
            "the reader's reason must be reported: {message}"
        );
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

    /// A picker that answers with a scripted result, so registration and
    /// validation are exercised without a real dialog (and CI never blocks on
    /// one).
    struct ScriptedPicker {
        answer: Mutex<Result<Option<PathBuf>, String>>,
    }

    impl ScriptedPicker {
        fn new(answer: Result<Option<PathBuf>, String>) -> Arc<Self> {
            Arc::new(Self {
                answer: Mutex::new(answer),
            })
        }

        fn cancelling() -> Arc<Self> {
            Self::new(Ok(None))
        }

        fn choosing(path: &FsPath) -> Arc<Self> {
            Self::new(Ok(Some(path.to_path_buf())))
        }

        fn failing(reason: &str) -> Arc<Self> {
            Self::new(Err(reason.to_owned()))
        }
    }

    impl ImagePicker for ScriptedPicker {
        fn pick(&self) -> Result<Option<PathBuf>, String> {
            self.answer.lock().expect("script lock").clone()
        }
    }

    /// A valid image *outside* the catalogue directory, so a picked file is never
    /// confused with a catalogue id. The bytes are the catalogue fixture's own.
    fn pick_fixture(name: &str) -> PathBuf {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../target/desktop-pick-tests");
        std::fs::create_dir_all(&directory).expect("create the pick fixture directory");
        let path = directory.join(name);
        // Replace whatever a previous run left behind, including a symlink a
        // failed run may have created at this path.
        let _ = std::fs::remove_file(&path);
        let source = std::fs::read(install_fixture_directory().join("demo.img"))
            .expect("read the catalogue fixture");
        std::fs::write(&path, source).expect("write the pick fixture");
        path
    }

    fn pick_webview(
        controller: Controller,
        picker: Arc<dyn ImagePicker>,
        now: impl Fn() -> SystemTime + Send + Sync + 'static,
    ) -> (tauri::App<tauri::test::MockRuntime>, MockWebview) {
        // The catalogue is a process-wide override that is set once, so point it
        // at the fixture directory before any surface reads it.
        set_image_directory(install_fixture_directory());
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        for command in IPC_COMMANDS {
            context.runtime_authority_mut().__allow_command(
                (*command).to_string(),
                tauri::utils::acl::ExecutionContext::Local,
            );
        }
        let surface = Arc::new(InstallSurface::new_for_tests(
            controller.clone(),
            picker,
            now,
        ));
        let app = tauri::test::mock_builder()
            .manage(controller)
            .manage(surface)
            .invoke_handler(command_handler())
            .build(context)
            .expect("mock desktop app builds");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
            .build()
            .expect("mock desktop webview builds");
        (app, webview)
    }

    fn apply_body(handle: &str, token: &str) -> serde_json::Value {
        serde_json::json!({
            "targetId": "sda",
            "imageId": handle,
            "planToken": token,
            "acknowledgement": "sda",
        })
    }

    /// The picker runs in Rust and the pick answer carries only a handle: the path
    /// must not appear anywhere in that answer, and the handle must not look like a
    /// path. The scope is this answer — the plan response does echo the resolved
    /// `image.path`, which this test does not assert. The handle then plans and
    /// applies through the same code path as a catalogue id.
    #[test]
    fn desktop_pick_image_answers_with_an_opaque_handle_not_a_path() {
        let image = pick_fixture("downloaded.img");
        // Live policy on the demo provider: the production write path runs against
        // the synthetic backend, so a handle is applied end to end without a device.
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::Live),
            ScriptedPicker::choosing(&image),
            SystemTime::now,
        );
        let picked = invoke(&webview, "system_install_pick_image", serde_json::json!({}))
            .expect("picking answers");

        assert_eq!(picked["cancelled"], serde_json::json!(false));
        let handle = picked["handle"].as_str().expect("a handle").to_owned();
        assert!(handle.starts_with("pick-"), "{handle}");
        assert!(!handle.contains('/'), "a handle is not a path: {handle}");
        assert_eq!(
            picked["name"].as_str(),
            Some("downloaded.img"),
            "the display name is the file name"
        );
        let size = std::fs::metadata(&image).expect("fixture metadata").len();
        assert_eq!(picked["sizeBytes"].as_u64(), Some(size));
        assert!(
            valid_pick_handle(&handle),
            "the handle has the documented shape: {handle}"
        );
        // The scope of this assertion: the pick answer carries no path. The plan
        // response echoes the resolved path; that is a different answer, asserted
        // where the plan is.
        let serialized = picked.to_string();
        assert!(
            !serialized.contains(&image.display().to_string()),
            "the pick answer must not carry the picked path: {serialized}"
        );
        assert!(
            !serialized.contains("desktop-pick-tests"),
            "not even part of the path: {serialized}"
        );

        // The handle is usable exactly like a catalogue id, and the plan echoes the
        // identity apply needs (t22/t25: one source, no UI memory).
        let plan = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": handle}),
        )
        .expect("the handle plans");
        assert_eq!(plan["image"]["id"].as_str(), Some(handle.as_str()));
        assert_eq!(plan["image"]["sizeBytes"].as_u64(), Some(size));

        let token = plan["planToken"].as_str().expect("a plan token").to_owned();
        let result = invoke(
            &webview,
            "system_install_apply",
            apply_body(&handle, &token),
        )
        .expect("the handle applies");
        assert_eq!(result["bytesWritten"].as_u64(), Some(size));
        assert_eq!(result["run"]["status"].as_str(), Some("succeeded"));
        let _ = app;
    }

    /// Cancelling is an answer, not a failure, and produces no handle at all.
    #[test]
    fn desktop_pick_image_reports_a_cancelled_dialog_as_an_answer() {
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            ScriptedPicker::cancelling(),
            SystemTime::now,
        );
        let picked = invoke(&webview, "system_install_pick_image", serde_json::json!({}))
            .expect("cancelling is not an error");
        assert_eq!(picked["cancelled"], serde_json::json!(true));
        assert_eq!(picked["handle"], serde_json::json!(null));
        assert_eq!(picked["name"], serde_json::json!(null));
        let _ = app;

        // A picker that cannot run explains itself instead of pretending the user
        // cancelled.
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            ScriptedPicker::failing("no file chooser is available"),
            SystemTime::now,
        );
        let error = invoke(&webview, "system_install_pick_image", serde_json::json!({}))
            .expect_err("a broken picker is reported");
        assert_eq!(error_code(&error), "install_unsupported");
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("no file chooser"),
            "{error}"
        );
        let _ = app;
    }

    /// The boundary invariant: nothing but a handle this process issued may reach
    /// the core. Path-shaped input is refused where it arrives, and it is never
    /// retried as a file name or a relative path.
    #[test]
    fn desktop_plan_refuses_everything_but_an_issued_handle() {
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            ScriptedPicker::cancelling(),
            SystemTime::now,
        );
        let unsigned = "pick-000102030405060708090a0b0c0d0e0f-demo.img";
        let refusals = [
            // Path shapes: neither the catalogue nor a handle may see these.
            "/etc/passwd",
            "/tmp/passwd.img",
            "../demo.img",
            "../../etc/passwd",
            "./demo.img",
            "~/demo.img",
            "~",
            "..",
            "desktop-pick-tests/demo.img",
            // Handle shapes this process never issued.
            unsigned,
            "pick-00000000000000000000000000000000",
            "pick-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-demo.img",
            "pick-000102030405060708090a0b0c0d0e0f-../../etc/passwd",
            "pick-000102030405060708090A0B0C0D0E0F-demo.img",
            "pick-000102030405060708090a0b0c0d0e0f",
            "pick--demo.img",
            "pick-000102030405060708090a0b0c0d0e0f-",
        ];
        for image_id in refusals {
            match invoke(
                &webview,
                "system_install_plan",
                serde_json::json!({"targetId": "sda", "imageId": image_id}),
            ) {
                Ok(value) => panic!("{image_id} was accepted and planned: {value}"),
                Err(error) => {
                    assert_eq!(
                        error_code(&error),
                        "invalid_install_request",
                        "{image_id} must be refused with a stable code: {error}"
                    );
                    println!("REFUSED[{image_id}]: {error}");
                }
            }
        }
        // Control: the catalogue route still works, so the refusals above are
        // targeted rather than a blanket failure.
        let plan = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        )
        .expect("a catalogue id still plans");
        assert_eq!(plan["image"]["id"].as_str(), Some("demo.img"));
        let _ = app;
    }

    /// A handle stops working when it expires, and it is refused in the same place
    /// as an unknown one.
    #[test]
    fn desktop_pick_image_handles_expire() {
        let image = pick_fixture("expiring.img");
        let clock = Arc::new(Mutex::new(SystemTime::now()));
        let clock_for_surface = clock.clone();
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            ScriptedPicker::choosing(&image),
            move || *clock_for_surface.lock().expect("clock lock"),
        );
        let picked = invoke(&webview, "system_install_pick_image", serde_json::json!({}))
            .expect("picking answers");
        let handle = picked["handle"].as_str().expect("a handle").to_owned();
        invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": handle}),
        )
        .expect("a fresh handle plans");

        // A fixed step, not "the constant plus one": otherwise a mutated lifetime
        // would move the test with it and the check would be vacuous.
        assert_eq!(
            PICK_HANDLE_TTL,
            Duration::from_secs(600),
            "the handle lifetime is part of the contract"
        );
        *clock.lock().expect("clock lock") += Duration::from_secs(601);
        let error = invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": handle}),
        )
        .expect_err("an expired handle is refused");
        assert_eq!(error_code(&error), "invalid_install_request");
        assert!(
            error["message"]
                .as_str()
                .unwrap_or_default()
                .contains("expired"),
            "{error}"
        );
        let _ = app;
    }

    /// Replacing the picked file behind the same handle must not silently write
    /// something else: the core re-checks the path and refuses a symlink.
    #[test]
    fn desktop_handle_cannot_be_repointed_at_another_file() {
        let image = pick_fixture("repointed.img");
        let other = pick_fixture("repointed-other.img");
        let (app, webview) = pick_webview(
            Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            ScriptedPicker::choosing(&image),
            SystemTime::now,
        );
        let picked = invoke(&webview, "system_install_pick_image", serde_json::json!({}))
            .expect("picking answers");
        let handle = picked["handle"].as_str().expect("a handle").to_owned();
        invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": handle}),
        )
        .expect("the picked file plans");

        // The same path now names a different file through a symlink.
        std::fs::rename(&image, image.with_extension("moved")).expect("move the picked file");
        std::os::unix::fs::symlink(&other, &image).expect("repoint the path at another file");

        match invoke(
            &webview,
            "system_install_plan",
            serde_json::json!({"targetId": "sda", "imageId": handle}),
        ) {
            Ok(value) => panic!("a symlinked path must not plan: {value}"),
            Err(error) => {
                assert_eq!(error_code(&error), "invalid_install_request");
                let message = error["message"].as_str().unwrap_or_default();
                assert!(
                    message.contains("not a regular file"),
                    "the reason names what the path became: {error}"
                );
                assert!(
                    message.contains("repointed.img"),
                    "and the path it refused: {error}"
                );
                println!("REPOINTED: {error}");
            }
        }
        let _ = app;
    }

    /// The pick command opens a dialog, so it must not be driven by the
    /// read-only JSON guard (which would block CI on a real dialog); it is the
    /// registry guard that covers it.
    #[test]
    fn desktop_pick_image_is_registered_but_not_read_only() {
        assert!(IPC_COMMANDS.contains(&"system_install_pick_image"));
        assert!(!READ_ONLY_COMMANDS.contains(&"system_install_pick_image"));
    }

    /// This crate keeps its own copy of the routing rule, because the CLI's lives
    /// in a binary crate it does not depend on. The table is the guard against
    /// the two copies drifting.
    /// The delegated path must keep the helper's classification instead of
    /// reporting every refusal as an internal error.
    #[test]
    fn the_helper_error_envelope_preserves_the_code() {
        let envelope = br#"{"error":{"code":"stale_plan","message":"the target changed"}}"#;
        assert_eq!(
            helper_error_envelope(envelope),
            Some(("stale_plan".to_owned(), "the target changed".to_owned()))
        );
        assert_eq!(helper_error_envelope(br#"{"bytesWritten":1}"#), None);
        assert_eq!(helper_error_envelope(b"Error: prose\n"), None);
        for code in [
            "install_unsupported",
            "invalid_install_request",
            "confirmation_required",
            "acknowledgement_mismatch",
            "plan_required",
            "stale_plan",
            "root_required",
            "install_conflict",
            "writes_running_system",
            "authorization_canceled",
        ] {
            assert_eq!(install_code_static(code), code, "{code}");
        }
        assert_eq!(install_code_static("something_new"), "internal_error");
    }

    #[test]
    fn install_privilege_routing_follows_policy_provider_and_uid() {
        let cases = [
            (true, false, Some(1000), true),
            (true, false, Some(0), false),
            (true, true, Some(1000), false),
            (false, false, Some(1000), false),
            (true, false, None, true),
        ];
        for (live, synthetic, uid, expected) in cases {
            assert_eq!(
                install_needs_privilege(live, synthetic, uid),
                expected,
                "live={live} synthetic={synthetic} uid={uid:?}"
            );
        }
    }

    /// The reported defect, at this surface: a live write the shell may not
    /// perform itself has to be delegated to the helper.
    ///
    /// The mode is set directly, so this needs no root, no `pkexec`, no device
    /// and no environment: delegation is observable because the helper's own
    /// precondition fails first on a host that does not ship it. A board that
    /// does ship it, or a run as root, would take the other branch and skips.
    #[test]
    fn install_apply_delegates_when_the_process_may_not_write() {
        if FsPath::new(PRIVILEGED_HELPER).is_file() || effective_uid() == Some(0) {
            return;
        }
        let surface = InstallSurface {
            controller: Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
            image_directory: PathBuf::from("/nonexistent-install-images"),
            mode: OnceLock::from(InstallMode {
                synthetic: false,
                live: true,
            }),
            manager: OnceLock::new(),
            picks: PickedImages::new(),
            picker: Arc::new(SystemImagePicker),
        };
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some("/nonexistent-install-images/demo.img".into()),
            image_id: None,
        };
        let error = surface
            .apply(&request, "preview-token", "sda")
            .expect_err("an unprivileged live write must not run in this process");
        assert_eq!(
            error.code, "root_required",
            "the delegated route was not taken: {error:?}"
        );
    }
}
