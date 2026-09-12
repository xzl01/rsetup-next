use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rsetup_core::{
    ActionRun, ActionSpec, ActivityEvent, Controller, DeviceSnapshot, FanCurveApplyResult,
    FanCurvePlan, FanCurveRequest, FanCurveStatus, GpioStatus, LedStatus, OverlayApplyResult,
    OverlayPlan, OverlayStatus, RgbLedConfig, SourceApplyResult, SourcePlan, SourceStatus,
    SpiFlashApplyResult, SpiFlashPlan, SpiFlashRequest, SpiFlashStatus, StorageStatus,
    ThermalStatus, VideoFrame, VideoStatus,
};
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc};
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
        .route("/api/v1/activity", get(activity))
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

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: String) -> Self {
        Self {
            status,
            code,
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
