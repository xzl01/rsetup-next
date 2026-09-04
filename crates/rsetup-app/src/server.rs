use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use rsetup_core::{
    ActionRun, ActionSpec, ActivityEvent, Controller, DeviceSnapshot, FanCurveApplyResult,
    FanCurvePlan, FanCurveRequest, FanCurveStatus, GpioStatus, LedStatus, OverlayApplyResult,
    OverlayPlan, OverlayStatus, RgbLedConfig, SourceApplyResult, SourcePlan, SourceStatus,
    SpiFlashApplyResult, SpiFlashPlan, SpiFlashRequest, SpiFlashStatus, ThermalStatus, VideoFrame,
    VideoStatus,
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
        .route("/api/v1/sources/plan", post(plan_sources))
        .route("/api/v1/sources/apply", post(apply_sources))
        .route("/api/v1/hardware/overlays", get(overlay_status))
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
        .route("/api/v1/activity", get(activity))
        .layer(TraceLayer::new_for_http())
        .with_state(Arc::new(controller))
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
    controller.snapshot().map(Json).map_err(ApiError::internal)
}

async fn actions(State(controller): State<Arc<Controller>>) -> Json<Vec<ActionSpec>> {
    Json(controller.actions())
}

async fn activity(State(controller): State<Arc<Controller>>) -> Json<Vec<ActivityEvent>> {
    Json(controller.activity())
}

async fn run_action(
    State(controller): State<Arc<Controller>>,
    Path(id): Path<String>,
    Json(request): Json<RunRequest>,
) -> Result<Json<ActionRun>, ApiError> {
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
}

async fn source_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<SourceStatus>, ApiError> {
    controller
        .source_status()
        .map(Json)
        .map_err(ApiError::from_source)
}

async fn plan_sources(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<SourcePlan>, ApiError> {
    controller
        .plan_source_change(&request.provider_id)
        .map(Json)
        .map_err(ApiError::from_source)
}

async fn apply_sources(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SourceRequest>,
) -> Result<Json<SourceApplyResult>, ApiError> {
    controller
        .apply_source_change(
            &request.provider_id,
            request.plan_token.as_deref().unwrap_or_default(),
            request.confirm,
        )
        .map(Json)
        .map_err(ApiError::from_source)
}

async fn overlay_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<OverlayStatus>, ApiError> {
    controller
        .overlay_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn plan_overlays(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<OverlayRequest>,
) -> Result<Json<OverlayPlan>, ApiError> {
    controller
        .plan_overlay_change(&request.selected_ids)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_overlays(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<OverlayRequest>,
) -> Result<Json<OverlayApplyResult>, ApiError> {
    controller
        .apply_overlay_change(
            &request.selected_ids,
            request.plan_token.as_deref().unwrap_or_default(),
            request.confirm,
        )
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn gpio_status(
    State(controller): State<Arc<Controller>>,
    Query(query): Query<GpioQuery>,
) -> Result<Json<GpioStatus>, ApiError> {
    controller
        .gpio_status_for_profile(query.profile.as_deref())
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn spi_flash_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<SpiFlashStatus>, ApiError> {
    controller
        .spi_flash_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn plan_spi_flash(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SpiFlashApiRequest>,
) -> Result<Json<SpiFlashPlan>, ApiError> {
    controller
        .plan_spi_flash(&request.request)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_spi_flash(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<SpiFlashApiRequest>,
) -> Result<Json<SpiFlashApplyResult>, ApiError> {
    controller
        .apply_spi_flash(
            &request.request,
            request.plan_token.as_deref().unwrap_or_default(),
            request.confirm,
        )
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn led_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<LedStatus>, ApiError> {
    controller
        .led_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_led_trigger(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<LedTriggerRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    controller
        .apply_led_trigger(&request.led_id, &request.trigger, request.confirm)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_rgb_led(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<RgbLedRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    controller
        .apply_rgb_led(&request.config, request.confirm)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn video_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<VideoStatus>, ApiError> {
    controller
        .video_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn capture_video(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<VideoCaptureRequest>,
) -> Result<Json<VideoFrame>, ApiError> {
    controller
        .capture_video_frame(&request.device_id)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn thermal_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<ThermalStatus>, ApiError> {
    controller
        .thermal_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_thermal_policy(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<ThermalPolicyRequest>,
) -> Result<Json<ActionRun>, ApiError> {
    controller
        .apply_thermal_policy(&request.policy, request.confirm)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn fan_curve_status(
    State(controller): State<Arc<Controller>>,
) -> Result<Json<FanCurveStatus>, ApiError> {
    controller
        .fan_curve_status()
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn plan_fan_curve(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<FanCurveApiRequest>,
) -> Result<Json<FanCurvePlan>, ApiError> {
    controller
        .plan_fan_curve(&request.request)
        .map(Json)
        .map_err(ApiError::from_hardware)
}

async fn apply_fan_curve(
    State(controller): State<Arc<Controller>>,
    Json(request): Json<FanCurveApiRequest>,
) -> Result<Json<FanCurveApplyResult>, ApiError> {
    controller
        .apply_fan_curve(
            &request.request,
            request.plan_token.as_deref().unwrap_or_default(),
            request.confirm,
        )
        .map(Json)
        .map_err(ApiError::from_hardware)
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
    let _ = tokio::signal::ctrl_c().await;
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
}
