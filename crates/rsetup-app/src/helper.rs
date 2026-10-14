use anyhow::{Context, Result, bail};
use rsetup_core::{
    Controller, ExecutionPolicy, FanCurveRequest, ProbeMode, RgbLedConfig, SpiFlashRequest,
    SystemInstallManager, SystemInstallRequest,
};
use std::env;

#[derive(Debug, PartialEq)]
enum HelperRequest {
    OverlaysInspect,
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
    SystemInstallApply {
        target_id: String,
        image_path: String,
        plan_token: String,
        acknowledgement: String,
    },
}

fn main() -> Result<()> {
    if !cfg!(target_os = "linux") || effective_uid() != Some(0) {
        bail!("rsetup-next-helper must run as root through Polkit");
    }

    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let controller = Controller::new(ProbeMode::Live, ExecutionPolicy::Live);
    let response = match parse_request(&arguments)? {
        HelperRequest::OverlaysInspect => serde_json::to_value(controller.overlay_status()?)?,
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
        HelperRequest::SystemInstallApply {
            target_id,
            image_path,
            plan_token,
            acknowledgement,
        } => {
            // The caller's token is the preview the operator was shown, not an
            // authorization: its signature was made by another process and cannot
            // be checked here. `apply_privileged` checks that the state still
            // matches it, then issues and consumes this process's own token.
            let manager = SystemInstallManager::new(None, false);
            let request = SystemInstallRequest {
                target_id,
                image_path: Some(image_path),
                image_id: None,
            };
            serde_json::to_value(apply_privileged(
                &manager,
                &request,
                &plan_token,
                &acknowledgement,
            )?)?
        }
    };

    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn parse_request(arguments: &[String]) -> Result<HelperRequest> {
    match arguments {
        [command] if command == "overlays-inspect" => Ok(HelperRequest::OverlaysInspect),
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
        [
            command,
            target_id,
            image_path,
            plan_token,
            acknowledgement,
            confirmation,
        ] if command == "system-install-apply" && confirmation == "--confirmed" => {
            Ok(HelperRequest::SystemInstallApply {
                target_id: target_id.clone(),
                image_path: image_path.clone(),
                plan_token: plan_token.clone(),
                acknowledgement: acknowledgement.clone(),
            })
        }
        [command] if command == "led-restore" => Ok(HelperRequest::LedRestore),
        _ => bail!("invalid helper request; expected a fixed native operation"),
    }
}

/// The privileged apply, split out so it can be tested without root: check the
/// caller's preview, then issue and consume this process's own token.
///
/// The caller's token is advisory (see `verify_preview_token`): a lie in it only
/// lets an older preview through a request that is rebuilt, re-gated and
/// re-confirmed here, and the typed acknowledgement is still judged by the core.
fn apply_privileged(
    manager: &SystemInstallManager,
    request: &SystemInstallRequest,
    caller_token: &str,
    acknowledgement: &str,
) -> Result<rsetup_core::SystemInstallApplyResult, rsetup_core::SystemInstallError> {
    manager.verify_preview_token(request, caller_token)?;
    let plan = manager.plan(request)?;
    manager.apply_live(request, &plan.plan_token, acknowledgement)
}

fn effective_uid() -> Option<u32> {
    // SAFETY: geteuid has no preconditions and does not depend on PATH.
    Some(unsafe { libc::geteuid() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch directory under `target/`, removed when the test ends.
    struct HelperScratch(std::path::PathBuf);

    impl Drop for HelperScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn helper_scratch(name: &str) -> HelperScratch {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/helper-install-tests")
            .join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create the scratch directory");
        HelperScratch(root)
    }

    /// A minimal image with a valid MBR.
    fn helper_image_bytes(size: usize) -> Vec<u8> {
        let mut image = vec![0u8; size];
        image[510] = 0x55;
        image[511] = 0xAA;
        image[446 + 4] = 0x83;
        image[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        image[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
        image
    }

    /// The privileged flow without root and without a device: the demo provider
    /// counts writes instead of storing them, so the flow itself can be checked.
    #[test]
    fn the_privileged_apply_checks_the_preview_then_uses_its_own_token() {
        let scratch = helper_scratch("privileged-apply");
        let image = scratch.0.join("system.img");
        std::fs::write(&image, helper_image_bytes(2 * 1024 * 1024)).expect("write the image");
        let manager = SystemInstallManager::new(None, true);
        let request = SystemInstallRequest {
            target_id: "sda".into(),
            image_path: Some(image.display().to_string()),
            image_id: None,
        };
        // The preview the operator was shown, made by another process (here: by
        // another manager, which is the same thing for token purposes).
        let preview = SystemInstallManager::new(None, true)
            .plan(&request)
            .expect("preview");

        let result = apply_privileged(&manager, &request, &preview.plan_token, "sda")
            .expect("the privileged apply succeeds for an unchanged state");
        assert_eq!(result.plan.target.id, "sda");
        assert!(result.run.synthetic, "the demo provider is labelled");

        // The typed name is judged here too, not only in the previewing process.
        assert!(matches!(
            apply_privileged(&manager, &request, &preview.plan_token, "true"),
            Err(rsetup_core::SystemInstallError::AcknowledgementMismatch { .. })
        ));
        assert!(matches!(
            apply_privileged(&manager, &request, &preview.plan_token, "mmcblk0"),
            Err(rsetup_core::SystemInstallError::AcknowledgementMismatch { .. })
        ));

        // A preview that no longer matches the state is refused.
        std::fs::write(&image, helper_image_bytes(4 * 1024 * 1024)).expect("change the image");
        assert!(matches!(
            apply_privileged(&manager, &request, &preview.plan_token, "sda"),
            Err(rsetup_core::SystemInstallError::StalePlan)
        ));

        // A token that is not a token, or one whose fields were edited, is refused
        // as well: the caller cannot mint authority by editing what it carries.
        for forged in [
            "install-plan-0000000000000000-0000000000-0000000000000000-0000000000000000000000000000000000000000000000000000000000000000000000",
            "",
            "not-a-token",
        ] {
            assert!(
                matches!(
                    apply_privileged(&manager, &request, forged, "sda"),
                    Err(rsetup_core::SystemInstallError::StalePlan)
                ),
                "{forged:?} must be refused"
            );
        }
    }

    #[test]
    fn helper_protocol_has_no_arbitrary_command_mode() {
        assert_eq!(
            parse_request(&["overlays-inspect".into()]).unwrap(),
            HelperRequest::OverlaysInspect
        );
        assert!(parse_request(&["overlays-inspect".into(), "/arbitrary/path".into()]).is_err());
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
        assert_eq!(
            parse_request(&[
                "system-install-apply".into(),
                "sda".into(),
                "/home/radxa/radxa.img.xz".into(),
                "install-plan-0123456789abcdef".into(),
                "sda".into(),
                "--confirmed".into(),
            ])
            .unwrap(),
            HelperRequest::SystemInstallApply {
                target_id: "sda".into(),
                image_path: "/home/radxa/radxa.img.xz".into(),
                plan_token: "install-plan-0123456789abcdef".into(),
                acknowledgement: "sda".into(),
            }
        );
        // The verb is only accepted in its exact shape: no extra arguments, and
        // the confirmation marker is required.
        for rejected in [
            vec![
                "system-install-apply",
                "sda",
                "/home/radxa/radxa.img",
                "install-plan-0",
                "sda",
            ],
            vec![
                "system-install-apply",
                "sda",
                "/home/radxa/radxa.img",
                "install-plan-0",
                "sda",
                "--confirmed",
                "extra",
            ],
            vec!["system-install-apply", "--confirmed"],
            vec![
                "system-install-apply",
                "sda",
                "/home/radxa/radxa.img",
                "install-plan-0",
            ],
            vec![
                "system-install-apply",
                "sda",
                "/home/radxa/radxa.img",
                "install-plan-0",
                "sda",
                "--yes",
            ],
        ] {
            assert!(
                parse_request(
                    &rejected
                        .iter()
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                )
                .is_err(),
                "{rejected:?} must be rejected"
            );
        }
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
