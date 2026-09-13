use crate::{HealthState, MmcHealth, NvmeSmartLog, TelemetryReadState, TelemetryStatus};

pub fn nvme_health_state(telemetry: &TelemetryStatus, smart: Option<&NvmeSmartLog>) -> HealthState {
    if telemetry.state != TelemetryReadState::Available {
        return HealthState::Unknown;
    }
    let Some(smart) = smart else {
        return HealthState::Unknown;
    };
    if smart.critical_warning != 0 {
        return HealthState::Critical;
    }
    if !smart.warning_flags.is_empty() {
        return HealthState::Warning;
    }
    HealthState::Healthy
}

pub fn mmc_health_state(telemetry: &TelemetryStatus, health: &MmcHealth) -> HealthState {
    if telemetry.state != TelemetryReadState::Available {
        return HealthState::Unknown;
    }
    let exceeded = [
        health.life_time_est_a_percent,
        health.life_time_est_b_percent,
    ]
    .into_iter()
    .flatten()
    .any(|p| p > 100);
    let at_limit = [
        health.life_time_est_a_percent,
        health.life_time_est_b_percent,
    ]
    .into_iter()
    .flatten()
    .any(|p| p == 100);
    let urgent_flag = health.warning_flags.iter().any(|f| {
        matches!(
            f.as_str(),
            "pre_eol_urgent" | "life_time_typ_a_exceeded" | "life_time_typ_b_exceeded"
        )
    });
    if health.pre_eol_info == 3 || exceeded || urgent_flag {
        return HealthState::Critical;
    }
    if health.pre_eol_info == 2 || at_limit || !health.warning_flags.is_empty() {
        return HealthState::Warning;
    }
    if health.pre_eol_info == 1
        || health.life_time_est_a_percent.is_some()
        || health.life_time_est_b_percent.is_some()
    {
        return HealthState::Healthy;
    }
    HealthState::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> TelemetryStatus {
        TelemetryStatus {
            state: TelemetryReadState::Available,
            error: None,
        }
    }

    #[test]
    fn mmc_health_state_keeps_real_pre_eol_warning() {
        let h = MmcHealth {
            pre_eol_info: 2,
            life_time_est_a_percent: Some(10),
            life_time_est_b_percent: Some(10),
            warning_flags: crate::mmc::generate_warning_flags(2, Some(10), Some(10)),
        };
        assert_eq!(mmc_health_state(&ready(), &h), HealthState::Warning);
        assert_eq!(
            mmc_health_state(
                &ready(),
                &MmcHealth {
                    pre_eol_info: 3,
                    ..h
                }
            ),
            HealthState::Critical
        );
    }

    #[test]
    fn missing_data_is_not_healthy() {
        let absent = TelemetryStatus::default();
        assert_eq!(nvme_health_state(&absent, None), HealthState::Unknown);
        assert_eq!(
            mmc_health_state(&absent, &MmcHealth::default()),
            HealthState::Unknown
        );
        assert_eq!(nvme_health_state(&ready(), None), HealthState::Unknown);
        assert_eq!(
            mmc_health_state(&ready(), &MmcHealth::default()),
            HealthState::Unknown
        );
    }

    #[test]
    fn nvme_health_state_is_table_driven_and_unknown_bits_are_critical() {
        let cases = [
            (
                "critical warning bit 0x01",
                0x01,
                vec![],
                HealthState::Critical,
            ),
            (
                "critical warning unknown bit 0x80",
                0x80,
                vec![],
                HealthState::Critical,
            ),
            (
                "known warning flag",
                0,
                vec!["available_spare"],
                HealthState::Warning,
            ),
            (
                "unknown warning flag",
                0,
                vec!["unknown"],
                HealthState::Warning,
            ),
            ("normal", 0, vec![], HealthState::Healthy),
        ];
        for (name, critical_warning, warning_flags, expected) in cases {
            let smart = NvmeSmartLog {
                critical_warning,
                warning_flags: warning_flags.into_iter().map(String::from).collect(),
                ..Default::default()
            };
            assert_eq!(
                nvme_health_state(&ready(), Some(&smart)),
                expected,
                "{name}"
            );
        }
        for state in [
            TelemetryReadState::Unsupported,
            TelemetryReadState::Unavailable,
        ] {
            let smart = NvmeSmartLog {
                critical_warning: 0x80,
                warning_flags: vec!["unknown".into()],
                ..Default::default()
            };
            assert_eq!(
                nvme_health_state(&TelemetryStatus { state, error: None }, Some(&smart)),
                HealthState::Unknown,
                "{state:?} with residual data"
            );
        }
    }

    #[test]
    fn mmc_health_state_is_table_driven_and_symmetric() {
        let cases = [
            ("all unknown", MmcHealth::default(), HealthState::Unknown),
            (
                "only A valid",
                MmcHealth {
                    life_time_est_a_percent: Some(10),
                    ..Default::default()
                },
                HealthState::Healthy,
            ),
            (
                "only B valid",
                MmcHealth {
                    life_time_est_b_percent: Some(10),
                    ..Default::default()
                },
                HealthState::Healthy,
            ),
            (
                "pre-EOL normal",
                MmcHealth {
                    pre_eol_info: 1,
                    ..Default::default()
                },
                HealthState::Healthy,
            ),
            (
                "A at limit",
                MmcHealth {
                    life_time_est_a_percent: Some(100),
                    ..Default::default()
                },
                HealthState::Warning,
            ),
            (
                "B at limit",
                MmcHealth {
                    life_time_est_b_percent: Some(100),
                    ..Default::default()
                },
                HealthState::Warning,
            ),
            (
                "A exceeded",
                MmcHealth {
                    life_time_est_a_percent: Some(101),
                    ..Default::default()
                },
                HealthState::Critical,
            ),
            (
                "B exceeded",
                MmcHealth {
                    life_time_est_b_percent: Some(101),
                    ..Default::default()
                },
                HealthState::Critical,
            ),
            (
                "unknown flag",
                MmcHealth {
                    warning_flags: vec!["unknown".into()],
                    ..Default::default()
                },
                HealthState::Warning,
            ),
            (
                "urgent flag",
                MmcHealth {
                    warning_flags: vec!["pre_eol_urgent".into()],
                    ..Default::default()
                },
                HealthState::Critical,
            ),
            (
                "A exceeded flag",
                MmcHealth {
                    warning_flags: vec!["life_time_typ_a_exceeded".into()],
                    ..Default::default()
                },
                HealthState::Critical,
            ),
            (
                "B exceeded flag",
                MmcHealth {
                    warning_flags: vec!["life_time_typ_b_exceeded".into()],
                    ..Default::default()
                },
                HealthState::Critical,
            ),
        ];
        for (name, health, expected) in cases {
            assert_eq!(mmc_health_state(&ready(), &health), expected, "{name}");
        }
        for state in [
            TelemetryReadState::Unsupported,
            TelemetryReadState::Unavailable,
        ] {
            let residual = MmcHealth {
                pre_eol_info: 3,
                life_time_est_a_percent: Some(101),
                life_time_est_b_percent: Some(101),
                warning_flags: vec!["pre_eol_urgent".into()],
            };
            assert_eq!(
                mmc_health_state(&TelemetryStatus { state, error: None }, &residual),
                HealthState::Unknown,
                "{state:?} with residual data"
            );
        }
    }

    #[test]
    fn telemetry_and_health_models_serialize_with_contract_names() {
        assert_eq!(
            serde_json::to_string(&TelemetryStatus::default()).unwrap(),
            r#"{"state":"unavailable","error":null}"#
        );
        assert_eq!(
            serde_json::to_string(&HealthState::default()).unwrap(),
            r#""unknown""#
        );
        let error = crate::TelemetryError {
            kind: crate::TelemetryErrorKind::NvmeStatus,
            code: Some(7),
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"kind":"nvme_status","code":7}"#
        );
    }
}
