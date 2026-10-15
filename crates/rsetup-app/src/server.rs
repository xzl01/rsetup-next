use anyhow::Result;
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rsetup_core::{
    ActionRun, ActionSpec, ActivityEvent, BlockDeviceBackend, Controller, DeviceSnapshot,
    ExecutionPolicy, FanCurveApplyResult, FanCurvePlan, FanCurveRequest, FanCurveStatus,
    GpioStatus, LedStatus, OverlayApplyResult, OverlayPlan, OverlayStatus, RgbLedConfig,
    SourceApplyResult, SourcePlan, SourceStatus, SpiFlashApplyResult, SpiFlashPlan,
    SpiFlashRequest, SpiFlashStatus, StorageStatus, SyntheticBackend, SystemBlockBackend,
    SystemImageOpener, SystemInstallApplyResult, SystemInstallError, SystemInstallManager,
    SystemInstallPartition, SystemInstallPlan, SystemInstallRequest, SystemInstallTarget,
    SystemMount, ThermalStatus, VideoFrame, VideoStatus,
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

const INDEX_HTML: &str = include_str!("../../../ui/index.html");
const STYLES_CSS: &str = include_str!("../../../ui/styles.css");
const I18N_JS: &str = include_str!("../../../ui/i18n.js");
const APP_JS: &str = include_str!("../../../ui/app.js");
const VENDOR_PARTNERS: &[u8] = include_bytes!("../../../ui/assets/vendor-partners.webp");
const VENDOR_CIX: &[u8] = include_bytes!("../../../ui/assets/vendor-cix.png");
const COMMUNITY_QQ: &[u8] = include_bytes!("../../../ui/assets/community-qq.webp");
const COMMUNITY_WECHAT: &[u8] = include_bytes!("../../../ui/assets/community-wechat.png");
const FONT_REGULAR: &[u8] = include_bytes!("../../../ui/fonts/open-sans-regular.woff2");
const FONT_DISPLAY: &[u8] = include_bytes!("../../../ui/fonts/open-sans-800.woff2");
const FONT_MONO: &[u8] = include_bytes!("../../../ui/fonts/source-code-pro.woff2");
const FONT_ENGINEERED_SEMIBOLD: &[u8] =
    include_bytes!("../../../ui/fonts/BarlowCondensed-SemiBold.ttf");
const FONT_ENGINEERED_BLACK: &[u8] = include_bytes!("../../../ui/fonts/BarlowCondensed-Black.ttf");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunRequest {
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceRequest {
    provider_id: String,
    #[serde(default)]
    plan_token: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverlayRequest {
    #[serde(default)]
    selected_ids: Vec<String>,
    #[serde(default)]
    plan_token: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GpioQuery {
    profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpiFlashApiRequest {
    #[serde(flatten)]
    request: SpiFlashRequest,
    #[serde(default)]
    plan_token: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoCaptureRequest {
    device_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThermalPolicyRequest {
    policy: String,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FanCurveApiRequest {
    #[serde(flatten)]
    request: FanCurveRequest,
    #[serde(default)]
    plan_token: Option<String>,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedTriggerRequest {
    led_id: String,
    trigger: String,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RgbLedRequest {
    config: RgbLedConfig,
    #[serde(default)]
    confirm: bool,
}

pub async fn serve(controller: Controller, listen: SocketAddr) -> Result<()> {
    anyhow::ensure!(
        listen.ip().is_loopback(),
        "The unauthenticated control console only accepts loopback listeners. Use an SSH tunnel for remote access."
    );
    let app = router(controller);
    let listener = TcpListener::bind(listen).await?;
    tracing::info!("control center ready at http://{listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

pub fn router(controller: Controller) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/styles.css", get(styles))
        .route("/i18n.js", get(i18n_script))
        .route("/app.js", get(script))
        .route("/assets/vendor-partners.webp", get(vendor_partners))
        .route("/assets/vendor-cix.png", get(vendor_cix))
        .route("/assets/community-qq.webp", get(community_qq))
        .route("/assets/community-wechat.png", get(community_wechat))
        .route("/fonts/open-sans-regular.woff2", get(font_regular))
        .route("/fonts/open-sans-800.woff2", get(font_display))
        .route("/fonts/source-code-pro.woff2", get(font_mono))
        .route(
            "/fonts/barlow-condensed-semibold.ttf",
            get(font_engineered_semibold),
        )
        .route(
            "/fonts/barlow-condensed-black.ttf",
            get(font_engineered_black),
        )
        .route("/api/v1/health", get(health))
        .route("/api/v1/snapshot", get(snapshot))
        .route("/api/v1/actions", get(actions))
        .route("/api/v1/actions/{id}/run", post(run_action))
        .route("/api/v1/sources", get(source_status))
        .route("/api/v1/sources/benchmark", post(benchmark_source))
        .route("/api/v1/sources/plan", post(plan_sources))
        .route("/api/v1/sources/apply", post(apply_sources))
        .route("/api/v1/hardware/overlays", get(overlay_status))
        .route(
            "/api/v1/hardware/overlays/authorize",
            post(authorize_overlay_read),
        )
        .route("/api/v1/hardware/overlays/plan", post(plan_overlays))
        .route("/api/v1/hardware/overlays/apply", post(apply_overlays))
        .route("/api/v1/hardware/gpio", get(gpio_status))
        .route("/api/v1/hardware/spi-flash", get(spi_flash_status))
        .route("/api/v1/hardware/spi-flash/plan", post(plan_spi_flash))
        .route("/api/v1/hardware/spi-flash/apply", post(apply_spi_flash))
        .route("/api/v1/hardware/leds", get(led_status))
        .route("/api/v1/hardware/leds/trigger", post(apply_led_trigger))
        .route("/api/v1/hardware/leds/rgb", post(apply_rgb_led))
        .route("/api/v1/hardware/video", get(video_status))
        .route("/api/v1/hardware/video/capture", post(capture_video))
        .route("/api/v1/hardware/thermal", get(thermal_status))
        .route("/api/v1/hardware/thermal/apply", post(apply_thermal_policy))
        .route("/api/v1/hardware/thermal/fan-curve", get(fan_curve_status))
        .route(
            "/api/v1/hardware/thermal/fan-curve/plan",
            post(plan_fan_curve),
        )
        .route(
            "/api/v1/hardware/thermal/fan-curve/apply",
            post(apply_fan_curve),
        )
        .route("/api/v1/hardware/storage", get(storage_status))
        .route("/api/v1/storage/install/targets", get(install_targets))
        .route("/api/v1/storage/install/plan", post(plan_system_install))
        .route("/api/v1/storage/install/apply", post(apply_system_install))
        .route("/api/v1/activity", get(activity))
        .layer(Extension(Arc::new(InstallSurface::new(controller.clone()))))
        .layer(middleware::from_fn(local_boundary))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(controller))
}

async fn local_boundary(request: Request, next: Next) -> Response {
    let valid = valid_request_boundary(&request);
    let mut response = if valid {
        next.run(request).await
    } else {
        (StatusCode::FORBIDDEN, Json(serde_json::json!({"error": {"code": "request_forbidden", "message": "Use the local console or an SSH tunnel; cross-origin requests are forbidden."}}))).into_response()
    };
    let headers = response.headers_mut();
    headers.insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("cache-control", "no-store".parse().unwrap());
    response
}

fn valid_request_boundary(request: &Request) -> bool {
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().count() != 1 {
        return false;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if host.contains('@') {
        return false;
    }
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let hostname = authority.host().trim_matches(['[', ']']);
    if !hostname.eq_ignore_ascii_case("localhost")
        && !hostname
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return false;
    }
    if headers.get_all(header::ORIGIN).iter().count() > 1 {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.to_str().ok() != Some(format!("http://{host}").as_str()) {
            return false;
        }
    }
    if let Some(site) = headers.get("sec-fetch-site") {
        if !matches!(site.to_str(), Ok("same-origin" | "none")) {
            return false;
        }
    }
    request.method() == axum::http::Method::GET
        || request.method() == axum::http::Method::HEAD
        || headers
            .get("x-rsetup-request")
            .is_some_and(|value| value == "1")
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let permit = SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "server_busy",
                "Too many operations are running; retry after they complete.".into(),
            )
        })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(ApiError::internal)?
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn i18n_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        I18N_JS,
    )
}

async fn vendor_partners() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/webp")], VENDOR_PARTNERS)
}

async fn vendor_cix() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], VENDOR_CIX)
}

async fn community_qq() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/webp")], COMMUNITY_QQ)
}

async fn community_wechat() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], COMMUNITY_WECHAT)
}

