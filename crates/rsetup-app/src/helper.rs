use anyhow::{Context, Result, bail};
use rsetup_core::{Controller, ExecutionPolicy, ProbeMode};
use std::{env, process::Command};

#[derive(Debug, PartialEq, Eq)]
enum HelperRequest {
    Action(String),
    SourcesApply {
        provider_id: String,
        plan_token: String,
    },
    OverlaysApply {
        selected_ids: Vec<String>,
        plan_token: String,
    },
    ThermalApply(String),
    ThermalRestore,
}

fn main() -> Result<()> {
    if !cfg!(target_os = "linux") || effective_uid() != Some(0) {
        bail!("rsetup-next-helper must run as root through Polkit");
    }

    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let controller = Controller::new(ProbeMode::Live, ExecutionPolicy::Live);
    let response = match parse_request(&arguments)? {
        HelperRequest::Action(action_id) => {
            serde_json::to_value(controller.execute(&action_id, true)?)?
        }
        HelperRequest::SourcesApply {
            provider_id,
            plan_token,
        } => serde_json::to_value(controller.apply_source_change(
            &provider_id,
            &plan_token,
            true,
        )?)?,
        HelperRequest::OverlaysApply {
            selected_ids,
            plan_token,
        } => serde_json::to_value(controller.apply_overlay_change(
            &selected_ids,
            &plan_token,
            true,
        )?)?,
        HelperRequest::ThermalApply(policy) => {
            serde_json::to_value(controller.apply_thermal_policy(&policy, true)?)?
        }
        HelperRequest::ThermalRestore => {
            serde_json::to_value(controller.restore_thermal_policy()?)?
        }
    };

    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn parse_request(arguments: &[String]) -> Result<HelperRequest> {
    match arguments {
        [command, action_id, confirmation]
            if command == "action" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::Action(action_id.clone()))
        }
        [command, provider_id, plan_token, confirmation]
            if command == "sources-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::SourcesApply {
                provider_id: provider_id.clone(),
                plan_token: plan_token.clone(),
            })
        }
        [command, selected, plan_token, confirmation]
            if command == "overlays-apply" && confirmation == "--confirmed" =>
        {
            let selected_ids = selected
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect();
            Ok(HelperRequest::OverlaysApply {
                selected_ids,
                plan_token: plan_token.clone(),
            })
        }
        [command, policy, confirmation]
            if command == "thermal-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::ThermalApply(policy.clone()))
        }
        [command] if command == "thermal-restore" => Ok(HelperRequest::ThermalRestore),
        _ => bail!("invalid helper request; expected a fixed native operation"),
    }
}

fn effective_uid() -> Option<u32> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("unable to inspect effective user")
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_protocol_has_no_arbitrary_command_mode() {
        assert!(parse_request(&["shell".into(), "id".into()]).is_err());
        assert!(
            parse_request(&[
                "action".into(),
                "system.inspect".into(),
                "--confirmed".into(),
                "extra".into(),
            ])
            .is_err()
        );
        assert_eq!(
            parse_request(&[
                "action".into(),
                "system.inspect".into(),
                "--confirmed".into(),
            ])
            .unwrap(),
            HelperRequest::Action("system.inspect".into())
        );
        assert_eq!(
            parse_request(&[
                "overlays-apply".into(),
                "uart.dtbo,spi.dtbo".into(),
                "plan-token".into(),
                "--confirmed".into(),
            ])
            .unwrap(),
            HelperRequest::OverlaysApply {
                selected_ids: vec!["uart.dtbo".into(), "spi.dtbo".into()],
                plan_token: "plan-token".into(),
            }
        );
        assert!(
            parse_request(&[
                "thermal-apply".into(),
                "step_wise".into(),
                "--confirmed".into(),
                "extra".into(),
            ])
            .is_err()
        );
    }
}
