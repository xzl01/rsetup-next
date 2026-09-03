use rsetup_core::{
    ActionError, ActionRun, ActionSpec, ActivityEvent, Controller, DeviceSnapshot,
    GpioStatus, HardwareError, OverlayApplyResult, OverlayPlan, OverlayStatus, SourceApplyResult,
    SourceError, SourcePlan, SourceStatus, ThermalStatus, VideoFrame, VideoStatus,
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
fn gpio_status(controller: tauri::State<'_, Controller>) -> Result<GpioStatus, CommandError> {
    controller.gpio_status().map_err(CommandError::from)
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

fn main() {
    tauri::Builder::default()
        .manage(Controller::from_environment())
        .invoke_handler(tauri::generate_handler![
            system_snapshot,
            list_actions,
            list_activity,
            run_action,
            source_status,
            plan_sources,
            apply_sources,
            overlay_status,
            plan_overlays,
            apply_overlays,
            gpio_status,
            video_status,
            capture_video_frame,
            thermal_status,
            apply_thermal_policy
        ])
        .run(tauri::generate_context!())
        .expect("failed to run rsetup desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

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
