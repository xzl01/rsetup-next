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
        _ => bail!("invalid helper request; expected a fixed action or an exact source plan"),
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
    }
}