async fn font_regular() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], FONT_REGULAR)
}

async fn font_display() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], FONT_DISPLAY)
}

async fn font_mono() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2")], FONT_MONO)
}

async fn font_engineered_semibold() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "font/ttf")],
        FONT_ENGINEERED_SEMIBOLD,
    )
}

async fn font_engineered_black() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/ttf")], FONT_ENGINEERED_BLACK)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok", "service": "rsetup-next"}))
}

async fn snapshot(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<DeviceSnapshot>, ApiError> {
    blocking(move || controller.snapshot().map(Json).map_err(ApiError::internal)).await
}

async fn actions(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<Vec<ActionSpec>>, ApiError> {
    blocking(move || Ok(Json(controller.actions()))).await
}

async fn activity(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<Vec<ActivityEvent>>, ApiError> {
    blocking(move || Ok(Json(controller.activity()))).await
}

async fn run_action(
    State(controller): State<Arc<Controller>>,
    Path(id): Path<String>,
    Json(request): Json<RunRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    blocking(move || {
        controller
            .execute(&id, request.confirm)
            .map(Json)
            .map_err(|error| {
                use rsetup_core::ActionError;
                match error {
                    ActionError::Unknown(_) => {
                        ApiError::new(StatusCode::NOT_FOUND, "unknown_action", error.to_string())
                    }
                    ActionError::ConfirmationRequired(_) => ApiError::new(
                        StatusCode::CONFLICT,
                        "confirmation_required",
                        error.to_string(),
                    ),
                    ActionError::Unavailable(_) => ApiError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "action_unavailable",
                        error.to_string(),
                    ),
                    ActionError::RootRequired(_) => {
                        ApiError::new(StatusCode::FORBIDDEN, "root_required", error.to_string())
                    }
                    ActionError::AuthorizationCanceled => ApiError::new(
                        StatusCode::FORBIDDEN,
                        "authorization_canceled",
                        error.to_string(),
                    ),
                    ActionError::Authorization(_, _) => ApiError::new(
                        StatusCode::FORBIDDEN,
                        "authorization_failed",
                        error.to_string(),
                    ),
                    ActionError::InputRequired(_) => ApiError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "input_required",
                        error.to_string(),
                    ),
                    ActionError::Launch(_) => ApiError::internal(error),
                }
            })
    })
    .await
}

async fn source_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<SourceStatus>, ApiError> {
    blocking(move || {
        controller
            .source_status()
            .map(Json)
            .map_err(ApiError::from_source)
    })
    .await
}

async fn plan_sources(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<SourcePlan>, ApiError> {
    blocking(move || {
        controller
            .plan_source_change(&request.provider_id)
            .map(Json)
            .map_err(ApiError::from_source)
    })
    .await
}

async fn benchmark_source(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<rsetup_core::MirrorBenchmark>, ApiError> {
    blocking(move || {
        controller
            .benchmark_source(&request.provider_id)
            .map(Json)
            .map_err(ApiError::from_source)
    })
    .await
}

async fn apply_sources(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<SourceApplyResult>, ApiError> {
    blocking(move || {
        controller
            .apply_source_change(
                &request.provider_id,
                request.plan_token.as_deref().unwrap_or_default(),
                request.confirm,
            )
            .map(Json)
            .map_err(ApiError::from_source)
    })
    .await
}

async fn overlay_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<OverlayStatus>, ApiError> {
    blocking(move || {
        controller
            .overlay_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn plan_overlays(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<OverlayRequest>,
) -> Result<Json<OverlayPlan>, ApiError> {
    blocking(move || {
        controller
            .plan_overlay_change(&request.selected_ids)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn authorize_overlay_read(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<OverlayStatus>, ApiError> {
    blocking(move || {
        controller
            .authorize_overlay_read()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_overlays(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<OverlayRequest>,
) -> Result<Json<OverlayApplyResult>, ApiError> {
    blocking(move || {
        controller
            .apply_overlay_change(
                &request.selected_ids,
                request.plan_token.as_deref().unwrap_or_default(),
                request.confirm,
            )
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn gpio_status(
    State(controller): State<Arc<Controller>>,
    Query(query): Query<GpioQuery>,
) -> Result<Json<GpioStatus>, ApiError> {
    blocking(move || {
        controller
            .gpio_status_for_profile(query.profile.as_deref())
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn spi_flash_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<SpiFlashStatus>, ApiError> {
    blocking(move || {
        controller
            .spi_flash_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn plan_spi_flash(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SpiFlashApiRequest>,
) -> Result<Json<SpiFlashPlan>, ApiError> {
    blocking(move || {
        controller
            .plan_spi_flash(&request.request)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_spi_flash(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SpiFlashApiRequest>,
) -> Result<Json<SpiFlashApplyResult>, ApiError> {
    blocking(move || {
        controller
            .apply_spi_flash(
                &request.request,
                request.plan_token.as_deref().unwrap_or_default(),
                request.confirm,
            )
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn led_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<LedStatus>, ApiError> {
    blocking(move || {
        controller
            .led_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_led_trigger(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<LedTriggerRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    blocking(move || {
        controller
            .apply_led_trigger(&request.led_id, &request.trigger, request.confirm)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_rgb_led(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<RgbLedRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    blocking(move || {
        controller
            .apply_rgb_led(&request.config, request.confirm)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn video_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<VideoStatus>, ApiError> {
    blocking(move || {
        controller
            .video_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn capture_video(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<VideoCaptureRequest>,
) -> Result<Json<VideoFrame>, ApiError> {
    blocking(move || {
        controller
            .capture_video_frame(&request.device_id)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn thermal_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<ThermalStatus>, ApiError> {
    blocking(move || {
        controller
            .thermal_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_thermal_policy(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<ThermalPolicyRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    blocking(move || {
        controller
            .apply_thermal_policy(&request.policy, request.confirm)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn fan_curve_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<FanCurveStatus>, ApiError> {
    blocking(move || {
        controller
            .fan_curve_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn plan_fan_curve(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<FanCurveApiRequest>,
) -> Result<Json<FanCurvePlan>, ApiError> {
    blocking(move || {
        controller
            .plan_fan_curve(&request.request)
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn apply_fan_curve(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<FanCurveApiRequest>,
) -> Result<Json<FanCurveApplyResult>, ApiError> {
    blocking(move || {
        controller
            .apply_fan_curve(
                &request.request,
                request.plan_token.as_deref().unwrap_or_default(),
                request.confirm,
            )
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

async fn storage_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<StorageStatus>, ApiError> {
    blocking(move || {
        controller
            .storage_status()
            .map(Json)
            .map_err(ApiError::from_hardware)
    })
    .await
}

// ---------------------------------------------------------------------------
// System image install: a fixed image catalogue and an explicit live selection
// ---------------------------------------------------------------------------

/// Directory the surfaces may read install images from. A client never submits a
/// filesystem path; it submits an id, and only a plain file name inside this
/// directory is accepted.
const DEFAULT_IMAGE_DIRECTORY: &str = "/var/lib/rsetup-next/images";
const IMAGE_DIRECTORY_ENV: &str = "RSETUP_IMAGE_DIR";

/// Test-only override for the catalogue directory. `set_var` is process-wide and
/// racy; this is set once, before the first router is built.
static IMAGE_DIRECTORY_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Test-only: point the catalogue at a fixture directory. The first call wins.
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

/// Resolves an image id to a file inside the catalogue directory. Every way a
/// browser could escape the directory is rejected, and the name has to look like
/// an image before the core ever sees it.
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

/// Marker the refusing writer leaves behind. The core wraps a failed write into
/// `Io(...)` because it cannot know the writer refused on purpose, so the surface
/// recognises its own refusal and reports the policy instead of a partial write.
const LIVE_REQUIRED_MARKER: &str = "live execution is not enabled on this surface";

/// Refuses every write. The HTTP and desktop surfaces never simulate a write, so
/// a client cannot mistake a simulated image write for a real one; reads still
/// come from the real (or demo) provider, so the consequences it shows are real.
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
    /// `--live-execution`. The surfaces never keep a policy of their own.
    live: bool,
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

/// One core manager per router: an image token issued by `plan` is consumed by
/// the same manager in `apply`, exactly as the CLI and the TUI do it in one
/// session. Tokens stay opaque here — the core issues and checks them.
struct InstallSurface {
    controller: Controller,
    image_directory: PathBuf,
    mode: OnceLock<InstallMode>,
    manager: OnceLock<SystemInstallManager>,
}

impl InstallSurface {
    fn new(controller: Controller) -> Self {
        Self {
            controller,
            image_directory: image_directory(),
            mode: OnceLock::new(),
            manager: OnceLock::new(),
        }
    }

    /// The provider (demo or real) comes from the controller's own snapshot and
    /// the write policy from its execution policy, so all four surfaces decide
    /// this once, in the same place.
    fn mode(&self) -> Result<InstallMode, ApiError> {
        if let Some(mode) = self.mode.get() {
            return Ok(*mode);
        }
        let synthetic = self
            .controller
            .snapshot()
            .map_err(ApiError::internal)?
            .synthetic;
        let mode = InstallMode {
            synthetic,
            live: self.controller.policy() == ExecutionPolicy::Live,
        };
        let _ = self.mode.set(mode);
        Ok(mode)
    }

    fn live_allowed(&self) -> Result<bool, ApiError> {
        Ok(self.mode()?.live)
    }

    fn manager(&self) -> Result<&SystemInstallManager, ApiError> {
        let mode = self.mode()?;
        Ok(self.manager.get_or_init(|| install_manager(mode)))
    }

    /// Turns a client supplied image id into the absolute catalog path the core
    /// validates. The core resolves ids only when a catalogue is configured, so
    /// the surface resolves the id here and hands the core a path inside its own
    /// fixed directory.
    fn request(
        &self,
        target_id: &str,
        image_id: &str,
    ) -> Result<SystemInstallRequest, SystemInstallError> {
        let image_path = catalog_image_path(&self.image_directory, image_id)?;
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

    /// Writes the image the way the CLI does: in this process when it may, and
    /// through the fixed-verb helper when it may not.
    ///
    /// The server runs unprivileged by design, so without this the browser's
    /// apply would be the one live write in the product that never asks the
    /// administrator: opening the device would simply fail with EACCES. The
    /// helper re-plans, re-gates and re-judges the typed name, and this surface's
    /// token is the advisory preview it is meant to be.
    fn apply(
        &self,
        request: &SystemInstallRequest,
        plan_token: &str,
        acknowledgement: &str,
    ) -> Result<SystemInstallApplyResult, ApiError> {
        let mode = self.mode()?;
        if crate::install_needs_privilege(mode.live, mode.synthetic, crate::effective_uid()) {
            return crate::run_privileged_system_install(request, plan_token, acknowledgement)
                .map_err(ApiError::from_install_apply);
        }
        self.manager()?
            .apply_live(request, plan_token, acknowledgement)
            .map_err(|error| self.install_error(error))
    }

    /// Maps a core error, recognising this surface's own write refusal so a dry
    /// run reports the policy instead of "a partial image was written".
    fn install_error(&self, error: SystemInstallError) -> ApiError {
        if !self.live_allowed().unwrap_or(true) && error.to_string().contains(LIVE_REQUIRED_MARKER)
        {
            return ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "install_unsupported",
                "Live execution is not enabled on this surface; restart with --live-execution to write an image. Nothing was written.".into(),
            );
        }
        ApiError::from_install(error)
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogImage {
    id: String,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallOptions {
    /// Whether a write is permitted at all. When false the surfaces refuse the
    /// write instead of simulating one.
    live_allowed: bool,
    targets: Vec<SystemInstallTarget>,
    images: Vec<CatalogImage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallPlanRequest {
    target_id: String,
    image_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallApplyRequest {
    target_id: String,
    image_id: String,
    plan_token: String,
    /// The device name the user typed. It is passed to the core untouched: empty
    /// is `confirmation_required`, a wrong name is `acknowledgement_mismatch`,
    /// and only the exact target name reaches the writer.
    acknowledgement: String,
}

async fn install_targets(
    Extension(surface): Extension<Arc<InstallSurface>>,
) -> Result<Json<InstallOptions>, ApiError> {
    blocking(move || {
        let targets = surface
            .manager()?
            .targets()
            .map_err(ApiError::from_install)?;
        Ok(Json(InstallOptions {
            live_allowed: surface.live_allowed()?,
            targets,
            images: surface.catalog(),
        }))
    })
    .await
}

async fn plan_system_install(
    Extension(surface): Extension<Arc<InstallSurface>>,
    Json(request): Json<InstallPlanRequest>,
) -> Result<Json<SystemInstallPlan>, ApiError> {
    blocking(move || {
        let install = surface
            .request(&request.target_id, &request.image_id)
            .map_err(ApiError::from_install)?;
        surface
            .manager()?
            .plan(&install)
            .map(Json)
            .map_err(ApiError::from_install)
    })
    .await
}

async fn apply_system_install(
    Extension(surface): Extension<Arc<InstallSurface>>,
    Json(request): Json<InstallApplyRequest>,
) -> Result<Json<SystemInstallApplyResult>, ApiError> {
    blocking(move || {
        let install = surface
            .request(&request.target_id, &request.image_id)
            .map_err(ApiError::from_install)?;
        // The token is opaque: it is the core's to issue and to check. The
        // acknowledgement is judged inside the core, so this surface cannot drift
        // from the CLI or the TUI.
        surface
            .apply(&install, &request.plan_token, &request.acknowledgement)
            .map(Json)
    })
    .await
}

struct ApiError {
    status: StatusCode,
    /// Owned, because a code that crossed the helper boundary is not a literal.
    code: String,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: impl Into<String>, message: String) -> Self {
        Self {
            status,
            code: code.into(),
            message,
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            error.to_string(),
        )
    }

    fn from_source(error: rsetup_core::SourceError) -> Self {
        use rsetup_core::SourceError;
        match error {
            SourceError::UnknownProvider(_) => {
                Self::new(StatusCode::NOT_FOUND, "unknown_mirror", error.to_string())
            }
            SourceError::Unsupported(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "sources_unsupported",
                error.to_string(),
            ),
            SourceError::ConfirmationRequired => Self::new(
                StatusCode::CONFLICT,
                "confirmation_required",
                error.to_string(),
            ),
            SourceError::PlanRequired => {
                Self::new(StatusCode::CONFLICT, "plan_required", error.to_string())
            }
            SourceError::StalePlan => {
                Self::new(StatusCode::CONFLICT, "stale_plan", error.to_string())
            }
            SourceError::RootRequired => {
                Self::new(StatusCode::FORBIDDEN, "root_required", error.to_string())
            }
            SourceError::AuthorizationCanceled => Self::new(
                StatusCode::FORBIDDEN,
                "authorization_canceled",
                error.to_string(),
            ),
            SourceError::Authorization(_) => Self::new(
                StatusCode::FORBIDDEN,
                "authorization_failed",
                error.to_string(),
            ),
            SourceError::Io(_) => Self::internal(error),
        }
    }

    /// The install codes come from `crate::system_install_error_code`, the same
    /// table the CLI prints and the TUI shows, so a client reads one vocabulary
    /// whichever door it came through.
    fn from_install(error: SystemInstallError) -> Self {
        let status = match &error {
            SystemInstallError::Unsupported(_) => StatusCode::UNPROCESSABLE_ENTITY,
            SystemInstallError::InvalidInput(_) => StatusCode::BAD_REQUEST,
            SystemInstallError::RootRequired => StatusCode::FORBIDDEN,
            SystemInstallError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            SystemInstallError::ConfirmationRequired
            | SystemInstallError::AcknowledgementMismatch { .. }
            | SystemInstallError::PlanRequired
            | SystemInstallError::StalePlan
            | SystemInstallError::Conflict(_)
            | SystemInstallError::RunningSystem(_) => StatusCode::CONFLICT,
        };
        Self::new(
            status,
            crate::system_install_error_code(&error),
            error.to_string(),
        )
    }

    /// A delegated install can also end in a dismissed administrator prompt.
    /// That keeps its own code so a page can tell a cancel from a failure, the
    /// way every other privileged operation already does.
    fn from_install_apply(error: crate::InstallApplyError) -> Self {
        match error {
            crate::InstallApplyError::Core(error) => Self::from_install(error),
            crate::InstallApplyError::AuthorizationCanceled => Self::new(
                StatusCode::FORBIDDEN,
                "authorization_canceled",
                // The envelope carries the code; a page replaces this with its own
                // translation, so the wording here is the fallback for a reader
                // without a catalogue.
                "Administrator authorization canceled.".to_owned(),
            ),
            // The helper classified it with the same vocabulary, so the status is
            // recovered from the code instead of being guessed from the prose.
            crate::InstallApplyError::Helper { code, message } => {
                Self::new(Self::install_status_for_code(&code), code, message)
            }
        }
    }

    /// The HTTP status for a code the helper reported. The helper runs the same
    /// core, so these are the statuses `from_install` would have chosen had the
    /// failure happened in this process.
    fn install_status_for_code(code: &str) -> StatusCode {
        match code {
            "install_unsupported" => StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_install_request" => StatusCode::BAD_REQUEST,
            "root_required" | "authorization_canceled" => StatusCode::FORBIDDEN,
            "confirmation_required"
            | "acknowledgement_mismatch"
            | "plan_required"
            | "stale_plan"
            | "install_conflict"
            | "writes_running_system" => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn from_hardware(error: rsetup_core::HardwareError) -> Self {
        use rsetup_core::HardwareError;
        match error {
            HardwareError::Unsupported(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "hardware_unsupported",
                error.to_string(),
            ),
            HardwareError::InvalidInput(_) => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_hardware_selection",
                error.to_string(),
            ),
            HardwareError::Conflict(_) => {
                Self::new(StatusCode::CONFLICT, "hardware_conflict", error.to_string())
            }
            HardwareError::ConfirmationRequired => Self::new(
                StatusCode::CONFLICT,
                "confirmation_required",
                error.to_string(),
            ),
            HardwareError::PlanRequired => {
                Self::new(StatusCode::CONFLICT, "plan_required", error.to_string())
            }
            HardwareError::StalePlan => {
                Self::new(StatusCode::CONFLICT, "stale_plan", error.to_string())
            }
            HardwareError::RootRequired => {
                Self::new(StatusCode::FORBIDDEN, "root_required", error.to_string())
            }
            HardwareError::AuthorizationCanceled => Self::new(
                StatusCode::FORBIDDEN,
                "authorization_canceled",
                error.to_string(),
            ),
            HardwareError::Authorization(_) => Self::new(
                StatusCode::FORBIDDEN,
                "authorization_failed",
                error.to_string(),
            ),
            HardwareError::Io(_) => Self::internal(error),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({"error": {"code": self.code, "message": self.message}})),
        )
            .into_response()
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsetup_core::{ExecutionPolicy, ProbeMode};

    #[test]
    fn router_builds_with_demo_controller() {
        let _router = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
    }

    #[test]
    fn community_qr_assets_are_embedded() {
        assert!(COMMUNITY_QQ.starts_with(b"RIFF"));
        assert!(COMMUNITY_WECHAT.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    /// The install routes share one process-wide back-pressure limiter (8
    /// concurrent operations) and one fixture directory, so the install tests take
    /// turns instead of racing each other for both. An async mutex because the
    /// tests hold it across awaits.
    static INSTALL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Fixture catalogue directory under `target/`, shared by every install test
    /// in this process. Only plain files with an image name are listed, and the
    /// core never sees a client supplied path.
    fn install_fixture_directory() -> PathBuf {
        static DIRECTORY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        DIRECTORY
            .get_or_init(|| {
                let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/http-install-tests");
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
                // A second image, identical in shape but different in content, so a
                // plan for one and an apply for the other is a real image change
                // without mutating a fixture another test may be reading.
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

    fn install_call(
        method: &str,
        uri: &str,
        body: serde_json::Value,
        request_header: bool,
    ) -> Request<axum::body::Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8788")
            .header("content-type", "application/json");
        if request_header {
            builder = builder.header("x-rsetup-request", "1");
        }
        builder
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    async fn install_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn install_plan_token(
        app: &Router,
        target_id: &str,
        image_id: &str,
    ) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/plan",
                serde_json::json!({"targetId": target_id, "imageId": image_id}),
                true,
            ))
            .await
            .unwrap();
        let status = response.status();
        (status, install_json(response).await)
    }

    #[tokio::test]
    async fn install_targets_reports_the_policy_and_the_fixed_catalogue() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        let response = app
            .oneshot(install_call(
                "GET",
                "/api/v1/storage/install/targets",
                serde_json::json!({}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = install_json(response).await;

        // DryRun is the default: the surface says so before a client can try to
        // write, so there is no silently live path.
        assert_eq!(body["liveAllowed"], serde_json::json!(false));
        let targets = body["targets"].as_array().expect("targets array");
        assert!(targets.iter().any(|target| target["id"] == "sda"), "{body}");
        assert!(
            targets.iter().any(|target| target["id"] == "mmcblk1"
                && target["holdsRunningSystem"] == serde_json::json!(true)),
            "{body}"
        );
        // Only catalogue images are offered: the non-image file is not listed.
        let ids = body["images"]
            .as_array()
            .expect("images array")
            .iter()
            .map(|image| image["id"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["demo.img".to_string(), "other.img".to_string()],
            "the catalogue listing is exactly what apply accepts: {body}"
        );
        assert!(directory.join("demo.img").is_file());
    }

    #[tokio::test]
    async fn install_plan_uses_only_the_fixed_catalogue() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        let directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));

        let (status, plan) = install_plan_token(&app, "sda", "demo.img").await;
        assert_eq!(status, StatusCode::OK, "{plan}");
        let expected = std::fs::canonicalize(directory.join("demo.img")).unwrap();
        assert_eq!(
            plan["image"]["path"].as_str(),
            Some(expected.display().to_string().as_str()),
            "the plan points at the catalogue file, {plan}"
        );
        assert_eq!(plan["target"]["path"], serde_json::json!("/dev/sda"));
        assert_eq!(plan["destroys"].as_array().unwrap().len(), 3);
        assert!(
            plan["image"]["sha256Prefix"]
                .as_str()
                .is_some_and(|value| value.len() == 64),
            "{plan}"
        );
        let warnings = plan["warnings"].as_array().unwrap();
        assert!(
            warnings.contains(&serde_json::json!(
                "writing_an_image_replaces_every_partition_on_the_target"
            )),
            "{plan}"
        );
        assert!(
            warnings.contains(&serde_json::json!("the_write_cannot_be_rolled_back")),
            "{plan}"
        );

        // A client cannot smuggle a path in: an unknown field is ignored, and the
        // plan still points inside the catalogue directory.
        use tower::ServiceExt;
        let response = app
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/plan",
                serde_json::json!({
                    "targetId": "sda",
                    "imageId": "demo.img",
                    "imagePath": "/etc/passwd"
                }),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let plan = install_json(response).await;
        assert_eq!(
            plan["image"]["path"].as_str(),
            Some(expected.display().to_string().as_str()),
            "{plan}"
        );
    }

    #[tokio::test]
    async fn install_plan_rejects_ids_that_escape_the_catalogue() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let _directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        for image_id in [
            "",
            ".",
            "..",
            "../demo.img",
            "../../etc/passwd",
            "sub/demo.img",
            "demo.img/..",
            "notes.txt",
            "/etc/passwd",
            // The desktop shell issues opaque handles for images the user chose in
            // its own native dialog. The browser surface has no notion of them: a
            // handle is just another name that is not a file in the fixed
            // catalogue, so it is rejected here exactly like a path. The two
            // surfaces stay independent — nothing about the desktop picker widens
            // what a browser may ask for.
            "pick-000102030405060708090a0b0c0d0e0f-demo.img",
            "pick-00000000000000000000000000000000",
        ] {
            let response = app
                .clone()
                .oneshot(install_call(
                    "POST",
                    "/api/v1/storage/install/plan",
                    serde_json::json!({"targetId": "sda", "imageId": image_id}),
                    true,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "image id {image_id:?} must be rejected"
            );
            let body = install_json(response).await;
            assert_eq!(
                body["error"]["code"],
                serde_json::json!("invalid_install_request"),
                "image id {image_id:?}: {body}"
            );
        }
        // A catalogue name that simply does not exist is rejected as well.
        let response = app
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/plan",
                serde_json::json!({"targetId": "sda", "imageId": "absent.img"}),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn install_routes_keep_the_local_boundary_and_stay_mutations() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let _directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));

        // The destructive route is a POST behind the non-simple request header,
        // exactly like every other mutation.
        for uri in [
            "/api/v1/storage/install/plan",
            "/api/v1/storage/install/apply",
        ] {
            let response = app
                .clone()
                .oneshot(install_call(
                    "POST",
                    uri,
                    serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
                    false,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
            let body = install_json(response).await;
            assert_eq!(
                body["error"]["code"],
                serde_json::json!("request_forbidden")
            );
        }

        // It is not reachable through a read-only method either.
        let response = app
            .clone()
            .oneshot(install_call(
                "GET",
                "/api/v1/storage/install/apply",
                serde_json::json!({}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);

        // The read-only targets route answers without the header, but it only
        // ever reports.
        let response = app
            .oneshot(install_call(
                "GET",
                "/api/v1/storage/install/targets",
                serde_json::json!({}),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn install_apply_checks_the_acknowledgement_in_the_core() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let _directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        let (status, plan) = install_plan_token(&app, "sda", "demo.img").await;
        assert_eq!(status, StatusCode::OK, "{plan}");
        let token = plan["planToken"].as_str().expect("plan token").to_string();

        for (acknowledgement, expected_code) in [
            ("", "confirmation_required"),
            ("   ", "confirmation_required"),
            ("true", "acknowledgement_mismatch"),
            ("yes", "acknowledgement_mismatch"),
            ("sda1", "acknowledgement_mismatch"),
            ("/dev/sda", "acknowledgement_mismatch"),
            ("SDA", "acknowledgement_mismatch"),
            ("mmcblk0", "acknowledgement_mismatch"),
        ] {
            let response = app
                .clone()
                .oneshot(install_call(
                    "POST",
                    "/api/v1/storage/install/apply",
                    serde_json::json!({
                        "targetId": "sda",
                        "imageId": "demo.img",
                        "planToken": token,
                        "acknowledgement": acknowledgement
                    }),
                    true,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::CONFLICT,
                "{acknowledgement:?}"
            );
            let body = install_json(response).await;
            assert_eq!(
                body["error"]["code"],
                serde_json::json!(expected_code),
                "{acknowledgement:?}: {body}"
            );
            if expected_code == "acknowledgement_mismatch" {
                let message = body["error"]["message"].as_str().unwrap_or_default();
                assert!(message.contains("sda"), "{acknowledgement:?}: {body}");
            }
        }
    }

    /// The gap this closes: everything an apply needs is in the plan response, so
    /// a client never has to remember the id it picked.
    #[tokio::test]
    async fn an_apply_payload_built_from_the_plan_response_is_accepted() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::Live));

        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/plan",
                serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let plan = install_json(response).await;
        // The plan echoes the identity apply accepts, and the path it resolved.
        assert_eq!(plan["image"]["id"], serde_json::json!("demo.img"), "{plan}");
        assert_eq!(
            plan["image"]["path"].as_str(),
            Some(
                std::fs::canonicalize(directory.join("demo.img"))
                    .unwrap()
                    .display()
                    .to_string()
                    .as_str()
            ),
            "{plan}"
        );

        // Build the apply payload from the plan response alone: no remembered id,
        // no remembered path, no client side bookkeeping.
        let payload = serde_json::json!({
            "targetId": plan["target"]["id"].clone(),
            "imageId": plan["image"]["id"].clone(),
            "planToken": plan["planToken"].clone(),
            "acknowledgement": plan["target"]["id"].clone(),
        });
        assert_eq!(payload["imageId"], serde_json::json!("demo.img"));
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/apply",
                payload,
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = install_json(response).await;
        assert_eq!(
            result["bytesWritten"],
            serde_json::json!(2 * 1024 * 1024),
            "{result}"
        );
        assert_eq!(result["plan"]["image"]["id"], serde_json::json!("demo.img"));
    }

    /// The same end to end flow over a real socket: a loopback server is bound and
    /// spoken to with a raw HTTP/1.1 request, so the middleware, the handlers and
    /// the serialization are all the production ones. The client is blocking
    /// (`std::net`), so the runtime keeps serving on its own worker threads.
    #[test]
    fn a_real_server_accepts_an_apply_payload_built_from_its_plan() {
        use std::io::{Read, Write};
        use std::net::TcpStream;

        fn post(
            address: std::net::SocketAddr,
            path: &str,
            body: &serde_json::Value,
        ) -> (u16, serde_json::Value) {
            let mut stream = TcpStream::connect(address).expect("connect to the server");
            let payload = body.to_string();
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nx-rsetup-request: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                address.port(),
                payload.len()
            );
            stream
                .write_all(request.as_bytes())
                .expect("write the request");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("read the response");
            let text = String::from_utf8(response).expect("utf-8 response");
            let (head, body) = text.split_once("\r\n\r\n").expect("response headers");
            let status = head
                .split_whitespace()
                .nth(1)
                .expect("status code")
                .parse()
                .expect("numeric status");
            let value = serde_json::from_str(body)
                .unwrap_or_else(|error| panic!("the response body must be JSON ({error}): {body}"));
            (status, value)
        }

        let _directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::Live));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build the test runtime");
        // The same serialising lock the async install tests take.
        let _guard = runtime.block_on(INSTALL_TEST_LOCK.lock());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let address = listener.local_addr().expect("bound address");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        // `from_std` registers with the reactor, so it has to run inside the
        // runtime context.
        let listener = runtime.block_on(async {
            tokio::net::TcpListener::from_std(listener).expect("tokio listener")
        });
        let server = runtime.spawn(async move { axum::serve(listener, app).await });

        let (status, plan) = post(
            address,
            "/api/v1/storage/install/plan",
            &serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        );
        assert_eq!(status, 200, "{plan}");
        println!(
            "PLAN RESPONSE FRAGMENT: {}",
            serde_json::json!({
                "target": {"id": plan["target"]["id"], "path": plan["target"]["path"]},
                "image": {"id": plan["image"]["id"], "path": plan["image"]["path"],
                          "sha256Prefix": plan["image"]["sha256Prefix"]},
                "planToken": plan["planToken"],
            })
        );

        // Everything below comes from the response above: no remembered id, no
        // remembered path.
        let (status, result) = post(
            address,
            "/api/v1/storage/install/apply",
            &serde_json::json!({
                "targetId": plan["target"]["id"].clone(),
                "imageId": plan["image"]["id"].clone(),
                "planToken": plan["planToken"].clone(),
                "acknowledgement": plan["target"]["id"].clone(),
            }),
        );
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["bytesWritten"], serde_json::json!(2 * 1024 * 1024));
        assert_eq!(result["plan"]["image"]["id"], serde_json::json!("demo.img"));

        // A plan whose image is then changed is refused.
        let (status, second_plan) = post(
            address,
            "/api/v1/storage/install/plan",
            &serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
        );
        assert_eq!(status, 200, "{second_plan}");
        let (status, body) = post(
            address,
            "/api/v1/storage/install/apply",
            &serde_json::json!({
                "targetId": second_plan["target"]["id"].clone(),
                "imageId": "other.img",
                "planToken": second_plan["planToken"].clone(),
                "acknowledgement": second_plan["target"]["id"].clone(),
            }),
        );
        assert_eq!(status, 409, "{body}");
        assert_eq!(body["error"]["code"], serde_json::json!("stale_plan"));

        server.abort();
    }

    /// The contrast case: preview one image, then apply for a different one. The
    /// token binds the image identity, so the write is refused instead of landing
    /// the wrong image.
    #[tokio::test]
    async fn a_plan_whose_image_changed_is_refused_on_apply() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::Live));
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/plan",
                serde_json::json!({"targetId": "sda", "imageId": "demo.img"}),
                true,
            ))
            .await
            .unwrap();
        let plan = install_json(response).await;
        assert_eq!(plan["image"]["id"], serde_json::json!("demo.img"));

        // A different catalogue image: same field shapes, different content.
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/apply",
                serde_json::json!({
                    "targetId": plan["target"]["id"].clone(),
                    "imageId": "other.img",
                    "planToken": plan["planToken"].clone(),
                    "acknowledgement": plan["target"]["id"].clone(),
                }),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = install_json(response).await;
        assert_eq!(
            body["error"]["code"],
            serde_json::json!("stale_plan"),
            "{body}"
        );

        // The same plan still applies the image it was made for, so the refusal
        // above is about the changed image and not a burned token.
        let response = app
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/apply",
                serde_json::json!({
                    "targetId": plan["target"]["id"].clone(),
                    "imageId": plan["image"]["id"].clone(),
                    "planToken": plan["planToken"].clone(),
                    "acknowledgement": plan["target"]["id"].clone(),
                }),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(directory.join("other.img").is_file());
    }

    #[tokio::test]
    async fn install_apply_refuses_the_write_without_live_execution() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let _directory = install_fixture_directory();
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        let (_, plan) = install_plan_token(&app, "sda", "demo.img").await;
        let token = plan["planToken"].as_str().expect("plan token").to_string();

        // A correct typed name still cannot write: this surface refuses instead of
        // simulating, so a client can never mistake the answer for a real write.
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/apply",
                serde_json::json!({
                    "targetId": "sda",
                    "imageId": "demo.img",
                    "planToken": token,
                    "acknowledgement": "sda"
                }),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = install_json(response).await;
        assert_eq!(
            body["error"]["code"],
            serde_json::json!("install_unsupported")
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.to_lowercase().contains("live execution")),
            "{body}"
        );
    }

    #[tokio::test]
    async fn install_apply_writes_when_the_policy_is_live() {
        let _guard = INSTALL_TEST_LOCK.lock().await;
        use tower::ServiceExt;

        let _directory = install_fixture_directory();
        // Demo provider plus the live policy: the write is real code but lands in
        // the demo provider, never on a device.
        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::Live));
        let response = app
            .clone()
            .oneshot(install_call(
                "GET",
                "/api/v1/storage/install/targets",
                serde_json::json!({}),
                false,
            ))
            .await
            .unwrap();
        let options = install_json(response).await;
        assert_eq!(options["liveAllowed"], serde_json::json!(true));

        let (status, plan) = install_plan_token(&app, "sda", "demo.img").await;
        assert_eq!(status, StatusCode::OK, "{plan}");
        let token = plan["planToken"].as_str().expect("plan token").to_string();
        let response = app
            .clone()
            .oneshot(install_call(
                "POST",
                "/api/v1/storage/install/apply",
                serde_json::json!({
                    "targetId": "sda",
                    "imageId": "demo.img",
                    "planToken": token,
                    "acknowledgement": "sda"
                }),
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result = install_json(response).await;
        assert_eq!(
            result["bytesWritten"],
            serde_json::json!(2 * 1024 * 1024),
            "{result}"
        );
        assert_eq!(result["run"]["synthetic"], serde_json::json!(true));

        // The core's running-system gate holds over HTTP too.
        let (_, plan) = install_plan_token(&app, "mmcblk1", "demo.img").await;
        assert_eq!(
            plan["error"]["code"],
            serde_json::json!("writes_running_system"),
            "{plan}"
        );
    }

    /// The reported defect, at this surface: a live write the server may not
    /// perform itself has to be delegated to the helper.
    ///
    /// The mode is set directly, so the assertion needs no root, no `pkexec`, no
    /// device and no environment: delegation is observable because the helper's
    /// own precondition fails first on a host that does not ship it. A board that
    /// does ship it, or a test run as root, would take the other branch, so those
    /// hosts skip.
    #[test]
    fn install_apply_delegates_when_the_process_may_not_write() {
        if std::path::Path::new(crate::PRIVILEGED_HELPER).is_file()
            || crate::effective_uid() == Some(0)
        {
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
            "the delegated route was not taken: {} {}",
            error.code, error.message
        );
    }

    /// A dismissed administrator prompt keeps its own code and status instead of
    /// being reported as a failed write.
    #[test]
    fn install_apply_cancel_keeps_its_own_code() {
        let canceled =
            ApiError::from_install_apply(crate::InstallApplyError::AuthorizationCanceled);
        assert_eq!(canceled.status, StatusCode::FORBIDDEN);
        assert_eq!(canceled.code, "authorization_canceled");

        // A core error must still travel as itself through the same mapping.
        let stale = ApiError::from_install_apply(crate::InstallApplyError::Core(
            SystemInstallError::StalePlan,
        ));
        assert_eq!(stale.status, StatusCode::CONFLICT);
        assert_eq!(stale.code, "stale_plan");
    }

    /// Before the helper's error envelope, every delegated refusal arrived here as
    /// a generic `internal_error`, so a stale plan read as a server fault and the
    /// page offered no way to recover.
    #[test]
    fn delegated_refusals_keep_their_code_and_status() {
        for (code, status) in [
            ("stale_plan", StatusCode::CONFLICT),
            ("confirmation_required", StatusCode::CONFLICT),
            ("acknowledgement_mismatch", StatusCode::CONFLICT),
            ("plan_required", StatusCode::CONFLICT),
            ("install_conflict", StatusCode::CONFLICT),
            ("writes_running_system", StatusCode::CONFLICT),
            ("invalid_install_request", StatusCode::BAD_REQUEST),
            ("install_unsupported", StatusCode::UNPROCESSABLE_ENTITY),
            ("root_required", StatusCode::FORBIDDEN),
        ] {
            let mapped = ApiError::from_install_apply(crate::InstallApplyError::Helper {
                code: code.to_owned(),
                message: "the helper's own explanation".to_owned(),
            });
            assert_eq!(mapped.status, status, "{code}");
            assert_eq!(mapped.code, code, "{code}");
            assert_eq!(mapped.message, "the helper's own explanation", "{code}");
        }

        // An unknown code from a newer helper stays an internal error rather than
        // being invented into the wrong class.
        let unknown = ApiError::from_install_apply(crate::InstallApplyError::Helper {
            code: "something_new".to_owned(),
            message: "?".to_owned(),
        });
        assert_eq!(unknown.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(unknown.code, "something_new");
    }

    #[tokio::test]
    async fn get_storage_status_returns_ok_with_demo_devices() {
        use axum::body::Body;
        use rsetup_core::StorageStatus;
        use tower::ServiceExt;

        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        let request = Request::builder()
            .uri("/api/v1/hardware/storage")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: StorageStatus = serde_json::from_slice(&bytes).unwrap();
        // NVMe side
        assert!(status.nvme.initialized);
        assert_eq!(status.nvme.devices.len(), 1);
        assert_eq!(status.nvme.devices[0].model, "Radxa M.2 NVMe SSD 512GB");
        // MMC side
        assert!(status.mmc.initialized);
        assert_eq!(status.mmc.devices.len(), 2);
        let emmc = &status.mmc.devices[0];
        assert_eq!(emmc.name, "mmc0:0001");
        assert_eq!(emmc.block_path, "/dev/mmcblk0");
        assert_eq!(emmc.card_type, "MMC");
        assert_eq!(emmc.model, "FE4MB4");
        assert_eq!(emmc.total_bytes, 62_537_072_640);
        assert_eq!(emmc.health.pre_eol_info, 1);
        assert_eq!(emmc.health.life_time_est_a_percent, Some(10));
        assert_eq!(emmc.health.life_time_est_b_percent, Some(10));
        assert!(emmc.health.warning_flags.is_empty());
        let sd = &status.mmc.devices[1];
        assert_eq!(sd.name, "mmc1:59b4");
        assert_eq!(sd.card_type, "SD");
        assert_eq!(sd.health.pre_eol_info, 0);
        assert_eq!(sd.health.life_time_est_a_percent, None);
        assert_eq!(sd.health.life_time_est_b_percent, None);
    }

    #[tokio::test]
    async fn legacy_nvme_route_is_removed() {
        use axum::body::Body;
        use tower::ServiceExt;

        let app = router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun));
        let request = Request::builder()
            .uri("/api/v1/hardware/nvme")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn storage_route_same_router_injected_storage_freshness() {
        use axum::body::Body;
        use rsetup_core::{
            ExecutionPolicy, HardwareError, HealthState, MmcDevice, MmcHealth, MmcStatus,
            NvmeDevice, NvmeSmartLog, NvmeStatus, ProbeMode, StorageReader, StorageStatus,
            TelemetryReadState, TelemetryStatus,
        };
        use std::sync::{Arc, Mutex};
        use tower::ServiceExt;

        #[derive(Clone)]
        struct FakeStorage(Arc<Mutex<Result<StorageStatus, HardwareError>>>);

        impl StorageReader for FakeStorage {
            fn nvme_status(&self) -> Result<NvmeStatus, HardwareError> {
                self.0
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.nvme.clone())
                    .map_err(|e| match e {
                        HardwareError::Io(msg) => HardwareError::Io(msg.clone()),
                        _ => HardwareError::Io("error".into()),
                    })
            }
            fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
                self.0
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map(|s| s.mmc.clone())
                    .map_err(|e| match e {
                        HardwareError::Io(msg) => HardwareError::Io(msg.clone()),
                        _ => HardwareError::Io("error".into()),
                    })
            }
        }

        let initial_storage = StorageStatus {
            nvme: NvmeStatus {
                initialized: true,
                devices: vec![NvmeDevice {
                    path: "/dev/nvme0".into(),
                    name: "nvme0".into(),
                    model: "SSD Initial".into(),
                    serial: "S1".into(),
                    firmware: "1".into(),
                    total_bytes: 1000,
                    smart: Some(NvmeSmartLog {
                        critical_warning: 0,
                        temperature_c: 35.0,
                        available_spare_percent: 100,
                        spare_threshold_percent: 10,
                        percentage_used: 1,
                        data_read_bytes: 100,
                        data_written_bytes: 100,
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
                    model: "EMMC".into(),
                    manufacturer: "Vendor".into(),
                    serial: "M1".into(),
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

        let shared = Arc::new(Mutex::new(Ok(initial_storage)));
        let fake = FakeStorage(Arc::clone(&shared));

        let controller = Controller::with_storage_reader(
            ProbeMode::Live,
            ExecutionPolicy::DryRun,
            Arc::new(fake),
        );
        let app = router(controller);

        // First request on router
        let req1 = Request::builder()
            .uri("/api/v1/hardware/storage")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();
        let res1 = app.clone().oneshot(req1).await.unwrap();
        assert_eq!(res1.status(), StatusCode::OK);
        let body1 = axum::body::to_bytes(res1.into_body(), usize::MAX)
            .await
            .unwrap();
        let val1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
        assert_eq!(val1["nvme"]["devices"][0]["smart"]["temperatureC"], 35.0);
        assert_eq!(val1["mmc"]["devices"].as_array().unwrap().len(), 1);

        // Mutate shared fake storage: set smart to null, telemetry to unavailable, and healthState unknown
        {
            let mut guard = shared.lock().unwrap();
            let state = guard.as_mut().unwrap();
            state.nvme.devices[0].smart = None;
            state.nvme.devices[0].telemetry = TelemetryStatus {
                state: TelemetryReadState::Unavailable,
                error: Some(rsetup_core::TelemetryError {
                    kind: rsetup_core::TelemetryErrorKind::Io,
                    code: Some(libc::EIO),
                }),
            };
            state.nvme.devices[0].health_state = HealthState::Unknown;
        }

        // Second request on SAME router clone
        let req2 = Request::builder()
            .uri("/api/v1/hardware/storage")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();
        let res2 = app.clone().oneshot(req2).await.unwrap();
        assert_eq!(res2.status(), StatusCode::OK);
        let body2 = axum::body::to_bytes(res2.into_body(), usize::MAX)
            .await
            .unwrap();
        let val2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert!(val2["nvme"]["devices"][0]["smart"].is_null());
        assert_eq!(
            val2["nvme"]["devices"][0]["telemetry"]["state"],
            "unavailable"
        );
        assert_eq!(val2["nvme"]["devices"][0]["healthState"], "unknown");

        // Test whole reader failure returns ApiError (e.g. 500) rather than 200 with empty list
        {
            let mut guard = shared.lock().unwrap();
            *guard = Err(HardwareError::Io("Disk subsystem failed".into()));
        }

        let req3 = Request::builder()
            .uri("/api/v1/hardware/storage")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();
        let res3 = app.clone().oneshot(req3).await.unwrap();
        assert_ne!(res3.status(), StatusCode::OK);
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use axum::body::Body;
    use rsetup_core::{ExecutionPolicy, ProbeMode};
    use tower::ServiceExt;

    fn app() -> Router {
        router(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun))
    }

    #[tokio::test]
    async fn http_rejects_rebinding_and_cross_origin_and_sets_headers() {
        for host in [
            "evil.example:8788",
            "192.168.2.186:8788",
            "evil@localhost:8788",
        ] {
            let request = Request::builder()
                .uri("/api/v1/health")
                .header("host", host)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        for origin in ["https://evil.example", "null", "http://localhost:9999"] {
            let request = Request::builder()
                .uri("/api/v1/health")
                .header("host", "localhost:8788")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app().oneshot(request).await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        let request = Request::builder()
            .uri("/app.js")
            .header("host", "127.0.0.1:8788")
            .body(Body::empty())
            .unwrap();
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("style-src 'self'")
        );
    }

    #[tokio::test]
    async fn mutation_requires_non_simple_request_header() {
        for allowed in [false, true] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/sources/plan")
                .header("host", "localhost:8788")
                .header("origin", "http://localhost:8788")
                .header("content-type", "application/json");
            if allowed {
                request = request.header("x-rsetup-request", "1");
            }
            let response = app()
                .oneshot(request.body(Body::from(r#"{"providerId":"cqu"}"#)).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if allowed {
                    StatusCode::OK
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
    }

    #[tokio::test]
    async fn rejects_non_loopback_listener_before_binding() {
        assert!(
            serve(
                Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun),
                "0.0.0.0:0".parse().unwrap()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronous_jobs_do_not_starve_health() {
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let job = tokio::spawn(blocking(move || {
            let _ = started.send(());
            release_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            Ok(())
        }));
        started_rx.await.unwrap();
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .header("host", "[::1]:8788")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        release.send(()).unwrap();
        assert!(job.await.unwrap().is_ok());
    }
}
