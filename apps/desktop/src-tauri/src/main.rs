use rsetup_core::{
    ActionError, ActionRun, ActionSpec, ActivityEvent, Controller, DeviceSnapshot,
    SourceApplyResult, SourceError, SourcePlan, SourceStatus,
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
            SourceError::Io(_) => "internal_error",
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
            apply_sources
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
