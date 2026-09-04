use anyhow::{Context, Result, bail};
use rsetup_core::{
    Controller, ExecutionPolicy, FanCurveRequest, ProbeMode, RgbLedConfig, SpiFlashRequest,
};
use std::{env, process::Command};

#[derive(Debug, PartialEq)]
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
    SpiFlashApply {
        request: SpiFlashRequest,
        plan_token: String,
    },
    ThermalApply(String),
    FanCurveApply {
        request: FanCurveRequest,
        plan_token: String,
    },
    ThermalRestore,
    LedTrigger {
        led_id: String,
        trigger: String,
    },
    LedRgb(RgbLedConfig),
    LedRestore,
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
        HelperRequest::SpiFlashApply {
            request,
            plan_token,
        } => serde_json::to_value(controller.apply_spi_flash(&request, &plan_token, true)?)?,
        HelperRequest::ThermalApply(policy) => {
            serde_json::to_value(controller.apply_thermal_policy(&policy, true)?)?
        }
        HelperRequest::FanCurveApply {
            request,
            plan_token,
        } => serde_json::to_value(controller.apply_fan_curve(&request, &plan_token, true)?)?,
        HelperRequest::ThermalRestore => {
            serde_json::to_value(controller.restore_thermal_policy()?)?
        }
        HelperRequest::LedTrigger { led_id, trigger } => {
            serde_json::to_value(controller.apply_led_trigger(&led_id, &trigger, true)?)?
        }
        HelperRequest::LedRgb(config) => {
            serde_json::to_value(controller.apply_rgb_led(&config, true)?)?
        }
        HelperRequest::LedRestore => serde_json::to_value(controller.restore_led_state()?)?,
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
        [
            command,
            operation,
            target_id,
            image_id,
            plan_token,
            confirmation,
        ] if command == "spi-flash-apply" && confirmation == "--confirmed" => {
            Ok(HelperRequest::SpiFlashApply {
                request: SpiFlashRequest {
                    operation: operation.clone(),
                    target_id: target_id.clone(),
                    image_id: (image_id != "-").then(|| image_id.clone()),
                },
                plan_token: plan_token.clone(),
            })
        }
        [command, policy, confirmation]
            if command == "thermal-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::ThermalApply(policy.clone()))
        }
        [command, request, plan_token, confirmation]
            if command == "fan-curve-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::FanCurveApply {
                request: serde_json::from_str(request).context("invalid fan curve request")?,
                plan_token: plan_token.clone(),
            })
        }
        [command] if command == "thermal-restore" => Ok(HelperRequest::ThermalRestore),
        [command, led_id, trigger, confirmation]
            if command == "led-trigger" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::LedTrigger {
                led_id: led_id.clone(),
                trigger: trigger.clone(),
            })
        }
        [
            command,
            group_id,
            mode,
            red,
            green,
            blue,
            brightness,
            cycle_ms,
            confirmation,
        ] if command == "led-rgb" && confirmation == "--confirmed" => {
            Ok(HelperRequest::LedRgb(RgbLedConfig {
                group_id: group_id.clone(),
                mode: mode.clone(),
                red: red.parse().context("invalid red channel")?,
                green: green.parse().context("invalid green channel")?,
                blue: blue.parse().context("invalid blue channel")?,
                brightness: brightness.parse().context("invalid brightness")?,
                cycle_ms: cycle_ms.parse().context("invalid cycle")?,
            }))
        }
        [command] if command == "led-restore" => Ok(HelperRequest::LedRestore),
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
                "led-trigger".into(),
                "status".into(),
                "heartbeat".into(),
                "--confirmed".into(),
            ])
            .unwrap(),
            HelperRequest::LedTrigger {
                led_id: "status".into(),
                trigger: "heartbeat".into(),
            }
        );
        assert!(
            parse_request(&[
                "led-rgb".into(),
                "rgb0".into(),
                "solid".into(),
                "255".into(),
                "0".into(),
                "0".into(),
                "101".into(),
                "5000".into(),
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
        assert_eq!(
            parse_request(&[
                "spi-flash-apply".into(),
                "install".into(),
                "mtd0".into(),
                "rock-5b-rk3588:rockchip-rk35".into(),
                "spi-plan-token".into(),
                "--confirmed".into(),
            ])
            .unwrap(),
            HelperRequest::SpiFlashApply {
                request: SpiFlashRequest {
                    operation: "install".into(),
                    target_id: "mtd0".into(),
                    image_id: Some("rock-5b-rk3588:rockchip-rk35".into()),
                },
                plan_token: "spi-plan-token".into(),
            }
        );
        assert!(
            parse_request(&[
                "spi-flash-apply".into(),
                "erase".into(),
                "mtd0".into(),
                "-".into(),
                "spi-plan-token".into(),
                "--confirmed".into(),
                "extra".into(),
            ])
            .is_err()
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
        let fan_curve_json = serde_json::json!({
            "enabled": true,
            "config": {
                "zoneId": "thermal_zone0",
                "coolingDeviceId": "cooling_device0",
                "pollIntervalMs": 2000,
                "hysteresisC": 2.0,
                "points": [
                    {"temperatureC": 40.0, "speedPercent": 20},
                    {"temperatureC": 82.0, "speedPercent": 100}
                ]
            }
        })
        .to_string();
        let request = parse_request(&[
            "fan-curve-apply".into(),
            fan_curve_json.clone(),
            "curve-plan-token".into(),
            "--confirmed".into(),
        ])
        .unwrap();
        assert!(matches!(
            request,
            HelperRequest::FanCurveApply {
                request: FanCurveRequest { enabled: true, .. },
                plan_token,
            } if plan_token == "curve-plan-token"
        ));
        assert!(
            parse_request(&[
                "fan-curve-apply".into(),
                fan_curve_json,
                "curve-plan-token".into(),
                "--confirmed".into(),
                "extra".into(),
            ])
            .is_err()
        );
    }
}
