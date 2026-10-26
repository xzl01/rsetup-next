//! Thermal management domain of the hardware module tree.
use super::{
    HardwareError, HardwareManager,
    shared::{hardware_run, read_trimmed, with_rollback_result},
};
use crate::{
    ActionRun, ActionStatus,
    transaction::{ProcessLock, atomic_replace},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::Path, process::Command};

const THERMAL_POLICY_FILE: &str = "/etc/rsetup-next/thermal-policy";
const THERMAL_POLICY_UNIT: &str = "rsetup-next-thermal-policy.service";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThermalStatus {
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub supported: bool,
    pub pwm_fan_detected: bool,
    pub current_policy: Option<String>,
    pub persisted_policy: Option<String>,
    pub available_policies: Vec<String>,
    pub recommended_policy: Option<String>,
    pub zones: Vec<ThermalZone>,
    pub cooling_devices: Vec<CoolingDevice>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThermalZone {
    pub id: String,
    pub kind: String,
    pub temperature_c: Option<f32>,
    pub policy: Option<String>,
    pub available_policies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CoolingDevice {
    pub id: String,
    pub kind: String,
    pub current_state: Option<u32>,
    pub max_state: Option<u32>,
}

impl HardwareManager {
    pub(crate) fn thermal_status(&self) -> Result<ThermalStatus, HardwareError> {
        if self.synthetic {
            return Ok(demo_thermal_status());
        }
        let thermal_root = self.root.join("sys/class/thermal");
        let observed =
            deviceinfo::observe_thermal(&self.root, &deviceinfo::ThermalOptions::default());
        let mut zones: Vec<_> = observed
            .data
            .temperatures
            .iter()
            .filter(|sensor| sensor.origin == deviceinfo::TemperatureOrigin::ThermalZone)
            .map(|sensor| {
                let base = thermal_root.join(&sensor.device);
                ThermalZone {
                    id: sensor.device.clone(),
                    kind: sensor
                        .label
                        .clone()
                        .unwrap_or_else(|| sensor.device.clone()),
                    temperature_c: sensor.temperature_millicelsius.map(|v| v as f32 / 1000.0),
                    policy: read_trimmed(base.join("policy")),
                    available_policies: read_trimmed(base.join("available_policies"))
                        .map(|value| value.split_whitespace().map(str::to_owned).collect())
                        .unwrap_or_default(),
                }
            })
            .collect();
        let mut cooling_devices: Vec<_> = observed
            .data
            .cooling_devices
            .iter()
            .map(|device| CoolingDevice {
                id: device.device.clone(),
                kind: device.kind.clone().unwrap_or_else(|| device.device.clone()),
                current_state: device.current_state.and_then(|v| u32::try_from(v).ok()),
                max_state: device.maximum_state.and_then(|v| u32::try_from(v).ok()),
            })
            .collect();
        zones.sort_by(|left, right| left.id.cmp(&right.id));
        cooling_devices.sort_by(|left, right| left.id.cmp(&right.id));
        let pwm_fan_detected = cooling_devices.iter().any(|device| {
            device
                .kind
                .to_ascii_lowercase()
                .replace('_', "-")
                .contains("pwm-fan")
        });
        let available_policies = policy_intersection(&zones);
        let current_policy = zones.iter().find_map(|zone| zone.policy.clone());
        let persisted_policy =
            read_trimmed(self.root.join(THERMAL_POLICY_FILE.trim_start_matches('/')));
        let supported = !zones.is_empty() && !available_policies.is_empty();
        let recommended_policy = if pwm_fan_detected
            && available_policies
                .iter()
                .any(|policy| policy == "step_wise")
        {
            Some("step_wise".into())
        } else if available_policies
            .iter()
            .any(|policy| policy == "power_allocator")
        {
            Some("power_allocator".into())
        } else {
            available_policies.first().cloned()
        };
        Ok(ThermalStatus {
            collected_at: Utc::now(),
            synthetic: false,
            supported,
            pwm_fan_detected,
            current_policy,
            persisted_policy,
            available_policies,
            recommended_policy,
            zones,
            cooling_devices,
            unavailable_reason: (!supported)
                .then(|| "No writable thermal governor was detected.".into()),
        })
    }

    pub(crate) fn apply_thermal_policy_live(
        &self,
        policy: &str,
    ) -> Result<ActionRun, HardwareError> {
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-fan-curve.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
        if self.root.join("etc/rsetup-next/fan-curve.json").exists() {
            return Err(HardwareError::Conflict(
                "disable the fan curve before changing the thermal policy".into(),
            ));
        }
        validate_policy(policy)?;
        let status = self.thermal_status()?;
        if !status.supported {
            return Err(HardwareError::Unsupported(
                status
                    .unavailable_reason
                    .unwrap_or_else(|| "thermal control is unavailable".into()),
            ));
        }
        if !status.available_policies.iter().any(|item| item == policy) {
            return Err(HardwareError::InvalidInput(format!(
                "policy {policy} is not available on every thermal zone"
            )));
        }
        if policy == "power_allocator" && status.pwm_fan_detected {
            return Err(HardwareError::Conflict(
                "power_allocator is incompatible with the detected pwm-fan cooling device".into(),
            ));
        }
        let started_at = Utc::now();
        let previous = status
            .zones
            .iter()
            .filter_map(|zone| {
                zone.policy
                    .as_ref()
                    .map(|policy| (zone.id.clone(), policy.clone()))
            })
            .collect::<Vec<_>>();
        let enable_output = Command::new("systemctl")
            .args(["enable", THERMAL_POLICY_UNIT])
            .output()
            .map_err(|error| {
                HardwareError::Io(format!(
                    "unable to enable thermal policy persistence: {error}"
                ))
            })?;
        if !enable_output.status.success() {
            return Err(HardwareError::Io(format!(
                "unable to enable thermal policy persistence: {}",
                String::from_utf8_lossy(&enable_output.stderr).trim()
            )));
        }
        for zone in &status.zones {
            if !zone
                .available_policies
                .iter()
                .any(|available| available == policy)
            {
                continue;
            }
            let path = self
                .root
                .join("sys/class/thermal")
                .join(&zone.id)
                .join("policy");
            if let Err(error) = fs::write(&path, format!("{policy}\n")) {
                return Err(with_rollback_result(
                    HardwareError::Io(format!("unable to update {}: {error}", zone.id)),
                    restore_zone_policies(&self.root, &previous),
                ));
            }
        }
        if let Err(error) = write_thermal_policy(&self.root, policy) {
            return Err(with_rollback_result(
                error,
                restore_zone_policies(&self.root, &previous),
            ));
        }
        Ok(hardware_run(
            "hardware.thermal-policy",
            "Set fan and thermal policy",
            ActionStatus::Succeeded,
            false,
            "Thermal policy applied and saved for boot.",
            Some(format!("policy={policy}")),
            started_at,
        ))
    }

    pub(crate) fn restore_thermal_policy_live(&self) -> Result<ActionRun, HardwareError> {
        let _lock = ProcessLock::acquire(&self.root, "rsetup-next-fan-curve.lock")
            .map_err(|error| HardwareError::Io(error.to_string()))?;
        if self.root.join("etc/rsetup-next/fan-curve.json").exists() {
            return Err(HardwareError::Conflict(
                "disable the fan curve before changing the thermal policy".into(),
            ));
        }
        let path = self.root.join(THERMAL_POLICY_FILE.trim_start_matches('/'));
        let policy = read_trimmed(path)
            .ok_or_else(|| HardwareError::Unsupported("no saved thermal policy".into()))?;
        self.apply_thermal_policy_without_persistence(&policy)
    }

    fn apply_thermal_policy_without_persistence(
        &self,
        policy: &str,
    ) -> Result<ActionRun, HardwareError> {
        validate_policy(policy)?;
        let status = self.thermal_status()?;
        if policy == "power_allocator" && status.pwm_fan_detected {
            return Err(HardwareError::Conflict(
                "saved power_allocator policy is incompatible with pwm-fan".into(),
            ));
        }
        let started_at = Utc::now();
        for zone in &status.zones {
            if zone.available_policies.iter().any(|item| item == policy) {
                fs::write(
                    self.root
                        .join("sys/class/thermal")
                        .join(&zone.id)
                        .join("policy"),
                    format!("{policy}\n"),
                )
                .map_err(|error| HardwareError::Io(error.to_string()))?;
            }
        }
        Ok(hardware_run(
            "hardware.thermal-policy",
            "Restore fan and thermal policy",
            ActionStatus::Succeeded,
            false,
            "Saved thermal policy restored.",
            Some(format!("policy={policy}")),
            started_at,
        ))
    }
}

fn validate_policy(policy: &str) -> Result<(), HardwareError> {
    if policy.is_empty()
        || policy.len() > 64
        || !policy
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(HardwareError::InvalidInput("invalid thermal policy".into()));
    }
    Ok(())
}

fn restore_zone_policies(root: &Path, previous: &[(String, String)]) -> Result<(), HardwareError> {
    let mut errors = Vec::new();
    for (zone, policy) in previous {
        if let Err(error) = fs::write(
            root.join("sys/class/thermal").join(zone).join("policy"),
            format!("{policy}\n"),
        ) {
            errors.push(format!("{zone}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(HardwareError::Io(errors.join("; ")))
    }
}

fn write_thermal_policy(root: &Path, policy: &str) -> Result<(), HardwareError> {
    let path = root.join(THERMAL_POLICY_FILE.trim_start_matches('/'));
    let parent = path
        .parent()
        .ok_or_else(|| HardwareError::Io("invalid thermal policy path".into()))?;
    fs::create_dir_all(parent).map_err(|error| HardwareError::Io(error.to_string()))?;
    atomic_replace(&path, format!("{policy}\n").as_bytes())
        .map_err(|error| HardwareError::Io(error.to_string()))
}

fn policy_intersection(zones: &[ThermalZone]) -> Vec<String> {
    let mut iter = zones
        .iter()
        .filter(|zone| !zone.available_policies.is_empty())
        .map(|zone| {
            zone.available_policies
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
        });
    let Some(mut policies) = iter.next() else {
        return Vec::new();
    };
    for zone in iter {
        policies = policies.intersection(&zone).cloned().collect();
    }
    policies.into_iter().collect()
}

fn demo_thermal_status() -> ThermalStatus {
    ThermalStatus {
        collected_at: Utc::now(),
        synthetic: true,
        supported: true,
        pwm_fan_detected: true,
        current_policy: Some("user_space".into()),
        persisted_policy: Some("step_wise".into()),
        available_policies: vec![
            "bang_bang".into(),
            "power_allocator".into(),
            "step_wise".into(),
            "user_space".into(),
        ],
        recommended_policy: Some("step_wise".into()),
        zones: vec![
            ThermalZone {
                id: "thermal_zone0".into(),
                kind: "soc-thermal".into(),
                temperature_c: Some(54.8),
                policy: Some("step_wise".into()),
                available_policies: vec![
                    "bang_bang".into(),
                    "power_allocator".into(),
                    "step_wise".into(),
                    "user_space".into(),
                ],
            },
            ThermalZone {
                id: "thermal_zone1".into(),
                kind: "gpu-thermal".into(),
                temperature_c: Some(50.2),
                policy: Some("step_wise".into()),
                available_policies: vec![
                    "bang_bang".into(),
                    "power_allocator".into(),
                    "step_wise".into(),
                    "user_space".into(),
                ],
            },
            ThermalZone {
                id: "thermal_zone2".into(),
                kind: "npu-thermal".into(),
                temperature_c: Some(47.6),
                policy: Some("step_wise".into()),
                available_policies: vec![
                    "bang_bang".into(),
                    "power_allocator".into(),
                    "step_wise".into(),
                    "user_space".into(),
                ],
            },
        ],
        cooling_devices: vec![CoolingDevice {
            id: "cooling_device0".into(),
            kind: "pwm-fan".into(),
            current_state: Some(2),
            max_state: Some(4),
        }],
        unavailable_reason: None,
    }
}
