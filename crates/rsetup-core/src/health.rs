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

    #[test]
    fn real_generate_warning_flags_produces_consistent_health_state_and_serialization() {
        use crate::mmc::generate_warning_flags;
        use crate::model::MmcDevice;

        let ready = TelemetryStatus {
            state: TelemetryReadState::Available,
            error: None,
        };

        // 1. pre_eol=2 (warning), 0x0A (100% endurance, not exceeded)
        let flags_0a = generate_warning_flags(2, Some(100), Some(50));
        assert_eq!(flags_0a, vec!["pre_eol_warning".to_string()]);
        let health_0a = MmcHealth {
            pre_eol_info: 2,
            life_time_est_a_percent: Some(100),
            life_time_est_b_percent: Some(50),
            warning_flags: flags_0a.clone(),
        };
        let state_0a = mmc_health_state(&ready, &health_0a);
        assert_eq!(state_0a, HealthState::Warning);

        let dev_0a = MmcDevice {
            name: "mmcblk0".into(),
            block_path: "/dev/mmcblk0".into(),
            card_type: "MMC".into(),
            model: "EMMC_TEST".into(),
            manufacturer: "Test".into(),
            serial: "123".into(),
            firmware: "1".into(),
            total_bytes: 64_000_000_000,
            health: health_0a,
            telemetry: ready.clone(),
            health_state: state_0a,
        };
        let json_0a = serde_json::to_string(&dev_0a).unwrap();
        assert!(json_0a.contains(r#""healthState":"warning""#));
        assert!(json_0a.contains(r#""warningFlags":["pre_eol_warning"]"#));
        assert!(json_0a.contains(r#""lifeTimeEstAPercent":100"#));

        // 2. 0x0B (101% endurance -> life_time_typ_a_exceeded -> Critical)
        let flags_0b = generate_warning_flags(1, Some(101), Some(50));
        assert_eq!(flags_0b, vec!["life_time_typ_a_exceeded".to_string()]);
        let health_0b = MmcHealth {
            pre_eol_info: 1,
            life_time_est_a_percent: Some(101),
            life_time_est_b_percent: Some(50),
            warning_flags: flags_0b.clone(),
        };
        let state_0b = mmc_health_state(&ready, &health_0b);
        assert_eq!(state_0b, HealthState::Critical);

        let dev_0b = MmcDevice {
            name: "mmcblk0".into(),
            block_path: "/dev/mmcblk0".into(),
            card_type: "MMC".into(),
            model: "EMMC_TEST".into(),
            manufacturer: "Test".into(),
            serial: "123".into(),
            firmware: "1".into(),
            total_bytes: 64_000_000_000,
            health: health_0b,
            telemetry: ready,
            health_state: state_0b,
        };
        let json_0b = serde_json::to_string(&dev_0b).unwrap();
        assert!(json_0b.contains(r#""healthState":"critical""#));
        assert!(json_0b.contains(r#""warningFlags":["life_time_typ_a_exceeded"]"#));
        assert!(json_0b.contains(r#""lifeTimeEstAPercent":101"#));
    }
}
