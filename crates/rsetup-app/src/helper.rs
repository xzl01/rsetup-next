use anyhow::{Context, Result, bail};
use rsetup_core::{
    Controller, ExecutionPolicy, FanCurveRequest, ProbeMode, RgbLedConfig, SpiFlashRequest,
    SystemInstallManager, SystemInstallRequest,
};
use std::env;
use std::ffi::{OsStr, OsString};

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

/// A failure the caller can classify without parsing prose.
///
/// The helper is a separate process, so an error has to cross the boundary as
/// data: a non-zero exit plus anyhow text on stderr flattens every distinct
/// refusal (`stale_plan`, `acknowledgement_mismatch`, an unknown verb) into one
/// generic code at the caller. The envelope below keeps the stable code the core
/// already assigned, and the prose stays as the human explanation.
struct HelperFailure {
    code: &'static str,
    message: String,
}

impl From<anyhow::Error> for HelperFailure {
    fn from(error: anyhow::Error) -> Self {
        // A typed core error carries its own stable code; anything else is a
        // helper-level failure with no finer classification to preserve.
        if let Some(install) = error.downcast_ref::<rsetup_core::SystemInstallError>() {
            return Self {
                code: install.code(),
                message: error.to_string(),
            };
        }
        Self {
            code: "helper_failed",
            message: error.to_string(),
        }
    }
}

fn main() {
    // 必须使用 args_os()：args() 在非法 UTF-8 argv 上于标准库内部 unwrap() panic（退出码 101），
    // 把携带原始字节的 panic 原文写入 stderr，既绕过下面的 JSON 错误信封，又回显操作员/上游输入。
    // args_os() 永不 panic，非法字节的处理交给 parse_request 的无损解码统一折叠为固定错误。
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    match run(&arguments) {
        Ok(response) => match serde_json::to_string(&response) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => emit_failure(&HelperFailure {
                code: "helper_failed",
                message: error.to_string(),
            }),
        },
        Err(error) => emit_failure(&HelperFailure::from(error)),
    }
}

/// The error envelope: the stable code plus the human explanation.
fn failure_envelope(failure: &HelperFailure) -> serde_json::Value {
    serde_json::json!({"error": {"code": failure.code, "message": failure.message}})
}

/// Reports a failure and exits non-zero.
///
/// The envelope goes to stdout, where a caller looks for the result, so a caller
/// that understands it can classify the refusal. The same prose also goes to
/// stderr, because the callers that predate the envelope — the source, overlay,
/// SPI, fan and LED operations in `rsetup-core` — report `stderr` as the reason
/// and would otherwise collapse every one of them into a bare exit status.
fn emit_failure(failure: &HelperFailure) {
    println!("{}", failure_envelope(failure));
    eprintln!("{}", failure.message);
    std::process::exit(1);
}

/// 参数非法时的固定错误文案（保持与原实现完全一致）：纯 ASCII，绝不含任何收到的 token。
const INVALID_REQUEST: &str = "invalid helper request; expected a fixed native operation";

/// 把一个业务参数从 `OsStr` 无损转成 `String`。
///
/// 只有当参数是合法 UTF-8 时才继续；任何非法 Unicode 一律返回同一条固定 ASCII 错误。绝不
/// `Debug`/`Display` 原始 `OsStr`，也绝不 `to_string_lossy` 后带着替换符继续危险操作 —— 与命令词
/// 精确比较不同，业务参数最终要落到底层动作，因此宁可拒绝也不能对字节做任何有损猜测。
fn text(value: &OsStr) -> Result<String> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{INVALID_REQUEST}"))
}

