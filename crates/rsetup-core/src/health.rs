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
        assert_eq!(
            nvme_health_state(
                &TelemetryStatus {
                    state: TelemetryReadState::Unavailable,
                    error: None,
                },
                Some(&NvmeSmartLog::default())
            ),
            HealthState::Unknown
        );
        assert_eq!(
            mmc_health_state(&ready(), &MmcHealth {
                pre_eol_info: 1,
                ..Default::default()
            }),
            HealthState::Healthy
        );
    }
    #[test]
    fn nvme_health_state_applies_warning_and_critical_priority() {
        let warning = NvmeSmartLog {
            warning_flags: vec!["unknown".into()],
            ..Default::default()
        };
        assert_eq!(
            nvme_health_state(&ready(), Some(&warning)),
            HealthState::Warning
        );
        let critical = NvmeSmartLog {
            critical_warning: 1,
            ..warning
        };
        assert_eq!(
            nvme_health_state(&ready(), Some(&critical)),
            HealthState::Critical
        );
        assert_eq!(
            nvme_health_state(&ready(), Some(&NvmeSmartLog::default())),
            HealthState::Healthy
        );
    }
    #[test]
    fn mmc_health_state_handles_boundaries_and_unknown_flags() {
        for (value, expected) in [(100, HealthState::Warning), (101, HealthState::Critical)] {
            let h = MmcHealth {
                life_time_est_a_percent: Some(value),
                ..Default::default()
            };
            assert_eq!(mmc_health_state(&ready(), &h), expected);
        }
        let h = MmcHealth {
            warning_flags: vec!["unknown".into()],
            ..Default::default()
        };
        assert_eq!(mmc_health_state(&ready(), &h), HealthState::Warning);
        assert_eq!(
            mmc_health_state(
                &TelemetryStatus {
                    state: TelemetryReadState::Unsupported,
                    error: None
                },
                &h
            ),
            HealthState::Unknown
        );
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
