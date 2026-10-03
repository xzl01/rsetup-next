use crate::ControllerError;
use serde_json::Value;

pub fn validate_singleton_ids(ids: &[i8]) -> Result<(), ControllerError> {
    if ids == [1] {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

pub fn validate_device_fields(state: &str, decision: &str) -> Result<(), ControllerError> {
    if matches!(
        (state, decision),
        ("PENDING", "none" | "denied") | ("APPROVED", "approved") | ("REVOKED", "revoked")
    ) {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

pub fn validate_grant_fields(
    source: &str,
    has_role: bool,
    permissions: Option<&Value>,
    scope: &str,
    has_group: bool,
    has_device: bool,
) -> Result<(), ControllerError> {
    let valid_source = match source {
        "role" => has_role && permissions.is_none(),
        "direct" => {
            !has_role
                && permissions.and_then(Value::as_array).is_some_and(|values| {
                    let mut seen = std::collections::HashSet::new();
                    !values.is_empty()
                        && values.iter().all(|value| {
                            value.as_str().is_some_and(|permission| {
                                matches!(
                                    permission,
                                    "device.read"
                                        | "device.status.read"
                                        | "device.reboot"
                                        | "device.task.read"
                                ) && seen.insert(permission)
                            })
                        })
                })
        }
        _ => false,
    };
    let valid_scope = match scope {
        "all" => !has_group && !has_device,
        "group" => has_group && !has_device,
        "device" => !has_group && has_device,
        _ => false,
    };
    if valid_source && valid_scope {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn application_shapes_reject_invalid_combinations() {
        assert!(validate_singleton_ids(&[1]).is_ok());
        for ids in [&[][..], &[2][..], &[1, 2][..]] {
            assert!(validate_singleton_ids(ids).is_err(), "singleton: {ids:?}");
        }
        for (state, decision) in [
            ("PENDING", "none"),
            ("PENDING", "denied"),
            ("APPROVED", "approved"),
            ("REVOKED", "revoked"),
        ] {
            assert!(validate_device_fields(state, decision).is_ok());
        }
        assert!(validate_device_fields("DENIED", "denied").is_err());
        assert!(validate_device_fields("APPROVED", "none").is_err());
        let p = json!(["device.read"]);
        assert!(validate_grant_fields("role", true, None, "all", false, false).is_ok());
        assert!(validate_grant_fields("direct", false, Some(&p), "device", false, true).is_ok());
        assert!(validate_grant_fields("direct", true, Some(&p), "all", false, false).is_err());
        assert!(validate_grant_fields("role", true, None, "all", true, false).is_err());
        for bad in [
            json!(null),
            json!([]),
            json!({}),
            json!([1]),
            json!(["unknown"]),
            json!(["device.read", "device.read"]),
        ] {
            assert!(
                validate_grant_fields("direct", false, Some(&bad), "all", false, false).is_err()
            );
        }
    }

    #[test]
    fn device_accepts_only_four_exact_pairs() {
        for state in ["PENDING", "APPROVED", "REVOKED", "DENIED", "", "pending"] {
            for decision in ["none", "approved", "denied", "revoked", "", "APPROVED"] {
                let valid = matches!(
                    (state, decision),
                    ("PENDING", "none" | "denied")
                        | ("APPROVED", "approved")
                        | ("REVOKED", "revoked")
                );
                assert_eq!(
                    validate_device_fields(state, decision).is_ok(),
                    valid,
                    "{state}/{decision}"
                );
            }
        }
    }

    #[test]
    fn grant_source_sql_null_and_permission_directory() {
        let all = json!([
            "device.read",
            "device.status.read",
            "device.reboot",
            "device.task.read"
        ]);
        assert!(validate_grant_fields("direct", false, Some(&all), "all", false, false).is_ok());
        let valid = json!(["device.reboot"]);
        for (source, role, permissions, expected) in [
            ("role", true, None, true),
            ("role", false, None, false),
            ("role", true, Some(&valid), false),
            ("direct", false, Some(&valid), true),
            ("direct", true, Some(&valid), false),
            ("direct", false, None, false),
            ("", false, Some(&valid), false),
            ("ROLE", true, None, false),
            ("other", true, None, false),
        ] {
            assert_eq!(
                validate_grant_fields(source, role, permissions, "all", false, false).is_ok(),
                expected,
                "{source}/{role}/{permissions:?}"
            );
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({}),
            json!("device.read"),
            json!([null]),
            json!([1]),
            json!([true]),
            json!(["unknown"]),
            json!(["Device.read"]),
            json!(["device.read", "device.read"]),
        ] {
            assert!(
                validate_grant_fields("direct", false, Some(&invalid), "all", false, false)
                    .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn grant_scope_requires_exactly_matching_id_presence() {
        let permissions = json!(["device.task.read"]);
        for scope in ["all", "group", "device", "", "ALL", "unknown"] {
            for has_group in [false, true] {
                for has_device in [false, true] {
                    let valid = matches!(
                        (scope, has_group, has_device),
                        ("all", false, false) | ("group", true, false) | ("device", false, true)
                    );
                    assert_eq!(
                        validate_grant_fields(
                            "direct",
                            false,
                            Some(&permissions),
                            scope,
                            has_group,
                            has_device
                        )
                        .is_ok(),
                        valid,
                        "{scope}/{has_group}/{has_device}"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_inputs_have_invalid_argument_error() {
        assert!(matches!(
            validate_singleton_ids(&[]),
            Err(ControllerError::InvalidArgument)
        ));
        assert!(matches!(
            validate_device_fields("PENDING", "approved"),
            Err(ControllerError::InvalidArgument)
        ));
        assert!(matches!(
            validate_grant_fields("direct", false, None, "all", false, false),
            Err(ControllerError::InvalidArgument)
        ));
    }
}