fn run(arguments: &[OsString]) -> Result<serde_json::Value> {
    if !cfg!(target_os = "linux") || effective_uid() != Some(0) {
        bail!("rsetup-next-helper must run as root through Polkit");
    }
    let controller = Controller::new(ProbeMode::Live, ExecutionPolicy::Live);
    let response = match parse_request(arguments)? {
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
    Ok(response)
}

fn parse_request(arguments: &[OsString]) -> Result<HelperRequest> {
    match arguments {
        [command] if command == "overlays-inspect" => Ok(HelperRequest::OverlaysInspect),
        [command, action_id, confirmation]
            if command == "action" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::Action(text(action_id)?))
        }
        [command, provider_id, plan_token, confirmation]
            if command == "sources-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::SourcesApply {
                provider_id: text(provider_id)?,
                plan_token: text(plan_token)?,
            })
        }
        [command, selected, plan_token, confirmation]
            if command == "overlays-apply" && confirmation == "--confirmed" =>
        {
            let selected = text(selected)?;
            let selected_ids = selected
                .split(',')
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect();
            Ok(HelperRequest::OverlaysApply {
                selected_ids,
                plan_token: text(plan_token)?,
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
            // "-" 是「无镜像」哨兵，只作 OsStr 精确比较；其余镜像 id 无损解码，非法字节直接拒绝。
            let image_id = if image_id.as_os_str() == OsStr::new("-") {
                None
            } else {
                Some(text(image_id)?)
            };
            Ok(HelperRequest::SpiFlashApply {
                request: SpiFlashRequest {
                    operation: text(operation)?,
                    target_id: text(target_id)?,
                    image_id,
                },
                plan_token: text(plan_token)?,
            })
        }
        [command, policy, confirmation]
            if command == "thermal-apply" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::ThermalApply(text(policy)?))
        }
        [command, request, plan_token, confirmation]
            if command == "fan-curve-apply" && confirmation == "--confirmed" =>
        {
            let request = text(request)?;
            Ok(HelperRequest::FanCurveApply {
                request: serde_json::from_str(&request).context("invalid fan curve request")?,
                plan_token: text(plan_token)?,
            })
        }
        [command] if command == "thermal-restore" => Ok(HelperRequest::ThermalRestore),
        [command, led_id, trigger, confirmation]
            if command == "led-trigger" && confirmation == "--confirmed" =>
        {
            Ok(HelperRequest::LedTrigger {
                led_id: text(led_id)?,
                trigger: text(trigger)?,
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
            let red = text(red)?;
            let green = text(green)?;
            let blue = text(blue)?;
            let brightness = text(brightness)?;
            let cycle_ms = text(cycle_ms)?;
            Ok(HelperRequest::LedRgb(RgbLedConfig {
                group_id: text(group_id)?,
                mode: text(mode)?,
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
                target_id: text(target_id)?,
                image_path: text(image_path)?,
                plan_token: text(plan_token)?,
                acknowledgement: text(acknowledgement)?,
            })
        }
        [command] if command == "led-restore" => Ok(HelperRequest::LedRestore),
        _ => bail!("{INVALID_REQUEST}"),
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

    /// A core refusal must reach the caller as the same code the other surfaces
    /// use, not as one generic helper failure.
    #[test]
    fn a_core_refusal_keeps_its_stable_code_in_the_envelope() {
        let failure = HelperFailure::from(anyhow::Error::from(
            rsetup_core::SystemInstallError::StalePlan,
        ));
        assert_eq!(failure.code, "stale_plan");
        let envelope = failure_envelope(&failure);
        assert_eq!(envelope["error"]["code"], "stale_plan");
        assert!(
            !envelope["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "the prose is kept as the explanation"
        );

        // Anything the core did not classify is a helper-level failure.
        let failure = HelperFailure::from(anyhow::anyhow!("invalid helper request"));
        assert_eq!(failure.code, "helper_failed");
        assert_eq!(failure_envelope(&failure)["error"]["code"], "helper_failed");
    }

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
            fan_curve_json.clone().into(),
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
                        .map(|value| OsString::from(*value))
                        .collect::<Vec<_>>()
                )
                .is_err(),
                "{rejected:?} must be rejected"
            );
        }
        assert!(
            parse_request(&[
                "fan-curve-apply".into(),
                fan_curve_json.into(),
                "curve-plan-token".into(),
                "--confirmed".into(),
                "extra".into(),
            ])
            .is_err()
        );
    }

    /// Unix argv 可以是任意非 NUL 字节。这里用 `from_vec` 构造真实含非法 UTF-8 的 OsString，
    /// 直接驱动 `parse_request`（不读环境、不受 root 门禁影响），证明非法字节既不 panic 也不回显，
    /// 并在任何真实特权动作之前被拒绝。
    #[cfg(unix)]
    #[test]
    fn non_utf8_os_arguments_are_rejected_without_panic_or_echo() {
        use std::os::unix::ffi::OsStringExt as _;

        let secret = OsString::from_vec(b"FAKE_CLI_SECRET_ONLY\xff".to_vec());
        let cases: Vec<Vec<OsString>> = vec![
            // 命令词位非法：不匹配任何固定动词。
            vec![secret.clone()],
            // 业务参数位非法：解码失败 → 同一条固定 ASCII 错误，绝不 lossy。
            vec![
                OsString::from("action"),
                secret.clone(),
                OsString::from("--confirmed"),
            ],
            vec![
                OsString::from("overlays-apply"),
                OsString::from("uart.dtbo"),
                secret.clone(),
                OsString::from("--confirmed"),
            ],
            vec![
                OsString::from("led-trigger"),
                OsString::from("status"),
                secret.clone(),
                OsString::from("--confirmed"),
            ],
            vec![
                OsString::from("system-install-apply"),
                OsString::from("sda"),
                secret.clone(),
                OsString::from("install-plan-0"),
                OsString::from("sda"),
                OsString::from("--confirmed"),
            ],
            // 纯非法字节混排。
            vec![OsString::from_vec(vec![0xff, 0xfe, 0x80])],
        ];
        for argv in cases {
            let error = parse_request(&argv).expect_err("非 UTF-8 参数必须被拒绝");
            assert_failure_is_ascii_without_secret(&error);
        }
    }

    /// `text()` 对纯非法字节直接返回固定错误，绝不返回带替换符的字符串；合法 UTF-8（含正常多字节
    /// 明文）则无损通过。
    #[cfg(unix)]
    #[test]
    fn text_never_lossy_decodes_non_utf8() {
        use std::os::unix::ffi::OsStringExt as _;

        let raw = OsString::from_vec(b"FAKE_CLI_SECRET_ONLY\xff".to_vec());
        let error = text(&raw).expect_err("非法 UTF-8 必须拒绝");
        assert_failure_is_ascii_without_secret(&error);
        assert_eq!(
            text(OsStr::new("valid-\u{4e2d}\u{6587}")).unwrap(),
            "valid-中文",
            "合法多字节 UTF-8 必须无损保留"
        );
    }

    #[track_caller]
    fn assert_failure_is_ascii_without_secret(error: &anyhow::Error) {
        let message = error.to_string();
        assert_eq!(message, INVALID_REQUEST, "错误必须是纯 ASCII 固定文案");
        assert!(message.is_ascii());
        assert!(!message.contains("FAKE_CLI_SECRET_ONLY"), "参数不得回显");
        assert!(!message.contains('\u{FFFD}'), "不得含 lossy 替换符");
        // Debug 同样不得带出原始字节（token 未被保留）。
        assert!(!format!("{error:?}").contains("FAKE_CLI_SECRET_ONLY"));
    }
}
